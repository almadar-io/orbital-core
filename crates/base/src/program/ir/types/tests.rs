use super::*;

#[test]
fn a_payload_field_of_a_std_value_type_has_the_type_an_entity_field_of_it_has() {
    for (word, ty) in [("orbital", OirType::Orbital), ("behavior", OirType::Behavior), ("trait", OirType::Trait)] {
        assert_eq!(payload_type_shape(word), ty, "{word}");
        assert_eq!(payload_type_shape(&format!("[{word}]")), OirType::array(ty.clone()), "[{word}]");
        let field: crate::schema::FieldDefinition =
            serde_json::from_value(serde_json::json!({ "name": "f", "type": word })).unwrap();
        assert_eq!(payload_type_shape(word), field_shape(&field), "payload and entity agree on {word}");
    }
    assert_eq!(
        payload_type_shape("Order"),
        OirType::Reference { entity: "Order".to_string() },
        "control: a program name stays a reference"
    );
}

#[test]
fn a_program_type_spelled_like_a_builtin_in_another_case_stays_the_programs_type() {
    for name in ["Orbital", "Trait", "Node", "File", "String", "Event"] {
        assert_eq!(payload_type_shape(name), OirType::Reference { entity: name.to_string() }, "{name}");
    }
    assert_eq!(payload_type_shape("CallKey"), OirType::CallKey, "control: a mixed-case builtin is spelled as the language spells it");
    assert_eq!(payload_type_shape("SExpr"), OirType::SExpr, "control");
    assert_eq!(payload_type_shape("orbital"), OirType::Orbital, "control: the builtin itself");
}

#[test]
fn payload_array_item_type_distinguishes_lists_from_tuples() {
    assert_eq!(payload_array_item_type(" [ string ] "), Some(" string "));
    assert_eq!(payload_array_item_type("[Map<string,number>]"), Some("Map<string,number>"));
    assert_eq!(payload_array_item_type("[[string,number]]"), Some("[string,number]"));
    for non_array in ["[string,number]", "[string,[number]]", "Map<string,number>", "string"] {
        assert_eq!(payload_array_item_type(non_array), None, "{non_array}");
    }
    assert_eq!(payload_type_shape("[string]"), OirType::array(OirType::String));
    let tuple = OirType::Tuple { items: vec![OirType::String, OirType::Number] };
    assert_eq!(payload_type_shape("[string,number]"), tuple);
    assert_eq!(payload_type_shape("[[string,number]]"), OirType::array(tuple));
}

#[test]
fn declared_shape_resolver_keeps_nullable_recursive_aliases_and_excludes_unrelated_types() {
    let reference = |name: &str| OirType::TypeRef { name: name.into() };
    let definitions = std::collections::BTreeMap::from([
        ("Alias", reference("V")),
        ("V", OirType::array(OirType::optional(reference("V")))),
        ("Unused", OirType::Boolean),
    ]);
    let shape = DeclaredShape::with_resolver(Some("Alias".into()), reference("Alias"), |name| {
        definitions.get(name).cloned()
    });
    assert_eq!(
        shape.named.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["Alias", "V"]
    );
    assert_eq!(
        shape.named["V"],
        OirType::array(OirType::optional(reference("V")))
    );
    assert_eq!(
        DeclaredShape::parse(&serde_json::to_string(&shape).unwrap()),
        Some(shape)
    );
}

#[test]
fn type_var_substitution_visits_nested_types_once_and_preserves_metadata() {
    let var = |name: &str| OirType::Var { name: name.into() };
    let mut field = OirField::new("slot", OirType::optional(var("t")));
    field.required = false;
    field.description = Some("keep".into());
    let mut shape = OirType::Tuple { items: vec![
        OirType::map(var("t"), OirType::array(var("t"))),
        OirType::Struct { fields: vec![field.clone()].into(), additional: None },
        OirType::Union { variants: vec![OirField::new("case", var("t"))].into() },
        OirType::TypeRef { name: "t".into() },
        OirType::reference("t"),
        var("unbound"),
    ] };
    let bindings = std::collections::BTreeMap::from([
        ("t".into(), var("next")),
        ("next".into(), OirType::Number),
    ]);
    assert!(shape.substitute_type_vars(&bindings));
    let OirType::Tuple { items } = &shape else { panic!("tuple preserved") };
    assert_eq!(items[0], OirType::map(var("next"), OirType::array(var("next"))));
    let OirType::Struct { fields, .. } = &items[1] else { panic!("record preserved") };
    field.field_type = OirType::optional(var("next"));
    assert_eq!(&fields[..], &[field]);
    assert_eq!(items[2], OirType::Union { variants: vec![OirField::new("case", var("next"))].into() });
    assert_eq!(items[3], OirType::TypeRef { name: "t".into() });
    assert_eq!(items[4], OirType::reference("t"));
    assert_eq!(items[5], var("unbound"));
    assert!(!shape.substitute_type_vars(&std::collections::BTreeMap::new()));
    let mut cycle = var("t");
    assert!(cycle.substitute_type_vars(&std::collections::BTreeMap::from([
        ("t".into(), OirType::array(var("t"))),
    ])));
    assert_eq!(cycle, OirType::array(var("t")), "new nodes are not revisited");
    let mut explicit = OirType::map(OirType::String, OirType::reference("Before"));
    assert!(explicit.rewrite(&|ty| match ty {
        OirType::Reference { entity } if entity == "Before" => Some(OirType::reference("After")),
        _ => None,
    }));
    assert_eq!(explicit, OirType::map(OirType::String, OirType::reference("After")));
    assert!(!explicit.rewrite(&|ty| Some(ty.clone())));
}

#[test]
fn declared_shape_contract_version_is_explicit_and_legacy_remains_unmarked() {
    let old = DeclaredShape::parse(r#"{"type":{"kind":"string"}}"#).unwrap();
    assert!(serde_json::to_value(&old).unwrap().get("version").is_none());
    let current = DeclaredShape::parse(r#"{"version":1,"type":{"kind":"string"}}"#).unwrap();
    assert_eq!(serde_json::to_value(&current).unwrap()["version"], 1);
    assert!(DeclaredShape::parse(r#"{"version":2,"type":{"kind":"string"}}"#).is_none());
    let aliases = std::collections::BTreeMap::new();
    let built = DeclaredShape::with_named(None, OirType::String, &aliases);
    assert!(serde_json::to_value(&built).unwrap().get("version").is_none());
    let legacy: crate::schema::OrbitalSchema = serde_json::from_value(serde_json::json!({"name":"old","orbitals":[]})).unwrap();
    assert!(serde_json::to_value(legacy).unwrap().get("typeContractVersion").is_none());
    let current: crate::schema::OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name":"new","orbitals":[],"typeContractVersion":1,
        "types":{"Row":[{"name":"", "type":"object", "nullable":false, "additionalProperties":{"type":"string","nullable":false}}]}
    })).unwrap();
    assert_eq!(current.type_contract_version, Some(1));
    assert!(current.types.as_ref().unwrap().contains_key("Row"));
    assert!(serde_json::from_value::<crate::schema::OrbitalSchema>(serde_json::json!({"name":"bad","orbitals":[],"typeContractVersion":2})).is_err());
}

#[test]
fn payload_container_items_preserve_explicit_nullable_value_contract() {
    for (container, key) in [("Map<string,string>", "value"), ("[string]", "inner")] {
        let field: crate::schema::PayloadField = serde_json::from_value(serde_json::json!({
            "type":container, "nullable":false,
            "items":{"type":"string","nullable":true}
        })).unwrap();
        let shape = serde_json::to_value(payload_field_shape(&field)).unwrap();
        assert_eq!(shape[key]["kind"], "optional", "{container}: {shape}");
        assert_eq!(shape[key]["inner"]["kind"], "string");
        if key == "value" { assert_eq!(shape["key"]["kind"], "string"); }
    }
}

#[test]
fn explicit_nullable_and_additional_values_survive_schema_and_shape_readers() {
    let value = serde_json::json!({
        "type": "object", "nullable": false,
        "properties": {
            "id": {"type":"string", "required":false, "nullable":false},
            "token": {"type":"string", "required":true, "nullable":true}
        },
        "additionalProperties": {"type":"number", "nullable":true}
    });
    let field: FieldDefinition = serde_json::from_value(value).unwrap();
    let wire = serde_json::to_value(&field).unwrap();
    assert_eq!(wire["nullable"], false);
    assert_eq!(wire["additionalProperties"]["nullable"], true);
    let shape = serde_json::to_value(field_shape(&field)).unwrap();
    assert_eq!(shape["kind"], "struct");
    assert_eq!(shape["additional"]["kind"], "optional");
    assert_eq!(shape["additional"]["inner"]["kind"], "number");
    let fields = shape["fields"].as_array().unwrap();
    assert_eq!(fields[0]["required"], false);
    assert_eq!(fields[0]["fieldType"]["kind"], "string");
    assert_eq!(fields[1]["required"], true);
    assert_eq!(fields[1]["fieldType"]["kind"], "optional");
    let payload: crate::schema::PayloadField = serde_json::from_value(serde_json::json!({
        "name":"row", "type":"object", "nullable":false,
        "properties":[{"name":"id", "type":"string", "required":false, "nullable":false}],
        "additionalProperties":{"type":"number", "nullable":true}
    })).unwrap();
    let payload_wire = serde_json::to_value(&payload).unwrap();
    assert_eq!(payload_wire["nullable"], false);
    assert_eq!(payload_wire["additionalProperties"]["nullable"], true);
    let payload_shape = serde_json::to_value(payload_field_shape(&payload)).unwrap();
    assert_eq!(payload_shape["additional"]["kind"], "optional");
    let catchall = OirType::Struct { fields: Default::default(), additional: Some(Box::new(OirType::TypeRef { name: "Cell".into() })) };
    let closed = DeclaredShape::with_resolver(None, catchall, |name| (name == "Cell").then_some(OirType::String));
    assert_eq!(closed.named.get("Cell"), Some(&OirType::String));
    let mut variable = OirType::Struct { fields: Default::default(), additional: Some(Box::new(OirType::Var { name: "cell".into() })) };
    assert!(variable.substitute_type_vars(&std::collections::BTreeMap::from([("cell".into(), OirType::String)])));
    assert_eq!(serde_json::to_value(variable).unwrap()["additional"]["kind"], "string");
}

fn transition(
    from: OirTransitionFrom,
    event: &str,
    renders: Vec<AwaitRender>,
) -> OirTransition {
    OirTransition {
        from,
        to: "done".into(),
        event: event.into(),
        guard: None,
        effects: vec![],
        await_render: renders,
    }
}

fn render(trait_name: &str, variant: &str) -> AwaitRender {
    AwaitRender {
        trait_name: trait_name.into(),
        slot: "main".into(),
        skeleton: SkeletonNode::Shape {
            shape: SkeletonShape {
                variant: variant.into(),
                rows: Some(3),
                columns: None,
                fields: None,
            },
        },
    }
}

#[test]
fn resolved_schema_rows_read_each_transitions_await_render() {
    let list = serde_json::json!({ "shape": { "variant": "list", "rows": 3 } });
    let resolved = serde_json::json!({ "orbitals": [{ "traits": [{
        "name": "Browse",
        "stateMachine": { "transitions": [
            { "from": "idle", "event": "LOAD", "awaitRender": [{ "trait": "Browse", "slot": "main", "skeleton": list }] },
            { "from": ["a", "b"], "event": "SAVE", "awaitRender": [{ "trait": "Browse", "slot": "drawer", "skeleton": list }] },
            { "from": "*", "event": "REFRESH", "awaitRender": [{ "trait": "Browse", "slot": "main", "skeleton": list }] },
            { "from": "idle", "event": "CLOSE" },
            { "from": "idle", "event": "BROKEN", "awaitRender": [{ "slot": "main" }] }
        ] }
    }] }] });
    let rows = await_render_rows_from_resolved(&resolved);
    let keys: Vec<(&str, Option<Vec<String>>, &str)> = rows
        .iter()
        .map(|r| (r.event.as_str(), r.from.clone(), r.slot.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("LOAD", Some(vec!["idle".to_string()]), "main"),
            (
                "SAVE",
                Some(vec!["a".to_string(), "b".to_string()]),
                "drawer"
            ),
            ("REFRESH", None, "main"),
        ],
        "control: a transition with no predictions, or an unreadable one, adds no row"
    );
    assert!(rows.iter().all(|r| r.request_trait == "Browse"));
    assert!(await_render_rows_from_resolved(&serde_json::json!({})).is_empty());
}

#[test]
fn await_render_rows_key_each_prediction_by_the_dispatch_that_leads_to_it() {
    let mut module = OirModule::new("App", "1.0.0");
    let mut browse = OirTrait::new("Browse");
    browse.transitions = vec![
        transition(
            OirTransitionFrom::Single("idle".into()),
            "LOAD",
            vec![render("Browse", "list")],
        ),
        transition(
            OirTransitionFrom::Any,
            "SAVE",
            vec![render("Detail", "detail"), render("Browse", "list")],
        ),
        transition(OirTransitionFrom::Single("idle".into()), "CLOSE", vec![]),
    ];
    module.traits.push(browse);
    let rows = module.await_render_rows();
    let keys: Vec<(&str, &str, Option<Vec<String>>, &str)> = rows
        .iter()
        .map(|r| {
            (
                r.request_trait.as_str(),
                r.event.as_str(),
                r.from.clone(),
                r.trait_name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            ("Browse", "LOAD", Some(vec!["idle".to_string()]), "Browse"),
            ("Browse", "SAVE", None, "Detail"),
            ("Browse", "SAVE", None, "Browse"),
        ],
        "control: a transition with no prediction contributes no row"
    );
    let json = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(json["requestTrait"], "Browse");
    assert_eq!(json["trait"], "Browse");
    assert_eq!(json["skeleton"]["shape"]["variant"], "list");
}

#[test]
fn test_oir_module_new() {
    let module = OirModule::new("TestApp", "1.0.0");
    assert_eq!(module.name, "TestApp");
    assert_eq!(module.version, "1.0.0");
    assert!(module.entities.is_empty());
}

#[test]
fn test_oir_entity_new() {
    let entity = OirEntity::new("Task");
    assert_eq!(entity.name, "Task");
    assert_eq!(entity.collection, "task");
    assert_eq!(entity.persistence, OirPersistence::Persistent);
}

#[test]
fn test_oir_type_array() {
    let arr_type = OirType::array(OirType::String);
    assert!(matches!(arr_type, OirType::Array { .. }));
    assert_eq!(arr_type.inner_type(), Some(&OirType::String));
}

fn bare_field(field_type: FieldType) -> FieldDefinition {
    FieldDefinition {
        nullable: None,
        additional_properties: None,
        name: None,
        field_type,
        required: None,
        primary_key: None,
        unique: None,
        default: None,
        values: None,
        relation: None,
        items: None,
        properties: None,
        intrinsic: None,
        description: None,
        synonyms: None,
        mock: None,
        merged_from: None,
        projected_from: None,
        record: None,
        derived: None,
    }
}

#[test]
fn distinct_entities_keeps_first_of_each_name_in_order() {
    let mut m = OirModule::new("M", "1.0.0".to_string());
    let mut first = OirEntity::new("A");
    first.fields.push(OirField::new("x", OirType::String));
    m.entities.push(first);
    m.entities.push(OirEntity::new("B"));
    m.entities.push(OirEntity::new("A"));
    let names: Vec<_> = m.distinct_entities().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["A", "B"]);
    assert_eq!(m.distinct_entities()[0].fields.len(), 1);
}

#[test]
fn distinct_entities_leaves_unique_names_untouched() {
    let mut m = OirModule::new("M", "1.0.0".to_string());
    m.entities.push(OirEntity::new("A"));
    m.entities.push(OirEntity::new("B"));
    assert_eq!(m.distinct_entities().len(), 2);
    assert_eq!(m.entities.len(), 2);
}

#[test]
fn a_field_knob_shapes_as_a_field_ref_of_its_value_type() {
    let mut typed = bare_field(FieldType::Field);
    typed.record = Some("@entity".to_string());
    typed.items = Some(Box::new(bare_field(FieldType::String)));
    let entity = OirType::Reference { entity: "@entity".to_string() };
    assert_eq!(
        field_shape(&typed),
        OirType::FieldRef { record: Box::new(entity.clone()), value: Some(Box::new(OirType::String)) }
    );
    // control: a field knob that names any field has no value type.
    typed.items = None;
    assert_eq!(
        field_shape(&typed),
        OirType::FieldRef { record: Box::new(entity), value: None }
    );
    // a type variable record is a variable; an alias record is its shape.
    typed.record = Some("$r".to_string());
    assert_eq!(
        field_shape(&typed),
        OirType::FieldRef { record: Box::new(OirType::Var { name: "r".to_string() }), value: None }
    );
    let mut alias = bare_field(FieldType::Field);
    alias.record = Some("Point".to_string());
    alias.properties = Some(crate::collections::IndexMap::from([("x".to_string(), bare_field(FieldType::Number))]));
    let OirType::FieldRef { record, .. } = field_shape(&alias) else { panic!("a field reference") };
    assert!(matches!(*record, OirType::Struct { ref fields, .. } if fields.len() == 1 && fields[0].name == "x"));
    assert_eq!(serde_json::to_string(&FieldType::Field).unwrap(), "\"field\"");
    assert_eq!(serde_json::from_str::<FieldType>("\"field\"").unwrap(), FieldType::Field);
    // control: the neighbouring name-carrying tag keeps its own spelling.
    assert_eq!(serde_json::to_string(&FieldType::Event).unwrap(), "\"event\"");
}

#[test]
fn a_bare_payload_object_has_a_record_shape_while_json_stays_unconstrained() {
    let object: crate::schema::types::PayloadField = serde_json::from_value(serde_json::json!({"name": "data", "type": "object"})).unwrap();
    let json: crate::schema::types::PayloadField = serde_json::from_value(serde_json::json!({"name": "data", "type": "json"})).unwrap();
    assert_eq!(payload_field_shape(&object), OirType::Struct { fields: Default::default(), additional: None });
    assert_eq!(payload_field_shape(&json), OirType::Json);
}

#[test]
fn field_shape_retains_closed_string_and_enum_values_through_nullable_containers() {
    let expected = OirType::enum_type("\"ready\" | \"done\"");
    for kind in [FieldType::String, FieldType::Enum] {
        let mut field = bare_field(kind);
        field.values = Some(vec!["ready".into(), "done".into()]);
        field.nullable = Some(false);
        assert_eq!(field_shape(&field), expected);
        let OirType::Enum { name } = field_shape(&field) else { panic!("closed values retained") };
        let members = crate::type_compat::literal_union_members(&name).unwrap();
        assert!(members.iter().any(|member| member == "ready"));
        assert!(!members.iter().any(|member| member == "wrong"));
        field.nullable = Some(true);
        let nullable = OirType::optional(expected.clone());
        assert_eq!(field_shape(&field), nullable);
        let mut map = bare_field(FieldType::Object);
        map.items = Some(Box::new(field.clone()));
        assert_eq!(field_shape(&map), OirType::map(OirType::String, nullable.clone()));
        let mut record = bare_field(FieldType::Object);
        record.properties = Some(crate::collections::IndexMap::new());
        record.additional_properties = Some(Box::new(field));
        assert_eq!(field_shape(&record), OirType::Struct { fields: Default::default(), additional: Some(Box::new(nullable)) });
    }
    assert_eq!(field_shape(&bare_field(FieldType::String)), OirType::String);
    assert_eq!(field_shape(&bare_field(FieldType::Enum)), OirType::enum_type("Enum"));
}

#[test]
fn field_shape_primitive() {
    assert_eq!(field_shape(&bare_field(FieldType::String)), OirType::String);
    assert_eq!(field_shape(&bare_field(FieldType::Number)), OirType::Number);
    assert_eq!(field_shape(&bare_field(FieldType::Money)), OirType::Money);
}

#[test]
fn field_shape_int_is_integer_and_number_stays_number() {
    assert_eq!(field_shape(&bare_field(FieldType::Int)), OirType::Integer);
    let mut nullable = bare_field(FieldType::Int);
    nullable.nullable = Some(true);
    assert_eq!(field_shape(&nullable), OirType::optional(OirType::Integer));
    assert_eq!(field_shape(&bare_field(FieldType::Number)), OirType::Number);
}

#[test]
fn field_shape_array_recurses_into_items() {
    let mut f = bare_field(FieldType::Array);
    f.items = Some(Box::new(bare_field(FieldType::String)));
    assert_eq!(
        field_shape(&f),
        OirType::Array {
            inner: Box::new(OirType::String)
        }
    );
}

#[test]
fn field_shape_bare_array_is_array_of_json() {
    // No `items` at all — the honest "no element information" shape.
    assert_eq!(
        field_shape(&bare_field(FieldType::Array)),
        OirType::Array {
            inner: Box::new(OirType::Json)
        }
    );
}

#[test]
fn field_shape_object_with_properties_is_the_struct_of_them() {
    let mut f = bare_field(FieldType::Object);
    let mut props = crate::collections::IndexMap::new();
    props.insert("label".to_string(), bare_field(FieldType::String));
    props.insert("count".to_string(), bare_field(FieldType::Number));
    f.properties = Some(props);
    let OirType::Struct { fields, .. } = field_shape(&f) else {
        panic!("an object with properties is a struct");
    };
    let named: Vec<(&str, &OirType)> = fields.iter().map(|f| (f.name.as_str(), &f.field_type)).collect();
    assert_eq!(named, vec![("count", &OirType::Number), ("label", &OirType::String)]);
}

#[test]
fn field_shape_object_with_items_is_a_map_of_the_item_type() {
    let mut f = bare_field(FieldType::Object);
    f.items = Some(Box::new(bare_field(FieldType::Number)));
    assert_eq!(field_shape(&f), OirType::map(OirType::String, OirType::Number));
}

#[test]
fn field_shape_keeps_a_recursive_back_reference_as_the_type_it_names() {
    // `type Node = { children : [Node] }`: the inner `Node` is the by-name form.
    let mut back = bare_field(FieldType::Union);
    back.values = Some(vec!["Node".to_string()]);
    let mut children = bare_field(FieldType::Array);
    children.items = Some(Box::new(back.clone()));
    assert_eq!(back.by_name_reference(), Some("Node"));
    assert_eq!(
        field_shape(&children),
        OirType::array(OirType::TypeRef { name: "Node".to_string() })
    );
}

#[test]
fn field_shape_of_a_union_carries_its_variants_sorted() {
    let mut union = bare_field(FieldType::Union);
    let mut props = crate::collections::IndexMap::new();
    props.insert("string".to_string(), bare_field(FieldType::String));
    props.insert("number".to_string(), bare_field(FieldType::Number));
    union.properties = Some(props);
    let OirType::Union { variants } = field_shape(&union) else {
        panic!("a union with variants is a union");
    };
    let named: Vec<(&str, &OirType)> = variants.iter().map(|v| (v.name.as_str(), &v.field_type)).collect();
    assert_eq!(named, vec![("number", &OirType::Number), ("string", &OirType::String)]);
    assert_eq!(field_shape(&bare_field(FieldType::Union)), OirType::Json, "no variants, no shape");
}

#[test]
fn a_union_with_variant_shapes_is_not_a_by_name_reference() {
    let mut tagged = bare_field(FieldType::Union);
    tagged.values = Some(vec!["A".to_string()]);
    let mut props = crate::collections::IndexMap::new();
    props.insert("A".to_string(), bare_field(FieldType::Object));
    tagged.properties = Some(props);
    assert_eq!(tagged.by_name_reference(), None);
    assert_eq!(bare_field(FieldType::Union).by_name_reference(), None);
    let mut two = bare_field(FieldType::Union);
    two.values = Some(vec!["A".to_string(), "B".to_string()]);
    assert_eq!(two.by_name_reference(), None, "a name list is not one reference");
}

#[test]
fn field_shape_relation_with_definition_is_reference() {
    let mut f = bare_field(FieldType::Relation);
    f.relation = Some(crate::schema::types::RelationDefinition {
        entity: "Article".to_string(),
        entity_id: None,
        cardinality: None,
        on_delete: None,
    });
    assert_eq!(
        field_shape(&f),
        OirType::Reference {
            entity: "Article".to_string()
        }
    );
}

fn relation_field(cardinality: Option<crate::schema::types::Cardinality>) -> FieldDefinition {
    let mut f = bare_field(FieldType::Relation);
    f.relation = Some(crate::schema::types::RelationDefinition {
        entity: "Revision".to_string(),
        entity_id: None,
        cardinality,
        on_delete: None,
    });
    f
}

#[test]
fn field_shape_to_many_relation_is_array_of_reference() {
    use crate::schema::types::Cardinality::*;
    for c in [Many, OneToMany, ManyToMany] {
        assert_eq!(
            field_shape(&relation_field(Some(c))),
            OirType::array(OirType::reference("Revision"))
        );
    }
}

#[test]
fn field_shape_intrinsic_to_many_relation_is_array_of_row() {
    use crate::schema::types::Cardinality::*;
    for c in [Many, OneToMany, ManyToMany] {
        let mut f = relation_field(Some(c));
        f.intrinsic = Some(true);
        assert_eq!(field_shape(&f), OirType::array(OirType::row("Revision")));
    }
}

#[test]
fn field_shape_explicitly_non_intrinsic_to_many_relation_stays_ids() {
    let mut f = relation_field(Some(crate::schema::types::Cardinality::OneToMany));
    f.intrinsic = Some(false);
    assert_eq!(
        field_shape(&f),
        OirType::array(OirType::reference("Revision"))
    );
}

/// `loadedRow : RecordItem @intrinsic` holds the fetched row (LOLO reference), not its id.
#[test]
fn field_shape_intrinsic_to_one_relation_is_a_row() {
    let mut f = relation_field(Some(crate::schema::types::Cardinality::ManyToOne));
    f.intrinsic = Some(true);
    assert_eq!(field_shape(&f), OirType::row("Revision"));
}

#[test]
fn row_serializes_under_its_own_kind() {
    let v = serde_json::to_value(OirType::row("Revision")).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "kind": "row", "entity": "Revision" })
    );
    assert_ne!(
        v,
        serde_json::to_value(OirType::reference("Revision")).unwrap()
    );
}

#[test]
fn field_shape_to_one_relation_stays_reference() {
    use crate::schema::types::Cardinality::*;
    for c in [None, Some(One), Some(ManyToOne)] {
        assert_eq!(
            field_shape(&relation_field(c)),
            OirType::reference("Revision")
        );
    }
}

#[test]
fn field_shape_relation_without_definition_degrades_to_json() {
    assert_eq!(field_shape(&bare_field(FieldType::Relation)), OirType::Json);
}

#[test]
fn test_oir_value_conversions() {
    let v: OirValue = true.into();
    assert_eq!(v.as_bool(), Some(true));

    let v: OirValue = 42.0.into();
    assert_eq!(v.as_number(), Some(42.0));

    let v: OirValue = "hello".into();
    assert_eq!(v.as_str(), Some("hello"));
}

#[test]
fn test_transition_from_matches() {
    let single = OirTransitionFrom::Single("idle".to_string());
    assert!(single.matches("idle"));
    assert!(!single.matches("running"));

    let multiple = OirTransitionFrom::Multiple(vec!["a".to_string(), "b".to_string()]);
    assert!(multiple.matches("a"));
    assert!(multiple.matches("b"));
    assert!(!multiple.matches("c"));

    let any = OirTransitionFrom::Any;
    assert!(any.matches("anything"));
}

#[test]
fn test_oir_entity_shared_round_trips_true() {
    let mut entity = OirEntity::new("Session");
    entity.shared = true;

    let serialized = serde_json::to_string(&entity).unwrap();
    assert!(serialized.contains("\"shared\":true"));

    let deserialized: OirEntity = serde_json::from_str(&serialized).unwrap();
    assert!(deserialized.shared);
}

#[test]
fn test_oir_entity_shared_defaults_false_when_absent() {
    let json = r#"{
            "name": "Task",
            "collection": "task",
            "persistence": "persistent",
            "fields": []
        }"#;
    let entity: OirEntity = serde_json::from_str(json).unwrap();
    assert!(!entity.shared);
}

#[test]
fn test_oir_entity_shared_false_omitted_from_serialization() {
    let entity = OirEntity::new("Task");
    assert!(!entity.shared);
    let serialized = serde_json::to_string(&entity).unwrap();
    assert!(!serialized.contains("shared"));
}

#[test]
fn test_oir_field_intrinsic_round_trips_true() {
    let mut field = OirField::new("viewState", OirType::String);
    field.intrinsic = true;
    let serialized = serde_json::to_string(&field).unwrap();
    assert!(serialized.contains("\"intrinsic\":true"));
    let deserialized: OirField = serde_json::from_str(&serialized).unwrap();
    assert!(deserialized.intrinsic);
}

#[test]
fn test_oir_field_intrinsic_defaults_false_when_absent() {
    let json = r#"{ "name": "x", "fieldType": { "kind": "string" } }"#;
    let field: OirField = serde_json::from_str(json).unwrap();
    assert!(!field.intrinsic);
}

#[test]
fn test_oir_field_intrinsic_false_omitted_from_serialization() {
    let field = OirField::new("x", OirType::String);
    let serialized = serde_json::to_string(&field).unwrap();
    assert!(!serialized.contains("intrinsic"));
}

#[test]
fn test_oir_entity_frame_fields_lists_only_intrinsic_names() {
    let mut entity = OirEntity::new("Wizard");
    entity.fields.push(OirField::new("id", OirType::String));
    let mut frame_field = OirField::new("currentStepIndex", OirType::Integer);
    frame_field.intrinsic = true;
    entity.fields.push(frame_field);
    assert_eq!(entity.frame_fields(), vec!["currentStepIndex"]);
}

#[test]
fn test_oir_entity_required_create_field_names_excludes_stamped_intrinsic_and_defaulted() {
    let mut entity = OirEntity::new("Note");
    entity.fields.push(OirField::new("id", OirType::String)); // FRAMEWORK_STAMPED_FIELDS
    entity
        .fields
        .push(OirField::new("content", OirType::String)); // the real required column
    entity
        .fields
        .push(OirField::optional("summary", OirType::String)); // not required at all
    let mut frame_field = OirField::new("workingCopy", OirType::String);
    frame_field.intrinsic = true;
    entity.fields.push(frame_field); // required but @intrinsic
    let mut defaulted = OirField::new("priority", OirType::String);
    defaulted.default = Some(OirValue::String("normal".to_string()));
    entity.fields.push(defaulted); // required but declares a default
    assert_eq!(entity.required_create_field_names(), vec!["content"]);
}

#[test]
fn test_oir_entity_required_create_field_names_excludes_merged_from() {
    // ChatMessage.content is host-authored and required; ChatMessage.name
    // was added by the inline phase's rebind merge from BrowseItem and
    // must NOT count as a required create column — BrowseItem's `!` is
    // its own write contract, never ChatMessage's.
    let mut entity = OirEntity::new("ChatMessage");
    entity
        .fields
        .push(OirField::new("content", OirType::String));
    let mut merged = OirField::new("name", OirType::String);
    merged.merged_from = Some("BrowseItem".to_string());
    entity.fields.push(merged);
    assert_eq!(entity.required_create_field_names(), vec!["content"]);
}

#[test]
fn test_oir_trait_dispatch_round_trips_declared_mode() {
    let mut t = OirTrait::new("Toggle");
    t.dispatch = Some(crate::schema::types::DispatchMode::Client);
    let serialized = serde_json::to_string(&t).unwrap();
    assert!(serialized.contains("\"dispatch\":\"client\""));
    let deserialized: OirTrait = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.dispatch, Some(crate::schema::types::DispatchMode::Client));
}

#[test]
fn test_oir_trait_dispatch_defaults_absent() {
    let t = OirTrait::new("Toggle");
    assert_eq!(t.dispatch, None);
    let serialized = serde_json::to_string(&t).unwrap();
    assert!(!serialized.contains("\"dispatch\""));
    let deserialized: OirTrait = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.dispatch, None);
}

#[test]
fn test_dispatch_mode_runtime_entity_without_declaration_is_server() {
    let mut entity = OirEntity::new("Cursor");
    entity.persistence = OirPersistence::Runtime;
    let t = OirTrait::new("Move");
    assert_eq!(
        t.dispatch_mode(Some(&entity), &crate::runs_on::Residence::none()),
        OirDispatchMode::PersistedAwaited
    );
}

#[test]
fn test_dispatch_mode_client_is_client_only_whatever_the_entity() {
    let mut runtime = OirEntity::new("Cursor");
    runtime.persistence = OirPersistence::Runtime;
    let persistent = OirEntity::new("Draft");
    let mut t = OirTrait::new("Move");
    t.dispatch = Some(crate::schema::types::DispatchMode::Client);
    for entity in [Some(&runtime), Some(&persistent), None] {
        assert_eq!(
            t.dispatch_mode(entity, &crate::runs_on::Residence::none()),
            OirDispatchMode::HybridClientOnly
        );
    }
}

#[test]
fn test_dispatch_mode_optimistic_is_declared() {
    let entity = OirEntity::new("Draft");
    let mut t = OirTrait::new("Edit");
    t.dispatch = Some(crate::schema::types::DispatchMode::Optimistic);
    assert_eq!(
        t.dispatch_mode(Some(&entity), &crate::runs_on::Residence::none()),
        OirDispatchMode::RuntimeOptimistic
    );
}

#[test]
fn test_dispatch_mode_persisted_awaited_by_default() {
    let entity = OirEntity::new("Order");
    let t = OirTrait::new("Checkout");
    assert_eq!(
        t.dispatch_mode(Some(&entity), &crate::runs_on::Residence::none()),
        OirDispatchMode::PersistedAwaited
    );
    assert_eq!(
        t.dispatch_mode(None, &crate::runs_on::Residence::none()),
        OirDispatchMode::PersistedAwaited
    );
}

#[test]
fn optional_constructor_is_idempotent() {
    let once = OirType::optional(OirType::String);
    let twice = OirType::optional(once.clone());
    assert_eq!(once, twice);
    assert_eq!(
        twice,
        OirType::Optional {
            inner: Box::new(OirType::String)
        }
    );
}

#[test]
fn effective_type_unwraps_exactly_one_optional_layer() {
    // Via the `OirField::optional` constructor (required:false + Optional wrapper).
    let via_constructor = OirField::optional("x", OirType::String);
    assert_eq!(via_constructor.effective_type(), (&OirType::String, true));

    // Via a struct literal recording optionality only as `required:false`
    // (the shape an event-payload field carries).
    let via_required_flag = OirField {
        declared: None,
        field_type: OirType::String,
        required: false,
        ..OirField::new("x", OirType::String)
    };
    assert_eq!(via_required_flag.effective_type(), (&OirType::String, true));

    let required = OirField::new("x", OirType::String);
    assert_eq!(required.effective_type(), (&OirType::String, false));
}
