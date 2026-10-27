#[test]
fn a_member_left_optional_either_way_is_the_same_member() {
    use crate::ir::types::{OirField, OirType};
    let record = |id: OirField| OirType::Struct {
        additional: None,
        fields: vec![id, {
            let mut x = OirField::new("x", OirType::Number);
            x.required = true;
            x
        }].into(),
    };
    let plain = {
        let mut id = OirField::new("id", OirType::String);
        id.required = false;
        id
    };
    let wrapped = OirField::optional("id", OirType::String);
    assert!(super::assignable(&record(wrapped.clone()), &record(plain.clone())), "an optional member read as T? fits the same member read as T");
    assert!(super::assignable(&record(plain), &record(wrapped)), "and the other way");
    let mut required = OirField::new("id", OirType::String);
    required.required = true;
    assert!(
        !super::assignable(&record(OirField::optional("id", OirType::String)), &record(required)),
        "control: a required member does not take one that may be missing"
    );
}

#[test]
fn a_field_ref_is_string_kind_at_run_time() {
    use crate::ir::types::OirType;
    let of = |record: &str, value: Option<OirType>| OirType::FieldRef {
        record: Box::new(OirType::reference(record)),
        value: value.map(Box::new),
    };
    let field_ref = of("@entity", Some(OirType::String));
    assert!(super::assignable(&field_ref, &OirType::String));
    assert!(super::assignable(&OirType::String, &field_ref));
    assert_eq!(super::oir_type_kind(&field_ref), "string");
    // control: a field name is not a number.
    assert!(!super::assignable(&field_ref, &OirType::Number));
    assert!(!super::assignable(&OirType::Number, &field_ref));
    // control: a field of any type is not a field of a given type; the other way round it is.
    let any = of("@entity", None);
    assert!(super::assignable(&field_ref, &any));
    assert!(!super::assignable(&any, &field_ref));
    // control: fields of different records are different types.
    assert!(!super::assignable(&field_ref, &of("Other", Some(OirType::String))));
}

#[test]
fn an_int_entity_field_fits_an_int_slot_widens_to_number_and_a_number_field_does_not_narrow() {
    use crate::ir::types::{field_shape, OirType};
    let field = |ty: &str| -> crate::schema::FieldDefinition {
        serde_json::from_value(serde_json::json!({ "name": "f", "type": ty })).unwrap()
    };
    let int_field = field_shape(&field("int"));
    let number_field = field_shape(&field("number"));
    assert_eq!(int_field, OirType::Integer);
    assert!(super::sub(&int_field, &OirType::Integer).is_some());
    assert!(super::sub(&int_field, &OirType::Number).is_some());
    assert!(super::sub(&number_field, &OirType::Integer).is_none());
    assert!(!super::set_value_type_compatible("number", "int"));
}

#[test]
fn known_closed_enum_assignments_reject_members_outside_the_declared_set() {
    let closed = |values: &[&str]| crate::ir::types::OirType::enum_type(
        super::literal_union_text(&values.iter().map(|value| value.to_string()).collect::<Vec<_>>()).unwrap()
    );
    let target = closed(&["ready", "done"]);
    let wrong = closed(&["wrong"]);
    assert!(!super::assignable(&wrong, &target));
    assert!(!super::castable(&wrong, &target));
    assert!(super::assignable(&closed(&["ready"]), &target));
    assert!(super::assignable(&target, &target));
    assert!(!super::assignable(&target, &closed(&["ready"])));
    assert!(!super::assignable(&record(&[("kind", wrong)]), &record(&[("kind", target)])));
    assert!(super::assignable(&crate::ir::types::OirType::String, &crate::ir::types::OirType::enum_type("Status")), "unresolved legacy nominal enums retain their contract");
}

#[test]
fn literal_union_codec_preserves_every_authored_string_member() {
    let values = vec![
        "ready".to_string(), "say \"ready\"".to_string(), r"C:\store\value".to_string(),
        "ready | done".to_string(), "line1\nline2".to_string(), "موجود".to_string(), String::new(),
    ];
    let encoded = super::literal_union_text(&values).expect("all source strings remain representable");
    let decoded = super::literal_union_members(&encoded).expect("canonical quoted members decode");
    assert_eq!(super::decode_literal_union(&encoded), Ok(Some(values.clone())));
    assert_eq!(decoded.iter().map(|value| value.to_string()).collect::<Vec<_>>(), values);
    let old = super::literal_union_members("\"line1\nline2\" | \"ready\"").expect("legacy raw newline remains readable");
    assert_eq!(old.iter().map(|value| value.to_string()).collect::<Vec<_>>(), vec!["line1\nline2", "ready"]);
    for malformed in [r#""unterminated"#, r#""a" |"#, r#""a" , "b""#, r#""a" trailing"#, r#""bad\q""#] {
        assert!(super::literal_union_members(malformed).is_none(), "{malformed}");
        assert!(super::decode_literal_union(malformed).is_err(), "{malformed}");
    }
    assert!(super::literal_union_members("Status").is_none());
    assert_eq!(super::decode_literal_union("Status"), Ok(None));
    assert!(super::literal_union_text(&[]).is_none());
}

#[test]
fn typed_additional_values_do_not_prove_an_opaque_empty_record() {
    let unknown = crate::ir::types::OirType::Struct { fields: Default::default(), additional: None };
    let target = crate::ir::types::OirType::Struct { fields: Default::default(), additional: Some(Box::new(crate::ir::types::OirType::Number)) };
    assert!(super::checked_sub_with_named(&unknown, &target, &|_| None).is_none());
    assert!(super::checked_sub_with_named(&target, &target, &|_| None).is_some());
    assert!(super::checked_sub_with_named(&unknown, &unknown, &|_| None).is_some());
}

#[test]
fn nullable_assignment_requires_a_guard_or_explicit_unwrap() {
    let nullable = crate::ir::types::OirType::optional(crate::ir::types::OirType::String);
    assert!(!super::assignable(&nullable, &crate::ir::types::OirType::String));
    assert!(super::castable(&nullable, &crate::ir::types::OirType::String));
    assert!(super::assignable(&nullable, &nullable));
    assert!(super::assignable(&crate::ir::types::OirType::String, &nullable));
}

#[test]
fn record_contract_checks_additional_values_and_required_nullable_presence() {
    let ty = |json| serde_json::from_value::<crate::ir::types::OirType>(json).unwrap();
    let target = ty(serde_json::json!({"kind":"struct","fields":[
        {"name":"id","fieldType":{"kind":"string"},"required":false}
    ],"additional":{"kind":"number"}}));
    let record = |field: &str, kind: &str| ty(serde_json::json!({"kind":"struct","fields":[
        {"name":field,"fieldType":{"kind":kind},"required":true}
    ]}));
    assert!(super::checked_sub_with_named(&record("extra", "string"), &target, &|_| None).is_none());
    assert!(super::checked_sub_with_named(&record("extra", "number"), &target, &|_| None).is_some());
    assert!(super::checked_sub_with_named(&record("id", "string"), &target, &|_| None).is_some());
    assert!(super::checked_sub_with_named(&record("id", "number"), &target, &|_| None).is_none());
    let required_nullable = ty(serde_json::json!({"kind":"struct","fields":[
        {"name":"token","fieldType":{"kind":"optional","inner":{"kind":"string"}},"required":true}
    ]}));
    assert!(super::checked_sub_with_named(&record("other", "number"), &required_nullable, &|_| None).is_none());
    let source = ty(serde_json::json!({"kind":"struct","fields":[
        {"name":"token","fieldType":{"kind":"optional","inner":{"kind":"string"}},"required":true}
    ]}));
    assert!(super::checked_sub_with_named(&source, &required_nullable, &|_| None).is_some());
    let ordinary = ty(serde_json::json!({"kind":"struct","fields":[
        {"name":"id","fieldType":{"kind":"string"},"required":false}
    ]}));
    assert!(super::checked_sub_with_named(&record("extra", "number"), &ordinary, &|_| None).is_some());
    let map = ty(serde_json::json!({"kind":"map","key":{"kind":"string"},"value":{"kind":"string"}}));
    assert!(super::checked_sub_with_named(&map, &target, &|_| None).is_none());
    let extra_source = ty(serde_json::json!({"kind":"struct","fields":[],"additional":{"kind":"string"}}));
    assert!(super::checked_sub_with_named(&extra_source, &target, &|_| None).is_none());
    let call_keys = ty(serde_json::json!({"kind":"struct","fields":[],"additional":{"kind":"callkey"}}));
    assert!(!super::assignable(&record("extra", "string"), &call_keys));
    assert!(super::castable(&record("extra", "string"), &call_keys));
}
#[test]
fn a_list_of_an_alias_satisfies_the_same_list_spelled_through_its_expansion() {
    use crate::ir::OirField;
    let member = OirType::Struct {
        additional: None,
        fields: vec![OirField::new("content", OirType::String)].into(),
    };
    let resolve = |name: &str| (name == "M").then(|| member.clone());
    let by_name = OirType::array(OirType::TypeRef { name: "M".into() });
    let expanded = OirType::array(member.clone());
    assert!(super::oir_type_satisfies_with_named(&by_name, &expanded, &resolve));
    assert!(super::oir_type_satisfies_with_named(&expanded, &by_name, &resolve));
    assert!(super::oir_type_satisfies_with_named(&by_name, &by_name, &resolve));
    let other = OirType::array(OirType::Struct {
        additional: None,
        fields: vec![OirField::new("content", OirType::Number)].into(),
    });
    assert!(!super::oir_type_satisfies_with_named(&by_name, &other, &resolve));
    assert!(!super::oir_type_satisfies_with_named(&other, &by_name, &resolve));
}
#[test]
fn checked_assignment_requires_known_identity_preserving_evidence() {
    use crate::ir::OirField;
    let resolve = |_: &str| None;
    let check =
        |source: &OirType, target: &OirType| checked_sub_with_named(source, target, &resolve);
    let map = OirType::Map {
        key: Box::new(OirType::String),
        value: Box::new(OirType::Number),
    };
    for (source, target) in [
        (OirType::Struct { fields: Default::default(), additional: None }, map.clone()),
        (
            OirType::array(OirType::Json),
            OirType::array(OirType::Number),
        ),
        (OirType::String, OirType::CallKey),
        (OirType::String, OirType::Email),
        (OirType::String, OirType::Date),
        (OirType::optional(OirType::Number), OirType::Number),
        (
            OirType::TypeRef {
                name: "Missing".into(),
            },
            OirType::TypeRef {
                name: "Missing".into(),
            },
        ),
    ] {
        assert!(check(&source, &target).is_none(), "{source:?} → {target:?}");
    }
    let record = OirType::Struct {
        additional: None,
        fields: vec![OirField::new("n", OirType::Number)].into(),
    };
    assert!(check(&record, &map).is_some());
    assert!(check(&OirType::Integer, &OirType::Number).is_some());
    assert!(check(&OirType::Number, &OirType::optional(OirType::Number)).is_some());
    assert!(
        sub(&OirType::String, &OirType::Email).is_some(),
        "ordinary assignment unchanged"
    );
}
use super::{checked_sub_with_named, enum_value_is_member, oir_type_satisfies_field, sub};
use crate::ir::types::OirType;

fn record(fields: &[(&str, OirType)]) -> OirType {
    OirType::Struct {
        additional: None,
        fields: fields
            .iter()
            .map(|(name, ty)| crate::ir::types::OirField::new(*name, ty.clone()))
            .collect(),
    }
}

fn union(types: &[OirType]) -> OirType {
    OirType::Union {
        variants: types
            .iter()
            .enumerate()
            .map(|(i, ty)| crate::ir::types::OirField::new(i.to_string(), ty.clone()))
            .collect(),
    }
}

#[test]
fn named_relation_resolves_recursive_lists_maps_and_known_record_fields() {
    let reference = |name: &str| OirType::TypeRef { name: name.into() };
    let value = reference("V");
    let mut next = crate::ir::types::OirField::new("next", OirType::optional(reference("Cell")));
    next.required = false;
    let names = std::collections::HashMap::from([
        (
            "V",
            union(&[
                OirType::String,
                OirType::Number,
                OirType::Boolean,
                OirType::array(OirType::optional(value.clone())),
                OirType::map(OirType::String, OirType::optional(value.clone())),
            ]),
        ),
        (
            "Cell",
            OirType::Struct {
                fields: vec![crate::ir::types::OirField::new("kind", OirType::String), next].into(),
                additional: None,
            },
        ),
    ]);
    let resolve = |name: &str| names.get(name).cloned();
    assert!(
        super::sub_with_named(&OirType::array(OirType::String), &value, &resolve).is_some()
    );
    assert!(super::sub_with_named(
        &OirType::map(OirType::String, OirType::array(OirType::String)),
        &OirType::map(OirType::String, OirType::optional(value)),
        &resolve
    )
    .is_some());
    let cell = reference("Cell");
    assert!(
        super::sub_with_named(&record(&[("kind", OirType::String)]), &cell, &resolve).is_some()
    );
    assert!(
        super::sub_with_named(&record(&[("kind", OirType::Number)]), &cell, &resolve).is_none()
    );
    assert!(super::sub_with_named(
        &record(&[
            ("kind", OirType::String),
            ("next", record(&[("kind", OirType::Number)]))
        ]),
        &cell,
        &resolve
    )
    .is_none());
    assert!(super::sub_with_named(
        &OirType::array(record(&[("kind", OirType::Number)])),
        &OirType::array(cell),
        &resolve
    )
    .is_none());
}

#[test]
fn named_relation_resolves_explicit_reference_aliases_without_changing_entity_ids() {
    let reference = |name: &str| OirType::Reference {
        entity: name.into(),
    };
    let names = std::collections::HashMap::from([
        ("NoteRecord", record(&[("kind", OirType::String)])),
        ("A", reference("B")),
        ("B", reference("A")),
    ]);
    let resolve = |name: &str| names.get(name).cloned();
    let alias = reference("NoteRecord");
    let good = record(&[("kind", OirType::String)]);
    let bad = record(&[("kind", OirType::Number)]);
    assert!(super::sub_with_named(&good, &alias, &resolve).is_some());
    assert!(super::sub_with_named(&bad, &alias, &resolve).is_none());
    assert!(super::sub_with_named(&OirType::String, &alias, &resolve).is_none());
    assert!(super::sub_with_named(&alias, &good, &resolve).is_some());
    assert!(super::sub_with_named(&alias, &bad, &resolve).is_none());
    let entity = reference("Person");
    assert!(super::sub_with_named(&OirType::String, &entity, &resolve).is_some());
    assert!(super::sub_with_named(&good, &entity, &resolve).is_some());
    assert!(super::sub(&bad, &alias).is_some());
    for actual in [OirType::String, OirType::array(OirType::String)] {
        assert!(super::sub_with_named(&actual, &reference("A"), &resolve).is_none());
    }
}

#[test]
fn named_relation_cycles_need_structural_progress_and_real_union_members() {
    let reference = |name: &str| OirType::TypeRef { name: name.into() };
    let names = std::collections::HashMap::from([
        ("A", reference("B")),
        ("B", reference("A")),
        ("V", union(&[reference("V"), OirType::Number])),
        (
            "NoList",
            union(&[
                OirType::String,
                OirType::map(OirType::String, reference("NoList")),
            ]),
        ),
        (
            "Left",
            record(&[
                ("kind", OirType::String),
                ("next", OirType::optional(reference("Left"))),
            ]),
        ),
        (
            "Right",
            record(&[
                ("kind", OirType::String),
                ("next", OirType::optional(reference("Right"))),
            ]),
        ),
        (
            "Bad",
            record(&[
                ("kind", OirType::Number),
                ("next", OirType::optional(reference("Bad"))),
            ]),
        ),
    ]);
    let resolve = |name: &str| names.get(name).cloned();
    for src in [OirType::String, OirType::array(OirType::String)] {
        assert!(super::sub_with_named(&src, &reference("A"), &resolve).is_none());
        assert!(super::sub_with_named(&src, &reference("V"), &resolve).is_none());
    }
    assert!(super::sub_with_named(&OirType::Number, &reference("V"), &resolve).is_some());
    assert!(super::sub_with_named(
        &OirType::array(OirType::String),
        &reference("NoList"),
        &resolve
    )
    .is_none());
    assert!(super::sub_with_named(&reference("Left"), &reference("Right"), &resolve).is_some());
    assert!(super::sub_with_named(&reference("Left"), &reference("Bad"), &resolve).is_none());
}

#[test]
fn structural_nested_record_fields_reject_contradictions() {
    let expected = record(&[(
        "cells",
        OirType::array(record(&[("kind", OirType::String)])),
    )]);
    let good = record(&[(
        "cells",
        OirType::array(record(&[
            ("kind", OirType::String),
            ("extra", OirType::Boolean),
        ])),
    )]);
    let bad = record(&[(
        "cells",
        OirType::array(record(&[("kind", OirType::Number)])),
    )]);
    assert!(super::assignable(&good, &expected));
    assert!(!super::assignable(&bad, &expected));
    assert!(super::assignable(
        &record(&[("cells", OirType::array(OirType::Json))]),
        &expected
    ));
}

#[test]
fn structural_typed_maps_check_values_and_record_members() {
    let expected = OirType::map(OirType::String, record(&[("kind", OirType::String)]));
    let good = OirType::map(OirType::String, record(&[("kind", OirType::String)]));
    let bad = OirType::map(OirType::String, record(&[("kind", OirType::Number)]));
    assert!(super::assignable(&good, &expected));
    assert!(!super::assignable(&bad, &expected));
    assert!(super::assignable(
        &record(&[("a", record(&[("kind", OirType::String)]))]),
        &expected
    ));
    assert!(!super::assignable(
        &record(&[("a", record(&[("kind", OirType::Number)]))]),
        &expected
    ));
    assert!(super::assignable(
        &OirType::map(OirType::String, OirType::Json),
        &expected
    ));
    assert!(super::assignable(
        &record(&[("a", OirType::String)]),
        &OirType::map(OirType::String, OirType::Json)
    ));
    assert!(!super::assignable(
        &OirType::map(OirType::String, OirType::Number),
        &record(&[("kind", OirType::String)])
    ));
}

#[test]
fn structural_unions_check_each_possible_source_member() {
    let strings = record(&[("kind", OirType::String)]);
    let numbers = record(&[("kind", OirType::Number)]);
    let expected = union(&[
        OirType::Boolean,
        strings.clone(),
        OirType::array(OirType::String),
    ]);
    assert!(super::assignable(&strings, &expected));
    assert!(super::assignable(
        &OirType::array(OirType::String),
        &expected
    ));
    assert!(super::assignable(
        &union(&[strings.clone(), OirType::Boolean]),
        &expected
    ));
    assert!(!super::assignable(&numbers, &expected));
    assert!(!super::assignable(
        &union(&[strings.clone(), numbers]),
        &strings
    ));
}

#[test]
fn tuple_routes_preserve_every_explicit_slot_coercion() {
    let tuple = |items| OirType::Tuple { items };
    let routes = [
        (
            tuple(vec![OirType::CallKey, OirType::String]),
            OirType::array(OirType::CallKey),
        ),
        (
            tuple(vec![OirType::CallKey, OirType::String]),
            tuple(vec![OirType::CallKey, OirType::CallKey]),
        ),
        (
            OirType::array(OirType::String),
            tuple(vec![OirType::String, OirType::CallKey]),
        ),
    ];
    for (source, target) in routes {
        assert!(
            !super::assignable(&source, &target),
            "{source:?} -> {target:?}"
        );
        assert!(super::castable(&source, &target));
    }
    let keys = tuple(vec![OirType::CallKey, OirType::CallKey]);
    assert!(super::assignable(&keys, &OirType::array(OirType::CallKey)));
    assert!(super::assignable(
        &keys,
        &tuple(vec![OirType::String, OirType::String])
    ));
    assert!(super::assignable(&keys, &OirType::array(OirType::String)));
    assert!(super::assignable(
        &OirType::array(OirType::CallKey),
        &tuple(vec![OirType::String, OirType::CallKey])
    ));
}

#[test]
fn unknown_list_elements_do_not_erase_the_known_outer_kind() {
    let unknown_list = OirType::array(OirType::Json);
    let source = record(&[("kind", unknown_list.clone())]);
    assert!(!super::assignable(
        &source,
        &record(&[("kind", OirType::String)])
    ));
    assert!(super::assignable(
        &source,
        &record(&[("kind", OirType::array(OirType::String))])
    ));
    assert!(!super::assignable(
        &OirType::map(OirType::String, unknown_list.clone()),
        &OirType::map(OirType::String, OirType::String)
    ));
    assert!(super::assignable(
        &OirType::map(OirType::String, unknown_list.clone()),
        &OirType::map(OirType::String, OirType::array(OirType::String))
    ));
    assert!(!super::oir_type_satisfies(&unknown_list, &OirType::String));
    assert!(!super::oir_type_satisfies_field(&unknown_list, "string"));
    assert!(!super::oir_type_accepts_kind(&OirType::String, "array"));
    assert!(super::oir_type_satisfies(
        &unknown_list,
        &OirType::array(OirType::String)
    ));
    assert!(super::assignable(
        &unknown_list,
        &OirType::Tuple {
            items: vec![OirType::String, OirType::Number]
        }
    ));
    assert!(super::oir_type_satisfies(&OirType::Json, &OirType::String));
    assert!(super::set_value_type_compatible("[string]", "array"));
    assert!(!super::set_value_type_compatible(
        "[string]",
        "[AgentActivity]"
    ));
    assert!(super::set_value_type_compatible("[Foo]", "[Bar]"));
}

#[test]
fn structural_fields_preserve_explicit_cast_requirements() {
    let plain = record(&[("key", OirType::String)]);
    let keyed = record(&[("key", OirType::CallKey)]);
    assert!(!super::assignable(&plain, &keyed));
    assert!(super::castable(&plain, &keyed));
    assert!(super::assignable(&keyed, &plain));
    assert!(!super::assignable(
        &OirType::map(OirType::String, OirType::String),
        &OirType::map(OirType::String, OirType::CallKey)
    ));
    assert!(!super::assignable(
        &union(&[OirType::String, OirType::CallKey]),
        &OirType::CallKey
    ));
    assert!(super::assignable(
        &OirType::String,
        &union(&[OirType::CallKey, OirType::String])
    ));
}

#[test]
fn structural_unknown_nullable_and_recursive_shapes_remain_compatible() {
    let string_record = record(&[("kind", OirType::String)]);
    let unknown = record(&[]);
    assert!(!super::assignable(&unknown, &string_record));
    assert!(super::assignable(&string_record, &unknown));
    assert!(!super::assignable(
        &record(&[("kind", OirType::String)]),
        &record(&[("kind", OirType::String), ("later", OirType::Boolean)])
    ));
    let mut optional_field = crate::ir::types::OirField::new("later", OirType::Boolean);
    optional_field.required = false;
    let optional_record = OirType::Struct {
        additional: None,
        fields: vec![
            crate::ir::types::OirField::new("kind", OirType::String),
            optional_field,
        ].into(),
    };
    assert!(super::assignable(&string_record, &optional_record));
    assert!(!super::assignable(
        &record(&[("kind", OirType::optional(OirType::String))]),
        &string_record
    ));
    let recursive = record(&[
        ("kind", OirType::String),
        (
            "next",
            OirType::optional(OirType::TypeRef {
                name: "Cell".into(),
            }),
        ),
    ]);
    assert!(super::assignable(&recursive, &recursive));
    assert!(!super::assignable(
        &record(&[
            ("kind", OirType::Number),
            (
                "next",
                OirType::TypeRef {
                    name: "Cell".into()
                }
            )
        ]),
        &recursive
    ));
}

#[test]
fn a_type_variable_is_opaque() {
    let t = OirType::Var { name: "t".into() };
    let u = OirType::Var { name: "u".into() };
    assert!(super::assignable(&t, &t));
    assert!(
        super::assignable(&t, &OirType::Json),
        "a variable erases to json"
    );
    assert!(
        super::assignable(&OirType::Json, &t),
        "an unknowable value may be a t"
    );
    assert!(!super::assignable(&OirType::optional(t.clone()), &t));
    assert!(super::assignable(
        &OirType::array(t.clone()),
        &OirType::array(t.clone())
    ));
    for concrete in [
        OirType::String,
        OirType::Number,
        OirType::Struct { fields: Default::default(), additional: None },
        OirType::Node,
    ] {
        assert!(
            !super::castable(&t, &concrete),
            "control: a t is no {concrete:?}"
        );
        assert!(
            !super::castable(&concrete, &t),
            "control: a {concrete:?} is no t"
        );
    }
    assert!(
        !super::castable(&t, &u),
        "control: two variables are distinct"
    );
    assert_eq!(
        super::parse_compat_type(&super::oir_type_kind(&t)),
        super::CompatType::Ty(t.clone())
    );
    assert_eq!(
        super::parse_compat_type("$"),
        super::CompatType::Unknown("$".into())
    );
}

#[test]
fn a_program_type_name_is_its_record_shape() {
    let viewer = crate::language::program_types::program_type("Viewer")
        .expect("the installed language declares Viewer")
        .clone();
    assert!(super::oir_type_accepts_kind(&viewer, "Viewer"));
    assert_eq!(
        super::parse_compat_type("Viewer"),
        super::CompatType::Ty(viewer.clone())
    );
    assert!(
        !super::oir_type_accepts_kind(&viewer, "NoSuchProgramType"),
        "control: an unknown name is not a record"
    );
    assert_eq!(
        super::parse_compat_type("Order"),
        super::CompatType::Named("Order".into()),
        "control: an entity name stays nominal"
    );
}

#[test]
fn a_list_of_a_nominal_std_type_is_that_list() {
    let orbitals = OirType::array(OirType::Orbital);
    assert!(super::oir_type_satisfies(
        &orbitals,
        &super::compat_kind_oir("[orbital]")
    ));
    assert!(
        !super::oir_type_satisfies(
            &OirType::array(OirType::String),
            &super::compat_kind_oir("[orbital]")
        ),
        "control: a list of strings is not a list of orbitals"
    );
    assert!(super::oir_type_satisfies(
        &OirType::array(OirType::Trait),
        &super::compat_kind_oir("[trait]")
    ));
    assert!(super::oir_type_satisfies(
        &OirType::array(OirType::Behavior),
        &super::compat_kind_oir("[behavior]")
    ));
}

#[test]
fn a_declared_list_of_lists_keeps_its_depth() {
    let lists = OirType::array(OirType::array(OirType::Number));
    assert!(super::oir_type_satisfies(
        &lists,
        &super::compat_kind_oir("[[number]]")
    ));
    assert!(
        !super::oir_type_satisfies(
            &OirType::array(OirType::array(OirType::String)),
            &super::compat_kind_oir("[[number]]")
        ),
        "control: a list of string lists is not a list of number lists"
    );
    assert!(
        !super::oir_type_satisfies(
            &OirType::array(OirType::Number),
            &super::compat_kind_oir("[[number]]")
        ),
        "control: a list of numbers is not a list of lists"
    );
}

#[test]
fn the_empty_list_fits_a_list_of_any_element() {
    let empty = OirType::Tuple { items: Vec::new() };
    assert!(super::oir_type_satisfies(
        &empty,
        &OirType::array(OirType::String)
    ));
    assert!(super::oir_type_satisfies(
        &empty,
        &OirType::array(OirType::array(OirType::Number))
    ));
    assert!(
        !super::oir_type_satisfies(&empty, &OirType::String),
        "control: an empty list is not a string"
    );
}

#[test]
fn enum_membership() {
    let members = vec!["active".to_string(), "archived".to_string()];
    assert!(enum_value_is_member("active", &members));
    assert!(!enum_value_is_member("zzz", &members));
}

#[test]
fn oir_type_compat_concrete() {
    assert!(oir_type_satisfies_field(&OirType::Number, "number"));
    assert!(!oir_type_satisfies_field(&OirType::String, "number"));
    assert!(oir_type_satisfies_field(
        &OirType::Array {
            inner: Box::new(OirType::Json)
        },
        "array"
    ));
    // A nullable value needs refinement before assignment to a nonnull slot.
    assert!(!oir_type_satisfies_field(
        &OirType::Optional {
            inner: Box::new(OirType::Number)
        },
        "number"
    ));
}

#[test]
fn oir_type_unknowable_never_rejects() {
    // open Json / SExpr (input-dependent ops we can't pin down) → compatible.
    assert!(oir_type_satisfies_field(&OirType::Json, "number"));
    assert!(oir_type_satisfies_field(&OirType::SExpr, "boolean"));
}
