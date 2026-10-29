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
        deep = SExpression::Array(vec![SExpression::string("do"), deep].into());
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
fn try_rewrite_that_changes_nothing_returns_the_shared_input() {
    let expr = rewrite_input(serde_json::json!(["do", ["a", 1], {"k": ["b"], "j": "c"}]));
    let out: SExpression = expr
        .try_rewrite(|_| Ok::<_, ()>(RewriteVisit::Descend), Ok)
        .unwrap();
    assert!(out.same_node(&expr), "an unchanged tree is the input, not a rebuilt copy");
    assert_eq!(out, expr);
}

#[test]
fn try_rewrite_that_changes_one_leaf_rebuilds_only_the_path_to_it() {
    let expr = rewrite_input(serde_json::json!(["do", ["a", 1], ["b", ["c"]]]));
    let out = expr
        .try_rewrite(
            |_| Ok::<_, ()>(RewriteVisit::Descend),
            |node| Ok(if node.as_str() == Some("a") { SExpression::string("A") } else { node }),
        )
        .unwrap();
    assert_eq!(out, rewrite_input(serde_json::json!(["do", ["A", 1], ["b", ["c"]]])));
    assert!(!out.same_node(&expr));
    let (SExpression::Array(old), SExpression::Array(new)) = (&expr, &out) else { panic!("lists") };
    assert!(!old[1].same_node(&new[1]), "the list holding the changed leaf is rebuilt");
    assert!(old[2].same_node(&new[2]), "a sibling subtree stays shared");
}

#[test]
fn control_unshared_gives_every_list_and_object_its_own_allocation() {
    let expr = rewrite_input(serde_json::json!(["do", ["a", 1], {"k": ["b"]}]));
    let copy = expr.unshared();
    assert_eq!(copy, expr);
    assert!(!copy.same_node(&expr));
    let (SExpression::Array(old), SExpression::Array(new)) = (&expr, &copy) else { panic!("lists") };
    assert!(!old[1].same_node(&new[1]));
    assert!(!old[2].same_node(&new[2]));
}

#[test]
fn any_string_reads_every_string_leaf_and_nothing_else() {
    let expr = rewrite_input(serde_json::json!(["do", [1, true, null], {"key": ["deep", "@x.y"]}]));
    assert!(expr.any_string(|s| s == "deep"));
    assert!(expr.any_string(|s| s.starts_with('@')));
    assert!(!expr.any_string(|s| s == "key"), "object keys are not strings of the tree");
    assert!(!SExpression::Number(1.0).any_string(|_| true));
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
    assert_eq!(items[7], SExpression::array(vec![]));
    let object = items[8].as_object().unwrap();
    assert_eq!(object.keys().collect::<Vec<_>>(), ["z", "a"]);
    assert_eq!(object["a"], SExpression::Array(vec![SExpression::Number(2.0)].into()));
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
        map.insert("zebra".into(), SExpression::Number(1.0));
        map.insert("alpha".into(), SExpression::Bool(true));
        map.insert("middle".into(), SExpression::string("x"));
        let expr = SExpression::Array(vec![
            SExpression::string("="),
            SExpression::object(map),
        ].into());
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
        ].into()),
        SExpression::Null,
    ].into());
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
    ].into());
    assert!(or.is_statically_true(), "one operand provably true");
    assert!(!or.is_statically_false());
    let and = SExpression::Array(vec![lit("and"), SExpression::Bool(true), lit("@payload.row")].into());
    assert!(!and.is_statically_true() && !and.is_statically_false(), "undecidable");
    let not = SExpression::Array(vec![lit("not"), SExpression::Bool(false)].into());
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
    SExpression::Array(vec![SExpression::string(op.to_string()), a, b].into())
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
    assert!(SExpression::object(SObject::default()).is_statically_false());
}

#[test]
fn is_statically_false_true_for_empty_array_literal() {
    assert!(SExpression::array(vec![]).is_statically_false());
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

#[test]
fn an_ir_node_is_16_bytes_because_lists_and_objects_are_shared_handles() {
    assert_eq!(std::mem::size_of::<SExpression>(), 16);
}

#[test]
fn a_boxed_object_keeps_the_plain_json_wire_form() {
    let json = serde_json::json!({"type": "page", "items": [1.5, {"k": null}]});
    let expr: SExpression = serde_json::from_value(json.clone()).unwrap();
    assert!(matches!(expr, SExpression::Object(_)));
    assert_eq!(serde_json::to_value(&expr).unwrap(), json);
}

fn string_of(expr: &SExpression) -> &crate::value::Shared<String> {
    match expr {
        SExpression::String(text) => text,
        other => panic!("expected a string, got {other:?}"),
    }
}

#[test]
fn reading_a_tree_shares_equal_string_values_and_keys_in_one_allocation() {
    let expr: SExpression = serde_json::from_str(r#"["go", {"go": "go", "other": ["go", "x"]}, "x"]"#).unwrap();
    let SExpression::Array(items) = &expr else { panic!("array") };
    let SExpression::Object(map) = &items[1] else { panic!("object") };
    let (key, value) = map.get_index(0).unwrap();
    let SExpression::Array(inner) = &map["other"] else { panic!("array") };
    let first = string_of(&items[0]);
    assert!(crate::value::Shared::ptr_eq(first, key), "a key shares the allocation of an equal value");
    assert!(crate::value::Shared::ptr_eq(first, string_of(value)));
    assert!(crate::value::Shared::ptr_eq(first, string_of(&inner[0])));
    assert!(crate::value::Shared::ptr_eq(string_of(&inner[1]), string_of(&items[2])));
    assert!(!crate::value::Shared::ptr_eq(first, string_of(&items[2])), "different text stays apart");
}

#[test]
fn the_interning_table_does_not_outlive_the_read() {
    let _: SExpression = serde_json::from_str(r#"["a", "a"]"#).unwrap();
    assert_eq!(crate::intern::held(), 0);
    let held = crate::intern::scope(|| {
        let _: SExpression = serde_json::from_str(r#"["a", "b", "a"]"#).unwrap();
        let _: SExpression = serde_json::from_str(r#"["b", "c"]"#).unwrap();
        crate::intern::held()
    });
    assert_eq!(held, 3, "a nested read joins the outer table");
    assert_eq!(crate::intern::held(), 0);
}

#[test]
fn separate_reads_do_not_share_allocations() {
    let a: SExpression = serde_json::from_str(r#""same""#).unwrap();
    let b: SExpression = serde_json::from_str(r#""same""#).unwrap();
    assert_eq!(a, b);
    assert!(!crate::value::Shared::ptr_eq(string_of(&a), string_of(&b)));
}

#[test]
fn interned_object_keys_answer_str_lookups_and_compare_by_text() {
    let expr: SExpression = serde_json::from_str(r#"{"alpha": 1, "beta": 2}"#).unwrap();
    let SExpression::Object(map) = &expr else { panic!("object") };
    assert_eq!(map.get("beta"), Some(&SExpression::Number(2.0)));
    assert!(map.contains_key("alpha"));
    assert!(!map.contains_key("gamma"));
    assert_eq!(map.keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
    let mut other = SObject::default();
    other.insert("alpha".into(), SExpression::Number(1.0));
    other.insert("beta".into(), SExpression::Number(2.0));
    assert_eq!(**map, other, "a built object equals a read one");
}

#[test]
fn a_resolved_program_round_trips_to_the_same_bytes_and_shares_its_strings() {
    use crate::schema::OrbitalSchema;
    for text in [
        include_str!("../../../../../../tests/fixtures/embed/resolved/basic-entity.json"),
        include_str!("../../../../../../tests/fixtures/embed/resolved/two-orbital-chore.json"),
        include_str!("../../../../../../tests/fixtures/embed/resolved/tick-counter.json"),
    ] {
        let schema: OrbitalSchema = serde_json::from_str(text).unwrap();
        let first = serde_json::to_string_pretty(&schema).unwrap();
        let again: OrbitalSchema = serde_json::from_str(&first).unwrap();
        assert_eq!(serde_json::to_string_pretty(&again).unwrap(), first, "serialize(parse(x)) is a fixed point");
        let tree: SExpression = serde_json::from_str(text).unwrap();
        let bytes = serde_json::to_string(&tree).unwrap();
        let mut plain: serde_json::Value = serde_json::from_str(text).unwrap();
        fn as_floats(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Number(n) => *value = serde_json::json!(n.as_f64().unwrap()),
                serde_json::Value::Array(items) => items.iter_mut().for_each(as_floats),
                serde_json::Value::Object(map) => map.values_mut().for_each(as_floats),
                _ => {}
            }
        }
        as_floats(&mut plain);
        assert_eq!(bytes, serde_json::to_string(&plain).unwrap(), "an interned read writes the bytes a plain read writes");
    }
}

#[test]
fn a_program_read_keeps_one_allocation_per_distinct_string() {
    let text = include_str!("../../../../../../tests/fixtures/embed/resolved/two-orbital-chore.json");
    let mut distinct: std::collections::HashMap<*const String, &str> = std::collections::HashMap::new();
    let mut by_text: std::collections::HashMap<&str, std::collections::HashSet<*const String>> = std::collections::HashMap::new();
    let mut total = 0usize;
    fn visit<'a>(
        expr: &'a SExpression,
        total: &mut usize,
        distinct: &mut std::collections::HashMap<*const String, &'a str>,
        by_text: &mut std::collections::HashMap<&'a str, std::collections::HashSet<*const String>>,
    ) {
        match expr {
            SExpression::String(text) => {
                *total += 1;
                let ptr: *const String = &**text;
                distinct.insert(ptr, text.as_str());
                by_text.entry(text.as_str()).or_default().insert(ptr);
            }
            SExpression::Array(items) => items.iter().for_each(|item| visit(item, total, distinct, by_text)),
            SExpression::Object(map) => {
                for (key, value) in map.iter() {
                    *total += 1;
                    let ptr: *const String = &**key;
                    distinct.insert(ptr, key.as_str());
                    by_text.entry(key.as_str()).or_default().insert(ptr);
                    visit(value, total, distinct, by_text);
                }
            }
            _ => {}
        }
    }
    let whole: SExpression = serde_json::from_str(text).unwrap();
    visit(&whole, &mut total, &mut distinct, &mut by_text);
    assert!(total > distinct.len(), "the fixture repeats strings");
    assert!(by_text.values().all(|ptrs| ptrs.len() == 1), "equal text is held once");
}

#[test]
#[ignore = "measurement: ORB_MEASURE_PROGRAM=<resolved .orb path> cargo test -p orbital-core-base --lib interning_hit_rate -- --ignored --nocapture"]
fn interning_hit_rate_on_a_resolved_program() {
    use crate::schema::OrbitalSchema;
    let path = std::env::var("ORB_MEASURE_PROGRAM").expect("ORB_MEASURE_PROGRAM names a resolved program");
    let text = std::fs::read_to_string(&path).unwrap();
    let stats = crate::intern::scope(|| {
        let _program: OrbitalSchema = serde_json::from_str(&text).unwrap();
        crate::intern::stats().unwrap()
    });
    println!("interned: {stats:?}");
    let tree: SExpression = crate::intern::scope(|| serde_json::from_str(&text).unwrap());
    let (mut expanded, mut held) = ((0usize, 0usize), (0usize, 0usize));
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(&tree, true)];
    fn cost(items: usize) -> usize {
        16 + items * std::mem::size_of::<SExpression>() + 24
    }
    while let Some((node, first)) = stack.pop() {
        let (children, bytes): (Vec<&SExpression>, usize) = match node {
            SExpression::Array(items) => (items.iter().collect(), cost(items.len())),
            SExpression::Object(fields) => (fields.values().collect(), cost(fields.len()) + fields.len() * 16),
            _ => continue,
        };
        expanded.0 += 1;
        expanded.1 += bytes;
        let ptr = match node {
            SExpression::Array(items) => &**items as *const Vec<SExpression> as usize,
            SExpression::Object(fields) => &**fields as *const SObject as usize,
            _ => unreachable!(),
        };
        let fresh = seen.insert(ptr);
        if fresh {
            held.0 += 1;
            held.1 += bytes;
        }
        if first || fresh {
            stack.extend(children.into_iter().map(|child| (child, true)));
        }
    }
    println!("list/object nodes as a tree: {} ({} B); as shared: {} ({} B)", expanded.0, expanded.1, held.0, held.1);
    println!(
        "SExpression={}B Shared<String>={}B String={}B",
        std::mem::size_of::<SExpression>(),
        std::mem::size_of::<crate::value::Shared<String>>(),
        std::mem::size_of::<String>()
    );
}

fn list_of(expr: &SExpression) -> &crate::value::Shared<Vec<SExpression>> {
    let SExpression::Array(items) = expr else { panic!("array") };
    items
}

fn record_of(expr: &SExpression) -> &crate::value::Shared<SObject> {
    let SExpression::Object(fields) = expr else { panic!("object") };
    fields
}

#[test]
fn identical_subtrees_in_one_read_share_one_node() {
    let expr: SExpression = serde_json::from_str(
        r#"["if", ["=", "@a", 1], ["+", ["*", 2, 3], {"k": ["*", 2, 3]}], ["+", ["*", 2, 3], {"k": ["*", 2, 3]}]]"#,
    )
    .unwrap();
    let items = list_of(&expr);
    let (then, other) = (list_of(&items[2]), list_of(&items[3]));
    assert!(crate::value::Shared::ptr_eq(then, other), "equal branches are one node");
    assert!(crate::value::Shared::ptr_eq(list_of(&then[1]), record_of(&then[2]).get("k").map(list_of).unwrap()));
    assert!(!crate::value::Shared::ptr_eq(list_of(&items[1]), then), "different content stays apart");
}

#[test]
fn control_nodes_that_differ_in_any_child_order_or_bit_stay_apart() {
    let expr: SExpression = serde_json::from_str(
        r#"[["a", 1], ["a", 2], ["a", 1.5], ["b", 1], [1, "a"], {"x": 1, "y": 2}, {"y": 2, "x": 1}, {"x": 1}, [0.0], [-0.0], [], {}]"#,
    )
    .unwrap();
    let items = list_of(&expr);
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            let apart = match (&items[i], &items[j]) {
                (SExpression::Array(a), SExpression::Array(b)) => !crate::value::Shared::ptr_eq(a, b),
                (SExpression::Object(a), SExpression::Object(b)) => !crate::value::Shared::ptr_eq(a, b),
                _ => true,
            };
            assert!(apart, "{i} and {j} hold different content");
        }
    }
}

#[test]
fn separate_reads_and_empty_nodes_follow_the_scope() {
    let a: SExpression = serde_json::from_str("[1, 2]").unwrap();
    let b: SExpression = serde_json::from_str("[1, 2]").unwrap();
    assert!(!crate::value::Shared::ptr_eq(list_of(&a), list_of(&b)), "no table outside a scope");
    let (c, d) = crate::intern::scope(|| {
        (serde_json::from_str::<SExpression>("[1, 2]").unwrap(), serde_json::from_str::<SExpression>("[1, 2]").unwrap())
    });
    assert!(crate::value::Shared::ptr_eq(list_of(&c), list_of(&d)), "a nested read joins the outer table");
}

#[test]
fn writing_through_one_holder_of_a_shared_node_leaves_the_other_untouched() {
    let mut expr: SExpression = serde_json::from_str(r#"[["a", 1], ["a", 1]]"#).unwrap();
    let SExpression::Array(items) = &mut expr else { panic!("array") };
    assert!(crate::value::Shared::ptr_eq(list_of(&items[0]), list_of(&items[1])));
    let SExpression::Array(first) = &mut items[0] else { panic!("array") };
    first.push(SExpression::Number(9.0));
    assert_eq!(serde_json::to_string(&items[0]).unwrap(), r#"["a",1.0,9.0]"#);
    assert_eq!(serde_json::to_string(&items[1]).unwrap(), r#"["a",1.0]"#);
}
