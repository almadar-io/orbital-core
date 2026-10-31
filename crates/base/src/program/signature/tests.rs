use super::*;
use serde_json::json;

fn op(
    params: serde_json::Value,
    returns: serde_json::Value,
    max_arity: Option<u32>,
) -> EmbeddedOperator {
    serde_json::from_value(json!({
        "category": "std-array",
        "minArity": 0,
        "maxArity": max_arity,
        "params": params,
        "returnType": returns,
    }))
    .unwrap()
}

#[test]
fn literal_option_selects_a_declared_return_branch_but_dynamic_keeps_the_union() {
    let mut operator = op(
        json!([]),
        json!({ "kind": "union", "of": ["bytes", "number"] }),
        None,
    );
    operator.return_semantics = Some("option-branch<1,result>".into());
    let signature = Signature::of(&operator);
    let bindings = CallBindings::new();
    assert_eq!(
        signature.result_with_options(&bindings, |index, field| {
            assert_eq!((index, field), (1, "result"));
            Some(true)
        }),
        Some(OirType::Number)
    );
    assert_eq!(
        signature.result_with_options(&bindings, |_, _| Some(false)),
        Some(OirType::Bytes)
    );
    assert_eq!(
        signature.result_with_options(&bindings, |_, _| None),
        signature.result(&bindings)
    );
}

#[test]
fn closed_optional_options_allow_omission_without_weakening_field_types() {
    let operator = op(
        json!([p(
            "options",
            json!({ "kind": "object", "fields": { "result": "boolean", "level": "number" }, "optional": ["result", "level"] })
        )]),
        json!("bytes"),
        None,
    );
    let signature = Signature::of(&operator);
    let check = |fields: Vec<(String, Actual<'_>)>| {
        signature
            .bind_shape_literal(&mut CallBindings::new(), 0, &fields, Mode::Static)
            .unwrap()
    };
    assert!(check(vec![]).is_ok());
    assert!(check(vec![("result".into(), Actual::of_type(OirType::Boolean))]).is_ok());
    assert!(check(vec![("result".into(), Actual::of_type(OirType::String))]).is_err());
    assert!(check(vec![(
        "undeclared".into(),
        Actual::of_type(OirType::Number)
    )])
    .is_err());
}

fn var(name: &str) -> serde_json::Value {
    json!({ "kind": "var", "name": name })
}

fn record(name: &str) -> serde_json::Value {
    json!({ "kind": "var", "name": name, "bound": { "kind": "record" } })
}

fn list(of: serde_json::Value) -> serde_json::Value {
    json!({ "kind": "array", "of": of })
}

fn p(name: &str, ty: serde_json::Value) -> serde_json::Value {
    json!({ "name": name, "type": ty })
}

fn s_arr(of: OirType) -> OirType {
    OirType::array(of)
}

fn spawn_config() -> EmbeddedOperator {
    let mode = json!({ "kind": "union", "of": [{ "kind": "literal", "value": "pipe" }, { "kind": "literal", "value": "null" }] });
    op(
        json!([p(
            "config",
            json!({ "kind": "object", "fields": { "stdout": mode, "detached": "boolean", "cwd": { "kind": "union", "of": ["string", { "kind": "literal", "value": null }] } } })
        )]),
        json!("void"),
        None,
    )
}

fn shape_check(fields: Vec<(&str, Actual<'static>)>) -> Option<Result<(), Mismatch>> {
    let op = spawn_config();
    let sig = Signature::of(&op);
    let mut b = CallBindings::new();
    let fields: Vec<(String, Actual<'static>)> = fields
        .into_iter()
        .map(|(n, a)| (n.to_string(), a))
        .collect();
    sig.bind_shape_literal(&mut b, 0, &fields, Mode::Static)
}

fn literal_text(v: &str) -> Actual<'static> {
    Actual {
        text: Some(v.to_string()),
        ..Actual::of_type(OirType::String)
    }
}

fn null() -> Actual<'static> {
    Actual {
        known: Some(Known::Absent),
        ..Actual::unknown()
    }
}

fn boolean() -> Actual<'static> {
    Actual::of_type(OirType::Boolean)
}

#[test]
fn a_literal_object_for_a_closed_shape_is_checked_field_by_field() {
    let good = vec![
        ("stdout", literal_text("pipe")),
        ("detached", boolean()),
        ("cwd", null()),
    ];
    assert_eq!(
        shape_check(good),
        Some(Ok(())),
        "control: every field, each of its type, null where the type allows it"
    );
    let wrong = shape_check(vec![
        ("stdout", literal_text("pipe")),
        ("detached", literal_text("yes")),
        ("cwd", null()),
    ]);
    let Some(Err(Mismatch::Type { expected, .. })) = wrong else {
        panic!("{wrong:?}")
    };
    assert!(
        expected.contains("`detached`") && expected.contains("boolean"),
        "{expected}"
    );
    let number = shape_check(vec![
        ("stdout", Actual::of_type(OirType::Number)),
        ("detached", boolean()),
        ("cwd", null()),
    ]);
    assert!(
        matches!(number, Some(Err(Mismatch::Type { .. }))),
        "a number is none of the literals: {number:?}"
    );
    let other = shape_check(vec![
        ("stdout", literal_text("inherit")),
        ("detached", boolean()),
        ("cwd", null()),
    ]);
    assert!(
        matches!(other, Some(Err(Mismatch::Type { .. }))),
        "a literal string the union does not list: {other:?}"
    );
}

#[test]
fn a_literal_object_for_a_closed_shape_names_a_missing_and_an_unknown_field() {
    let missing = shape_check(vec![("stdout", literal_text("pipe")), ("cwd", null())]);
    let Some(Err(Mismatch::Type { actual, .. })) = missing else {
        panic!("{missing:?}")
    };
    assert!(
        actual.contains("`detached`") && actual.contains("missing"),
        "{actual}"
    );
    let extra = shape_check(vec![
        ("stdout", literal_text("pipe")),
        ("detached", boolean()),
        ("cwd", null()),
        ("shell", Actual::unknown()),
    ]);
    let Some(Err(Mismatch::Type { actual, .. })) = extra else {
        panic!("{extra:?}")
    };
    assert!(actual.contains("`shell`"), "{actual}");
}

#[test]
fn a_parameter_that_is_not_a_closed_shape_is_left_to_bind() {
    let any = op(json!([p("x", json!("object"))]), json!("void"), None);
    let open = op(
        json!([p(
            "x",
            json!({ "kind": "object", "fields": { "a": "string" }, "open": true })
        )]),
        json!("void"),
        None,
    );
    for o in [&any, &open] {
        let mut b = CallBindings::new();
        assert_eq!(
            Signature::of(o).bind_shape_literal(
                &mut b,
                0,
                &[("z".to_string(), Actual::unknown())],
                Mode::Static
            ),
            None
        );
    }
}

fn struct_of(fields: &[(&str, OirType)]) -> OirType {
    OirType::Struct {
        additional: None,
        fields: fields
            .iter()
            .map(|(n, t)| OirField::new((*n).to_string(), t.clone()))
            .collect(),
    }
}

/// Binds every argument in order and returns the result type, or the first mismatch.
fn call(
    operator: &EmbeddedOperator,
    args: &[Actual<'_>],
    mode: Mode,
) -> Result<Option<OirType>, Mismatch> {
    let sig = Signature::of(operator);
    let mut b = CallBindings::new();
    for (i, a) in args.iter().enumerate() {
        sig.bind(&mut b, i, a, mode)?;
    }
    sig.finish(&b, args.len())
        .map_err(|(_, mismatch)| mismatch)?;
    Ok(sig.result(&b))
}

fn ty(t: OirType) -> Actual<'static> {
    Actual::of_type(t)
}

fn text(t: &str) -> Actual<'static> {
    Actual {
        known: Some(Known::Type(OirType::String)),
        text: Some(t.to_string()),
        texts: None,
        lambda: None,
    }
}

#[test]
fn two_arguments_bound_to_one_variable_must_agree() {
    let eq = op(
        json!([p("a", var("T")), p("b", var("T"))]),
        json!("boolean"),
        Some(2),
    );
    assert_eq!(
        call(
            &eq,
            &[ty(OirType::Number), ty(OirType::Number)],
            Mode::Static
        ),
        Ok(Some(OirType::Boolean))
    );
    assert_eq!(
        call(
            &eq,
            &[ty(OirType::Number), ty(OirType::String)],
            Mode::Static
        ),
        Err(Mismatch::Conflict {
            expected: "number".into(),
            actual: "string".into()
        })
    );
}

fn binary_var_operator() -> EmbeddedOperator {
    op(
        json!([p("a", var("T")), p("b", var("T"))]),
        var("T"),
        Some(2),
    )
}

#[test]
fn pairs_of_unlike_value_types_bind_one_variable() {
    let pair = |a: OirType, b: OirType| OirType::Tuple { items: vec![a, b] };
    let eq = binary_var_operator();
    let merged = call(
        &eq,
        &[
            ty(pair(OirType::String, OirType::Number)),
            ty(pair(OirType::String, OirType::String)),
        ],
        Mode::Runtime,
    );
    assert_eq!(merged, Ok(Some(pair(OirType::String, absent_slot()))));
    assert!(
        call(
            &eq,
            &[
                ty(pair(OirType::String, OirType::Number)),
                ty(OirType::Number)
            ],
            Mode::Runtime
        )
        .is_err(),
        "control: a pair is not a number"
    );
    let triple = OirType::Tuple {
        items: vec![OirType::String, OirType::Number, OirType::String],
    };
    assert_eq!(
        call(
            &eq,
            &[ty(pair(OirType::String, OirType::Number)), ty(triple)],
            Mode::Runtime
        ),
        Ok(Some(OirType::Tuple {
            items: vec![OirType::String, OirType::Number, absent_slot()]
        })),
        "tuples of unlike widths are one shape: the positions they share"
    );
    let word = OirType::Tuple {
        items: vec![OirType::String],
    };
    let digits = OirType::Tuple {
        items: vec![OirType::Number, OirType::Number],
    };
    assert!(
        call(&eq, &[ty(word), ty(digits)], Mode::Runtime).is_err(),
        "control: lists of unlike scalars are not one shape, whatever their lengths"
    );
    let numbers = OirType::Tuple {
        items: vec![OirType::Number, OirType::Number],
    };
    assert_eq!(
        call(
            &eq,
            &[ty(s_arr(OirType::Number)), ty(numbers)],
            Mode::Runtime
        ),
        Ok(Some(s_arr(OirType::Number))),
        "a tuple of one type is a list of it"
    );
}

#[test]
fn a_map_whose_values_differ_in_type_is_a_record() {
    let entries = op(
        json!([p(
            "entries",
            list(
                json!({ "kind": "tuple", "items": [var("K"), { "kind": "var", "name": "V", "each": true }] })
            )
        )]),
        json!({ "kind": "map", "key": var("K"), "value": var("V") }),
        Some(1),
    );
    let pair = |a: OirType, b: OirType| OirType::Tuple { items: vec![a, b] };
    let mixed = call(
        &entries,
        &[ty(s_arr(pair(OirType::String, OirType::Number)))],
        Mode::Runtime,
    );
    assert_eq!(
        mixed,
        Ok(Some(OirType::map(OirType::String, OirType::Number))),
        "control: one value type is a map"
    );
    let sig = Signature::of(&entries);
    let mut b = CallBindings::new();
    sig.bind(
        &mut b,
        0,
        &ty(s_arr(pair(OirType::String, OirType::Number))),
        Mode::Runtime,
    )
    .unwrap();
    sig.bind(
        &mut b,
        0,
        &ty(s_arr(pair(OirType::String, OirType::String))),
        Mode::Runtime,
    )
    .unwrap();
    assert_eq!(sig.result(&b), Some(OirType::Struct { fields: Default::default(), additional: None }));
}

#[test]
fn a_list_literal_of_unlike_tuples_is_a_list_of_one_element_type() {
    let count = op(json!([p("arr", list(var("T")))]), json!("number"), Some(1));
    let pair = |a: OirType, b: OirType| OirType::Tuple { items: vec![a, b] };
    let unlike = OirType::Tuple {
        items: vec![
            pair(OirType::String, OirType::Number),
            pair(OirType::String, OirType::String),
        ],
    };
    assert_eq!(
        call(&count, &[ty(unlike)], Mode::Static),
        Ok(Some(OirType::Number))
    );
    let lengths = OirType::Tuple {
        items: vec![
            pair(OirType::String, OirType::Number),
            OirType::Tuple {
                items: vec![OirType::String],
            },
        ],
    };
    assert_eq!(
        call(&count, &[ty(lengths)], Mode::Static),
        Ok(Some(OirType::Number)),
        "tuples of different widths"
    );
    let uniform = OirType::Tuple {
        items: vec![
            OirType::Tuple {
                items: vec![OirType::Number, OirType::Number],
            },
            OirType::Tuple {
                items: vec![OirType::String],
            },
        ],
    };
    assert!(
        call(&count, &[ty(uniform)], Mode::Static).is_err(),
        "control: lists of unlike scalars of different lengths"
    );
    let scalars = OirType::Tuple {
        items: vec![OirType::Number, OirType::String],
    };
    assert!(
        call(&count, &[ty(scalars)], Mode::Static).is_err(),
        "control: unlike scalars"
    );
}

#[test]
fn lists_of_unlike_width_tuples_bound_in_turn_are_one_list_type() {
    let flatten = op(json!([p("arr", list(list(var("T"))))]), list(var("T")), Some(1));
    let update = OirType::Tuple {
        items: vec![OirType::String, OirType::String, OirType::String, OirType::Struct { fields: Default::default(), additional: None }],
    };
    let delete = OirType::Tuple {
        items: vec![OirType::String, OirType::String, OirType::String],
    };
    let sig = Signature::of(&flatten);
    let mut b = CallBindings::new();
    sig.bind(&mut b, 0, &ty(s_arr(s_arr(s_arr(update)))), Mode::Runtime).unwrap();
    assert_eq!(
        sig.bind(&mut b, 0, &ty(s_arr(s_arr(s_arr(delete)))), Mode::Runtime),
        Ok(())
    );
    assert_eq!(
        sig.result(&b),
        Some(s_arr(s_arr(OirType::Tuple {
            items: vec![OirType::String, OirType::String, OirType::String, absent_slot()],
        })))
    );
    let rekey = OirType::Tuple {
        items: vec![OirType::String, OirType::String, absent_slot()],
    };
    let update = OirType::Tuple {
        items: vec![OirType::String, OirType::String, OirType::String, OirType::Struct { fields: Default::default(), additional: None }],
    };
    let mut b = CallBindings::new();
    sig.bind(&mut b, 0, &ty(s_arr(s_arr(s_arr(rekey)))), Mode::Runtime).unwrap();
    assert_eq!(
        sig.bind(&mut b, 0, &ty(s_arr(s_arr(s_arr(update)))), Mode::Runtime),
        Ok(()),
        "a joined delete/create group beside an update group"
    );
    let mut b = CallBindings::new();
    sig.bind(&mut b, 0, &ty(s_arr(s_arr(s_arr(OirType::Number)))), Mode::Runtime).unwrap();
    assert!(
        sig.bind(&mut b, 0, &ty(s_arr(s_arr(s_arr(OirType::String)))), Mode::Runtime).is_err(),
        "control: lists of unlike scalars stay two types"
    );
}

#[test]
fn an_absent_argument_binds_nothing_and_passes() {
    let eq = op(
        json!([p("a", var("T")), p("b", var("T"))]),
        json!("boolean"),
        Some(2),
    );
    assert!(call(&eq, &[absent(), ty(OirType::String)], Mode::Static).is_ok());
    assert!(call(&eq, &[ty(OirType::String), absent()], Mode::Static).is_ok());
    assert!(call(
        &eq,
        &[Actual::unknown(), ty(OirType::String)],
        Mode::Runtime
    )
    .is_ok());
}

#[test]
fn a_list_element_and_a_bare_argument_share_a_variable() {
    let append = op(
        json!([p("arr", list(var("T"))), p("item", var("T"))]),
        list(var("T")),
        Some(2),
    );
    assert_eq!(
        call(
            &append,
            &[ty(s_arr(OirType::String)), ty(OirType::String)],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::String)))
    );
    assert_eq!(
        call(
            &append,
            &[ty(s_arr(OirType::Number)), ty(OirType::String)],
            Mode::Static
        ),
        Err(Mismatch::Conflict {
            expected: "number".into(),
            actual: "string".into()
        })
    );
    assert_eq!(
        call(
            &append,
            &[
                Actual {
                    known: Some(Known::EmptyList),
                    ..Actual::unknown()
                },
                ty(OirType::String)
            ],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::String))),
        "an empty list takes its element type from the sibling that binds it"
    );
    assert!(
        is_undetermined(call(
            &append,
            &[
                Actual {
                    known: Some(Known::EmptyList),
                    ..Actual::unknown()
                },
                Actual::unknown()
            ],
            Mode::Static
        )),
        "control: an empty list beside an undetermined item has no element type"
    );
    assert_eq!(
        call(
            &append,
            &[ty(OirType::Number), ty(OirType::Number)],
            Mode::Static
        ),
        Err(Mismatch::Type {
            expected: "array".into(),
            actual: "number".into()
        })
    );
}

#[test]
fn the_result_of_a_polymorphic_call_is_the_concrete_type() {
    let first = op(json!([p("arr", list(var("T")))]), var("T"), Some(1));
    assert_eq!(
        call(&first, &[ty(s_arr(OirType::String))], Mode::Static),
        Ok(Some(OirType::String))
    );
    assert!(
        is_undetermined(call(&first, &[Actual::unknown()], Mode::Static)),
        "control: an undetermined list types nothing and is an error"
    );
    assert!(
        is_undetermined(call(
            &first,
            &[Actual {
                known: Some(Known::EmptyList),
                ..Actual::unknown()
            }],
            Mode::Static
        )),
        "an empty list with nothing to type it is an error, not a list of any element"
    );
    assert_eq!(
        call(
            &first,
            &[Actual {
                known: Some(Known::EmptyList),
                ..Actual::unknown()
            }],
            Mode::Runtime
        ),
        Ok(None),
        "at run time an empty list is a value; the rule is a static one"
    );
}

#[test]
fn accumulator_record_empty_list_fields_take_the_callbacks_element_type() {
    let reduce = op(
        json!([
            p("items", list(var("T"))),
            p("seed", var("A")),
            p(
                "step",
                json!({"kind":"fn", "params":[var("A"), var("T")], "returns":var("A")})
            )
        ]),
        var("A"),
        Some(3),
    );
    let seed = struct_of(&[("cells", OirType::Tuple { items: Vec::new() })]);
    let filled = struct_of(&[(
        "cells",
        OirType::array(struct_of(&[("kind", OirType::String)])),
    )]);
    let step = |_: &[Option<OirType>]| Some(filled.clone());
    let callback = Actual {
        known: None,
        text: None,
        texts: None,
        lambda: Some(&step),
    };
    assert_eq!(
        call(
            &reduce,
            &[ty(s_arr(OirType::Number)), ty(seed), callback],
            Mode::Static
        ),
        Ok(Some(filled))
    );
    let wrong = |_: &[Option<OirType>]| Some(struct_of(&[("cells", s_arr(OirType::Number))]));
    let callback = Actual {
        known: None,
        text: None,
        texts: None,
        lambda: Some(&wrong),
    };
    assert!(
        call(
            &reduce,
            &[
                ty(s_arr(OirType::Number)),
                ty(struct_of(&[("cells", s_arr(OirType::String))])),
                callback
            ],
            Mode::Static
        )
        .is_err(),
        "a known nonempty field cannot change its declared element type"
    );
}

#[test]
fn record_bounds_require_every_declared_union_member_to_be_a_record() {
    let operator = op(json!([p("value", record("R"))]), var("R"), Some(1));
    let shape = struct_of(&[("name", OirType::String)]);
    let union = |other: OirType| OirType::Union {
        variants: vec![
            OirField::new("record", shape.clone()),
            OirField::new("other", other),
        ].into(),
    };
    assert!(call(
        &operator,
        &[ty(union(OirType::map(OirType::String, OirType::Number)))],
        Mode::Static
    )
    .is_ok());
    assert!(
        call(&operator, &[ty(union(OirType::String))], Mode::Static).is_err(),
        "a possible scalar cannot satisfy a record bound"
    );
    let sig = Signature::of(&operator);
    let check_alias = |name: &str, definition: Option<OirType>| {
        let mut bindings = CallBindings::new();
        if let Some(definition) = definition {
            bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([(
                "Value".into(),
                definition,
            )])));
        }
        sig.bind(
            &mut bindings,
            0,
            &ty(OirType::TypeRef { name: name.into() }),
            Mode::Static,
        )
    };
    assert!(check_alias("Value", Some(union(shape.clone()))).is_ok());
    assert!(check_alias("Value", Some(union(OirType::String))).is_err());
    let mut bindings = CallBindings::new();
    bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([(
        "Value".into(),
        union(OirType::String),
    )])));
    assert!(sig
        .bind(
            &mut bindings,
            0,
            &ty(OirType::reference("Value")),
            Mode::Static
        )
        .is_err());
    bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([(
        "Value".into(),
        OirType::TypeRef {
            name: "Value".into(),
        },
    )])));
    assert!(sig
        .bind(
            &mut bindings,
            0,
            &ty(OirType::TypeRef {
                name: "Value".into()
            }),
            Mode::Static
        )
        .is_err());
    assert!(
        check_alias("ExternalRow", None).is_ok(),
        "an undeclared external row retains its existing opaque contract"
    );
}

#[test]
fn a_bound_variable_restricts_what_it_binds() {
    let lt = op(
        json!([
            p(
                "a",
                json!({ "kind": "var", "name": "T", "bound": { "kind": "union", "of": ["number", "string"] } })
            ),
            p(
                "b",
                json!({ "kind": "var", "name": "T", "bound": { "kind": "union", "of": ["number", "string"] } })
            ),
        ]),
        json!("boolean"),
        Some(2),
    );
    assert!(call(
        &lt,
        &[ty(OirType::String), ty(OirType::String)],
        Mode::Static
    )
    .is_ok());
    assert_eq!(
        call(
            &lt,
            &[ty(OirType::Boolean), ty(OirType::Boolean)],
            Mode::Static
        ),
        Err(Mismatch::Type {
            expected: "number | string".into(),
            actual: "boolean".into()
        })
    );
    assert_eq!(
        call(
            &lt,
            &[ty(OirType::Number), ty(OirType::String)],
            Mode::Static
        ),
        Err(Mismatch::Conflict {
            expected: "number".into(),
            actual: "string".into()
        })
    );
}

#[test]
fn a_function_parameter_binds_the_lambda_result() {
    let map = op(
        json!([
            p("arr", list(var("T"))),
            p(
                "fn",
                json!({ "kind": "fn", "params": [var("T")], "returns": var("U") })
            ),
        ]),
        list(var("U")),
        Some(2),
    );
    let seen = std::cell::RefCell::new(Vec::new());
    let body = |params: &[Option<OirType>]| {
        seen.borrow_mut().push(params.to_vec());
        Some(OirType::Boolean)
    };
    let lambda = Actual {
        known: None,
        text: None,
        texts: None,
        lambda: Some(&body),
    };
    let sig = Signature::of(&map);
    let mut b = CallBindings::new();
    sig.bind(&mut b, 0, &ty(s_arr(OirType::Number)), Mode::Static)
        .unwrap();
    sig.bind(&mut b, 1, &lambda, Mode::Static).unwrap();
    assert_eq!(sig.result(&b), Some(s_arr(OirType::Boolean)));
    assert_eq!(seen.borrow().as_slice(), &[vec![Some(OirType::Number)]]);
}

fn tree_fold_op() -> EmbeddedOperator {
    let kids = json!({ "kind": "childrenOf", "of": "T", "result": "R" });
    op(
        json!([
            p("value", var("T")),
            p(
                "step",
                json!({ "kind": "fn", "params": [var("T"), kids], "returns": var("R") })
            ),
            p("shape", json!({ "kind": "shape" })),
        ]),
        var("R"),
        Some(4),
    )
}

fn tree_type() -> OirType {
    let own = || OirType::TypeRef {
        name: "Tree".to_string(),
    };
    struct_of(&[
        ("label", OirType::String),
        ("kids", s_arr(own())),
        ("parent", own()),
        ("named", OirType::map(OirType::String, own())),
        ("tags", s_arr(OirType::String)),
    ])
}

/// Binds the shape first (as the checker does), then the value and the step.
fn fold(
    t: OirType,
    shape: Option<&str>,
    step: LambdaResult<'_>,
) -> Result<Option<OirType>, Mismatch> {
    let operator = tree_fold_op();
    let sig = Signature::of(&operator);
    let mut b = CallBindings::new();
    if let Some(shape) = shape {
        sig.bind(&mut b, 2, &text(shape), Mode::Static)?;
    }
    sig.bind(&mut b, 0, &ty(t), Mode::Static)?;
    let lambda = Actual {
        known: None,
        text: None,
        texts: None,
        lambda: Some(step),
    };
    sig.bind(&mut b, 1, &lambda, Mode::Static)?;
    Ok(sig.result(&b))
}

fn optional_fields(fields: &[(&str, OirType)]) -> OirType {
    let OirType::Struct { mut fields, additional } = struct_of(fields) else {
        unreachable!()
    };
    fields.iter_mut().for_each(|f| f.required = false);
    OirType::Struct { fields, additional }
}

#[test]
fn children_reference_identity_requires_a_declared_alias() {
    let node = OirType::array(OirType::reference("Tree"));
    let mut bindings = CallBindings::new();
    assert_eq!(
        children_of(&node, "Tree", &OirType::String, &bindings),
        Some(OirType::Struct { fields: Default::default(), additional: None })
    );
    let required = struct_of(&[(
        "kids",
        OirType::array(OirType::TypeRef {
            name: "Tree".into(),
        }),
    )]);
    let Some(OirType::Struct { fields, .. }) =
        children_of(&required, "Tree", &OirType::String, &bindings)
    else {
        panic!("child fields")
    };
    assert!(
        fields[0].required,
        "a required nonnullable child container is always in childResults"
    );
    for optional in [
        optional_fields(&[(
            "kids",
            OirType::array(OirType::TypeRef {
                name: "Tree".into(),
            }),
        )]),
        struct_of(&[(
            "kids",
            OirType::optional(OirType::array(OirType::TypeRef {
                name: "Tree".into(),
            })),
        )]),
    ] {
        let Some(OirType::Struct { fields, .. }) =
            children_of(&optional, "Tree", &OirType::String, &bindings)
        else {
            panic!("child fields")
        };
        assert!(
            !fields[0].required,
            "optional or nullable children may be absent"
        );
    }
    bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([
        ("Tree".into(), tree_type()),
        (
            "Alias".into(),
            OirType::TypeRef {
                name: "Tree".into(),
            },
        ),
        (
            "MaybeTree".into(),
            OirType::optional(OirType::TypeRef {
                name: "Tree".into(),
            }),
        ),
    ])));
    assert_eq!(
        children_of(&node, "Alias", &OirType::String, &bindings),
        Some(OirType::array(OirType::String))
    );
    let nullable_alias = struct_of(&[(
        "parent",
        OirType::TypeRef {
            name: "MaybeTree".into(),
        },
    )]);
    let Some(OirType::Struct { fields, .. }) =
        children_of(&nullable_alias, "Tree", &OirType::String, &bindings)
    else {
        panic!("nullable alias child")
    };
    assert!(
        !fields[0].required,
        "nullable aliases do not prove child presence"
    );
    bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([
        (
            "Tree".into(),
            OirType::TypeRef {
                name: "Alias".into(),
            },
        ),
        (
            "Alias".into(),
            OirType::TypeRef {
                name: "Tree".into(),
            },
        ),
    ])));
    assert_eq!(
        children_of(&node, "Alias", &OirType::String, &bindings),
        Some(OirType::Struct { fields: Default::default(), additional: None })
    );
}

#[test]
fn children_declared_optional_come_back_optional_in_a_list_or_map() {
    let bindings = CallBindings::new();
    let own = || OirType::TypeRef { name: "Tree".into() };
    let maybe = || OirType::optional(own());
    let kids = struct_of(&[
        ("kids", s_arr(maybe())),
        ("named", OirType::map(OirType::String, maybe())),
        ("plain", s_arr(own())),
    ]);
    let Some(OirType::Struct { fields, .. }) = children_of(&kids, "Tree", &OirType::String, &bindings) else {
        panic!("child fields")
    };
    let of = |name: &str| fields.iter().find(|f| f.name == name).map(|f| f.field_type.clone());
    assert_eq!(of("kids"), Some(s_arr(OirType::optional(OirType::String))), "a null element folds to null");
    assert_eq!(
        of("named"),
        Some(OirType::map(OirType::String, OirType::optional(OirType::String))),
        "a null map value folds to null"
    );
    assert_eq!(of("plain"), Some(s_arr(OirType::String)), "control: non-optional children fold to the result alone");
    assert_eq!(
        children_of(&s_arr(maybe()), "Tree", &OirType::String, &bindings),
        Some(s_arr(OirType::optional(OirType::String))),
        "a list node of optional elements"
    );
}

#[test]
fn a_fold_step_reads_its_children_typed_by_its_own_result() {
    let seen = std::cell::RefCell::new(Vec::new());
    let step = |params: &[Option<OirType>]| {
        seen.borrow_mut().push(params.to_vec());
        Some(OirType::String)
    };
    assert_eq!(
        fold(tree_type(), Some("Tree"), &step),
        Ok(Some(OirType::String))
    );
    let kids = struct_of(&[
        ("kids", s_arr(OirType::String)),
        ("parent", OirType::String),
        ("named", OirType::map(OirType::String, OirType::String)),
    ]);
    assert_eq!(
        seen.borrow().as_slice(),
        &[
            vec![Some(tree_type()), None],
            vec![Some(tree_type()), Some(kids)]
        ],
        "a provisional reading, then one with the children typed; `tags` is no child"
    );
}

#[test]
fn a_fold_step_whose_result_changes_with_its_children_is_a_mismatch() {
    let step = |params: &[Option<OirType>]| {
        Some(if params[1].is_some() {
            OirType::Number
        } else {
            OirType::String
        })
    };
    let Err(Mismatch::Type { expected, actual }) = fold(tree_type(), Some("Tree"), &step)
    else {
        panic!("a step returning number over string children is refused");
    };
    assert!(
        expected.contains("childResults") && expected.contains("string"),
        "{expected}"
    );
    assert!(actual.contains("number"), "{actual}");
}

#[test]
fn control_a_fold_step_over_a_non_recursive_type_gets_no_children() {
    let seen = std::cell::RefCell::new(Vec::new());
    let step = |params: &[Option<OirType>]| {
        seen.borrow_mut().push(params[1].clone());
        Some(OirType::Boolean)
    };
    let note = struct_of(&[("of", s_arr(OirType::String)), ("Note", OirType::String)]);
    assert_eq!(fold(note, Some("Note"), &step), Ok(Some(OirType::Boolean)));
    assert_eq!(seen.borrow().last(), Some(&Some(optional_fields(&[]))));
}

#[test]
fn a_fold_over_a_tagged_union_types_the_children_of_every_variant() {
    let own = || OirType::TypeRef {
        name: "Pred".to_string(),
    };
    let union = OirType::Union {
        variants: vec![
            OirField::new(
                "All",
                struct_of(&[("kind", OirType::String), ("of", s_arr(own()))]),
            ),
            OirField::new(
                "Not",
                struct_of(&[("kind", OirType::String), ("of", own())]),
            ),
        ].into(),
    };
    let last = std::cell::RefCell::new(None);
    let step = |params: &[Option<OirType>]| {
        *last.borrow_mut() = params[1].clone();
        Some(OirType::String)
    };
    assert_eq!(fold(union, Some("Pred"), &step), Ok(Some(OirType::String)));
    let expected = OirType::Union {
        variants: vec![
            OirField::new("All", struct_of(&[("of", s_arr(OirType::String))])),
            OirField::new("Not", struct_of(&[("of", OirType::String)])),
        ].into(),
    };
    assert_eq!(last.borrow().clone(), Some(expected));
}

fn pred_children() -> OirType {
    OirType::Union {
        variants: vec![
            OirField::new("All", struct_of(&[("of", s_arr(OirType::String))])),
            OirField::new("Not", struct_of(&[("of", OirType::String)])),
        ].into(),
    }
}

#[test]
fn a_union_member_read_whose_variants_differ_is_their_union_not_one_variants_type() {
    let Path::Found(OirType::Union { variants }) = walk_path(&pred_children(), "of") else {
        panic!("a member declared with different types reads as the union of them")
    };
    let types: Vec<_> = variants.iter().map(|v| v.field_type.clone()).collect();
    assert_eq!(types, vec![s_arr(OirType::String), OirType::String]);
}

#[test]
fn control_a_union_member_read_whose_variants_agree_is_one_type() {
    let alike = OirType::Union {
        variants: vec![
            OirField::new("A", struct_of(&[("of", s_arr(OirType::String))])),
            OirField::new("B", struct_of(&[("of", s_arr(OirType::String))])),
        ].into(),
    };
    let Path::Found(found) = walk_path(&alike, "of") else {
        panic!("agreeing variants read as one type")
    };
    assert_eq!(found, s_arr(OirType::String));
}

#[test]
fn control_without_the_shape_the_children_stay_untyped() {
    let seen = std::cell::RefCell::new(Vec::new());
    let step = |params: &[Option<OirType>]| {
        seen.borrow_mut().push(params[1].clone());
        Some(OirType::String)
    };
    assert_eq!(fold(tree_type(), None, &step), Ok(Some(OirType::String)));
    assert_eq!(seen.borrow().as_slice(), &[None, None]);
}

#[test]
fn a_predicate_lambda_must_return_a_boolean() {
    let filter = op(
        json!([
            p("arr", list(var("T"))),
            p(
                "pred",
                json!({ "kind": "fn", "params": [var("T")], "returns": "boolean" })
            ),
        ]),
        list(var("T")),
        Some(2),
    );
    let returns_number = |_: &[Option<OirType>]| Some(OirType::Number);
    let lambda = Actual {
        known: None,
        text: None,
        texts: None,
        lambda: Some(&returns_number),
    };
    let sig = Signature::of(&filter);
    let mut b = CallBindings::new();
    sig.bind(&mut b, 0, &ty(s_arr(OirType::String)), Mode::Static)
        .unwrap();
    assert_eq!(
        sig.bind(&mut b, 1, &lambda, Mode::Static),
        Err(Mismatch::Type {
            expected: "boolean".into(),
            actual: "number".into()
        })
    );
}

fn get() -> EmbeddedOperator {
    op(
        json!([
            p("obj", record("R")),
            p("path", json!({ "kind": "keyOf", "of": "R" })),
            p("default", json!({ "kind": "fieldOf", "of": "R" })),
        ]),
        json!({ "kind": "fieldOf", "of": "R" }),
        Some(3),
    )
}

#[test]
fn a_key_read_through_a_maybe_missing_record_is_that_fields_type_maybe_missing() {
    let row = struct_of(&[("name", OirType::String), ("count", OirType::Number)]);
    assert_eq!(
        call(&get(), &[ty(OirType::optional(row.clone())), text("count")], Mode::Static),
        Ok(Some(OirType::optional(OirType::Number)))
    );
    assert_eq!(
        call(&get(), &[ty(OirType::optional(row.clone())), text("count"), ty(OirType::Number)], Mode::Static),
        Ok(Some(OirType::Number)),
        "control: a present default answers the missing record"
    );
    assert_eq!(call(&get(), &[ty(row), text("count")], Mode::Static), Ok(Some(OirType::Number)), "control: a present record");
}

#[test]
fn a_key_names_a_field_and_the_result_is_that_fields_type() {
    let row = struct_of(&[("name", OirType::String), ("count", OirType::Number)]);
    assert_eq!(
        call(&get(), &[ty(row.clone()), text("count")], Mode::Static),
        Ok(Some(OirType::Number))
    );
    assert_eq!(
        call(&get(), &[ty(row.clone()), text("missing")], Mode::Static),
        Err(Mismatch::NoField {
            path: "missing".into(),
            record: "object".into()
        })
    );
}

#[test]
fn a_dotted_key_reads_through_a_declared_name_between_its_segments() {
    let density = struct_of(&[("h", OirType::String)]);
    let outer = struct_of(&[
        ("density", OirType::optional(OirType::TypeRef { name: "D".into() })),
        ("name", OirType::String),
    ]);
    let get_op = get();
    let sig = Signature::of(&get_op);
    let read = |names: &[(&str, OirType)], path: &str| {
        let mut b = CallBindings::new();
        b.use_named_types(std::sync::Arc::new(
            names.iter().map(|(n, t)| ((*n).to_string(), t.clone())).collect(),
        ));
        sig.bind(&mut b, 0, &ty(outer.clone()), Mode::Static)?;
        sig.bind(&mut b, 1, &text(path), Mode::Static)?;
        Ok::<_, Mismatch>(sig.result(&b))
    };
    assert_eq!(
        read(&[("D", density.clone())], "density.h"),
        Ok(Some(OirType::String))
    );
    assert!(
        matches!(read(&[("D", density)], "density.zzz"), Err(Mismatch::NoField { .. }) | Ok(None)),
        "control: a segment the named type does not declare is not a field"
    );
    assert_eq!(read(&[], "density.h"), Ok(None), "control: an undeclared name stays opaque");
}

fn get_of_record_or_list() -> EmbeddedOperator {
    let bound = json!({ "kind": "union", "of": [{ "kind": "record" }, list(var("E"))] });
    op(
        json!([
            p("obj", json!({ "kind": "var", "name": "R", "bound": bound })),
            p("path", json!({ "kind": "keyOf", "of": "R" })),
            p("default", json!({ "kind": "fieldOf", "of": "R" })),
        ]),
        json!({ "kind": "fieldOf", "of": "R" }),
        Some(3),
    )
}

#[test]
fn a_tuple_is_addressed_by_index_whatever_its_items_are() {
    let get = get_of_record_or_list();
    let pair = OirType::Tuple {
        items: vec![OirType::Number, OirType::String],
    };
    assert_eq!(
        call(&get, &[ty(pair.clone()), text("0")], Mode::Static),
        Ok(Some(OirType::Number))
    );
    assert_eq!(
        call(&get, &[ty(pair.clone()), text("1")], Mode::Static),
        Ok(Some(OirType::String))
    );
    assert!(
        call(&get, &[ty(pair), text("2")], Mode::Static).is_err(),
        "control: a tuple of two has no third item"
    );
    assert_eq!(
        call(&get, &[ty(s_arr(OirType::Number)), text("3")], Mode::Static),
        Ok(Some(OirType::Number)),
        "control: a list of one type"
    );
    assert!(
        call(&get, &[ty(OirType::Number), text("0")], Mode::Static).is_err(),
        "control: a number is neither a record nor a list"
    );
}

#[test]
fn a_tagged_union_member_read_is_the_field_its_variants_declare() {
    let variant = |name: &str, fields: &[(&str, OirType)]| {
        OirField::new(name.to_string(), struct_of(fields))
    };
    let union = OirType::Union {
        variants: vec![
            variant(
                "shape",
                &[("type", OirType::String), ("radius", OirType::Number)],
            ),
            variant(
                "text",
                &[("type", OirType::String), ("content", OirType::String)],
            ),
        ].into(),
    };
    assert_eq!(
        type_at_path(&union, &["type".to_string()]),
        Some(OirType::String),
        "declared by every variant"
    );
    assert_eq!(
        type_at_path(&union, &["radius".to_string()]),
        Some(OirType::optional(OirType::Number)),
        "declared by one of two"
    );
    assert_eq!(
        type_at_path(&union, &["nope".to_string()]),
        None,
        "control: declared by none"
    );
}

#[test]
fn a_default_stands_in_for_a_field_the_record_does_not_declare() {
    let row = struct_of(&[("count", OirType::Number)]);
    assert_eq!(
        call(
            &get(),
            &[ty(row.clone()), text("roles"), ty(s_arr(OirType::String))],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::String)))
    );
    assert_eq!(
        call(&get(), &[ty(row.clone()), text("roles")], Mode::Static),
        Err(Mismatch::NoField {
            path: "roles".into(),
            record: "object".into()
        }),
        "control: without a default the missing field is an error"
    );
    let has = op(
        json!([
            p("obj", record("R")),
            p("path", json!({ "kind": "keyOf", "of": "R" }))
        ]),
        json!("boolean"),
        Some(2),
    );
    assert!(
        call(&has, &[ty(row), text("roles")], Mode::Static).is_ok(),
        "asking whether a field exists is not reading it"
    );
}

#[test]
fn a_default_must_have_the_fields_type() {
    let row = struct_of(&[("count", OirType::Number)]);
    assert!(call(
        &get(),
        &[ty(row.clone()), text("count"), ty(OirType::Number)],
        Mode::Static
    )
    .is_ok());
    assert_eq!(
        call(
            &get(),
            &[ty(row), text("count"), ty(OirType::String)],
            Mode::Static
        ),
        Err(Mismatch::Type {
            expected: "number".into(),
            actual: "string".into()
        })
    );
}

#[test]
fn a_default_for_an_expression_field_is_any_literal_and_a_typed_field_still_checks() {
    let row = struct_of(&[("default", OirType::SExpr), ("count", OirType::Number)]);
    assert!(call(
        &get(),
        &[ty(row.clone()), text("default"), ty(OirType::String)],
        Mode::Static
    )
    .is_ok());
    assert!(call(
        &get(),
        &[ty(row.clone()), text("default"), ty(OirType::Number)],
        Mode::Static
    )
    .is_ok());
    assert!(
        call(
            &get(),
            &[ty(row), text("count"), ty(OirType::String)],
            Mode::Static
        )
        .is_err(),
        "control: a number field still rejects a string default"
    );
}

#[test]
fn a_dot_path_walks_nested_records_and_a_map_takes_any_key() {
    let nested = struct_of(&[("owner", struct_of(&[("name", OirType::String)]))]);
    assert_eq!(
        call(&get(), &[ty(nested), text("owner.name")], Mode::Static),
        Ok(Some(OirType::String))
    );
    let map = OirType::map(OirType::String, OirType::Number);
    assert_eq!(
        call(&get(), &[ty(map), text("anything")], Mode::Static),
        Ok(Some(OirType::Number))
    );
}

#[test]
fn branch_record_list_fields_keep_members_from_both_element_shapes() {
    let mut pick = op(
        json!([
            p("condition", json!("boolean")),
            p("yes", var("T")),
            p("no", var("T"))
        ]),
        var("T"),
        Some(3),
    );
    pick.return_semantics = Some("branch-union".into());
    let left = struct_of(&[("cells", s_arr(struct_of(&[("kind", OirType::String)])))]);
    let right = struct_of(&[(
        "cells",
        s_arr(struct_of(&[
            ("kind", OirType::String),
            ("kids", s_arr(OirType::String)),
        ])),
    )]);
    let joined = call(
        &pick,
        &[ty(OirType::Boolean), ty(left), ty(right)],
        Mode::Static,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        type_at_path(&joined, &["cells".into(), "0".into(), "kids".into()]),
        Some(OirType::optional(s_arr(OirType::String))),
        "a member only one element shape declares may be absent, so its read is optional"
    );
    assert_eq!(
        type_at_path(&joined, &["cells".into(), "0".into(), "kind".into()]),
        Some(OirType::String),
        "control: a member both shapes require reads non-optional"
    );
    let optional = optional_fields(&[("note", OirType::String)]);
    let joined = call(
        &pick,
        &[ty(OirType::Boolean), ty(optional.clone()), ty(optional)],
        Mode::Static,
    )
    .unwrap()
    .unwrap();
    let OirType::Struct { fields, .. } = joined else {
        panic!("record")
    };
    assert!(
        !fields[0].required,
        "a member optional in both branches remains optional"
    );
}

#[test]
fn lists_of_record_elements_share_the_canonical_optional_field_join() {
    let mut concat = op(
        json!([p("left", list(var("E"))), p("right", list(var("E")))]),
        list(var("E")),
        Some(2),
    );
    concat.return_semantics = Some("union-of-args".into());
    let left = struct_of(&[("kind", OirType::String), ("kids", s_arr(OirType::String))]);
    let right = struct_of(&[("kind", OirType::String), ("val", OirType::Number)]);
    let actual = call(&concat, &[ty(s_arr(left)), ty(s_arr(right))], Mode::Static);
    let expected = OirType::array(optional_fields(&[
        ("kind", OirType::String),
        ("kids", s_arr(OirType::String)),
        ("val", OirType::Number),
    ]));
    let Ok(Some(OirType::Array { inner })) = actual else {
        panic!("record lists must join: {actual:?}")
    };
    let OirType::Array { inner: expected } = expected else {
        unreachable!()
    };
    let OirType::Struct { fields, .. } = &*inner else {
        panic!("record element")
    };
    assert!(fields.iter().find(|f| f.name == "kind").unwrap().required);
    assert!(!fields.iter().find(|f| f.name == "kids").unwrap().required);
    assert!(!fields.iter().find(|f| f.name == "val").unwrap().required);
    assert_eq!(
        fields
            .iter()
            .map(|f| (&f.name, &f.field_type))
            .collect::<Vec<_>>(),
        match &*expected {
            OirType::Struct { fields, .. } => fields
                .iter()
                .map(|f| (&f.name, &f.field_type))
                .collect::<Vec<_>>(),
            _ => unreachable!(),
        }
    );
    assert!(call(
        &concat,
        &[ty(s_arr(OirType::String)), ty(s_arr(OirType::Number))],
        Mode::Static
    )
    .is_err());
}

#[test]
fn a_computed_key_over_a_record_of_alike_members_is_their_join() {
    let stat = |hp: OirType| struct_of(&[("hp", hp), ("name", OirType::String)]);
    let roster = struct_of(&[
        ("goblin", stat(OirType::Number)),
        ("orc", stat(OirType::Number)),
    ]);
    let key = || Actual::of_type(OirType::String);
    assert_eq!(
        call(&get(), &[ty(roster), key()], Mode::Static),
        Ok(Some(OirType::optional(stat(OirType::Number)))),
        "whichever member the key names has the members' shape, and a key naming none answers null"
    );
    // members that differ in a field: it is optional in the join.
    let mixed = struct_of(&[
        ("a", struct_of(&[("hp", OirType::Number)])),
        (
            "b",
            struct_of(&[("hp", OirType::Number), ("tag", OirType::String)]),
        ),
    ]);
    let joined = call(&get(), &[ty(mixed), key()], Mode::Static)
        .unwrap()
        .unwrap();
    let OirType::Optional { inner } = joined else {
        panic!("an optional struct")
    };
    let OirType::Struct { fields, .. } = *inner else {
        panic!("a struct")
    };
    assert!(fields.iter().find(|f| f.name == "hp").unwrap().required);
    assert!(!fields.iter().find(|f| f.name == "tag").unwrap().required);
    // members of different kinds are the union of their types; an empty record has no member to name.
    let unlike = struct_of(&[("a", OirType::Number), ("b", OirType::String)]);
    assert_eq!(
        call(&get(), &[ty(unlike), key()], Mode::Static),
        Ok(Some(OirType::optional(OirType::Union {
            variants: vec![
                OirField::new("0".to_string(), OirType::Number),
                OirField::new("1".to_string(), OirType::String),
            ]
            .into(),
        })))
    );
    assert_eq!(
        call(&get(), &[ty(struct_of(&[])), key()], Mode::Static),
        Ok(None)
    );
    // a scalar member list is the member type.
    let uniform = struct_of(&[("a", OirType::Number), ("b", OirType::Number)]);
    assert_eq!(
        call(&get(), &[ty(uniform), key()], Mode::Static),
        Ok(Some(OirType::optional(OirType::Number)))
    );
}

#[test]
fn a_missing_field_read_with_the_empty_list_default_is_typed_by_what_it_is_handed_to() {
    let row = struct_of(&[("name", OirType::String)]);
    let includes = op(
        json!([p("arr", list(var("T"))), p("item", var("T"))]),
        json!("boolean"),
        Some(2),
    );
    let getter = get();
    let sig_get = Signature::of(&getter);
    let mut b = CallBindings::new();
    sig_get.bind(&mut b, 0, &ty(row), Mode::Static).unwrap();
    sig_get
        .bind(&mut b, 1, &text("roles"), Mode::Static)
        .unwrap();
    sig_get.bind(&mut b, 2, &empty(), Mode::Static).unwrap();
    sig_get.finish(&b, 3).unwrap();
    let read = sig_get.result(&b);
    assert_eq!(
        read,
        Some(OirType::Tuple { items: Vec::new() }),
        "the default is the read"
    );
    // handed to a list operator with a typed sibling, it is that list; with none, it is untyped.
    assert_eq!(
        call(
            &includes,
            &[
                Actual {
                    known: read.clone().map(Known::Type),
                    ..Actual::unknown()
                },
                ty(OirType::String)
            ],
            Mode::Static
        ),
        Ok(Some(OirType::Boolean))
    );
    assert!(is_undetermined(call(
        &op(json!([p("arr", list(var("T")))]), json!("number"), Some(1)),
        &[Actual {
            known: read.map(Known::Type),
            ..Actual::unknown()
        }],
        Mode::Static
    )));
}

#[test]
fn a_key_read_from_a_value_is_the_value_type_of_a_map_and_nothing_of_a_struct() {
    // The key is an expression (a payload field), not a literal: a map answers its value type whatever key.
    let dynamic = Actual::of_type(OirType::String);
    let map = OirType::map(OirType::String, OirType::Number);
    assert_eq!(
        call(&get(), &[ty(map.clone()), Actual::of_type(OirType::String)], Mode::Static),
        Ok(Some(OirType::optional(OirType::Number))),
        "a map holds one value type under every key, and a key it lacks answers null"
    );
    assert_eq!(
        call(&get(), &[ty(map), dynamic, ty(OirType::Number)], Mode::Static),
        Ok(Some(OirType::Number)),
        "control: a default answers the missing key"
    );
    let row = struct_of(&[("name", OirType::String), ("count", OirType::Number)]);
    assert_eq!(
        call(&get(), &[ty(row), Actual::of_type(OirType::String)], Mode::Static),
        Ok(Some(OirType::optional(OirType::Union {
            variants: vec![
                OirField::new("0".to_string(), OirType::String),
                OirField::new("1".to_string(), OirType::Number),
            ]
            .into(),
        }))),
        "a computed key over a closed record is the union of its field types, and null when it names none"
    );
    let list = s_arr(OirType::Number);
    assert_eq!(
        call(
            &get_of_record_or_list(),
            &[ty(list), Actual::of_type(OirType::String)],
            Mode::Static
        ),
        Ok(Some(OirType::Number)),
        "a list answers its element type at a computed index"
    );
}

#[test]
fn at_run_time_a_key_is_not_checked_but_the_record_is() {
    let row = struct_of(&[]);
    assert!(call(&get(), &[ty(row), text("whatever")], Mode::Runtime).is_ok());
    assert_eq!(
        call(&get(), &[ty(OirType::Number), text("x")], Mode::Runtime),
        Err(Mismatch::Type {
            expected: "object".into(),
            actual: "number".into()
        })
    );
}

#[test]
fn additional_records_transforms_preserve_the_same_value_slot() {
    let prior = OirType::Struct {
        fields: Default::default(),
        additional: Some(Box::new(OirType::Tuple { items: vec![] })),
    };
    let actual = OirType::Struct {
        fields: Default::default(),
        additional: Some(Box::new(OirType::array(OirType::Number))),
    };
    assert_eq!(fill_empty_list_slots(&prior, &actual), Some(actual));
    let node = OirType::Struct {
        fields: Default::default(),
        additional: Some(Box::new(OirType::TypeRef { name: "Node".into() })),
    };
    assert_eq!(
        children_of(&node, "Node", &OirType::String, &CallBindings::new()),
        Some(OirType::Struct {
            fields: Default::default(),
            additional: Some(Box::new(OirType::String)),
        })
    );
}

#[test]
fn additional_records_paths_and_pick_omit_keep_presence_distinct() {
    let optional = optional_fields(&[("maybe", OirType::String)]);
    assert_eq!(type_at_path(&optional, &["maybe".into()]), Some(OirType::optional(OirType::String)));
    let row = OirType::Struct {
        fields: vec![OirField::new("fixed", OirType::String)].into(),
        additional: Some(Box::new(OirType::Number)),
    };
    assert_eq!(type_at_path(&row, &["extra".into()]), Some(OirType::optional(OirType::Number)));
    let operator = |kind: &str| op(
        json!([p("obj", record("R")), p("keys", list(json!({ "kind": "keyOf", "of": "R" })))]),
        json!({ "kind": kind, "of": "R" }), Some(2),
    );
    let keys = |name: &str| Actual {
        known: None, text: None, texts: Some(vec![name.into()]), lambda: None,
    };
    let mut extra = OirField::new("extra", OirType::Number);
    extra.required = false;
    assert_eq!(call(&operator("pickOf"), &[ty(row.clone()), keys("extra")], Mode::Static),
        Ok(Some(OirType::Struct { fields: vec![extra].into(), additional: None })));
    assert_eq!(call(&operator("omitOf"), &[ty(row), keys("fixed")], Mode::Static),
        Ok(Some(OirType::Struct { fields: Default::default(), additional: Some(Box::new(OirType::Number)) })));
}

#[test]
fn omit_with_literal_keys_is_exact_and_with_computed_keys_makes_each_field_optional() {
    let omit = op(
        json!([p("obj", record("R")), p("keys", list(json!({ "kind": "keyOf", "of": "R" })))]),
        json!({ "kind": "omitOf", "of": "R" }), Some(2),
    );
    let row = struct_of(&[("a", OirType::String), ("b", OirType::Number)]);
    let literal = Actual { known: None, text: None, texts: Some(vec!["b".into()]), lambda: None };
    assert_eq!(call(&omit, &[ty(row.clone()), literal], Mode::Static), Ok(Some(struct_of(&[("a", OirType::String)]))));
    let computed = ty(OirType::array(OirType::String));
    let mut a = OirField::new("a", OirType::String);
    a.required = false;
    let mut b = OirField::new("b", OirType::Number);
    b.required = false;
    assert_eq!(
        call(&omit, &[ty(row), computed], Mode::Static),
        Ok(Some(OirType::Struct { fields: vec![a, b].into(), additional: None })),
        "keys known only at run time drop any subset"
    );
}

#[test]
fn a_map_position_takes_a_mixed_closed_record_and_hands_back_optional_fields() {
    let filter = op(
        json!([p("obj", json!({ "kind": "map", "key": { "kind": "var", "name": "K" }, "value": { "kind": "var", "name": "V" } }))]),
        json!({ "kind": "map", "key": { "kind": "var", "name": "K" }, "value": { "kind": "var", "name": "V" } }), Some(1),
    );
    let mixed = struct_of(&[("a", OirType::String), ("b", OirType::Number)]);
    let mut a = OirField::new("a", OirType::String);
    a.required = false;
    let mut b = OirField::new("b", OirType::Number);
    b.required = false;
    assert_eq!(
        call(&filter, &[ty(mixed)], Mode::Static),
        Ok(Some(OirType::Struct { fields: vec![a, b].into(), additional: None })),
    );
    let map = OirType::map(OirType::String, OirType::Number);
    assert_eq!(call(&filter, &[ty(map.clone())], Mode::Static), Ok(Some(map)), "a map keeps its value type");
    assert!(
        matches!(call(&filter, &[ty(OirType::Row { entity: "E".into() })], Mode::Static), Err(Mismatch::Undetermined { .. })),
        "control: a record whose fields the call cannot see stays undetermined"
    );
}

#[test]
fn a_map_position_takes_a_union_of_a_map_and_empty_records_as_the_map() {
    let filter = op(
        json!([p("obj", json!({ "kind": "map", "key": { "kind": "var", "name": "K" }, "value": { "kind": "var", "name": "V" } }))]),
        json!({ "kind": "map", "key": { "kind": "var", "name": "K" }, "value": { "kind": "var", "name": "V" } }), Some(1),
    );
    let empty = || OirType::Struct { fields: Default::default(), additional: None };
    let map = OirType::map(OirType::String, OirType::optional(OirType::Number));
    let union = OirType::Union {
        variants: vec![OirField::new("number", empty()), OirField::new("boolean", empty()), OirField::new("map", map.clone())].into(),
    };
    assert_eq!(call(&filter, &[ty(union)], Mode::Static), Ok(Some(map)), "empty records hold no values");
    let only_empty = OirType::Union { variants: vec![OirField::new("number", empty()), OirField::new("boolean", empty())].into() };
    assert!(
        matches!(call(&filter, &[ty(only_empty)], Mode::Static), Err(Mismatch::Undetermined { .. })),
        "control: a union of only empty records stays undetermined"
    );
}

#[test]
fn a_type_mismatch_names_an_optional_actual_with_its_question_mark() {
    let cond = op(json!([p("cond", json!("boolean"))]), json!("boolean"), Some(1));
    assert_eq!(
        call(&cond, &[ty(OirType::optional(OirType::Boolean))], Mode::Proof),
        Err(Mismatch::Type { expected: "boolean".into(), actual: "boolean?".into() })
    );
    assert_eq!(
        call(&cond, &[ty(OirType::String)], Mode::Proof),
        Err(Mismatch::Type { expected: "boolean".into(), actual: "string".into() }),
        "control: a present type prints bare"
    );
}

#[test]
fn additional_records_branch_join_covers_both_extra_value_types() {
    let branch = |additional| OirType::Struct {
        fields: vec![OirField::new("fixed", OirType::String)].into(),
        additional: Some(Box::new(additional)),
    };
    let joined = unify_structs(&[branch(OirType::Number), branch(OirType::Boolean)]).unwrap();
    let OirType::Struct { additional: Some(additional), .. } = joined else { panic!("typed extras retained") };
    assert!(crate::type_compat::sub(&OirType::Number, &additional).is_some());
    assert!(crate::type_compat::sub(&OirType::Boolean, &additional).is_some());
    let mixed = unify_structs(&[branch(OirType::Number), struct_of(&[("fixed", OirType::String)])]).unwrap();
    assert!(matches!(mixed, OirType::Struct { additional: None, .. }));
}

#[test]
fn additional_records_merge_retains_typed_extras_and_right_known_fields() {
    let merge = op(json!([p("...objs", json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true }))]),
        json!({ "kind": "mergeOf", "of": "R" }), None);
    let left = OirType::Struct { fields: vec![OirField::new("left", OirType::Number)].into(), additional: Some(Box::new(OirType::Number)) };
    let right = OirType::Struct { fields: vec![OirField::new("right", OirType::String)].into(), additional: Some(Box::new(OirType::Number)) };
    assert_eq!(call(&merge, &[ty(left), ty(right)], Mode::Static), Ok(Some(OirType::Struct {
        fields: vec![OirField::new("left", OirType::Number), OirField::new("right", OirType::String)].into(),
        additional: Some(Box::new(OirType::Number)),
    })));
}

#[test]
fn a_nullable_merge_is_the_optional_of_the_merged_record() {
    let objs = json!([p("...objs", json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true }))]);
    let nullable = op(objs.clone(), json!({ "kind": "union", "of": [{ "kind": "mergeOf", "of": "R" }, { "kind": "literal", "value": null }] }), None);
    let plain = op(objs, json!({ "kind": "mergeOf", "of": "R" }), None);
    let operands = [ty(struct_of(&[("a", OirType::Number)])), ty(struct_of(&[("b", OirType::String)]))];
    let merged = struct_of(&[("a", OirType::Number), ("b", OirType::String)]);
    assert_eq!(call(&plain, &operands, Mode::Static), Ok(Some(merged.clone())), "control: the plain merge");
    assert_eq!(call(&nullable, &operands, Mode::Static), Ok(Some(OirType::optional(merged))));
}

#[test]
fn additional_records_unknown_branch_does_not_claim_a_missing_named_field() {
    let mut choose = op(json!([p("condition", json!("boolean")), p("yes", var("T")), p("no", var("T"))]),
        var("T"), Some(3));
    choose.return_semantics = Some("branch-union".into());
    let typed = OirType::Struct {
        fields: vec![OirField::new("amount", OirType::Number)].into(),
        additional: Some(Box::new(OirType::Number)),
    };
    for operands in [[typed.clone(), struct_of(&[])], [struct_of(&[]), typed.clone()]] {
        assert!(matches!(call(&choose,
            &[ty(OirType::Boolean), ty(operands[0].clone()), ty(operands[1].clone())],
            Mode::Static), Err(Mismatch::Undetermined { .. })));
    }
    let mut concat = op(json!([p("left", list(var("E"))), p("right", list(var("E")))]),
        list(var("E")), Some(2));
    concat.return_semantics = Some("union-of-args".into());
    assert!(matches!(call(&concat,
        &[ty(s_arr(typed.clone())), ty(s_arr(struct_of(&[])))], Mode::Static),
        Err(Mismatch::Undetermined { .. })));
    let array = op(json!([]), list(json!({ "kind": "var", "name": "T", "each": true })), None);
    let mut bindings = CallBindings::new();
    bindings.vars.insert("T".into(), vec![typed.clone(), struct_of(&[])]);
    assert_eq!(Signature::of(&array).result(&bindings), None);
    let roster = struct_of(&[("typed", typed), ("unknown", struct_of(&[]))]);
    assert_eq!(call(&get(), &[ty(roster), ty(OirType::String)], Mode::Static), Ok(None));
}

#[test]
fn additional_records_unknown_merge_cannot_overwrite_known_members_unnoticed() {
    let merge = op(json!([p("...objs", json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true }))]),
        json!({ "kind": "mergeOf", "of": "R" }), None);
    let typed = OirType::Struct {
        fields: vec![OirField::new("amount", OirType::Number)].into(),
        additional: Some(Box::new(OirType::Number)),
    };
    assert_eq!(call(&merge, &[ty(typed.clone()), ty(struct_of(&[]))], Mode::Static), Ok(None));
    let mut optional = typed.clone();
    if let OirType::Struct { fields, .. } = &mut optional { fields[0].required = false; }
    assert_eq!(call(&merge, &[ty(struct_of(&[])), ty(optional)], Mode::Static), Ok(None));
    assert_eq!(call(&merge, &[ty(struct_of(&[])), ty(typed)], Mode::Static),
        Ok(Some(struct_of(&[("amount", OirType::Number)]))));
    let ordinary = struct_of(&[("amount", OirType::Number)]);
    assert_eq!(call(&merge, &[ty(ordinary.clone()), ty(struct_of(&[]))], Mode::Static), Ok(Some(ordinary)));
    let left = OirType::Struct { fields: Default::default(), additional: Some(Box::new(OirType::Number)) };
    let mut amount = OirField::new("amount", OirType::String);
    amount.required = false;
    let right = OirType::Struct { fields: vec![amount].into(), additional: Some(Box::new(OirType::Number)) };
    let merged = call(&merge, &[ty(left), ty(right)], Mode::Static).unwrap().unwrap();
    let OirType::Struct { fields, additional: Some(_), .. } = merged else { panic!("typed merge retained") };
    assert!(!fields[0].required);
    assert!(crate::type_compat::sub(&OirType::Number, &fields[0].field_type).is_some());
    assert!(crate::type_compat::sub(&OirType::String, &fields[0].field_type).is_some());
}

#[test]
fn pick_and_omit_keep_and_drop_the_named_fields() {
    let pick = op(
        json!([
            p("obj", record("R")),
            p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
        ]),
        json!({ "kind": "pickOf", "of": "R" }),
        Some(2),
    );
    let omit = op(
        json!([
            p("obj", record("R")),
            p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
        ]),
        json!({ "kind": "omitOf", "of": "R" }),
        Some(2),
    );
    let row = struct_of(&[("a", OirType::Number), ("b", OirType::String)]);
    let keys = Actual {
        known: None,
        text: None,
        texts: Some(vec!["a".to_string()]),
        lambda: None,
    };
    assert_eq!(
        call(&pick, &[ty(row.clone()), keys], Mode::Static),
        Ok(Some(struct_of(&[("a", OirType::Number)])))
    );
    let keys = Actual {
        known: None,
        text: None,
        texts: Some(vec!["a".to_string()]),
        lambda: None,
    };
    assert_eq!(
        call(&omit, &[ty(row.clone()), keys], Mode::Static),
        Ok(Some(struct_of(&[("b", OirType::String)])))
    );
    let unknown = Actual {
        known: None,
        text: None,
        texts: Some(vec!["zzz".to_string()]),
        lambda: None,
    };
    assert_eq!(
        call(&pick, &[ty(row), unknown], Mode::Static),
        Ok(Some(struct_of(&[])))
    );
}

#[test]
fn omitting_or_picking_keys_of_a_map_is_the_map() {
    let omit = op(
        json!([
            p("obj", record("R")),
            p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
        ]),
        json!({ "kind": "omitOf", "of": "R" }),
        Some(2),
    );
    let pick = op(
        json!([
            p("obj", record("R")),
            p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
        ]),
        json!({ "kind": "pickOf", "of": "R" }),
        Some(2),
    );
    let bag = OirType::map(OirType::String, OirType::Number);
    let computed = || ty(s_arr(OirType::String));
    assert_eq!(
        call(&omit, &[ty(bag.clone()), computed()], Mode::Static),
        Ok(Some(bag.clone()))
    );
    assert_eq!(
        call(&pick, &[ty(bag.clone()), computed()], Mode::Static),
        Ok(Some(bag.clone()))
    );
    assert_eq!(
        call(
            &omit,
            &[ty(OirType::optional(bag.clone())), computed()],
            Mode::Static
        ),
        Ok(Some(bag)),
        "an optional map is read as the map"
    );
    let row = struct_of(&[("a", OirType::Number)]);
    let mut maybe = OirField::new("a", OirType::Number);
    maybe.required = false;
    assert_eq!(
        call(&omit, &[ty(row.clone()), computed()], Mode::Static),
        Ok(Some(OirType::Struct { fields: vec![maybe].into(), additional: None })),
        "keys computed from data say nothing about which fields of a record go: each may be gone"
    );
    assert_eq!(
        call(&pick, &[ty(row), computed()], Mode::Static),
        Ok(None),
        "control: computed keys pick no known field"
    );
}

#[test]
fn a_literal_key_list_that_also_has_a_type_still_names_its_keys() {
    let pick = op(
        json!([
            p("obj", record("R")),
            p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
        ]),
        json!({ "kind": "pickOf", "of": "R" }),
        Some(2),
    );
    let row = struct_of(&[("a", OirType::Number), ("b", OirType::String)]);
    let keys = Actual {
        known: Some(Known::Type(OirType::array(OirType::String))),
        text: None,
        texts: Some(vec!["a".to_string()]),
        lambda: None,
    };
    assert_eq!(
        call(&pick, &[ty(row.clone()), keys], Mode::Static),
        Ok(Some(struct_of(&[("a", OirType::Number)])))
    );
    let control = Actual {
        known: Some(Known::Type(OirType::array(OirType::String))),
        text: None,
        texts: None,
        lambda: None,
    };
    assert_eq!(
        call(&pick, &[ty(row), control], Mode::Static),
        Ok(None),
        "computed keys name nothing"
    );
}

#[test]
fn a_merge_is_the_fields_of_every_operand_later_winning() {
    let merge = op(
        json!([p(
            "...objs",
            json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true })
        )]),
        json!({ "kind": "mergeOf", "of": "R" }),
        None,
    );
    let a = struct_of(&[("a", OirType::Number), ("b", OirType::Number)]);
    let b = struct_of(&[("b", OirType::String)]);
    assert_eq!(
        call(&merge, &[ty(a), ty(b)], Mode::Static),
        Ok(Some(struct_of(&[
            ("a", OirType::Number),
            ("b", OirType::String)
        ])))
    );
    assert!(call(
        &merge,
        &[ty(OirType::Number), ty(struct_of(&[]))],
        Mode::Static
    )
    .is_err());
}

#[test]
fn a_merge_of_maps_is_a_map_of_the_value_type_they_share() {
    let merge = op(
        json!([p(
            "...objs",
            json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true })
        )]),
        json!({ "kind": "mergeOf", "of": "R" }),
        None,
    );
    let words = OirType::map(OirType::String, OirType::String);
    assert_eq!(
        call(
            &merge,
            &[ty(words.clone()), ty(words.clone())],
            Mode::Static
        ),
        Ok(Some(words.clone()))
    );
    let optional = OirType::optional(words.clone());
    assert_eq!(
        call(&merge, &[ty(optional), ty(words.clone())], Mode::Static),
        Ok(Some(words.clone())),
        "an optional map merges like a map"
    );
    let named = struct_of(&[("a", OirType::String), ("b", OirType::String)]);
    assert_eq!(
        call(&merge, &[ty(words.clone()), ty(named)], Mode::Static),
        Ok(Some(words.clone())),
        "a record whose members are the map's value type adds its keys to the map"
    );
    let counts = OirType::map(OirType::String, OirType::Number);
    assert_eq!(
        call(&merge, &[ty(words.clone()), ty(counts)], Mode::Static),
        Ok(None),
        "control: maps of unlike values have no one value type"
    );
    let mixed = struct_of(&[("a", OirType::String), ("n", OirType::Number)]);
    assert_eq!(
        call(&merge, &[ty(words), ty(mixed)], Mode::Static),
        Ok(None),
        "control: a member of another type"
    );
}

#[test]
fn a_rest_parameter_covers_every_further_argument() {
    let concat = op(json!([p("...arrs", list(var("T")))]), list(var("T")), None);
    assert!(call(
        &concat,
        &[
            ty(s_arr(OirType::Number)),
            ty(s_arr(OirType::Number)),
            ty(s_arr(OirType::Number))
        ],
        Mode::Static
    )
    .is_ok());
    assert!(call(
        &concat,
        &[ty(s_arr(OirType::Number)), ty(s_arr(OirType::String))],
        Mode::Static
    )
    .is_err());
}

#[test]
fn a_shape_argument_names_the_result_type() {
    let parse = op(
        json!([
            p("s", json!("string")),
            p("shape", json!({ "kind": "shape" }))
        ]),
        json!({ "kind": "shape" }),
        Some(2),
    );
    assert_eq!(
        call(
            &parse,
            &[ty(OirType::String), text("Program")],
            Mode::Static
        ),
        Ok(Some(OirType::TypeRef {
            name: "Program".into()
        }))
    );
    assert!(call(
        &parse,
        &[ty(OirType::String), ty(OirType::Number)],
        Mode::Static
    )
    .is_err());
}

#[test]
fn at_run_time_a_shape_argument_is_a_string() {
    let parse = op(
        json!([
            p("s", json!("string")),
            p("shape", json!({ "kind": "shape" }))
        ]),
        json!({ "kind": "shape" }),
        Some(2),
    );
    assert!(call(
        &parse,
        &[ty(OirType::String), ty(OirType::String)],
        Mode::Runtime
    )
    .is_ok());
    assert_eq!(
        call(
            &parse,
            &[ty(OirType::String), ty(OirType::Number)],
            Mode::Runtime
        ),
        Err(Mismatch::Type {
            expected: "string".into(),
            actual: "number".into()
        })
    );
}

#[test]
fn a_nominal_std_type_is_not_checked_and_a_date_takes_a_string_and_no_number() {
    let nominal = op(
        json!([p("x", json!("tensor")), p("d", json!("date"))]),
        json!("boolean"),
        Some(2),
    );
    assert!(call(
        &nominal,
        &[ty(OirType::Number), ty(OirType::String)],
        Mode::Static
    )
    .is_ok());
    assert!(call(
        &nominal,
        &[ty(OirType::Boolean), ty(OirType::Number)],
        Mode::Static
    )
    .is_err());
    assert!(call(
        &nominal,
        &[ty(OirType::Boolean), ty(OirType::Boolean)],
        Mode::Static
    )
    .is_err());
}

#[test]
fn a_union_parameter_binds_the_member_that_fits() {
    let len = op(
        json!([p(
            "value",
            json!({ "kind": "union", "of": ["string", list(var("T"))] })
        )]),
        json!("boolean"),
        Some(1),
    );
    assert!(call(&len, &[ty(OirType::String)], Mode::Static).is_ok());
    assert!(call(&len, &[ty(s_arr(OirType::Number))], Mode::Static).is_ok());
    assert_eq!(
        call(&len, &[ty(OirType::Number)], Mode::Static),
        Err(Mismatch::Type {
            expected: "string | array".into(),
            actual: "number".into()
        })
    );
}

#[test]
fn a_union_argument_binds_when_every_variant_fits_a_union_parameter() {
    let spawn = op(
        json!([p("command", json!({ "kind": "union", "of": ["string", "object"] }))]),
        json!("boolean"),
        Some(1),
    );
    let config = struct_of(&[("cmd", OirType::String)]);
    let string_or_config = OirType::Union {
        variants: vec![
            OirField::new("string", OirType::String),
            OirField::new("config", config),
        ].into(),
    };
    assert!(call(&spawn, &[ty(string_or_config)], Mode::Static).is_ok());
}

#[test]
fn control_a_union_argument_with_a_variant_no_member_fits_is_refused() {
    let show = op(
        json!([p("v", json!({ "kind": "union", "of": ["string", "number"] }))]),
        json!("boolean"),
        Some(1),
    );
    let string_or_boolean = OirType::Union {
        variants: vec![
            OirField::new("string", OirType::String),
            OirField::new("boolean", OirType::Boolean),
        ].into(),
    };
    assert!(call(&show, &[ty(string_or_boolean)], Mode::Static).is_err());
}

#[test]
fn a_nullable_result_is_optional() {
    let max = op(
        json!([p("arr", list("number".into()))]),
        json!({ "kind": "union", "of": ["number", { "kind": "literal", "value": null }] }),
        Some(1),
    );
    assert_eq!(
        call(&max, &[ty(s_arr(OirType::Number))], Mode::Static),
        Ok(Some(OirType::optional(OirType::Number)))
    );
}

#[test]
fn a_failed_bind_leaves_the_bindings_untouched() {
    let eq = op(
        json!([p("a", var("T")), p("b", var("T"))]),
        json!("boolean"),
        Some(2),
    );
    let sig = Signature::of(&eq);
    let mut b = CallBindings::new();
    sig.bind(&mut b, 0, &ty(OirType::Number), Mode::Static)
        .unwrap();
    assert!(sig
        .bind(&mut b, 1, &ty(OirType::String), Mode::Static)
        .is_err());
    assert!(sig
        .bind(&mut b, 1, &ty(OirType::Number), Mode::Static)
        .is_ok());
}

#[test]
fn a_literal_sequence_is_a_list_of_one_type_or_a_tuple_of_many() {
    let each = json!({ "kind": "var", "name": "T", "each": true });
    let list = op(json!([p("...items", each.clone())]), list(each), None);
    assert_eq!(
        call(
            &list,
            &[ty(OirType::Number), ty(OirType::Number)],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::Number)))
    );
    assert_eq!(
        call(
            &list,
            &[ty(OirType::Number), ty(OirType::String)],
            Mode::Static
        ),
        Ok(Some(OirType::Tuple {
            items: vec![OirType::Number, OirType::String]
        }))
    );
}

fn undetermined() -> Actual<'static> {
    Actual {
        known: None,
        text: None,
        texts: None,
        lambda: None,
    }
}

fn absent() -> Actual<'static> {
    Actual {
        known: Some(Known::Absent),
        text: None,
        texts: None,
        lambda: None,
    }
}

fn is_undetermined(r: Result<Option<OirType>, Mismatch>) -> bool {
    matches!(r, Err(Mismatch::Undetermined { .. }))
}

#[test]
fn a_literal_sequence_with_an_undetermined_item_is_an_error() {
    let each = json!({ "kind": "var", "name": "T", "each": true });
    let list = op(json!([p("...items", each.clone())]), list(each), None);
    assert!(is_undetermined(call(
        &list,
        &[undetermined(), ty(OirType::Number)],
        Mode::Static
    )));
    assert!(is_undetermined(call(
        &list,
        &[ty(OirType::Number), undetermined()],
        Mode::Static
    )));
    assert_eq!(
        call(&list, &[ty(OirType::Number)], Mode::Static),
        Ok(Some(s_arr(OirType::Number))),
        "control: with every item determined it is typed"
    );
    assert!(
        is_undetermined(call(&list, &[ty(OirType::erased())], Mode::Static)),
        "json is no type"
    );
}

#[test]
fn a_variable_handed_an_undetermined_or_opaque_value_is_an_error() {
    let prepend = op(
        json!([p("arr", list(var("T"))), p("item", var("T"))]),
        list(var("T")),
        Some(2),
    );
    assert_eq!(
        call(
            &prepend,
            &[ty(s_arr(OirType::String)), ty(OirType::String)],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::String))),
        "control: a list of strings and a string is a list of strings"
    );
    assert!(is_undetermined(call(
        &prepend,
        &[ty(s_arr(OirType::erased())), ty(OirType::String)],
        Mode::Static
    )));
    assert!(is_undetermined(call(
        &prepend,
        &[undetermined(), ty(OirType::String)],
        Mode::Static
    )));
    assert!(is_undetermined(call(
        &prepend,
        &[ty(s_arr(OirType::String)), undetermined()],
        Mode::Static
    )));
    let pick = op(
        json!([p("c", json!("boolean")), p("a", var("T")), p("b", var("T"))]),
        var("T"),
        Some(3),
    );
    assert!(is_undetermined(call(
        &pick,
        &[ty(OirType::Boolean), undetermined(), ty(OirType::Number)],
        Mode::Static
    )));
    assert_eq!(
        call(
            &pick,
            &[
                ty(OirType::Boolean),
                ty(OirType::Number),
                ty(OirType::Number)
            ],
            Mode::Static
        ),
        Ok(Some(OirType::Number)),
        "control"
    );
    let heterogeneous = OirType::Tuple {
        items: vec![OirType::Number, OirType::String],
    };
    assert!(is_undetermined(call(
        &prepend,
        &[ty(heterogeneous), ty(OirType::String)],
        Mode::Static
    )));
}

fn nth_operator() -> EmbeddedOperator {
    let each = json!({ "kind": "var", "name": "E", "each": true });
    op(
        json!([
            p(
                "arr",
                json!({ "kind": "union", "of": [list(var("T")), list(each)] })
            ),
            p("index", json!("number"))
        ]),
        var("T"),
        Some(2),
    )
}

#[test]
fn a_pair_of_unlike_types_is_indexed_by_array_nth_and_a_list_still_types_its_element() {
    let nth = nth_operator();
    let pair = OirType::Tuple {
        items: vec![OirType::String, OirType::Number],
    };
    assert!(
        call(&nth, &[ty(pair), ty(OirType::Number)], Mode::Static).is_ok(),
        "an unlike pair is addressable"
    );
    assert_eq!(
        call(
            &nth,
            &[ty(s_arr(OirType::Number)), ty(OirType::Number)],
            Mode::Static
        ),
        Ok(Some(OirType::Number)),
        "control: a list of one type still gives its element type"
    );
    assert!(
        call(
            &nth,
            &[ty(OirType::Number), ty(OirType::Number)],
            Mode::Static
        )
        .is_err(),
        "control: a number is no list"
    );
}

#[test]
fn tuples_of_unlike_widths_are_one_list() {
    let prepend = op(
        json!([p("arr", list(var("T"))), p("item", var("T"))]),
        list(var("T")),
        Some(2),
    );
    let tuple = |items: Vec<OirType>| OirType::Tuple { items };
    let delete = tuple(vec![OirType::String, OirType::String, OirType::String]);
    let update = tuple(vec![
        OirType::String,
        OirType::String,
        OirType::String,
        OirType::Number,
    ]);
    assert!(call(
        &prepend,
        &[ty(s_arr(update.clone())), ty(delete.clone())],
        Mode::Static
    )
    .is_ok());
    assert!(call(
        &prepend,
        &[ty(s_arr(delete.clone())), ty(update.clone())],
        Mode::Static
    )
    .is_ok());
    assert!(
        call(
            &prepend,
            &[ty(s_arr(update.clone())), ty(s_arr(OirType::String))],
            Mode::Static
        )
        .is_ok(),
        "a list of strings is the delete tuple"
    );
    let list = op(json!([p("arr", list(var("T")))]), list(var("T")), Some(1));
    assert!(call(
        &list,
        &[ty(tuple(vec![delete.clone(), update.clone(), delete]))],
        Mode::Static
    )
    .is_ok());
    assert!(
        call(
            &prepend,
            &[ty(s_arr(OirType::Boolean)), ty(update)],
            Mode::Static
        )
        .is_err(),
        "control: a list of booleans takes no tuple with no boolean slot"
    );
}

#[test]
fn a_held_program_is_a_type_a_variable_binds_and_nothing_can_be_read_from() {
    let items = json!({ "kind": "var", "name": "T", "each": true });
    let literal = op(json!([p("...items", items.clone())]), list(items), None);
    assert_eq!(
        call(
            &literal,
            &[ty(OirType::SExpr), ty(OirType::SExpr)],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::SExpr))),
        "a list of held programs is a list of them"
    );
    assert!(
        is_undetermined(call(
            &literal,
            &[ty(OirType::SExpr), ty(OirType::erased())],
            Mode::Static
        )),
        "control: json beside it is still no type"
    );
    let prepend = op(
        json!([p("arr", list(var("T"))), p("item", var("T"))]),
        list(var("T")),
        Some(2),
    );
    assert_eq!(
        call(
            &prepend,
            &[ty(s_arr(OirType::SExpr)), ty(OirType::SExpr)],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::SExpr)))
    );
    assert_eq!(
        call(
            &prepend,
            &[ty(s_arr(OirType::SExpr)), ty(OirType::String)],
            Mode::Static
        ),
        Ok(Some(s_arr(OirType::SExpr))),
        "an atom is a program too"
    );
    assert!(
        call(
            &prepend,
            &[ty(s_arr(OirType::SExpr)), ty(OirType::Date)],
            Mode::Static
        )
        .is_err(),
        "control: a date is not program data"
    );
    let show = op(json!([p("v", var("T"))]), json!("string"), Some(1));
    assert_eq!(
        call(&show, &[ty(OirType::SExpr)], Mode::Static),
        Ok(Some(OirType::String))
    );
    assert!(
        is_undetermined(call(&show, &[ty(OirType::erased())], Mode::Static)),
        "control: json"
    );
    assert!(
        is_undetermined(call(
            &get(),
            &[ty(OirType::SExpr), text("name")],
            Mode::Static
        )),
        "a field cannot be read out of a held program"
    );
    assert!(
        is_undetermined(call(
            &get(),
            &[ty(OirType::SExpr), text("name"), ty(OirType::String)],
            Mode::Static
        )),
        "nor with a default"
    );
    let bag = OirType::map(OirType::String, OirType::SExpr);
    assert!(
        call(&get(), &[ty(bag), text("name")], Mode::Static).is_ok(),
        "control: a map of held programs is read by key"
    );
}

#[test]
fn a_concrete_position_and_a_lambda_are_not_undetermined() {
    let upper = op(json!([p("s", json!("string"))]), json!("string"), Some(1));
    assert!(
        call(&upper, &[undetermined()], Mode::Static).is_ok(),
        "no variable is bound by a concrete position"
    );
}

#[test]
fn the_null_value_binds_nothing_and_makes_the_result_optional() {
    let eq = op(
        json!([p("a", var("T")), p("b", var("T"))]),
        json!("boolean"),
        Some(2),
    );
    assert!(call(&eq, &[ty(OirType::String), absent()], Mode::Static).is_ok());
    assert!(call(&eq, &[absent(), absent()], Mode::Static).is_ok());
    let pick = op(
        json!([p("c", json!("boolean")), p("a", var("T")), p("b", var("T"))]),
        var("T"),
        Some(3),
    );
    assert_eq!(
        call(
            &pick,
            &[ty(OirType::Boolean), ty(OirType::Number), absent()],
            Mode::Static
        ),
        Ok(Some(OirType::optional(OirType::Number)))
    );
}

fn effect() -> Actual<'static> {
    Actual {
        known: Some(Known::Void),
        text: None,
        texts: None,
        lambda: None,
    }
}

/// The result type and whether the result is void, after binding every argument.
fn run(
    operator: &EmbeddedOperator,
    args: &[Actual<'_>],
) -> Result<(Option<OirType>, bool), Mismatch> {
    let sig = Signature::of(operator);
    let mut b = CallBindings::new();
    for (i, a) in args.iter().enumerate() {
        sig.bind(&mut b, i, a, Mode::Static)?;
    }
    Ok((sig.result(&b), sig.result_is_void(&b)))
}

#[test]
fn an_effect_is_void_and_a_conditional_of_effects_is_void() {
    let cond = op(
        json!([
            p("c", json!("boolean")),
            p("then", var("T")),
            p("else", var("T"))
        ]),
        var("T"),
        Some(3),
    );
    assert_eq!(
        run(&cond, &[ty(OirType::Boolean), effect(), effect()]),
        Ok((None, true))
    );
    assert_eq!(
        run(&cond, &[ty(OirType::Boolean), effect(), absent()]),
        Ok((None, true)),
        "an effect and null"
    );
    assert_eq!(
        run(
            &cond,
            &[
                ty(OirType::Boolean),
                ty(OirType::Number),
                ty(OirType::Number)
            ]
        ),
        Ok((Some(OirType::Number), false)),
        "control: two numbers are a number"
    );
    assert!(
        matches!(
            run(
                &cond,
                &[ty(OirType::Boolean), effect(), ty(OirType::Number)]
            ),
            Err(Mismatch::Conflict { .. })
        ),
        "an effect and a value are not one type"
    );
    assert!(
        matches!(
            run(
                &cond,
                &[ty(OirType::Boolean), ty(OirType::Number), effect()]
            ),
            Err(Mismatch::Conflict { .. })
        ),
        "a value and an effect are not one type"
    );
}

#[test]
fn a_sequence_is_the_type_of_its_last_expression() {
    let each = json!({ "kind": "var", "name": "T", "each": true });
    let seq = op(
        json!([p("...exprs", json!({ "kind": "sexpr", "of": each }))]),
        var("T"),
        None,
    );
    assert_eq!(run(&seq, &[effect(), effect()]), Ok((None, true)));
    assert_eq!(
        run(&seq, &[effect(), ty(OirType::Number)]),
        Ok((Some(OirType::Number), false))
    );
    assert_eq!(
        run(&seq, &[ty(OirType::Number), effect()]),
        Ok((None, true))
    );
}

#[test]
fn an_effect_is_not_a_value_at_a_typed_position_and_an_operator_can_return_void() {
    let upper = op(json!([p("s", json!("string"))]), json!("string"), Some(1));
    assert_eq!(
        call(&upper, &[effect()], Mode::Static),
        Err(Mismatch::Type {
            expected: "string".into(),
            actual: "void".into()
        })
    );
    let when = op(
        json!([p("c", json!("boolean")), p("e", json!({ "kind": "sexpr" }))]),
        json!("void"),
        Some(2),
    );
    assert!(
        call(&when, &[ty(OirType::Boolean), effect()], Mode::Static).is_ok(),
        "an effect position takes an effect"
    );
    assert!(Signature::of(&when).returns_void());
    assert!(!Signature::of(&upper).returns_void());
}

fn empty() -> Actual<'static> {
    Actual {
        known: Some(Known::EmptyList),
        text: None,
        texts: None,
        lambda: None,
    }
}

#[test]
fn an_empty_list_takes_its_type_from_a_sibling_binding_and_never_from_nothing() {
    let or = op(json!([p("...xs", var("T"))]), var("T"), None);
    assert_eq!(
        call(&or, &[ty(s_arr(OirType::String)), empty()], Mode::Static),
        Ok(Some(s_arr(OirType::String))),
        "a list of strings or the empty list is a list of strings"
    );
    assert_eq!(
        call(&or, &[empty(), ty(s_arr(OirType::String))], Mode::Static),
        Ok(Some(s_arr(OirType::String))),
        "the empty list met by a list of strings is that list"
    );
    assert!(
        is_undetermined(call(&or, &[empty(), empty()], Mode::Static)),
        "only empty lists: nothing types them"
    );
    assert!(
        call(&or, &[ty(OirType::Number), empty()], Mode::Static).is_err(),
        "control: a number is not a list"
    );
    assert!(
        call(&or, &[empty(), ty(OirType::Number)], Mode::Static).is_err(),
        "control: the empty list does not take a number's place either"
    );
    assert_eq!(
        call(&or, &[empty(), empty()], Mode::Runtime),
        Ok(Some(OirType::Tuple { items: Vec::new() })),
        "at run time two empty lists are two values"
    );

    let length = op(json!([p("arr", list(var("T")))]), json!("number"), Some(1));
    assert!(
        is_undetermined(call(&length, &[empty()], Mode::Static)),
        "the length of an empty list has no element type to bind"
    );
    assert_eq!(
        call(&length, &[ty(s_arr(OirType::String))], Mode::Static),
        Ok(Some(OirType::Number)),
        "control: the length of a typed list"
    );
    let strings = op(
        json!([p("arr", list(json!("string")))]),
        json!("number"),
        Some(1),
    );
    assert_eq!(
        call(&strings, &[empty()], Mode::Static),
        Ok(Some(OirType::Number)),
        "an empty list in a slot that declares its element needs no sibling"
    );

    let reduce = op(
        json!([
            p("arr", list(var("T"))),
            p("init", var("A")),
            p(
                "fn",
                json!({ "kind": "fn", "params": [var("A"), var("T")], "returns": var("A") })
            ),
        ]),
        var("A"),
        Some(3),
    );
    let fold = |returned: Option<OirType>| {
        let sig = Signature::of(&reduce);
        let mut b = CallBindings::new();
        sig.bind(&mut b, 0, &ty(s_arr(OirType::String)), Mode::Static)
            .unwrap();
        sig.bind(&mut b, 1, &empty(), Mode::Static).unwrap();
        let body = |_: &[Option<OirType>]| returned.clone();
        let lambda = Actual {
            known: None,
            text: None,
            texts: None,
            lambda: Some(&body),
        };
        sig.bind(&mut b, 2, &lambda, Mode::Static).unwrap();
        sig.finish(&b, 3)
            .map(|()| sig.result(&b))
            .map_err(|(_, m)| m)
    };
    assert_eq!(
        fold(Some(s_arr(OirType::String))),
        Ok(Some(s_arr(OirType::String))),
        "a fold from the empty list is the list its body builds"
    );
    assert!(
        is_undetermined(fold(Some(OirType::Tuple { items: Vec::new() }))),
        "control: a fold whose body also builds only empty lists is untyped"
    );
    assert!(
        is_undetermined(fold(None)),
        "control: a fold whose body is undetermined is untyped"
    );
}

#[test]
fn a_fold_with_a_present_start_answers_the_start_type_and_a_nullable_step_or_start_answers_optional() {
    let reduce = op(
        json!([
            p("arr", list(var("T"))),
            p("init", var("A")),
            p(
                "fn",
                json!({ "kind": "fn", "params": [var("A"), var("T")], "returns": var("A") })
            ),
        ]),
        var("A"),
        Some(3),
    );
    let fold = |start: OirType, returned: OirType| {
        let sig = Signature::of(&reduce);
        let mut b = CallBindings::new();
        sig.bind(&mut b, 0, &ty(s_arr(OirType::Number)), Mode::Static)
            .unwrap();
        sig.bind(&mut b, 1, &ty(start), Mode::Static).unwrap();
        let body = |_: &[Option<OirType>]| Some(returned.clone());
        let lambda = Actual {
            known: None,
            text: None,
            texts: None,
            lambda: Some(&body),
        };
        sig.bind(&mut b, 2, &lambda, Mode::Static).unwrap();
        sig.finish(&b, 3)
            .map(|()| sig.result(&b))
            .map_err(|(_, m)| m)
    };
    let optional = |inner: OirType| OirType::Optional {
        inner: Box::new(inner),
    };
    assert_eq!(
        fold(OirType::Number, OirType::Number),
        Ok(Some(OirType::Number)),
        "a present start and a plain step answer the start type"
    );
    assert_eq!(
        fold(OirType::Number, optional(OirType::Number)),
        Ok(Some(optional(OirType::Number))),
        "a step answering T? makes the fold T?"
    );
    assert_eq!(
        fold(optional(OirType::Number), OirType::Number),
        Ok(Some(optional(OirType::Number))),
        "a null-able start makes the fold T?"
    );
}

#[test]
fn at_run_time_a_value_always_has_a_kind() {
    let eq = op(
        json!([p("a", var("T")), p("b", var("T"))]),
        json!("boolean"),
        Some(2),
    );
    assert!(
        call(&eq, &[undetermined(), ty(OirType::String)], Mode::Runtime).is_ok(),
        "an absent value passes"
    );
}

#[test]
fn a_semantic_string_and_a_reference_are_strings_and_a_scalar_is_checked() {
    let upper = op(json!([p("s", json!("string"))]), json!("string"), Some(1));
    for text in [
        OirType::Email,
        OirType::Url,
        OirType::Uuid,
        OirType::Reference {
            entity: "User".into(),
        },
    ] {
        assert!(
            call(&upper, &[ty(text.clone())], Mode::Static).is_ok(),
            "{text:?}"
        );
    }
    assert!(
        call(&upper, &[ty(OirType::Number)], Mode::Static).is_err(),
        "control"
    );
    let show = op(
        json!([p(
            "v",
            json!({ "kind": "union", "of": ["string", "number"] })
        )]),
        json!("string"),
        Some(1),
    );
    assert!(call(&show, &[ty(OirType::Number)], Mode::Static).is_ok());
    assert!(call(&show, &[ty(OirType::Boolean)], Mode::Static).is_err());
    assert!(call(&show, &[ty(OirType::array(OirType::String))], Mode::Static).is_err());
}

#[test]
fn an_orbital_trait_and_behavior_value_are_the_records_the_language_declares() {
    let read = |value: OirType, key: &str| call(&get(), &[ty(value), text(key)], Mode::Static);
    assert_eq!(
        read(OirType::Orbital, "behavior"),
        Ok(Some(OirType::String))
    );
    assert_eq!(
        read(OirType::Orbital, "pages"),
        Ok(Some(OirType::optional(OirType::map(
            OirType::String,
            OirType::String
        )))),
        "pages is declared optional, so it may be absent"
    );
    assert_eq!(read(OirType::Trait, "trait"), Ok(Some(OirType::String)));
    assert_eq!(
        read(OirType::Behavior, "behavior"),
        Ok(Some(OirType::String))
    );
    assert_eq!(
        read(OirType::optional(OirType::Orbital), "orbital"),
        Ok(Some(OirType::optional(OirType::String))),
        "a value that may be unbound reads a member that may be missing (G-ORBITAL-CORE-060)"
    );
    assert!(
        read(OirType::Orbital, "nonsense").is_err(),
        "control: an orbital value declares no such member"
    );
    assert!(
        read(OirType::Behavior, "orbital").is_err(),
        "control: a behavior reference has no orbital"
    );
    assert!(
        read(OirType::Number, "name").is_err(),
        "control: a number is no record"
    );
    let with_default = call(
        &get(),
        &[ty(OirType::Orbital), text("nonsense"), ty(OirType::String)],
        Mode::Static,
    );
    assert_eq!(
        with_default,
        Ok(Some(OirType::String)),
        "a default stands in for a member the value does not declare"
    );
}

#[test]
fn a_member_of_a_value_that_is_program_data_is_held_as_such() {
    let app = call(&get(), &[ty(OirType::Orbital), text("app")], Mode::Static).unwrap();
    assert_eq!(
        app,
        Some(OirType::optional(OirType::map(OirType::String, OirType::SExpr)))
    );
}

mod field_projection_contract {
    use super::*;

    fn projection(operator: &EmbeddedOperator) -> Option<(usize, usize, Option<usize>)> {
        Signature::of(operator)
            .field_projection()
            .map(|projection| {
                (
                    projection.source_arg,
                    projection.key_arg,
                    projection.default_arg,
                )
            })
    }

    #[test]
    fn field_projection_uses_the_existing_declared_get_relationship() {
        assert_eq!(projection(&get()), Some((0, 1, Some(2))));
        assert_eq!(projection(&get_of_record_or_list()), Some((0, 1, Some(2))));
    }

    #[test]
    fn field_projection_can_have_no_default_formal() {
        let operator = op(
            json!([
                p("value", record("R")),
                p("selector", json!({ "kind": "keyOf", "of": "R" }))
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(2),
        );
        assert_eq!(projection(&operator), Some((0, 1, None)));
    }

    #[test]
    fn field_projection_ignores_names_categories_and_parameter_positions() {
        for name in ["object/get", "domain/project-member"] {
            let mut operator = op(
                json!([
                    p("chosen", json!({ "kind": "keyOf", "of": "LedgerRow" })),
                    p("otherwise", json!({ "kind": "fieldOf", "of": "LedgerRow" })),
                    p("unrelated", json!("boolean")),
                    p("document", record("LedgerRow")),
                ]),
                json!({ "kind": "fieldOf", "of": "LedgerRow" }),
                Some(4),
            );
            operator.category = format!("custom-{name}");
            operator.description = name.into();
            assert_eq!(projection(&operator), Some((3, 0, Some(1))), "{name}");
        }
    }

    #[test]
    fn field_projection_requires_a_direct_field_of_return() {
        for returns in [
            json!("string"),
            var("R"),
            json!({ "kind": "keyOf", "of": "R" }),
            list(json!({ "kind": "fieldOf", "of": "R" })),
        ] {
            let mut operator = get();
            operator.return_type = serde_json::from_value(returns).unwrap();
            assert_eq!(projection(&operator), None);
        }
    }

    #[test]
    fn field_projection_refuses_a_mismatched_source_variable() {
        let operator = op(
            json!([
                p("obj", record("Other")),
                p("key", json!({ "kind": "keyOf", "of": "R" }))
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(2),
        );
        assert_eq!(projection(&operator), None);
    }

    #[test]
    fn field_projection_refuses_a_mismatched_key_variable() {
        let operator = op(
            json!([
                p("obj", record("R")),
                p("key", json!({ "kind": "keyOf", "of": "Other" }))
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(2),
        );
        assert_eq!(projection(&operator), None);
    }

    #[test]
    fn field_projection_refuses_a_mismatched_return_variable() {
        let mut operator = get();
        operator.return_type =
            serde_json::from_value(json!({ "kind": "fieldOf", "of": "Other" })).unwrap();
        assert_eq!(projection(&operator), None);
    }

    #[test]
    fn field_projection_refuses_ambiguous_sources() {
        let operator = op(
            json!([
                p("first", record("R")),
                p("second", record("R")),
                p("key", json!({ "kind": "keyOf", "of": "R" }))
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(3),
        );
        assert_eq!(projection(&operator), None);
    }

    #[test]
    fn field_projection_refuses_ambiguous_keys() {
        let operator = op(
            json!([
                p("obj", record("R")),
                p("first", json!({ "kind": "keyOf", "of": "R" })),
                p("second", json!({ "kind": "keyOf", "of": "R" }))
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(3),
        );
        assert_eq!(projection(&operator), None);
    }

    #[test]
    fn field_projection_refuses_ambiguous_defaults() {
        let operator = op(
            json!([
                p("obj", record("R")),
                p("key", json!({ "kind": "keyOf", "of": "R" })),
                p("first", json!({ "kind": "fieldOf", "of": "R" })),
                p("second", json!({ "kind": "fieldOf", "of": "R" }))
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(4),
        );
        assert_eq!(projection(&operator), None);
    }

    #[test]
    fn field_projection_only_counts_relationships_for_the_return_variable() {
        let operator = op(
            json!([
                p("other", record("Other")),
                p("otherKey", json!({ "kind": "keyOf", "of": "Other" })),
                p("obj", record("R")),
                p("key", json!({ "kind": "keyOf", "of": "R" })),
                p("unrelated", json!({ "kind": "fieldOf", "of": "Other" })),
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(5),
        );
        assert_eq!(projection(&operator), Some((2, 3, None)));
    }

    #[test]
    fn field_projection_does_not_unwrap_nested_formals() {
        for params in [
            json!([
                p("obj", list(record("R"))),
                p("key", json!({ "kind": "keyOf", "of": "R" }))
            ]),
            json!([
                p("obj", record("R")),
                p("key", list(json!({ "kind": "keyOf", "of": "R" })))
            ]),
        ] {
            let operator = op(params, json!({ "kind": "fieldOf", "of": "R" }), Some(2));
            assert_eq!(projection(&operator), None);
        }
    }
}

fn union_of(types: &[OirType]) -> OirType {
    OirType::Union {
        variants: types
            .iter()
            .enumerate()
            .map(|(i, ty)| crate::ir::types::OirField::new(format!("v{i}"), ty.clone()))
            .collect(),
    }
}

/// G-CROSS-335: `Mode::Proof` binds only an argument every value of whose type fits; `Mode::Static` stays lenient.
#[test]
fn a_proof_binds_only_what_every_value_of_the_argument_type_fits() {
    let upper = op(json!([p("s", json!("string"))]), json!("string"), Some(1));
    let sig = Signature::of(&upper);
    let bind = |ty: OirType, mode: Mode| sig.bind(&mut CallBindings::new(), 0, &Actual::of_type(ty), mode);
    let string_or_number = union_of(&[OirType::String, OirType::Number]);
    assert!(bind(string_or_number, Mode::Proof).is_err());
    assert!(bind(OirType::Date, Mode::Static).is_ok(), "control: a date passes a string position at rest");
    assert!(bind(OirType::Date, Mode::Proof).is_ok(), "a date stays a string at rest in a strict call");

    let when = op(json!([p("d", json!("date"))]), json!("number"), Some(1));
    let when_sig = Signature::of(&when);
    let bind_date = |ty: OirType, mode: Mode| when_sig.bind(&mut CallBindings::new(), 0, &Actual::of_type(ty), mode);
    let refused = bind_date(OirType::String, Mode::Proof).unwrap_err().to_string();
    assert!(refused.contains("time/parse") && refused.contains("declare the field `date`"), "{refused}");
    assert!(bind_date(OirType::Optional { inner: Box::new(OirType::String) }, Mode::Proof).is_err());
    assert!(bind_date(OirType::Date, Mode::Proof).is_ok(), "control: a date-typed value passes a date position");
    assert!(bind_date(OirType::Number, Mode::Proof).is_err(), "epoch ms is not a date: a point in time is ISO text");
    assert!(bind_date(OirType::String, Mode::Static).is_ok(), "control: only a strict call refuses a string at a date position");
    assert!(matches!(bind(OirType::erased(), Mode::Proof), Err(Mismatch::Undetermined { .. })));
    assert!(bind(OirType::String, Mode::Proof).is_ok());
    assert!(
        bind(OirType::Optional { inner: Box::new(OirType::String) }, Mode::Proof).is_err(),
        "an optional value does not pass a position that excludes null"
    );
    assert!(bind(union_of(&[OirType::String, OirType::String]), Mode::Proof).is_ok(), "every variant fits");
}

#[test]
fn a_proof_checks_each_list_element_variant_and_a_bounded_variable() {
    let first = op(json!([p("xs", list(json!("string")))]), json!("string"), Some(1));
    let sig = Signature::of(&first);
    let bind = |ty: OirType, mode: Mode| sig.bind(&mut CallBindings::new(), 0, &Actual::of_type(ty), mode);
    assert!(bind(s_arr(union_of(&[OirType::String, OirType::Number])), Mode::Proof).is_err());
    assert!(bind(s_arr(OirType::String), Mode::Proof).is_ok());
    let bounded = op(
        json!([p("x", json!({ "kind": "var", "name": "T", "bound": "number" }))]),
        json!({ "kind": "var", "name": "T" }),
        Some(1),
    );
    let sig = Signature::of(&bounded);
    let bind = |ty: OirType, mode: Mode| sig.bind(&mut CallBindings::new(), 0, &Actual::of_type(ty), mode);
    assert!(bind(union_of(&[OirType::Number, OirType::String]), Mode::Proof).is_err());
    assert!(bind(OirType::Number, Mode::Proof).is_ok());
}

fn strict_bind(params: serde_json::Value, actual: Actual<'_>) -> Result<(), Mismatch> {
    let operator = op(params, json!("boolean"), None);
    Signature::of(&operator).bind(&mut CallBindings::new(), 0, &actual, Mode::Proof)
}

#[test]
fn a_loose_value_at_a_concrete_position_is_undetermined_and_a_narrowed_one_binds() {
    for formal in [json!("number"), json!("bytes"), json!("vector"), json!({ "kind": "record" })] {
        let loose = strict_bind(json!([p("x", formal.clone())]), Actual::of_type(OirType::erased()));
        assert!(matches!(loose, Err(Mismatch::Undetermined { .. })), "{formal}: {loose:?}");
    }
    let object = json!({ "kind": "object", "fields": { "a": "number" } });
    assert!(matches!(
        strict_bind(json!([p("x", object)]), Actual::of_type(OirType::SExpr)),
        Err(Mismatch::Undetermined { .. })
    ));
    assert!(strict_bind(json!([p("x", json!("number"))]), Actual::of_type(OirType::Number)).is_ok());
    assert!(strict_bind(json!([p("x", json!("bytes"))]), Actual::of_type(OirType::Bytes)).is_ok());
}

#[test]
fn a_value_of_unknown_type_is_undetermined_at_every_declared_type() {
    for formal in [json!("string"), json!("bytes"), json!({ "kind": "record" }), list(json!("number"))] {
        let found = strict_bind(json!([p("x", formal.clone())]), Actual::unknown());
        assert!(matches!(found, Err(Mismatch::Undetermined { .. })), "{formal}: {found:?}");
    }
    let operator = op(json!([p("x", json!("string"))]), json!("boolean"), None);
    assert!(
        Signature::of(&operator).bind(&mut CallBindings::new(), 0, &Actual::unknown(), Mode::Static).is_ok(),
        "control: the lenient binder lets it pass"
    );
    assert!(strict_bind(json!([p("x", json!({ "kind": "fn", "params": [], "returns": "number" }))]), Actual::unknown()).is_ok());
}

#[test]
fn nominal_std_structs_and_entity_names_are_checked() {
    let vector = OirType::Struct {
        fields: vec![OirField::new("x", OirType::Number), OirField::new("y", OirType::Number)].into(),
        additional: None,
    };
    assert!(strict_bind(json!([p("v", json!("vector"))]), Actual::of_type(vector)).is_ok());
    assert!(strict_bind(json!([p("v", json!("vector"))]), Actual::of_type(OirType::Number)).is_err());
    assert!(strict_bind(json!([p("b", json!("bytes"))]), Actual::of_type(OirType::String)).is_err());
    let entity = json!({ "kind": "entity" });
    assert!(strict_bind(json!([p("e", entity.clone())]), Actual::of_type(OirType::String)).is_ok());
    assert!(strict_bind(json!([p("e", entity)]), Actual::of_type(OirType::Number)).is_err());
}

#[test]
fn an_optional_value_binds_only_where_the_formal_admits_null() {
    let optional = |ty: OirType| Actual::of_type(OirType::Optional { inner: Box::new(ty) });
    assert!(strict_bind(json!([p("x", list(json!("number")))]), optional(OirType::Number)).is_err());
    let nullable = json!({ "kind": "union", "of": ["number", { "kind": "literal", "value": null }] });
    assert!(strict_bind(json!([p("x", nullable)]), optional(OirType::Number)).is_ok(), "the formal admits null");
    assert!(strict_bind(json!([p("x", var("T"))]), optional(OirType::Number)).is_ok(), "an unbounded variable holds it");
    assert!(strict_bind(json!([p("x", json!("number"))]), Actual::of_type(OirType::Number)).is_ok());
}

#[test]
fn a_loose_union_at_a_concrete_position_asks_for_a_narrowing_and_binds_to_a_variable_whole() {
    let loose = union_of(&[OirType::String, OirType::Number]);
    let found = strict_bind(json!([p("x", json!("string"))]), Actual::of_type(loose.clone()));
    assert!(matches!(found, Err(Mismatch::Undetermined { .. })), "{found:?}");
    assert!(strict_bind(json!([p("x", var("T"))]), Actual::of_type(loose)).is_ok());
}

#[test]
fn a_number_path_is_an_index_only_into_a_list() {
    let indexable = json!({ "kind": "var", "name": "R", "bound": { "kind": "union", "of": [{ "kind": "record" }, { "kind": "array", "of": { "kind": "var", "name": "E", "each": true } }] } });
    let get = op(json!([p("obj", indexable), p("path", json!({ "kind": "keyOf", "of": "R" }))]), json!("boolean"), Some(2));
    let sig = Signature::of(&get);
    let bind = |record: OirType, path: OirType| {
        let mut b = CallBindings::new();
        sig.bind(&mut b, 0, &Actual::of_type(record), Mode::Proof)?;
        sig.bind(&mut b, 1, &Actual::of_type(path), Mode::Proof)
    };
    assert!(bind(s_arr(OirType::String), OirType::Number).is_ok(), "an index into a list");
    assert!(bind(OirType::Tuple { items: vec![OirType::String] }, OirType::Integer).is_ok());
    assert!(bind(OirType::map(OirType::String, OirType::Number), OirType::Number).is_err(), "a map needs a string key");
    let record = OirType::Struct { fields: vec![OirField::new("a", OirType::Number)].into(), additional: None };
    assert!(bind(record.clone(), OirType::Number).is_err());
    assert!(bind(record, OirType::String).is_ok());
}

#[test]
fn the_null_alternative_left_out_keeps_an_optional_the_variable_itself_binds() {
    let nullable = json!({ "kind": "union", "of": [var("R"), { "kind": "literal", "value": null }] });
    let operator = op(json!([p("x", var("R"))]), nullable, Some(1));
    let sig = Signature::of(&operator);
    let result = |bound: OirType| {
        let mut b = CallBindings::new();
        sig.bind(&mut b, 0, &ty(bound), Mode::Static).unwrap();
        (sig.result(&b), sig.result_without_null(&b))
    };
    assert_eq!(result(OirType::Number), (Some(OirType::optional(OirType::Number)), Some(OirType::Number)));
    let both = OirType::optional(OirType::Number);
    assert_eq!(result(both.clone()).1, Some(both), "an optional the variable holds survives");
    let plain = op(json!([p("x", var("R"))]), var("R"), Some(1));
    let mut b = CallBindings::new();
    Signature::of(&plain).bind(&mut b, 0, &ty(OirType::Number), Mode::Static).unwrap();
    assert_eq!(Signature::of(&plain).result_without_null(&b), None, "control: no null alternative declared");
}

fn map_param(value: serde_json::Value) -> EmbeddedOperator {
    op(
        json!([p("env", json!({ "kind": "map", "key": "string", "value": value }))]),
        json!("void"),
        None,
    )
}

fn with_nulls(present: &[(&str, OirType)], nulls: &[&str]) -> Actual<'static> {
    Actual {
        known: Some(Known::RecordWithNulls {
            present: struct_of(present),
            nulls: nulls.iter().map(|n| (*n).to_string()).collect(),
        }),
        ..Actual::unknown()
    }
}

#[test]
fn a_null_value_in_a_record_literal_fits_a_map_whose_value_type_admits_null() {
    let nullable = map_param(json!({ "kind": "union", "of": ["string", { "kind": "literal", "value": null }] }));
    let strict = map_param(json!("string"));
    let all_null = with_nulls(&[], &["PATH"]);
    assert!(call(&nullable, &[all_null], Mode::Proof).is_ok(), "every value null, the value type admits null");
    let mixed = with_nulls(&[("PATH", OirType::String)], &["HOME"]);
    assert!(call(&nullable, &[mixed], Mode::Proof).is_ok(), "control: mixed string and null, the value type admits null");
    let all_null = with_nulls(&[], &["PATH"]);
    assert!(call(&strict, &[all_null], Mode::Proof).is_err(), "control: a Map string string refuses a null value");
    let mixed = with_nulls(&[("PATH", OirType::String)], &["HOME"]);
    assert!(call(&strict, &[mixed], Mode::Proof).is_err(), "control: mixed is refused when the value type excludes null");
    assert!(
        call(&nullable, &[ty(struct_of(&[]))], Mode::Proof).is_ok() && call(&strict, &[ty(struct_of(&[]))], Mode::Proof).is_ok(),
        "control: the empty record fits either"
    );
    let wrong = with_nulls(&[("PATH", OirType::Number)], &["HOME"]);
    assert!(call(&nullable, &[wrong], Mode::Proof).is_err(), "control: a present value of the wrong type is still refused");
}

fn branch_pick() -> EmbeddedOperator {
    let mut pick = op(json!([p("condition", json!("boolean")), p("yes", var("T")), p("no", var("T"))]), var("T"), Some(3));
    pick.return_semantics = Some("branch-union".into());
    pick
}

#[test]
fn a_branch_of_a_record_and_null_is_the_optional_record_either_way_round() {
    let record = struct_of(&[("hp", OirType::Number)]);
    let pick = branch_pick();
    for mode in [Mode::Static, Mode::Proof] {
        assert_eq!(
            call(&pick, &[boolean(), ty(record.clone()), absent()], mode),
            Ok(Some(OirType::optional(record.clone())))
        );
        assert_eq!(
            call(&pick, &[boolean(), absent(), ty(record.clone())], mode),
            Ok(Some(OirType::optional(record.clone())))
        );
        let maybe = OirType::optional(record.clone());
        assert_eq!(call(&pick, &[boolean(), ty(maybe.clone()), ty(record.clone())], mode), Ok(Some(maybe.clone())), "a nested branch");
        assert_eq!(call(&pick, &[boolean(), ty(record.clone()), ty(maybe.clone())], mode), Ok(Some(maybe)), "a nested branch, other side");
    }
}

#[test]
fn control_branches_of_two_records_stay_non_null_and_two_nulls_are_null() {
    let record = struct_of(&[("hp", OirType::Number)]);
    let other = struct_of(&[("hp", OirType::Number)]);
    let pick = branch_pick();
    assert_eq!(call(&pick, &[boolean(), ty(record.clone()), ty(other)], Mode::Static), Ok(Some(record.clone())));
    assert_eq!(call(&pick, &[boolean(), ty(record), ty(OirType::Number)], Mode::Static).is_err(), true);
    assert_eq!(call(&pick, &[boolean(), absent(), absent()], Mode::Static), Ok(None));
}

#[test]
fn a_list_of_x_y_shape_return_types_as_a_list_of_cell_records() {
    let reachable = op(json!([p("steps", json!("number"))]), list(json!({ "kind": "object", "fields": { "x": "number", "y": "number" } })), Some(1));
    assert_eq!(
        call(&reachable, &[ty(OirType::Number)], Mode::Static),
        Ok(Some(s_arr(struct_of(&[("x", OirType::Number), ("y", OirType::Number)]))))
    );
}
