use super::*;

#[test]
fn nested_sampler_lowering_fits_an_ordinary_worker_stack() {
    let mut expr = SExpression::Number(1.0);
    let mut expected = OirExpr::number(1.0);
    for _ in 0..128 {
        expr = call("if", vec![
            SExpression::Bool(true),
            call("+", vec![SExpression::Number(2.0), expr]),
            SExpression::Number(0.0),
        ]);
        expected = OirExpr::if_then_else(
            OirExpr::bool(true), OirExpr::add(OirExpr::number(2.0), expected),
            OirExpr::number(0.0),
        );
    }
    let lowered = std::thread::Builder::new()
        .name("lowering-ordinary-stack".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || lower_sexpr(&expr))
        .expect("ordinary worker starts")
        .join().expect("ordinary worker finishes")
        .expect("sampler expression lowers");
    assert_eq!(lowered, expected);
}

#[test]
fn syntax_names_are_not_lowered_as_children_and_a_declared_shape_is_kept_as_text() {
    let expr = call("let", vec![
        SExpression::Array(vec![SExpression::Array(vec![
            SExpression::string("if"), SExpression::Number(7.0), SExpression::string("{\"type\":\"number\"}"),
        ].into())].into()),
        SExpression::string("@local.if"),
    ]);
    let expected = OirExpr::call("let", vec![
        OirExpr::array(vec![OirExpr::array(vec![
            OirExpr::string("if"), OirExpr::number(7.0), OirExpr::string("{\"type\":\"number\"}"),
        ])]),
        OirExpr::binding(OirBinding::parse("@local.if").expect("local binding")),
    ]);
    assert_eq!(lower_sexpr(&expr), Ok(expected));
    let pair = call("let", vec![
        SExpression::Array(vec![SExpression::Array(vec![SExpression::string("if"), SExpression::Number(7.0)].into())].into()),
        SExpression::string("@local.if"),
    ]);
    assert!(lower_sexpr(&pair).is_ok(), "control: a pair lowers");
    let malformed = call("let", vec![
        SExpression::Array(vec![SExpression::Array(vec![
            SExpression::string("x"), SExpression::Number(7.0), SExpression::Number(1.0),
        ].into())].into()),
        SExpression::string("@local.x"),
    ]);
    assert!(lower_sexpr(&malformed).is_err());
}

#[test]
fn child_failures_keep_the_canonical_argument_order() {
    let left = call("if", vec![SExpression::Bool(true)]);
    let right = call("not", vec![]);
    let first = lower_sexpr(&left).expect_err("left child is malformed");
    assert_eq!(lower_sexpr(&call("+", vec![left, right])), Err(first));
}

#[test]
fn synthesized_beacon_and_batch_requests_keep_their_lowered_contracts() {
    let beacon: SExpression = serde_json::from_value(serde_json::json!([
        "browser/beacon", "https://example.com/e",
        {"text":"@entity.text", "key":"@entity.key"},
        {"key":"@payload.key", "emit":{"success":"SENT"}}
    ])).expect("beacon input");
    let OirExpr::Effect { effect } = lower_sexpr(&beacon).expect("beacon lowers") else {
        panic!("beacon effect");
    };
    let OirEffect::BrowserBeacon { request, key, emit, .. } = *effect else {
        panic!("beacon effect");
    };
    let mut fields = orbital_core_base::ir::expr::OirFields::default();
    fields.insert("text".into(), OirExpr::binding(OirBinding::parse("@entity.text").unwrap()));
    assert_eq!(request, OirExpr::object(fields));
    assert_eq!(key, Some(OirExpr::binding(OirBinding::parse("@payload.key").unwrap())));
    assert_eq!(emit.and_then(|routes| routes.success), Some("SENT".into()));

    let batch: SExpression = serde_json::from_value(serde_json::json!([
        "persist", "batch", [["create", "Task", {"id":"@entity.id"}]]
    ])).expect("batch input");
    let OirExpr::Effect { effect } = lower_sexpr(&batch).expect("batch lowers") else {
        panic!("batch effect");
    };
    let OirEffect::PersistBatch { ops, entities, .. } = *effect else {
        panic!("batch effect");
    };
    assert_eq!(entities, vec!["Task"]);
    let mut row = orbital_core_base::ir::expr::OirFields::default();
    row.insert("id".into(), OirExpr::binding(OirBinding::parse("@entity.id").unwrap()));
    assert_eq!(ops, OirExpr::array(vec![OirExpr::call("list", vec![
        OirExpr::string("create"), OirExpr::string("Task"), OirExpr::object(row),
    ])]));
}

#[test]
fn synthesized_http_pagination_keeps_quoted_callbacks_and_request_fields() {
    let raw: SExpression = serde_json::from_value(serde_json::json!([
        "http", "GET", "https://example.com", "/items",
        {"headers":{"x-id":"@entity.id"}, "paginate":{
            "next":["get", "@response", "next"],
            "collect":["quote", "[]"], "maxPages":3
        }}
    ])).expect("http input");
    let OirExpr::Effect { effect } = lower_sexpr(&raw).expect("http lowers") else {
        panic!("http effect");
    };
    let OirEffect::Http { request: Some(OirExpr::Object { fields }), .. } = *effect else {
        panic!("http request object");
    };
    let OirExpr::Object { fields: paginate } = &fields["paginate"] else {
        panic!("pagination object");
    };
    assert_eq!(paginate["next"], OirExpr::call("quote", vec![
        OirExpr::string(r#"["get","\u0040response","next"]"#),
    ]));
    assert_eq!(paginate["collect"], OirExpr::call("quote", vec![OirExpr::string("[]")]));
    assert_eq!(paginate["maxPages"], OirExpr::number(3.0));
    let OirExpr::Object { fields: headers } = &fields["headers"] else {
        panic!("headers object");
    };
    assert_eq!(headers["x-id"], OirExpr::binding(OirBinding::parse("@entity.id").unwrap()));
}

fn call(op: &str, args: Vec<SExpression>) -> SExpression {
    let mut items = vec![SExpression::string(op)];
    items.extend(args);
    SExpression::Array(items.into())
}

fn lowered_log_level(args: Vec<SExpression>) -> Result<orbital_core_base::ir::effects::OirLogLevel, LowerError> {
    match lower_sexpr(&call("log", args))? {
        OirExpr::Effect { effect } => match *effect {
            OirEffect::Log { level, .. } => Ok(level),
            other => panic!("not a log effect: {other:?}"),
        },
        other => panic!("not an effect: {other:?}"),
    }
}

#[test]
fn a_log_lowers_its_trailing_level() {
    use orbital_core_base::ir::effects::OirLogLevel;
    let level = |l: &str| lowered_log_level(vec![SExpression::string("m"), SExpression::string(l)]).unwrap();
    assert_eq!(level("debug"), OirLogLevel::Debug);
    assert_eq!(level("warn"), OirLogLevel::Warn);
    assert_eq!(level("error"), OirLogLevel::Error);
}

#[test]
fn control_a_log_without_a_level_lowers_as_info() {
    let level = lowered_log_level(vec![SExpression::string("m")]).unwrap();
    assert_eq!(level, orbital_core_base::ir::effects::OirLogLevel::Info);
}

#[test]
fn a_log_level_outside_the_four_is_refused() {
    assert!(lowered_log_level(vec![SExpression::string("m"), SExpression::string("loud")]).is_err());
    assert!(lowered_log_level(vec![SExpression::string("m"), SExpression::Number(1.0)]).is_err());
}

#[test]
fn an_operator_call_lowers_and_keeps_its_bindings() {
    let lowered = lower_sexpr(&call(
        "+",
        vec![
            SExpression::Number(1.0),
            SExpression::string("@entity.count"),
        ],
    ))
    .expect("lowers");
    assert!(lowered
        .collect_bindings()
        .iter()
        .any(|b| b.root == OirBindingRoot::Entity));
}

fn bindings_of(op: &str, n: usize) -> Result<Vec<String>, LowerError> {
    let args = (0..n).map(|i| SExpression::string(format!("@entity.a{i}"))).collect();
    let lowered = lower_sexpr(&call(op, args))?;
    let mut names: Vec<String> = lowered.collect_bindings().iter().map(|b| b.path.join(".")).collect();
    names.sort();
    Ok(names)
}

#[test]
fn an_n_ary_sum_or_product_lowers_every_operand() {
    for op in ["+", "*"] {
        assert_eq!(bindings_of(op, 4).expect("lowers"), ["a0", "a1", "a2", "a3"], "{op}");
    }
}

#[test]
fn an_n_ary_sum_folds_left_as_the_kernel_adds() {
    let lowered = lower_sexpr(&call("+", vec![SExpression::Number(9.0), SExpression::Number(4.0), SExpression::Number(2.0)]))
        .expect("lowers");
    let OirExpr::BinaryOp { op: OirBinaryOp::Add, left, right } = lowered else {
        panic!("a sum: {lowered:?}");
    };
    assert!(matches!(*right, OirExpr::Number { value } if value == 2.0), "{right:?}");
    assert!(matches!(*left, OirExpr::BinaryOp { op: OirBinaryOp::Add, .. }), "(9 + 4) + 2: {left:?}");
}

#[test]
fn control_a_difference_quotient_or_remainder_of_three_operands_is_refused_by_its_declared_arity() {
    for op in ["-", "/", "%"] {
        assert!(bindings_of(op, 3).is_err(), "{op}");
    }
}

#[test]
fn control_binary_arithmetic_of_two_operands_is_unchanged() {
    for op in ["+", "-", "*", "/", "%"] {
        assert_eq!(bindings_of(op, 2).expect("lowers"), ["a0", "a1"], "{op}");
    }
}

fn fetch_with(options: serde_json::Value) -> OirEffect {
    let expr: SExpression =
        serde_json::from_value(serde_json::json!(["fetch", "Doc", options])).unwrap();
    match lower_sexpr(&expr).expect("lowers") {
        OirExpr::Effect { effect } => *effect,
        other => panic!("not an effect: {other:?}"),
    }
}

#[test]
fn a_fetch_nearest_option_lowers_with_its_bindings_and_without_it_the_field_is_absent() {
    let OirEffect::Fetch { nearest, .. } = fetch_with(serde_json::json!({
        "nearest": { "field": "embedding", "vector": "@entity.embedding", "k": 3 }
    })) else {
        panic!("not a fetch")
    };
    let nearest = nearest.expect("nearest lowered");
    assert!(nearest
        .collect_bindings()
        .iter()
        .any(|b| b.root == OirBindingRoot::Entity));
    // control: no option, no field
    let OirEffect::Fetch { nearest, .. } = fetch_with(serde_json::json!({ "limit": 2 })) else {
        panic!("not a fetch")
    };
    assert!(nearest.is_none());
}

#[test]
fn control_a_literal_lowers_to_itself() {
    assert_eq!(
        lower_sexpr(&SExpression::Bool(true)),
        Ok(OirExpr::bool(true))
    );
}

#[test]
fn a_malformed_call_is_an_invalid_sexpr_error() {
    let error = lower_sexpr(&call("if", vec![SExpression::Bool(true)])).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("Failed to resolve S-expression: "),
        "{error}"
    );
}

#[test]
fn the_list_marker_is_stripped_and_nothing_else() {
    let marked = [SExpression::string("list"), SExpression::Number(1.0)];
    assert_eq!(strip_literal_list_marker(&marked), &marked[1..]);
    let bare = [SExpression::string("+"), SExpression::Number(1.0)];
    assert_eq!(strip_literal_list_marker(&bare), &bare[..]);
}

#[test]
fn a_marked_batch_tuple_is_an_op_tuple_and_lowers_with_one_marker() {
    use orbital_core_base::persist_batch::{is_op_tuple, op_tuple};
    let marked: SExpression = serde_json::from_value(serde_json::json!(["list", "delete", "E", "@r.id"])).unwrap();
    let bare: SExpression = serde_json::from_value(serde_json::json!(["delete", "E", "@r.id"])).unwrap();
    assert!(is_op_tuple(&marked), "a data-marked tuple is an op tuple");
    assert_eq!(op_tuple(&marked).map(|t| t.len()), Some(3), "its items are read without the marker");
    assert!(is_op_tuple(&bare), "control: a bare tuple still is");
    let batch = |tuple: serde_json::Value| -> String {
        let call: SExpression = serde_json::from_value(serde_json::json!(["persist", "batch", ["array/map", "@payload.xs", ["fn", "r", tuple]]])).unwrap();
        format!("{:?}", lower_sexpr(&call).expect("lowers"))
    };
    let lowered = batch(serde_json::json!(["list", "delete", "E", "@r.id"]));
    assert!(!lowered.contains("\"list\", \"list\"") && lowered.matches("\"list\"").count() <= 1, "one marker: {lowered}");
    assert_eq!(lowered, batch(serde_json::json!(["delete", "E", "@r.id"])), "control: marked and bare tuples lower alike");
}

fn redis_call(trailing: Vec<serde_json::Value>) -> SExpression {
    let mut items = vec![serde_json::json!("redis"), serde_json::json!("redis://cache.internal"), serde_json::json!(["GET", "@payload.k"])];
    items.extend(trailing);
    serde_json::from_value(serde_json::Value::Array(items)).expect("s-expression")
}

#[test]
fn a_redis_call_lowers_to_the_redis_effect_with_options_key_and_routes() {
    let lowered = lower_sexpr(&redis_call(vec![
        serde_json::json!({ "auth": "@config.pw", "db": 1 }),
        serde_json::json!({ "key": "@payload.k", "emit": { "success": "GOT", "failure": "FAILED" } }),
    ]))
    .expect("lowers");
    let OirExpr::Effect { effect } = lowered else { panic!("not an effect: {lowered:?}") };
    let OirEffect::Redis { options, key, emit, command, .. } = *effect else { panic!("not redis") };
    assert!(matches!(options, Some(OirExpr::Object { ref fields }) if fields.contains_key("auth") && fields.contains_key("db") && !fields.contains_key("key")));
    assert!(key.is_some());
    let emit = emit.expect("routes");
    assert_eq!((emit.success.as_deref(), emit.failure.as_deref()), (Some("GOT"), Some("FAILED")));
    assert!(command.collect_bindings().iter().any(|b| b.root == OirBindingRoot::Payload));
}

#[test]
fn control_a_redis_call_with_no_trailing_object_has_no_options_key_or_routes() {
    let lowered = lower_sexpr(&redis_call(Vec::new())).expect("lowers");
    let OirExpr::Effect { effect } = lowered else { panic!("not an effect") };
    let OirEffect::Redis { options, key, emit, .. } = *effect else { panic!("not redis") };
    assert!(options.is_none() && key.is_none() && emit.is_none());
}

#[test]
fn a_postgres_call_lowers_with_params_options_key_and_routes_and_a_bare_one_has_none() {
    let call = |trailing: Vec<serde_json::Value>| {
        let mut items = vec![serde_json::json!("postgres"), serde_json::json!("postgres://db.internal"), serde_json::json!("SELECT $1::int"), serde_json::json!([1])];
        items.extend(trailing);
        serde_json::from_value::<SExpression>(serde_json::Value::Array(items)).expect("s-expression")
    };
    let lowered = lower_sexpr(&call(vec![
        serde_json::json!({ "auth": "@config.pw", "database": "main" }),
        serde_json::json!({ "key": "@payload.k", "emit": { "success": "ROWS", "failure": "FAILED" } }),
    ]))
    .expect("lowers");
    let OirExpr::Effect { effect } = lowered else { panic!("not an effect") };
    let OirEffect::Postgres { options, key, emit, params, .. } = *effect else { panic!("not postgres") };
    assert!(matches!(options, Some(OirExpr::Object { ref fields }) if fields.contains_key("auth") && !fields.contains_key("key")));
    assert!(key.is_some() && matches!(params, OirExpr::Array { .. }));
    assert_eq!(emit.expect("routes").success.as_deref(), Some("ROWS"));
    let OirExpr::Effect { effect } = lower_sexpr(&call(Vec::new())).expect("lowers") else { panic!("not an effect") };
    let OirEffect::Postgres { options, key, emit, .. } = *effect else { panic!("not postgres") };
    assert!(options.is_none() && key.is_none() && emit.is_none());
    let short: SExpression = serde_json::from_value(serde_json::json!(["postgres", "postgres://h", "SELECT 1"])).expect("s-expression");
    assert!(lower_sexpr(&short).is_err());
}

#[test]
fn a_redis_call_with_the_wrong_arity_or_a_non_object_trailer_is_invalid() {
    let bare: SExpression = serde_json::from_value(serde_json::json!(["redis", "redis://h"])).expect("s-expression");
    assert!(lower_sexpr(&bare).is_err());
    assert!(lower_sexpr(&redis_call(vec![serde_json::json!("x")])).is_err());
}

#[test]
fn a_browser_tab_effect_lowers_with_its_request_key_and_routes() {
    use orbital_core_base::tab::TabOp;
    let open: SExpression = serde_json::from_value(serde_json::json!([
        "browser/tab-open",
        { "url": "@config.searchUrl", "active": false },
        { "key": "@entity.id", "emit": { "success": "OPENED", "failure": "OPEN_FAILED" } }
    ]))
    .expect("s-expression");
    let OirExpr::Effect { effect } = lower_sexpr(&open).expect("lowers") else { panic!("not an effect") };
    let OirEffect::BrowserTab { op, request, key, emit } = *effect else { panic!("not a tab effect") };
    assert_eq!(op, TabOp::Open);
    assert!(matches!(request, OirExpr::Object { ref fields } if fields.contains_key("url") && fields.contains_key("active")));
    assert!(key.is_some());
    assert_eq!(emit.expect("routes").failure.as_deref(), Some("OPEN_FAILED"));
    for tab_op in TabOp::ALL {
        let bare: SExpression = serde_json::from_value(serde_json::json!([tab_op.name(), {}])).expect("s-expression");
        let OirExpr::Effect { effect } = lower_sexpr(&bare).expect("lowers") else { panic!("not an effect") };
        assert_eq!(effect.kind(), tab_op.name());
    }
}

#[test]
fn control_a_browser_tab_effect_without_a_request_is_invalid() {
    let none: SExpression = serde_json::from_value(serde_json::json!(["browser/tab-list"])).expect("s-expression");
    assert!(lower_sexpr(&none).is_err());
    let many: SExpression = serde_json::from_value(serde_json::json!(["browser/tab-list", {}, {}, {}])).expect("s-expression");
    assert!(lower_sexpr(&many).is_err());
}

#[test]
fn the_retired_oal_family_no_longer_resolves_to_an_operator() {
    use crate::evaluator::operators::get_operator;
    assert!(get_operator("oal/battery-status").is_none());
    assert!(get_operator("oal/cpu-usage").is_none());
    assert!(get_operator("os/path-join").is_some());
}

#[test]
fn identical_subtrees_at_different_positions_each_lower_in_place() {
    let expr: SExpression = serde_json::from_str(r#"["if", ["=", "@a", 1], ["+", ["-", 2, 3], ["-", 2, 3]], ["+", ["-", 2, 3], 9]]"#).unwrap();
    let product = || OirExpr::sub(OirExpr::number(2.0), OirExpr::number(3.0));
    let expected = OirExpr::if_then_else(
        OirExpr::eq(OirExpr::binding(OirBinding::parse("@a").expect("binding")), OirExpr::number(1.0)),
        OirExpr::add(product(), product()),
        OirExpr::add(product(), OirExpr::number(9.0)),
    );
    assert_eq!(lower_sexpr(&expr), Ok(expected));
}

#[test]
fn control_distinct_subtrees_lower_in_place() {
    let expr: SExpression = serde_json::from_str(r#"["+", ["-", 2, 3], ["-", 4, 5]]"#).unwrap();
    let product = |a: f64, b: f64| OirExpr::sub(OirExpr::number(a), OirExpr::number(b));
    assert_eq!(lower_sexpr(&expr), Ok(OirExpr::add(product(2.0, 3.0), product(4.0, 5.0))));
}
