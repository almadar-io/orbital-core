use super::*;

fn rewrite_input(json: serde_json::Value) -> SExpression {
    serde_json::from_value(json).unwrap()
}

#[test]
fn try_rewrite_rebuilds_bottom_up_in_walk_order_and_keeps_what_enter_keeps() {
    let expr = rewrite_input(serde_json::json!(["a", ["quote", ["a"]], {"k": ["a", 1], "j": "a"}, "b"]));
    let mut entered: Vec<SExpression> = Vec::new();
    let rewritten: Result<SExpression, ()> = expr.try_rewrite(
        |node| {
            entered.push(node.clone());
            Ok(if node.operator() == Some("quote") { RewriteVisit::Keep } else { RewriteVisit::Descend })
        },
        |node| Ok(if node.as_str() == Some("a") { SExpression::string("A") } else { node }),
    );
    assert_eq!(rewritten.unwrap(), rewrite_input(serde_json::json!(["A", ["quote", ["a"]], {"k": ["A", 1], "j": "A"}, "b"])));
    let order: Vec<String> = entered.iter().map(|n| serde_json::to_string(n).unwrap()).collect();
    assert_eq!(order[1], "\"a\"");
    assert_eq!(order[2], "[\"quote\",[\"a\"]]", "a kept node's children are never entered");
    assert_eq!(order[3], "{\"k\":[\"a\",1.0],\"j\":\"a\"}");
    assert_eq!(order[4], "[\"a\",1.0]", "an object's values are entered in key order, depth first");
    assert_eq!(order.last().unwrap(), "\"b\"");
}

#[test]
fn try_rewrite_stops_at_the_first_error_in_walk_order() {
    let expr = rewrite_input(serde_json::json!([["bad", 1], ["bad", 2]]));
    let failed = expr.try_rewrite(
        |_| Ok(RewriteVisit::Descend),
        |node| {
            let bad = match node.as_array().map(Vec::as_slice) {
                Some([SExpression::String(head), SExpression::Number(n)]) if head == "bad" => Some(*n),
                _ => None,
            };
            bad.map_or(Ok(node), Err)
        },
    );
    assert_eq!(failed, Err(1.0));
}

#[test]
fn try_rewrite_of_a_tree_deeper_than_a_small_stack_completes() {
    let mut deep = SExpression::Number(0.0);
    for _ in 0..100_000 {
        deep = SExpression::Array(vec![SExpression::string("do"), deep]);
    }
    let rewritten = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let out = deep.try_rewrite(|_| Ok::<_, ()>(RewriteVisit::Descend), Ok).expect("rewritten");
            let mut depth = 0usize;
            let mut at = &out;
            while let SExpression::Array(items) = at {
                depth += 1;
                at = &items[1];
            }
            let leaf = at.clone();
            // Comparing or dropping a tree this deep recurses, so both are leaked on purpose.
            std::mem::forget(out);
            std::mem::forget(deep);
            (depth, leaf)
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(rewritten, (100_000, SExpression::Number(0.0)));
}

#[test]
fn control_try_rewrite_of_a_leaf_is_the_leaf_after_exit() {
    let leaf = SExpression::Number(3.0);
    assert_eq!(leaf.try_rewrite(|_| Ok::<_, ()>(RewriteVisit::Descend), Ok), Ok(SExpression::Number(3.0)));
    assert_eq!(leaf.try_rewrite(|_| Ok::<_, ()>(RewriteVisit::Keep), |_| Err(())), Ok(SExpression::Number(3.0)));
}

#[test]
fn every_variant_reads_from_json_and_integers_are_numbers() {
    let expr: SExpression = serde_json::from_str(
        r#"[null, true, 3, -2, 1.5, 18446744073709551615, "@a.b", [], {"z": 1, "a": [2]}]"#,
    )
    .unwrap();
    let SExpression::Array(items) = expr else {
        panic!("an array");
    };
    assert_eq!(items[0], SExpression::Null);
    assert_eq!(items[1], SExpression::Bool(true));
    assert_eq!(items[2], SExpression::Number(3.0));
    assert_eq!(items[3], SExpression::Number(-2.0));
    assert_eq!(items[4], SExpression::Number(1.5));
    assert_eq!(items[5], SExpression::Number(18446744073709551615u64 as f64));
    assert_eq!(items[6], SExpression::string("@a.b"));
    assert_eq!(items[7], SExpression::Array(vec![]));
    let object = items[8].as_object().unwrap();
    assert_eq!(object.keys().collect::<Vec<_>>(), ["z", "a"]);
    assert_eq!(object["a"], SExpression::Array(vec![SExpression::Number(2.0)]));
}

#[test]
fn a_serialized_expression_reads_back_equal() {
    let json = r#"["and",["=","@x",1],{"k":[true,null,"s"]}]"#;
    let expr: SExpression = serde_json::from_str(json).unwrap();
    let again: SExpression = serde_json::from_str(&serde_json::to_string(&expr).unwrap()).unwrap();
    assert_eq!(expr, again);
}

#[test]
fn control_malformed_json_is_an_error_not_a_null() {
    assert!(serde_json::from_str::<SExpression>("[1, ").is_err());
    assert!(serde_json::from_str::<SExpression>("{1: 2}").is_err());
}

#[test]
fn test_parse_guard() {
    let json = r#"["and", ["!=", "@payload.data.email", ""], [">=", "@payload.data.connectionSlots", 1]]"#;
    let expr: SExpression = serde_json::from_str(json).unwrap();

    assert!(expr.is_call());
    assert_eq!(expr.operator(), Some("and"));

    let args = expr.arguments().unwrap();
    assert_eq!(args.len(), 2);

    // First arg: ["!=", "@payload.data.email", ""]
    assert!(args[0].is_call());
    assert_eq!(args[0].operator(), Some("!="));

    // Check binding
    let ne_args = args[0].arguments().unwrap();
    assert!(ne_args[0].is_binding());
    assert_eq!(ne_args[0].binding_path(), Some("payload.data.email"));
}

#[test]
fn test_parse_effect() {
    let json =
        r#"["render-ui", "main", { "type": "page-header", "title": "Community Members" }]"#;
    let expr: SExpression = serde_json::from_str(json).unwrap();

    assert!(expr.is_call());
    assert_eq!(expr.operator(), Some("render-ui"));

    let args = expr.arguments().unwrap();
    assert_eq!(args[0].as_str(), Some("main"));

    let props = args[1].as_object().unwrap();
    assert_eq!(
        props.get("type").and_then(|v| v.as_str()),
        Some("page-header")
    );
}

#[test]
fn test_parse_persist_effect() {
    let json = r#"["persist", "create", "User", "@payload.data"]"#;
    let expr: SExpression = serde_json::from_str(json).unwrap();

    assert_eq!(expr.operator(), Some("persist"));
    let args = expr.arguments().unwrap();
    assert_eq!(args[0].as_str(), Some("create"));
    assert_eq!(args[1].as_str(), Some("User"));
    assert!(args[2].is_binding());
}

#[test]
fn mapped_children_recognizes_single_param_tuple() {
    // ["array/map", "@entity.items", ["fn", "item", { type: badge, ... }]]
    let json = r#"["array/map", "@entity.items", ["fn", "item", { "type": "badge", "label": "@item.name" }]]"#;
    let expr: SExpression = serde_json::from_str(json).unwrap();
    let mc = expr
        .as_mapped_children()
        .expect("must recognize array/map tuple");
    assert_eq!(mc.collection.as_str(), Some("@entity.items"));
    assert_eq!(mc.params, vec!["item"]);
    assert_eq!(
        mc.item_node
            .as_object()
            .unwrap()
            .get("type")
            .and_then(|v| v.as_str()),
        Some("badge")
    );
}

#[test]
fn mapped_children_recognizes_grouped_param_form() {
    // Normalized grouped lambda: ["fn", ["q", "i"], node]
    let json = r#"["array/map", "@config.questions", ["fn", ["q", "i"], { "type": "card" }]]"#;
    let expr: SExpression = serde_json::from_str(json).unwrap();
    let mc = expr.as_mapped_children().expect("grouped param form");
    assert_eq!(mc.params, vec!["q", "i"]);
}

#[test]
fn mapped_children_rejects_non_map_arrays() {
    // Ordinary children entries / operator calls are NOT mapped-children.
    for json in [
        r#"{ "type": "badge" }"#,
        r#""@trait.Foo""#,
        r#"["array/filter", "@entity.items", ["fn", "item", true]]"#,
        r#"["array/map", "@entity.items"]"#,
        r#"["array/map", "@entity.items", { "type": "badge" }]"#,
    ] {
        let expr: SExpression = serde_json::from_str(json).unwrap();
        assert!(
            expr.as_mapped_children().is_none(),
            "must not recognize: {json}"
        );
    }
}

#[test]
fn test_complex_guard_from_orbital_pattern() {
    // From Orbital_Pattern.orb - Dunbar's number guard
    let json = r#"["<", ["count", "Connection", ["and", ["=", "@Connection.requesterId", "@user.id"], ["=", "@Connection.status", "accepted"]]], 150]"#;
    let expr: SExpression = serde_json::from_str(json).unwrap();

    assert_eq!(expr.operator(), Some("<"));
    let args = expr.arguments().unwrap();

    // First arg is count expression
    assert_eq!(args[0].operator(), Some("count"));

    // Second arg is 150
    assert_eq!(args[1].as_number(), Some(150.0));
}

#[test]
fn canonical_json_sorts_object_keys_so_emission_is_byte_stable() {
    // Build the SAME logical object many times. HashMap iteration order is
    // randomly seeded, so a non-canonical serializer disagrees with itself.
    let render = || {
        let mut map = SObject::default();
        map.insert("zebra".to_string(), SExpression::Number(1.0));
        map.insert("alpha".to_string(), SExpression::Bool(true));
        map.insert("middle".to_string(), SExpression::string("x"));
        let expr = SExpression::Array(vec![
            SExpression::string("="),
            SExpression::Object(map),
        ]);
        serde_json::to_string(&expr.to_canonical_json()).expect("serializes")
    };
    let first = render();
    for _ in 0..64 {
        assert_eq!(
            render(),
            first,
            "canonical JSON must not vary between renders"
        );
    }
    assert_eq!(first, r#"["=",{"alpha":true,"middle":"x","zebra":1.0}]"#);
}

#[test]
fn canonical_json_round_trips_nested_arrays_and_scalars() {
    let expr = SExpression::Array(vec![
        SExpression::string("or"),
        SExpression::Array(vec![
            SExpression::string("="),
            SExpression::string("@user.role"),
            SExpression::string("supervisor"),
        ]),
        SExpression::Null,
    ]);
    assert_eq!(
        serde_json::to_string(&expr.to_canonical_json()).expect("serializes"),
        r#"["or",["=","@user.role","supervisor"],null]"#
    );
}

fn test_lookup(op: &str) -> Option<OperatorReturn> {
    let (semantics, returns_function) = match op {
        "if" => (Some("branch-union"), false),
        "array/filter" | "array/slice" | "array/sort" => (Some("identity-of-arg<0>"), false),
        "array/concat" => (Some("union-of-args"), false),
        "array/append" => (Some("union-of-args<0,1>"), false),
        "array/insert" => (Some("union-of-args<0,2>"), false),
        "do" => (Some("last-of-args"), false),
        "let" => (Some("identity-of-arg<1>"), false),
        "array/map" => (Some("lambda-result"), false),
        "array/zip" => (None, false),
        "fn" => (None, true),
        _ => return None,
    };
    Some(OperatorReturn {
        semantics,
        returns_function,
    })
}

const WALK: ValueWalk<'static> = ValueWalk {
    lookup: &test_lookup,
    skip_dead_branches: false,
};

fn sx(json: &str) -> SExpression {
    serde_json::from_str(json).unwrap()
}

fn leaf_strings(expr: &SExpression) -> Vec<String> {
    expr.value_leaves(&WALK)
        .iter()
        .map(|l| serde_json::to_string(&l.to_canonical_json()).unwrap())
        .collect()
}

#[test]
fn value_leaves_returns_self_for_plain_and_undeclared() {
    assert_eq!(leaf_strings(&sx(r#""plain""#)), vec![r#""plain""#]);
    assert_eq!(leaf_strings(&sx(r#"["a", "b"]"#)), vec![r#"["a","b"]"#]);
    assert_eq!(
        leaf_strings(&sx(r#"{"event": "X"}"#)),
        vec![r#"{"event":"X"}"#]
    );
}

#[test]
fn value_leaves_branch_union_takes_every_branch_never_the_condition() {
    assert_eq!(
        leaf_strings(&sx(r#"["if", "@user.role", "T", "E"]"#)),
        vec![r#""T""#, r#""E""#]
    );
    assert_eq!(leaf_strings(&sx(r#"["if", "@c", "T"]"#)), vec![r#""T""#]);
    assert_eq!(
        leaf_strings(&sx(r#"["if", "@a", ["if", "@b", "x", "y"], "z"]"#)),
        vec![r#""x""#, r#""y""#, r#""z""#]
    );
    // no branch at all: opaque, exactly the call itself
    assert_eq!(leaf_strings(&sx(r#"["if", "@c"]"#)), vec![r#"["if","@c"]"#]);
}

#[test]
fn value_leaves_identity_kinds_see_through_subsets_and_reorders() {
    for op in ["array/filter", "array/slice", "array/sort"] {
        let call = format!(r#"["{op}", "LIST", 1, 2]"#);
        assert_eq!(leaf_strings(&sx(&call)), vec![r#""LIST""#], "{op}");
    }
    // nested: filter inside slice inside if
    assert_eq!(
        leaf_strings(&sx(
            r#"["array/slice", ["if", "@c", ["array/filter", "A", ["fn", "i", true]], "B"], 0, 3]"#
        )),
        vec![r#""A""#, r#""B""#]
    );
}

#[test]
fn value_leaves_union_of_args_covers_every_argument_or_the_listed_ones() {
    assert_eq!(
        leaf_strings(&sx(r#"["array/concat", "A", ["if", "@c", "B", "C"], "D"]"#)),
        vec![r#""A""#, r#""B""#, r#""C""#, r#""D""#]
    );
    // append: the list and the added element, never the extra args
    assert_eq!(
        leaf_strings(&sx(r#"["array/append", "LIST", {"event": "X"}]"#)),
        vec![r#""LIST""#, r#"{"event":"X"}"#]
    );
    // insert: arg 0 and arg 2, skipping the index at arg 1
    assert_eq!(
        leaf_strings(&sx(r#"["array/insert", "LIST", 4, "ELEM"]"#)),
        vec![r#""LIST""#, r#""ELEM""#]
    );
    // a listed arg that is absent makes the call opaque
    assert_eq!(
        leaf_strings(&sx(r#"["array/append", "LIST"]"#)),
        vec![r#"["array/append","LIST"]"#]
    );
}

#[test]
fn passthrough_arg_follows_only_identity_kinds() {
    let filtered = sx(r#"["array/filter", "LIST", ["fn", "i", true]]"#);
    assert_eq!(
        filtered.passthrough_arg(&WALK),
        Some(&SExpression::string("LIST"))
    );
    let bound = sx(r#"["let", [["x", 1]], "BODY"]"#);
    assert_eq!(
        bound.passthrough_arg(&WALK),
        Some(&SExpression::string("BODY"))
    );
    for other in [
        r#"["array/concat", "A", "B"]"#,
        r#"["if", "@c", "A", "B"]"#,
        r#"["array/zip", "A", "B"]"#,
        r#""plain""#,
        r#"["array/filter"]"#,
    ] {
        assert_eq!(sx(other).passthrough_arg(&WALK), None, "{other}");
    }
}

#[test]
fn value_leaves_last_of_args_and_body_kinds() {
    assert_eq!(
        leaf_strings(&sx(r#"["do", ["set", "@x", 1], "LAST"]"#)),
        vec![r#""LAST""#]
    );
    assert_eq!(
        leaf_strings(&sx(r#"["let", [["x", 1]], ["if", "@c", "P", "Q"]]"#)),
        vec![r#""P""#, r#""Q""#]
    );
    assert_eq!(leaf_strings(&sx(r#"["do"]"#)), vec![r#"["do"]"#]);
}

#[test]
fn value_leaves_lambda_result_yields_the_lambda_body_leaves() {
    assert_eq!(
        leaf_strings(&sx(
            r#"["array/map", "@entity.items", ["fn", "item", ["if", "@item.ok", {"event": "A"}, {"event": "B"}]]]"#
        )),
        vec![r#"{"event":"A"}"#, r#"{"event":"B"}"#]
    );
    // no lambda argument: opaque
    assert_eq!(
        leaf_strings(&sx(r#"["array/map", "@entity.items", "@config.fn"]"#)),
        vec![r#"["array/map","@entity.items","@config.fn"]"#]
    );
}

#[test]
fn value_leaves_undeclared_operator_is_opaque_even_over_a_declared_one() {
    let zip = sx(r#"["array/zip", ["array/filter", "A", ["fn", "i", true]], "B"]"#);
    assert_eq!(
        leaf_strings(&zip),
        vec![serde_json::to_string(&zip.to_canonical_json()).unwrap()]
    );
    // a declared wrapper around an undeclared call ends at the undeclared call
    let wrapped = sx(r#"["array/slice", ["array/zip", "A", "B"], 0, 2]"#);
    assert_eq!(leaf_strings(&wrapped), vec![r#"["array/zip","A","B"]"#]);
}

#[test]
fn value_leaves_skip_dead_branches_only_when_asked() {
    let expr = sx(r#"["if", false, "DEAD", "LIVE"]"#);
    assert_eq!(leaf_strings(&expr), vec![r#""DEAD""#, r#""LIVE""#]);
    let folding = ValueWalk {
        lookup: &test_lookup,
        skip_dead_branches: true,
    };
    assert_eq!(
        expr.value_leaves(&folding),
        vec![&SExpression::string("LIVE")]
    );
    let only_then = sx(r#"["if", "", "DEAD"]"#);
    assert!(only_then.value_leaves(&folding).is_empty());
}

#[test]
fn value_leaves_mut_reaches_every_source_and_allows_rewrite() {
    let mut expr = sx(
        r#"["array/concat", ["array/filter", "A", ["fn", "i", true]], ["if", "@c", "B", "C"], ["array/map", "M", ["fn", "x", "BODY"]]]"#,
    );
    for leaf in expr.value_leaves_mut(&WALK) {
        if let SExpression::String(s) = leaf {
            *s = format!("rw-{s}").into();
        }
    }
    assert_eq!(
        expr,
        sx(
            r#"["array/concat", ["array/filter", "rw-A", ["fn", "i", true]], ["if", "@c", "rw-B", "rw-C"], ["array/map", "M", ["fn", "x", "rw-BODY"]]]"#
        )
    );
}

#[test]
fn json_value_leaves_mirror_the_sexpression_walk() {
    let value = serde_json::json!(["array/concat",
        ["array/slice", [{"event": "A"}], 0, 1],
        ["if", "@c", [{"event": "B"}], [{"event": "C"}]],
        ["array/map", "M", ["fn", "x", {"event": "D"}]],
        ["array/zip", [{"event": "NOPE"}], []]
    ]);
    let events: Vec<String> = json_value_leaves(&value, &WALK)
        .iter()
        .filter_map(|l| {
            l.as_array()
                .and_then(|a| a.first())
                .or(Some(l))
                .and_then(|d| d.get("event"))
                .and_then(|e| e.as_str())
                .map(str::to_string)
        })
        .collect();
    assert_eq!(events, vec!["A", "B", "C", "D"]);
}

#[test]
fn json_value_leaves_mut_rewrites_in_place_and_skips_undeclared() {
    let mut value = serde_json::json!(["array/concat",
        ["array/sort", [{"action": "X"}], "k", "asc"],
        ["array/zip", [{"action": "Y"}], []]
    ]);
    for leaf in json_value_leaves_mut(&mut value, &WALK) {
        if let Some(arr) = leaf.as_array_mut() {
            for d in arr.iter_mut() {
                if let Some(a) = d.get("action").and_then(|a| a.as_str()) {
                    d["action"] = serde_json::json!(format!("T.{a}"));
                }
            }
        }
    }
    assert_eq!(value[1][1][0]["action"], "T.X");
    assert_eq!(value[2][1][0]["action"], "Y");
}

#[test]
fn is_statically_true_is_the_dual_of_is_statically_false() {
    assert!(SExpression::Bool(true).is_statically_true());
    assert!(eq("==", lit("create"), lit("create")).is_statically_true());
    assert!(eq("!=", lit("a"), lit("b")).is_statically_true());
    assert!(!eq("==", lit("a"), lit("b")).is_statically_true());
    assert!(!eq("==", lit("@config.mode"), lit("create")).is_statically_true(), "a binding is undecidable");
    let or = SExpression::Array(vec![
        lit("or"),
        eq("==", lit("create"), lit("create")),
        lit("@payload.row"),
    ]);
    assert!(or.is_statically_true(), "one operand provably true");
    assert!(!or.is_statically_false());
    let and = SExpression::Array(vec![lit("and"), SExpression::Bool(true), lit("@payload.row")]);
    assert!(!and.is_statically_true() && !and.is_statically_false(), "undecidable");
    let not = SExpression::Array(vec![lit("not"), SExpression::Bool(false)]);
    assert!(not.is_statically_true());
    assert!(!SExpression::Bool(false).is_statically_true());
    assert!(!SExpression::string(String::new()).is_statically_true());
    assert!(!SExpression::Null.is_statically_true());
    assert!(!SExpression::string("@config.knob").is_statically_true());
}

#[test]
fn is_statically_false_true_for_bool_false() {
    assert!(SExpression::Bool(false).is_statically_false());
}

fn eq(op: &str, a: SExpression, b: SExpression) -> SExpression {
    SExpression::Array(vec![SExpression::string(op.to_string()), a, b])
}

fn lit(v: &str) -> SExpression {
    SExpression::string(v.to_string())
}

#[test]
fn single_equals_on_differing_literals_is_statically_false() {
    assert!(eq("=", lit("panel"), lit("document")).is_statically_false());
}

#[test]
fn control_single_equals_on_matching_literals_is_live() {
    assert!(!eq("=", lit("document"), lit("document")).is_statically_false());
}

#[test]
fn control_single_equals_on_a_binding_is_undecidable() {
    assert!(!eq("=", lit("@config.detailLook"), lit("document")).is_statically_false());
}

#[test]
fn double_equals_still_folds() {
    assert!(eq("==", lit("a"), lit("b")).is_statically_false());
    assert!(eq("!=", lit("a"), lit("a")).is_statically_false());
}

#[test]
fn is_statically_false_true_for_empty_string() {
    // A `string`-typed config knob defaulting to "" folds to this, not
    // `Bool(false)` — the "optional slot unfulfilled" contract.
    assert!(SExpression::string(String::new()).is_statically_false());
}

#[test]
fn is_statically_false_false_for_non_empty_string() {
    assert!(!SExpression::string("hi").is_statically_false());
}

#[test]
fn is_statically_false_true_for_null() {
    assert!(SExpression::Null.is_statically_false());
}

#[test]
fn is_statically_false_true_for_zero() {
    assert!(SExpression::Number(0.0).is_statically_false());
}

#[test]
fn is_statically_false_false_for_nonzero_number() {
    assert!(!SExpression::Number(1.0).is_statically_false());
}

#[test]
fn is_statically_false_true_for_empty_object() {
    assert!(SExpression::Object(SObject::default()).is_statically_false());
}

#[test]
fn is_statically_false_true_for_empty_array_literal() {
    assert!(SExpression::Array(vec![]).is_statically_false());
}

#[test]
fn is_statically_false_false_for_binding_string() {
    // `@`-prefixed strings are bindings, not literals — undecidable.
    assert!(!SExpression::string("@config.knob").is_statically_false());
}

fn reads(node: serde_json::Value) -> Vec<String> {
    let expr: SExpression = serde_json::from_value(node).expect("s-expression");
    let mut out = std::collections::HashSet::new();
    expr.collect_entity_reads(&mut out);
    let mut v: Vec<String> = out.into_iter().collect();
    v.sort();
    v
}

#[test]
fn entity_reads_skip_the_set_target() {
    assert!(reads(serde_json::json!([
        "set",
        "@entity.searchTerm",
        "?searchTerm"
    ]))
    .is_empty());
}

#[test]
fn entity_reads_count_a_field_read_inside_the_set_value() {
    assert_eq!(
        reads(serde_json::json!([
            "set",
            "@entity.total",
            ["+", "@entity.subtotal", 1]
        ])),
        vec!["subtotal"]
    );
}

#[test]
fn entity_reads_count_render_bindings_by_their_root_field() {
    assert_eq!(
        reads(
            serde_json::json!(["render-ui", "main", { "type": "typography", "content": "@entity.profile.name", "items": "@entity.rows[0]" }])
        ),
        vec!["profile", "rows"]
    );
}

#[test]
fn entity_reads_ignore_payload_and_config_bindings() {
    assert!(reads(serde_json::json!(["if", "@config.on", "@payload.x", "?y"])).is_empty());
}
