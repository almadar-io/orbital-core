use orbital_core_base::schema::{OperatorReturn, SExpression, ValueSlot, ValueWalk};

fn lookup(name: &str) -> Option<OperatorReturn> {
    let (semantics, returns_function) = match name {
        "select" => (Some("identity-of-arg<2>"), false),
        "branch" => (Some("branch-union"), false),
        "combine" => (Some("union-of-args"), false),
        "selected-union" => (Some("union-of-args<2,0>"), false),
        "last" => (Some("last-of-args"), false),
        "map-value" => (Some("lambda-result"), false),
        "function" | "other-function" => (None, true),
        "opaque" => (None, false),
        "bad-selection" => (Some("identity-of-arg<bad>"), false),
        "other-semantics" => (Some("computed-value"), false),
        _ => return None,
    };
    Some(OperatorReturn {
        semantics,
        returns_function,
    })
}

const WALK: ValueWalk<'static> = ValueWalk {
    lookup: &lookup,
    skip_dead_branches: false,
};

fn sx(input: &str) -> SExpression {
    serde_json::from_str(input).expect("expression fixture")
}

fn leaf_strings<'a>(expr: &'a SExpression, walk: &ValueWalk<'_>) -> Vec<&'a str> {
    expr.value_leaves(walk)
        .into_iter()
        .map(|leaf| leaf.as_str().expect("string fixture leaf"))
        .collect()
}

#[test]
fn identity_slots_are_direct_argument_positions_not_recursive_leaves() {
    let expr = sx(r#"["select", "IGNORED", 7, ["branch", "@condition", "T", "E"]]"#);
    assert!(matches!(
        expr.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(2)])
    ));
    assert_eq!(leaf_strings(&expr, &WALK), vec!["T", "E"]);

    let wrapped = sx(r#"["select", "IGNORED", 7, ["opaque", "INNER"]]"#);
    assert!(matches!(
        wrapped.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(2)])
    ));
    let opaque = sx(r#"["opaque", "INNER"]"#);
    assert_eq!(wrapped.value_leaves(&WALK), vec![&opaque]);
}

#[test]
fn branch_slots_exclude_the_condition_and_keep_each_declared_branch() {
    let expr = sx(r#"["branch", "@condition", "T", "E"]"#);
    assert!(matches!(
        expr.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(1), ValueSlot::Arg(2)])
    ));
    assert_eq!(leaf_strings(&expr, &WALK), vec!["T", "E"]);

    let one_branch = sx(r#"["branch", "@condition", "T"]"#);
    assert!(matches!(
        one_branch.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(1)])
    ));
    assert_eq!(leaf_strings(&one_branch, &WALK), vec!["T"]);
}

#[test]
fn dead_branch_policy_preserves_an_empty_plan_distinct_from_opaque() {
    let live = ValueWalk {
        lookup: &lookup,
        skip_dead_branches: true,
    };
    let expr = sx(r#"["branch", false, "DEAD", "LIVE"]"#);
    assert!(matches!(
        expr.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(1), ValueSlot::Arg(2)])
    ));
    assert_eq!(leaf_strings(&expr, &WALK), vec!["DEAD", "LIVE"]);
    assert!(matches!(
        expr.value_source_slots(&live).as_deref(),
        Some([ValueSlot::Arg(2)])
    ));
    assert_eq!(leaf_strings(&expr, &live), vec!["LIVE"]);

    let no_live_branch = sx(r#"["branch", false, "DEAD"]"#);
    assert!(matches!(
        no_live_branch.value_source_slots(&live).as_deref(),
        Some([])
    ));
    assert!(no_live_branch.value_leaves(&live).is_empty());
    assert!(matches!(
        no_live_branch.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(1)])
    ));
    assert_eq!(leaf_strings(&no_live_branch, &WALK), vec!["DEAD"]);
}

#[test]
fn union_slots_keep_all_or_exactly_the_listed_argument_order() {
    let expr = sx(r#"["combine", "A", ["branch", "@condition", "B", "C"], "D"]"#);
    assert!(matches!(
        expr.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(0), ValueSlot::Arg(1), ValueSlot::Arg(2)])
    ));
    assert_eq!(leaf_strings(&expr, &WALK), vec!["A", "B", "C", "D"]);

    let selected = sx(r#"["selected-union", "A", "IGNORED", "C"]"#);
    assert!(matches!(
        selected.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(2), ValueSlot::Arg(0)])
    ));
    assert_eq!(leaf_strings(&selected, &WALK), vec!["C", "A"]);
}

#[test]
fn last_slots_select_the_last_argument_before_recursive_leaf_resolution() {
    let expr = sx(r#"["last", "IGNORED", 7, ["branch", "@condition", "T", "E"]]"#);
    assert!(matches!(
        expr.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::Arg(2)])
    ));
    assert_eq!(leaf_strings(&expr, &WALK), vec!["T", "E"]);
}

#[test]
fn lambda_result_slots_select_the_last_declared_lambda_argument_body() {
    let expr = sx(r#"["map-value", "SOURCE", ["function", "item", ["branch", "@item.ok", "T", "E"]]]"#);
    assert!(matches!(
        expr.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::LambdaBody(1)])
    ));
    assert_eq!(leaf_strings(&expr, &WALK), vec!["T", "E"]);

    let several = sx(r#"["map-value", ["function", "first", "FIRST"], "IGNORED", ["other-function", "last", "LAST"]]"#);
    assert!(matches!(
        several.value_source_slots(&WALK).as_deref(),
        Some([ValueSlot::LambdaBody(2)])
    ));
    assert_eq!(leaf_strings(&several, &WALK), vec!["LAST"]);
}

#[test]
fn missing_named_arguments_or_lambda_bodies_make_the_plan_opaque() {
    for input in [
        r#"["select", "A", "B"]"#,
        r#"["branch", "@condition"]"#,
        r#"["combine"]"#,
        r#"["selected-union", "A", "B"]"#,
        r#"["last"]"#,
        r#"["map-value", "SOURCE", "@config.callback"]"#,
        r#"["map-value", "SOURCE", ["function", "item"]]"#,
        r#"["map-value", "SOURCE", ["undeclared-function", "item", "BODY"]]"#,
    ] {
        let expr = sx(input);
        assert!(expr.value_source_slots(&WALK).is_none(), "{input}");
        assert_eq!(expr.value_leaves(&WALK), vec![&expr], "{input}");
    }
}

#[test]
fn literals_undeclared_and_non_provenance_metadata_keep_the_expression_opaque() {
    for input in [
        r#""plain""#,
        "null",
        "false",
        r#"{ "event": "A" }"#,
        "[]",
        r#"[1, "A"]"#,
        r#"["undeclared", ["branch", "@condition", "T", "E"]]"#,
        r#"["opaque", ["branch", "@condition", "T", "E"]]"#,
        r#"["bad-selection", "A", "B", "C"]"#,
        r#"["other-semantics", "A"]"#,
    ] {
        let expr = sx(input);
        assert!(expr.value_source_slots(&WALK).is_none(), "{input}");
        assert_eq!(expr.value_leaves(&WALK), vec![&expr], "{input}");
    }
}

#[test]
fn a_declared_kind_derives_one_slot_descriptor_and_a_malformed_one_derives_none() {
    use orbital_core_base::schema::ValueSourceSlots as S;
    for (kind, expected) in [
        ("branch-union", Some(S::Branches)),
        ("union-of-args", Some(S::Args)),
        ("coalesce", Some(S::Args)),
        ("last-of-args", Some(S::Last)),
        ("lambda-result", Some(S::LambdaResult)),
        ("identity-of-arg<2>", Some(S::Listed { args: vec![2] })),
        ("union-of-args<2,0>", Some(S::Listed { args: vec![2, 0] })),
        ("union-of-args< 1 , 3 >", Some(S::Listed { args: vec![1, 3] })),
        ("identity-of-arg<bad>", None),
        ("identity-of-arg<1", None),
        ("computed-value", None),
        ("", None),
    ] {
        assert_eq!(S::of_semantics(kind), expected, "{kind}");
    }
    assert_eq!(
        serde_json::to_string(&S::Listed { args: vec![1, 2] }).unwrap(),
        r#"{"kind":"listed","args":[1,2]}"#
    );
    assert_eq!(serde_json::to_string(&S::LambdaResult).unwrap(), r#"{"kind":"lambdaResult"}"#);
}
