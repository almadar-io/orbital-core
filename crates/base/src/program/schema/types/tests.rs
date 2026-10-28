#[test]
fn an_int_entity_field_is_int_on_the_wire_and_float_number_duration_stay_number() {
    use super::FieldType;
    use serde_json::json;
    assert_eq!(FieldType::from_primitive_name("int"), Some(FieldType::Int));
    for name in ["float", "number", "duration"] {
        assert_eq!(
            FieldType::from_primitive_name(name),
            Some(FieldType::Number),
            "{name}"
        );
    }
    let field: super::FieldDefinition =
        serde_json::from_value(json!({ "name": "count", "type": "int", "default": 3 })).unwrap();
    assert_eq!(field.field_type, FieldType::Int);
    let wire = serde_json::to_value(&field).unwrap();
    assert_eq!(wire["type"], json!("int"));
    let back: super::FieldDefinition = serde_json::from_value(wire).unwrap();
    assert_eq!(back.field_type, FieldType::Int);
    assert_eq!(back.default, Some(super::SExpression::Number(3.0)));
    let number: super::FieldDefinition =
        serde_json::from_value(json!({ "name": "price", "type": "number" })).unwrap();
    assert_eq!(
        serde_json::to_value(&number).unwrap()["type"],
        json!("number")
    );
}

#[test]
fn generic_substitution_composes_nullable_without_changing_field_presence() {
    use serde_json::json;
    for (occurrence, bound, expected) in [
        (Some(false), Some(true), Some(true)),
        (Some(true), Some(false), Some(true)),
        (Some(false), Some(false), Some(false)),
        (None, Some(true), Some(true)),
        (None, None, None),
    ] {
        let mut field: super::FieldDefinition = serde_json::from_value(json!({
            "name":"value", "type":"$v", "nullable":occurrence, "required":true,
            "description":"keep"
        }))
        .unwrap();
        let binding: super::FieldDefinition = serde_json::from_value(json!({
            "type":"string", "nullable":bound, "required":false, "values":["ready"]
        }))
        .unwrap();
        let bindings = crate::collections::IndexMap::from([("v".to_string(), binding)]);
        field.substitute_type_vars(&bindings);
        assert_eq!(
            field.nullable, expected,
            "occurrence={occurrence:?}, bound={bound:?}"
        );
        assert_eq!(field.required, Some(true));
        assert_eq!(field.name.as_deref(), Some("value"));
        assert_eq!(field.description.as_deref(), Some("keep"));
        assert_eq!(field.values, Some(vec!["ready".to_string()]));
    }
    let mut container: super::FieldDefinition = serde_json::from_value(json!({
        "type":"object", "properties":{"slot":{"type":"$v","nullable":false,"required":false}},
        "items":{"type":"$v","nullable":false},
        "additionalProperties":{"type":"$v","nullable":false}
    }))
    .unwrap();
    let binding = serde_json::from_value(json!({"type":"string","nullable":true})).unwrap();
    container.substitute_type_vars(&crate::collections::IndexMap::from([(
        "v".to_string(),
        binding,
    )]));
    assert_eq!(
        container.properties.as_ref().unwrap()["slot"].nullable,
        Some(true)
    );
    assert_eq!(
        container.properties.as_ref().unwrap()["slot"].required,
        Some(false)
    );
    assert_eq!(container.items.as_ref().unwrap().nullable, Some(true));
    assert_eq!(
        container.additional_properties.as_ref().unwrap().nullable,
        Some(true)
    );
}

#[test]
fn a_type_variable_is_dollar_named_on_the_wire_and_binds_by_substitution() {
    use serde_json::json;
    let page = json!({ "type": "object", "properties": {
        "items": { "name": "items", "type": "array", "required": true, "items": { "type": "$t" } },
        "cursor": { "name": "cursor", "type": "string", "required": false }
    }});
    let mut def: super::FieldDefinition = serde_json::from_value(page.clone()).unwrap();
    let items = &def.properties.as_ref().unwrap()["items"];
    assert_eq!(
        items.items.as_ref().unwrap().field_type,
        super::FieldType::type_var("t")
    );
    assert_eq!(
        serde_json::to_value(&def).unwrap(),
        page,
        "round-trips as `$t`"
    );
    let plain: super::FieldType = serde_json::from_value(json!("string")).unwrap();
    assert_eq!(
        plain,
        super::FieldType::String,
        "control: a tag is not a variable"
    );
    assert!(serde_json::from_value::<super::FieldType>(json!("strnig")).is_err());
    assert!(serde_json::from_value::<super::FieldType>(json!("$")).is_err());

    let order: super::FieldDefinition =
        serde_json::from_value(json!({ "type": "relation", "relation": { "entity": "Order" } }))
            .unwrap();
    let other: super::FieldDefinition =
        serde_json::from_value(json!({ "type": "number" })).unwrap();
    let bindings = crate::collections::IndexMap::from([("u".to_string(), other)]);
    def.substitute_type_vars(&bindings);
    let unbound = &def.properties.as_ref().unwrap()["items"];
    assert_eq!(
        unbound.items.as_ref().unwrap().field_type,
        super::FieldType::type_var("t"),
        "control: another binding leaves `t`"
    );
    def.substitute_type_vars(&crate::collections::IndexMap::from([(
        "t".to_string(),
        order,
    )]));
    let bound = &def.properties.as_ref().unwrap()["items"];
    assert_eq!(bound.required, Some(true));
    let elem = bound.items.as_ref().unwrap();
    assert_eq!(elem.field_type, super::FieldType::Relation);
    assert_eq!(elem.relation.as_ref().unwrap().entity, "Order");
    assert_eq!(
        def.properties.as_ref().unwrap()["cursor"].field_type,
        super::FieldType::String
    );
}

#[test]
fn a_field_derived_expression_round_trips_and_skips_when_absent() {
    use serde_json::json;
    let field = json!({ "name": "emailKey", "type": "string", "derived": ["str/lower", ["object/get", "@entity", "email"]] });
    let f: super::FieldDefinition = serde_json::from_value(field.clone()).unwrap();
    assert!(f.derived.is_some());
    assert_eq!(
        serde_json::to_value(&f).unwrap()["derived"],
        field["derived"]
    );
    let plain: super::FieldDefinition =
        serde_json::from_value(json!({ "name": "email", "type": "string" })).unwrap();
    assert!(plain.derived.is_none());
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("derived")
            .is_none(),
        "control: absent stays absent"
    );
}

#[test]
fn entity_indexes_round_trip_camel_case_and_skip_when_absent() {
    use serde_json::json;
    let entity = json!({
        "name": "Edge",
        "fields": [{ "name": "graphId", "type": "string" }],
        "indexes": [{ "fields": ["graphId"] }, { "fields": ["graphId", "relation"], "orderBy": "weight:desc" }]
    });
    let e: super::EntityDefinition = serde_json::from_value(entity.clone()).unwrap();
    let indexes = e.indexes.as_ref().expect("indexes parsed");
    assert_eq!(indexes.len(), 2);
    assert_eq!(
        indexes[1].fields,
        vec!["graphId".to_string(), "relation".to_string()]
    );
    assert_eq!(indexes[1].order_by.as_deref(), Some("weight:desc"));
    assert_eq!(indexes[0].order_by, None);
    assert_eq!(
        serde_json::to_value(&e).unwrap()["indexes"],
        entity["indexes"]
    );
    let bare: super::EntityDefinition =
        serde_json::from_value(json!({ "name": "A", "fields": [] })).unwrap();
    assert!(serde_json::to_value(&bare)
        .unwrap()
        .get("indexes")
        .is_none());
    let unknown =
        json!({ "name": "A", "fields": [], "indexes": [{ "fields": ["x"], "unique": true }] });
    assert!(
        serde_json::from_value::<super::EntityDefinition>(unknown).is_err(),
        "control: an unknown index key is refused"
    );
}

#[test]
fn propositions_on_real_declarations_and_on_the_transition_expectation_round_trip() {
    use serde_json::json;
    let ensures = json!([[
        "=",
        "@entity.balance",
        ["-", "@fromEntity.balance", "@payload.amount"]
    ]]);
    let entity = json!({ "name": "Account", "fields": [{ "name": "balance", "type": "number" }], "invariants": [[">=", "@entity.balance", 0.0]] });
    let state = json!({ "name": "active", "holds": [">=", "@entity.balance", 0.0] });
    let transition =
        json!({ "from": "active", "to": "active", "event": "SPEND", "ensures": ensures });
    let expectation = json!({ "kind": "transition", "traitName": "Wallet", "from": "active", "event": "SPEND", "to": "active", "ensures": ensures, "strict": true });
    let e: super::EntityDefinition = serde_json::from_value(entity.clone()).unwrap();
    assert_eq!(e.invariants.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        serde_json::to_value(&e).unwrap()["invariants"],
        entity["invariants"]
    );
    let s: super::StateDefinition = serde_json::from_value(state.clone()).unwrap();
    assert_eq!(serde_json::to_value(&s).unwrap(), state);
    let t: super::TransitionDefinition = serde_json::from_value(transition.clone()).unwrap();
    assert_eq!(serde_json::to_value(&t).unwrap(), transition);
    let x: super::ExpectDeclaration = serde_json::from_value(expectation.clone()).unwrap();
    assert_eq!(serde_json::to_value(&x).unwrap(), expectation);
}

#[test]
fn control_declarations_without_propositions_serialize_no_proposition_keys() {
    use serde_json::json;
    let state = json!({ "name": "idle" });
    let transition = json!({ "from": "idle", "to": "busy", "event": "GO" });
    let expectation = json!({ "kind": "transition", "traitName": "T", "from": "idle", "event": "GO", "to": "busy" });
    let s: super::StateDefinition = serde_json::from_value(state.clone()).unwrap();
    assert_eq!(serde_json::to_value(&s).unwrap(), state);
    let t: super::TransitionDefinition = serde_json::from_value(transition.clone()).unwrap();
    assert_eq!(serde_json::to_value(&t).unwrap(), transition);
    let x: super::ExpectDeclaration = serde_json::from_value(expectation.clone()).unwrap();
    assert_eq!(serde_json::to_value(&x).unwrap(), expectation);
    let e: super::EntityDefinition = serde_json::from_value(
        json!({ "name": "A", "fields": [{ "name": "n", "type": "number" }] }),
    )
    .unwrap();
    assert!(serde_json::to_value(&e)
        .unwrap()
        .get("invariants")
        .is_none());
}

#[test]
fn a_tick_interval_is_a_duration_only_when_it_is_a_number_or_digits_with_a_known_unit() {
    let tick = |interval: StringOrNumber| TickDefinition {
        interval,
        ..serde_json::from_value(serde_json::json!({ "name": "t", "interval": 1, "effects": [] }))
            .unwrap()
    };
    let text = |s: &str| tick(StringOrNumber::String(s.to_string()));
    assert_eq!(
        tick(StringOrNumber::Number(1500.0)).duration_ms(),
        Some(1500)
    );
    assert_eq!(text("5s").duration_ms(), Some(5000));
    assert_eq!(text("2m").duration_ms(), Some(120_000));
    assert_eq!(text("1h").duration_ms(), Some(3_600_000));
    assert_eq!(text("40ms").duration_ms(), Some(40));
    assert_eq!(text("40").duration_ms(), Some(40));
    for not in ["frame", "0 9 * * *", "1d", "5 s", "s", "1.5s", ""] {
        assert_eq!(text(not).duration_ms(), None, "{not:?}");
    }
    assert_eq!(text("frame").interval_ms(), None);
    assert_eq!(text("every-frame").interval_ms(), None);
    assert_eq!(
        text("0 9 * * *").interval_ms(),
        Some(1000),
        "the fallback the cron caller never reaches"
    );
    assert_eq!(text("7s").interval_ms(), Some(7000));
}

#[test]
fn busy_delay_reads_css_milliseconds_and_seconds() {
    let tokens = |delay: &str| super::MotionTokens {
        durations: None,
        easings: None,
        intents: None,
        shapes: None,
        entry: None,
        busy_delay: Some(delay.to_string()),
        awaiting_skeleton: None,
    };
    assert_eq!(tokens("450ms").busy_delay_ms(), Some(450));
    assert_eq!(tokens("0.3s").busy_delay_ms(), Some(300));
    assert_eq!(tokens(" 2s ").busy_delay_ms(), Some(2000));
    assert_eq!(
        tokens("var(--x)").busy_delay_ms(),
        None,
        "control: unreadable is None, never guessed"
    );
    assert_eq!(tokens("-5ms").busy_delay_ms(), None);
    let unset = super::MotionTokens {
        busy_delay: None,
        ..tokens("1ms")
    };
    assert_eq!(unset.busy_delay_ms(), None);
}

#[test]
fn motion_tokens_keep_the_busy_delay_and_awaiting_skeleton_switch() {
    let tokens: super::MotionTokens = serde_json::from_value(
        serde_json::json!({ "busyDelay": "450ms", "awaitingSkeleton": true }),
    )
    .unwrap();
    assert_eq!(tokens.busy_delay.as_deref(), Some("450ms"));
    assert_eq!(tokens.awaiting_skeleton, Some(true));
    assert_eq!(
        serde_json::to_value(&tokens).unwrap(),
        serde_json::json!({ "busyDelay": "450ms", "awaitingSkeleton": true })
    );
    let bare: super::MotionTokens = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        serde_json::json!({}),
        "control: an undeclared switch stays absent"
    );
}

use super::*;

mod union_variant {
    use super::*;
    use serde_json::json;

    fn union(variants: serde_json::Value, order: &[&str]) -> FieldDefinition {
        serde_json::from_value(json!({ "type": "union", "values": order, "properties": variants }))
            .unwrap()
    }

    fn tag(value: &str) -> serde_json::Value {
        json!({ "type": "string", "required": true, "values": [value] })
    }

    fn by_name(shape: &str) -> serde_json::Value {
        json!({ "type": "union", "values": [shape] })
    }

    fn pick<'d>(def: &'d FieldDefinition, value: &[(&str, &str)]) -> UnionVariantChoice<'d> {
        let keys: Vec<&str> = value.iter().map(|(k, _)| *k).collect();
        def.select_union_variant(&keys, |field, tag| {
            match value.iter().find(|(k, _)| *k == field) {
                Some((_, v)) if v.starts_with('@') => TagReading::LateBound,
                Some((_, v)) if *v == tag => TagReading::Equal,
                _ => TagReading::Differs,
            }
        })
    }

    fn chosen<'d>(def: &'d FieldDefinition, c: UnionVariantChoice<'d>) -> Option<&'d str> {
        let UnionVariantChoice::Matched(v) = c else {
            return None;
        };
        let (names, variants) = (def.values.as_ref()?, def.properties.as_ref()?);
        names
            .iter()
            .find(|n| variants.get(*n).is_some_and(|d| std::ptr::eq(d, v)))
            .map(String::as_str)
    }

    fn agent_tool() -> FieldDefinition {
        let text = json!({ "type": "string", "required": true });
        union(
            json!({
                "Scope": { "type": "object", "properties": { "scope": tag("declared") } },
                "Input": { "type": "object", "properties": { "event": text } },
                "Read": { "type": "object", "properties": { "read": text } }
            }),
            &["Scope", "Input", "Read"],
        )
    }

    /// A union variant whose only single-valued field is a by-name back-reference (`of : Pred`).
    fn predicate() -> FieldDefinition {
        union(
            json!({
                "Compare": { "type": "object", "properties": { "kind": tag("compare"), "field": { "type": "string" } } },
                "Not": { "type": "object", "properties": { "of": by_name("Pred") } },
                "Leaf": { "type": "object", "properties": { "label": { "type": "string" } } }
            }),
            &["Compare", "Not", "Leaf"],
        )
    }

    #[test]
    fn a_by_name_reference_is_not_a_variant_tag() {
        let def = predicate();
        assert_eq!(chosen(&def, pick(&def, &[("of", "x")])), Some("Not"));
        assert_eq!(
            chosen(&def, pick(&def, &[("of", "Pred")])),
            Some("Not"),
            "a value equal to the referenced name is still not a tag match"
        );
    }

    #[test]
    fn control_a_real_tag_still_selects_beside_a_by_name_variant() {
        let def = predicate();
        assert_eq!(
            chosen(&def, pick(&def, &[("kind", "compare"), ("field", "a")])),
            Some("Compare")
        );
        assert_eq!(chosen(&def, pick(&def, &[("label", "l")])), Some("Leaf"));
    }

    #[test]
    fn a_tag_selects_and_an_untagged_value_selects_by_closed_shape() {
        let def = agent_tool();
        assert_eq!(
            chosen(&def, pick(&def, &[("scope", "declared")])),
            Some("Scope")
        );
        assert_eq!(
            chosen(&def, pick(&def, &[("read", "Product")])),
            Some("Read")
        );
        assert_eq!(
            chosen(&def, pick(&def, &[("event", "Shop.ADD")])),
            Some("Input")
        );
    }

    #[test]
    fn a_value_merging_two_variants_fits_none() {
        let def = agent_tool();
        assert!(matches!(
            pick(&def, &[("event", "Shop.ADD"), ("read", "Product")]),
            UnionVariantChoice::NoMatch
        ));
    }

    #[test]
    fn equal_fits_are_ambiguous_and_required_members_break_a_tie() {
        let x = json!({ "type": "string" });
        let x_req = json!({ "type": "string", "required": true });
        let tie = union(
            json!({ "A": { "type": "object", "properties": { "x": x } },
                    "B": { "type": "object", "properties": { "x": x, "y": x } } }),
            &["A", "B"],
        );
        assert!(
            matches!(pick(&tie, &[("x", "1")]), UnionVariantChoice::Ambiguous(n) if n == ["A", "B"])
        );
        let broken = union(
            json!({ "A": { "type": "object", "properties": { "x": x_req } },
                    "B": { "type": "object", "properties": { "x": x, "y": x_req } } }),
            &["B", "A"],
        );
        assert_eq!(chosen(&broken, pick(&broken, &[("x", "1")])), Some("A"));
    }

    #[test]
    fn a_late_bound_tag_defers() {
        let def = agent_tool();
        assert!(matches!(
            pick(&def, &[("scope", "@config.scope")]),
            UnionVariantChoice::Deferred
        ));
    }

    #[test]
    fn an_undeclared_variant_or_a_by_name_reference_is_malformed() {
        let def = union(
            json!({ "A": { "type": "object", "properties": {} } }),
            &["A", "Missing"],
        );
        assert!(
            matches!(pick(&def, &[]), UnionVariantChoice::Malformed(m) if m.contains("Missing"))
        );
        let back: FieldDefinition = serde_json::from_value(by_name("Pred")).unwrap();
        assert!(matches!(pick(&back, &[]), UnionVariantChoice::Malformed(_)));
    }
}

#[test]
fn field_type_event_serde_round_trips_as_lowercase_tag() {
    let json = serde_json::to_string(&FieldType::Event).expect("serializes");
    assert_eq!(json, "\"event\"");
    let back: FieldType = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(back, FieldType::Event);
}

/// `ConfigField.default: Option<SExpression>` must distinguish "no
/// default was declared" (absent `default` key) from "the default IS
/// `Null`" (a `trait`-typed knob's `none` sentinel, owner ruling
/// 2026-09-13) through a full JSON round trip — the shape a `.orb`
/// registry file actually persists and every downstream consumer
/// (`uses` load, `orb validate`'s re-inline, the runtime) reads back.
/// Without `deserialize_present_default`, `Option<T>`'s default serde
/// impl collapses a present-but-null JSON value to `None`, same as an
/// absent key — silently erasing the distinction on every round trip.
#[test]
fn config_field_default_null_round_trips_as_some_null_not_none() {
    let bound = ConfigField {
        nullable: None,
        additional_properties: None,
        field_type: "trait".to_string(),
        default: Some(SExpression::string("@trait.DefaultForm")),
        label: None,
        description: None,
        synonyms: None,
        tier: None,
        values: None,
        items: None,
        properties: None,
        required: None,
        ref_id: None,
        forwarded_from: None,
        nested_forwarded_from: Vec::new(),
        wired: false,
        payload_for: None,
        projected_from: None,
        record: None,
    };
    let unbound = ConfigField {
        default: Some(SExpression::Null),
        ..bound.clone()
    };
    let no_default = ConfigField {
        default: None,
        ..bound.clone()
    };

    for (field, expected) in [
        (bound, Some(SExpression::string("@trait.DefaultForm"))),
        (unbound, Some(SExpression::Null)),
        (no_default, None),
    ] {
        let json = serde_json::to_string(&field).expect("serializes");
        let back: ConfigField = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.default, expected, "round trip of {json}");
    }

    // The on-disk shape itself: a present `"default": null` key, not an
    // absent one — pins the JSON representation, not just the Rust value.
    let json = serde_json::to_value(&ConfigField {
        default: Some(SExpression::Null),
        ..ConfigField {
            nullable: None,
            additional_properties: None,
            field_type: "trait".to_string(),
            default: None,
            label: None,
            description: None,
            synonyms: None,
            tier: None,
            values: None,
            items: None,
            properties: None,
            required: None,
            ref_id: None,
            forwarded_from: None,
            nested_forwarded_from: Vec::new(),
            wired: false,
            payload_for: None,
            projected_from: None,
            record: None,
        }
    })
    .expect("serializes to value");
    assert_eq!(json.get("default"), Some(&serde_json::Value::Null));
}

#[test]
fn retag_callsite_payload_keeps_local_field_as_payload() {
    // `data` IS declared by the child's rendering transition → the binding
    // resolves locally → ordinary @payload. Incoming @callsitePayload (from
    // a lower-time eager tag) is corrected back to @payload.
    let local: std::collections::HashSet<String> = ["data".to_string()].into_iter().collect();
    let mut e = SExpression::object(
        [(
            "entity".to_string(),
            SExpression::string("@callsitePayload.data"),
        )]
        .into_iter()
        .collect(),
    );
    retag_callsite_payload(&mut e, &local);
    if let SExpression::Object(m) = &e {
        assert_eq!(m["entity"], SExpression::string("@payload.data"));
    } else {
        panic!("shape changed");
    }
}

#[test]
fn retag_callsite_payload_captures_nonlocal_field() {
    // `error` is NOT declared by the child's rendering transition (leaf under
    // INIT {}) → the field must come from the composing event → capture.
    // Preserves the legit `content={?error}` embed case.
    let local: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut e = SExpression::string("@payload.error");
    retag_callsite_payload(&mut e, &local);
    assert_eq!(e, SExpression::string("@callsitePayload.error"));
    // idempotent + source-agnostic: an already-captured token stays captured.
    retag_callsite_payload(&mut e, &local);
    assert_eq!(e, SExpression::string("@callsitePayload.error"));
}

#[test]
fn retag_callsite_payload_uses_field_head_for_dotted_and_indexed_paths() {
    let local: std::collections::HashSet<String> = ["row".to_string()].into_iter().collect();
    let mut dotted = SExpression::string("@callsitePayload.row.name");
    retag_callsite_payload(&mut dotted, &local);
    assert_eq!(dotted, SExpression::string("@payload.row.name"));
    let mut indexed = SExpression::string("@payload.data[0].id");
    retag_callsite_payload(&mut indexed, &local); // data not local → capture
    assert_eq!(indexed, SExpression::string("@callsitePayload.data[0].id"));
}

#[test]
fn test_parse_entity() {
    let json = r#"{
            "name": "User",
            "collection": "users",
            "fields": [
                { "name": "id", "type": "string", "required": true, "primaryKey": true },
                { "name": "email", "type": "string", "required": true, "unique": true },
                { "name": "status", "type": "enum", "values": ["pending", "active", "suspended"], "default": "pending" },
                { "name": "connectionSlots", "type": "number", "default": 10 }
            ]
        }"#;

    let entity: EntityDefinition = serde_json::from_str(json).unwrap();
    assert_eq!(entity.name, "User");
    assert_eq!(entity.collection, Some("users".to_string()));
    assert_eq!(entity.fields.len(), 4);

    // Check enum field
    let status_field = &entity.fields[2];
    assert_eq!(status_field.name, Some("status".to_string()));
    assert_eq!(status_field.field_type, FieldType::Enum);
    assert_eq!(
        status_field.values,
        Some(vec![
            "pending".to_string(),
            "active".to_string(),
            "suspended".to_string()
        ])
    );
}

#[test]
fn test_parse_relation() {
    let json = r#"{
            "name": "requesterId",
            "type": "relation",
            "relation": { "entity": "User", "cardinality": "one" },
            "required": true
        }"#;

    let field: FieldDefinition = serde_json::from_str(json).unwrap();
    assert_eq!(field.field_type, FieldType::Relation);

    let relation = field.relation.unwrap();
    assert_eq!(relation.entity, "User");
    assert_eq!(relation.cardinality, Some(Cardinality::One));
}

#[test]
fn test_parse_transition() {
    let json = r#"{
            "from": "browsing",
            "to": "registering",
            "event": "REGISTER",
            "effects": [
                ["render-ui", "main", { "type": "page-header", "title": "Register User" }]
            ]
        }"#;

    let transition: TransitionDefinition = serde_json::from_str(json).unwrap();
    assert_eq!(
        transition.from,
        StringOrArray::Single("browsing".to_string())
    );
    assert_eq!(transition.to, "registering");
    assert_eq!(transition.event, "REGISTER");
    assert!(transition.effects.is_some());
}

#[test]
fn test_parse_transition_with_guard() {
    let json = r#"{
            "from": "registering",
            "to": "browsing",
            "event": "SAVE",
            "guard": ["and", ["!=", "@payload.data.email", ""], ["!=", "@payload.data.name", ""]],
            "effects": [
                ["persist", "create", "User", "@payload.data"],
                ["emit", "USER_REGISTERED", { "userId": "@payload.data.id" }]
            ]
        }"#;

    let transition: TransitionDefinition = serde_json::from_str(json).unwrap();
    assert!(transition.guard.is_some());

    let guard = transition.guard.unwrap();
    assert_eq!(guard.operator(), Some("and"));
}

#[test]
fn test_string_or_array() {
    // Single string
    let json = r#""browsing""#;
    let soa: StringOrArray = serde_json::from_str(json).unwrap();
    assert!(soa.matches("browsing"));
    assert!(!soa.matches("editing"));

    // Array
    let json = r#"["browsing", "viewing", "editing"]"#;
    let soa: StringOrArray = serde_json::from_str(json).unwrap();
    assert!(soa.matches("browsing"));
    assert!(soa.matches("viewing"));
    assert!(soa.matches("editing"));
    assert!(!soa.matches("other"));
}

#[test]
fn test_parse_listen_definition() {
    let json = r#"{
            "event": "CONNECTION_ACCEPTED",
            "triggers": "CREATE_RELATIONSHIP",
            "scope": "external",
            "description": "Creates a new relationship health record",
            "payloadMapping": {
                "connectionId": "@payload.connectionId"
            }
        }"#;

    let listen: ListenDefinition = serde_json::from_str(json).unwrap();
    assert_eq!(listen.event, "CONNECTION_ACCEPTED");
    assert_eq!(listen.triggers, Some("CREATE_RELATIONSHIP".to_string()));
    assert_eq!(listen.scope, Some(EventScope::External));
    assert!(listen.payload_mapping.is_some());
}

#[test]
fn test_entity_shared_true_round_trips() {
    let json = r#"{ "name": "Session", "fields": [], "shared": true }"#;
    let entity: EntityDefinition = serde_json::from_str(json).unwrap();
    assert!(entity.shared);

    let reserialized = serde_json::to_string(&entity).unwrap();
    assert!(reserialized.contains("\"shared\":true"));
}

#[test]
fn test_store_binding_round_trips_on_entity_and_app_and_is_omitted_when_absent() {
    let entity: EntityDefinition = serde_json::from_str(
        r#"{ "name": "Ticket", "persistence": "persistent", "store": "Rows", "fields": [] }"#,
    )
    .unwrap();
    assert_eq!(entity.store.as_deref(), Some("Rows"));
    assert_eq!(serde_json::to_value(&entity).unwrap()["store"], "Rows");
    let plain: EntityDefinition =
        serde_json::from_str(r#"{ "name": "Note", "fields": [] }"#).unwrap();
    assert_eq!(plain.store, None);
    assert!(serde_json::to_value(&plain).unwrap().get("store").is_none());
    let app: OrbitalSchema =
        serde_json::from_str(r#"{ "name": "shop", "store": "Rows", "orbitals": [] }"#).unwrap();
    assert_eq!(app.store.as_deref(), Some("Rows"));
    let bare: OrbitalSchema =
        serde_json::from_str(r#"{ "name": "shop", "orbitals": [] }"#).unwrap();
    assert!(serde_json::to_value(&bare).unwrap().get("store").is_none());
}

#[test]
fn test_entity_shared_defaults_false_when_absent() {
    let json = r#"{ "name": "Task", "fields": [] }"#;
    let entity: EntityDefinition = serde_json::from_str(json).unwrap();
    assert!(!entity.shared);
}

#[test]
fn test_entity_shared_false_omitted_from_serialization() {
    let entity = EntityDefinition {
        timestamps: None,
        soft_delete: None,
        description: None,
        visual_prompt: None,
        asset_ref: None,
        store: None,
        name: "Task".to_string(),
        id: None,
        collection: None,
        persistence: None,
        fields: vec![],
        instances: None,
        shared: false,
        identity: false,
        browser: false,
        locale_instances: None,
        read_policy: None,
        create_policy: None,
        update_policy: None,
        delete_policy: None,
        access_waivers: None,
        invariants: None,
        indexes: None,
    };
    let serialized = serde_json::to_string(&entity).unwrap();
    assert!(!serialized.contains("shared"));
}

#[test]
fn test_entity_identity_defaults_false_when_absent() {
    let json = r#"{ "name": "Task", "fields": [] }"#;
    let entity: EntityDefinition = serde_json::from_str(json).unwrap();
    assert!(!entity.identity);
}

/// The property that keeps all committed `.orb` files byte-identical after
/// the `[identity]` tag lands: a non-identity entity must not gain a key.
#[test]
fn test_entity_identity_false_omitted_from_serialization() {
    let json = r#"{"name":"Task","fields":[]}"#;
    let entity: EntityDefinition = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_string(&entity).unwrap();
    assert!(!serialized.contains("identity"));
    assert_eq!(serialized, json);
}

#[test]
fn test_entity_identity_round_trips_when_set() {
    let json = r#"{"name":"Person","fields":[],"identity":true}"#;
    let entity: EntityDefinition = serde_json::from_str(json).unwrap();
    assert!(entity.identity);
    assert!(serde_json::to_string(&entity)
        .unwrap()
        .contains("\"identity\":true"));
}

// =========================================================================
// Orbital import wire contract (Almadar_Orbital_Import.md §4, W1-R)
// =========================================================================

#[test]
fn orbital_ref_object_roundtrips_through_serde() {
    let mut fields = crate::collections::IndexMap::new();
    fields.insert("dueDate".to_string(), "deadline".to_string());

    let mut pages = crate::collections::IndexMap::new();
    pages.insert("/notes".to_string(), "/scratch-notes".to_string());

    let mut config = crate::collections::IndexMap::new();
    config.insert(
        "pageSize".to_string(),
        ConfigField {
            nullable: None,
            additional_properties: None,
            field_type: "number".to_string(),
            default: Some(SExpression::Number(50.0)),
            label: None,
            description: None,
            synonyms: None,
            tier: None,
            values: None,
            items: None,
            properties: None,
            required: None,
            ref_id: None,
            forwarded_from: None,
            nested_forwarded_from: Vec::new(),
            wired: false,
            payload_for: None,
            projected_from: None,
            record: None,
        },
    );

    let mut events = crate::collections::IndexMap::new();
    events.insert("NOTE_SAVED".to_string(), "SCRATCH_NOTE_SAVED".to_string());

    let obj = OrbitalRefObject {
        reference: "Notes.orbitals.NoteOrbital".to_string(),
        ref_id: Some("orbital_note123".to_string()),
        entity: Some("ScratchNote".to_string()),
        fields: Some(fields),
        pages: Some(pages),
        omit: Some(vec!["NoteDelete".to_string()]),
        only: None,
        config: Some(config),
        events: Some(events),
        roles: None,
        entities: None,
        mounts: None,
        extend: None,
        mock: None,
        listens: None,
        ..Default::default()
    };

    let value = serde_json::to_value(&obj).expect("serializes");
    // Wire keys are `ref`/`refId`, never the Rust field names.
    assert!(value.get("ref").is_some());
    assert!(value.get("refId").is_some());
    assert!(value.get("reference").is_none());
    assert!(value.get("ref_id").is_none());
    assert_eq!(value["ref"], "Notes.orbitals.NoteOrbital");

    let back: OrbitalRefObject = serde_json::from_value(value.clone()).expect("deserializes");
    let round_tripped = serde_json::to_value(&back).expect("serializes again");
    assert_eq!(value, round_tripped);
}

#[test]
fn orbital_config_defaults_to_none_on_missing_key() {
    let orbital_json = r#"{
            "name": "TestOrbital",
            "entity": { "name": "Thing", "fields": [] },
            "traits": []
        }"#;
    let orbital: Orbital = serde_json::from_str(orbital_json).unwrap();
    assert!(orbital.config.is_none());
    assert!(orbital.reference.is_none());

    let schema_json = r#"{
            "name": "TestApp",
            "orbitals": []
        }"#;
    let schema: OrbitalSchema = serde_json::from_str(schema_json).unwrap();
    assert!(schema.config.is_none());
}

/// `ColorTokens`/`IllustrationTokens` round-trip on their own — the two
/// typed axes `ThemeTokens`/`ThemeVariant` gained to mirror
/// `@almadar/core`'s `ColorTokens`/`IllustrationTokens`
/// (`src/types/domain.ts`). Neither axis is emitted by
/// `themeTokensToCssVars` yet (the migrated presets never populate
/// them), so this is the only coverage of the fields themselves.
#[test]
fn color_and_illustration_tokens_round_trip() {
    let tokens = ThemeTokens {
        color: Some(ColorTokens {
            primary: Some("#14b8a6".to_string()),
            primary_hover: None,
            primary_foreground: Some("#ffffff".to_string()),
            secondary: None,
            secondary_hover: None,
            secondary_foreground: None,
            accent: None,
            accent_foreground: None,
            muted: None,
            muted_foreground: None,
            background: Some("#0b0b0f".to_string()),
            foreground: None,
            card: None,
            card_foreground: None,
            surface: None,
            border: None,
            input: None,
            ring: None,
            error: None,
            error_foreground: None,
            success: None,
            success_foreground: None,
            warning: None,
            warning_foreground: None,
            info: None,
            info_foreground: None,
            table_header: None,
            table_border: None,
            table_row_hover: None,
            surface_hover: None,
            border_hover: None,
            placeholder: None,
            scrim: None,
        }),
        colors: None,
        radii: None,
        spacing: None,
        typography: None,
        shadows: None,
        density: None,
        type_scale: None,
        motion: None,
        iconography: None,
        elevation: None,
        geometry: None,
        illustration: Some(IllustrationTokens {
            style: Some(IllustrationStyle::Mascot),
            empty_asset: Some("empty.svg".to_string()),
            loading_asset: None,
            error_asset: None,
            onboarding_asset: None,
        }),
        surface: None,
        diagram: None,
        scene: None,
    };

    let json = serde_json::to_value(&tokens).expect("serializes");
    assert_eq!(json["color"]["primary"], "#14b8a6");
    assert_eq!(json["color"]["primaryHover"], serde_json::Value::Null);
    assert!(json["color"]
        .as_object()
        .unwrap()
        .get("primaryHover")
        .is_none());
    assert_eq!(json["illustration"]["style"], "mascot");

    let back: ThemeTokens = serde_json::from_value(json.clone()).expect("deserializes");
    let round_tripped = serde_json::to_value(&back).expect("serializes again");
    assert_eq!(json, round_tripped);
    assert_eq!(back.color.unwrap().primary, Some("#14b8a6".to_string()));
    assert!(back.illustration.unwrap().style.is_some());
}

/// `ThemeVariant` (the dark-mode override shape) carries the same
/// `color`/`illustration` axes as `ThemeTokens` — assert the wire key
/// is `typeScale` (camelCase), not `type_scale`, and that an absent
/// `color`/`illustration` never appears in the serialized object.
#[test]
fn theme_variant_color_and_illustration_are_optional_and_omitted_when_absent() {
    let variant = ThemeVariant {
        color: None,
        colors: Some(ThemeMap::from_iter([(
            "background".to_string(),
            "#111".to_string(),
        )])),
        radii: None,
        spacing: None,
        typography: None,
        shadows: None,
        density: None,
        type_scale: None,
        motion: None,
        iconography: None,
        elevation: None,
        geometry: None,
        illustration: None,
        diagram: None,
        scene: None,
        surface: None,
    };
    let json = serde_json::to_value(&variant).expect("serializes");
    let obj = json.as_object().expect("object");
    assert!(!obj.contains_key("color"));
    assert!(!obj.contains_key("illustration"));
    assert!(!obj.contains_key("typeScale"));
    assert_eq!(json["colors"]["background"], "#111");
}

/// Loads two of the migrated theme presets (`@almadar/core` `themes/*.json`,
/// read from the installed core (`language_data`); produced by
/// `almadar-core/scripts/migrate-css-themes.ts` from the hand-authored
/// CSS — see plan `groovy-inventing-cloud.md` Program C, C1) through
/// `ThemeDefinition` and asserts NO field is lost: reserializing must
/// reproduce the exact same JSON value the TypeScript side wrote.
#[test]
fn migrated_theme_presets_round_trip_losslessly() {
    let installed = &crate::language_data::language_data_after_preflight().themes;
    let preset = |name: &str| -> &str {
        installed
            .iter()
            .find(|json| {
                serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v["name"] == name)
            })
            .unwrap_or_else(|| panic!("installed core has no `{name}` preset"))
    };

    for (name, raw) in [
        ("clay", preset("clay")),
        ("minimalist", preset("minimalist")),
    ] {
        let original: serde_json::Value =
            serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}: invalid JSON: {e}"));
        let parsed: ThemeDefinition = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{name}: does not parse as ThemeDefinition: {e}"));
        let round_tripped = serde_json::to_value(&parsed).expect("serializes");
        assert_eq!(
            original, round_tripped,
            "{name}: lost or changed a field on round trip"
        );

        // Sanity: both presets declare colors, a dark variant, and the
        // typed skin axes the migration produced.
        assert!(
            parsed.tokens.colors.is_some(),
            "{name}: expected legacy colors map"
        );
        assert!(
            parsed.tokens.density.is_some(),
            "{name}: expected density axis"
        );
        assert!(
            parsed.tokens.type_scale.is_some(),
            "{name}: expected typeScale axis"
        );
        let dark = parsed.variants.as_ref().and_then(|v| v.get("dark"));
        assert!(dark.is_some(), "{name}: expected a dark variant");
        assert!(
            dark.unwrap().colors.is_some(),
            "{name}: expected dark colors override"
        );
    }
}

#[test]
fn field_properties_keep_their_declaration_order_through_a_round_trip() {
    let text = r#"{"name":"p","type":"object","properties":{"b":{"type":"string"},"a":{"type":"string"},"c":{"type":"string"}}}"#;
    for _ in 0..8 {
        let field: super::FieldDefinition = serde_json::from_str(text).unwrap();
        let keys: Vec<&str> = field
            .properties
            .as_ref()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["b", "a", "c"]);
        let out = serde_json::to_string(&field).unwrap();
        let at = |k: &str| out.find(&format!("\"{k}\":{{")).unwrap();
        assert!(at("b") < at("a") && at("a") < at("c"), "{out}");
    }
}

fn key_positions(wire: &str, keys: &[&str]) -> Vec<usize> {
    keys.iter()
        .map(|k| {
            wire.find(&format!("\"{k}\":"))
                .unwrap_or_else(|| panic!("{k} missing in {wire}"))
        })
        .collect()
}

fn is_ascending(positions: &[usize]) -> bool {
    positions.windows(2).all(|w| w[0] < w[1])
}

#[test]
fn schema_maps_serialize_in_insertion_order() {
    use super::{DesignHints, DomainContext, TraitReference};
    let build = |keys: &[&str]| {
        let entries = |tag: &str| {
            keys.iter()
                .map(|k| {
                    format!(
                        "\"{tag}{k}\":{}",
                        if tag == "c" {
                            format!("{{\"type\":\"string\",\"default\":\"{k}\"}}")
                        } else {
                            format!("\"{k}\"")
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        let trait_ref = format!(
            "{{\"ref\":\"A.traits.B\",\"config\":{{{}}},\"events\":{{{}}},\"eventIds\":{{{}}},\"fields\":{{{}}},\"typeArgs\":{{{}}}}}",
            entries("c"), entries("e"), entries("i"), entries("f"), entries("t")
        );
        let vocab = format!("{{\"vocabulary\":{{{}}}}}", entries("v"));
        let hints = format!("{{\"uxHints\":{{{}}}}}", entries("u"));
        let trait_ref: TraitReference = serde_json::from_str(&trait_ref).unwrap();
        let vocab: DomainContext = serde_json::from_str(&vocab).unwrap();
        let hints: DesignHints = serde_json::from_str(&hints).unwrap();
        (
            serde_json::to_string(&trait_ref).unwrap(),
            serde_json::to_string(&vocab).unwrap(),
            serde_json::to_string(&hints).unwrap(),
        )
    };
    let forward = ["zeta", "alpha", "mid", "beta"];
    let reversed = ["beta", "mid", "alpha", "zeta"];
    let (a_trait, a_vocab, a_hints) = build(&forward);
    for _ in 0..8 {
        assert_eq!(
            build(&forward),
            (a_trait.clone(), a_vocab.clone(), a_hints.clone())
        );
    }
    let (b_trait, b_vocab, b_hints) = build(&reversed);
    assert_ne!(a_trait, b_trait);
    assert_ne!(a_vocab, b_vocab);
    assert_ne!(a_hints, b_hints);
    for tag in ["c", "e", "i", "f", "t"] {
        let fwd: Vec<String> = forward.iter().map(|k| format!("{tag}{k}")).collect();
        let rev: Vec<String> = reversed.iter().map(|k| format!("{tag}{k}")).collect();
        let fwd: Vec<&str> = fwd.iter().map(String::as_str).collect();
        let rev: Vec<&str> = rev.iter().map(String::as_str).collect();
        assert!(
            is_ascending(&key_positions(&a_trait, &fwd)),
            "{tag} forward"
        );
        assert!(
            is_ascending(&key_positions(&b_trait, &rev)),
            "{tag} reversed"
        );
    }
    assert!(is_ascending(&key_positions(
        &a_vocab,
        &["vzeta", "valpha", "vmid", "vbeta"]
    )));
    assert!(is_ascending(&key_positions(
        &a_hints,
        &["uzeta", "ualpha", "umid", "ubeta"]
    )));
}

#[test]
fn a_wired_config_entry_is_not_a_declaration_whatever_type_it_carries() {
    let wired: ConfigField = serde_json::from_value(
        serde_json::json!({ "type": "string", "wired": true, "default": "@config.k" }),
    )
    .unwrap();
    assert!(!wired.is_declaration_form());
    assert_eq!(serde_json::to_value(&wired).unwrap()["wired"], true);
    let declared: ConfigField =
        serde_json::from_value(serde_json::json!({ "type": "string", "default": "x" })).unwrap();
    assert!(
        declared.is_declaration_form(),
        "control: a typed entry without the mark declares a knob"
    );
    assert!(serde_json::to_value(&declared)
        .unwrap()
        .get("wired")
        .is_none());
    let legacy: ConfigField =
        serde_json::from_value(serde_json::json!({ "type": "unknown", "default": 1 })).unwrap();
    assert!(
        !legacy.is_declaration_form(),
        "control: hand-written unknown wiring still reads as wiring"
    );
}
