use orbital_core_base::schema::{parse_schema, EntityDefinition, EntityRef};
use serde_json::{json, Value};

fn schema(entity: Value) -> String {
    json!({"name": "PolicySchema", "orbitals": [{
        "name": "Records", "entity": entity, "traits": []
    }]}).to_string()
}

#[test]
fn unknown_entity_keys_fail_at_load_with_accepted_spellings() {
    for (key, accepted) in [("readPolicy", "read_policy"), ("persistense", "persistence")] {
        let mut entity = json!({"name": "Record", "fields": []});
        entity[key] = json!(true);
        let error = parse_schema(&schema(entity)).unwrap_err().to_string();
        assert!(error.contains("orbitals[0].entity"), "{error}");
        assert!(error.contains(&format!("unknown field `{key}`")), "{error}");
        assert!(error.contains(accepted), "{error}");
    }
}

#[test]
fn policy_waivers_and_persistence_overrides_reject_unknown_keys() {
    for (entity, key, accepted) in [
        (json!({"name": "Record", "fields": [], "access_waivers": {"raed": "public"}}), "raed", "read"),
        (json!({"extends": "Base.entity", "persistense": "runtime"}), "persistense", "persistence"),
    ] {
        let error = parse_schema(&schema(entity)).unwrap_err().to_string();
        assert!(error.contains(&format!("unknown field `{key}`")), "{error}");
        assert!(error.contains(accepted), "{error}");
    }
}

#[test]
fn declared_policy_and_entity_keys_round_trip() {
    let value = json!({
        "name": "Record", "fields": [], "collection": "records", "persistence": "persistent",
        "store": "Store", "shared": true, "identity": true, "browser": true,
        "instances": [], "localeInstances": {"en": []},
        "read_policy": false, "create_policy": true, "update_policy": false, "delete_policy": true,
        "access_waivers": {"read": "public", "create": "external", "update": "external", "delete": "external"},
        "invariants": [true], "id": "record-node",
        "timestamps": true, "softDelete": false, "description": "Records",
        "visual_prompt": "A record", "assetRef": {"role": "record", "category": "document",
            "animations": ["idle"], "style": "vector", "variant": "blue", "dimension": "2d", "aspect": "1:1"}
    });
    let direct: EntityDefinition = serde_json::from_value(value.clone()).unwrap();
    assert!(direct.read_policy.is_some());
    assert_eq!(serde_json::to_value(direct).unwrap(), value);
    let loaded = parse_schema(&schema(value)).unwrap();
    assert!(loaded.orbitals[0].entity.as_inline().unwrap().read_policy.is_some());
}

#[test]
fn entity_reference_variants_preserve_wire_format() {
    for value in [
        json!("Base.entity"),
        json!({"extends": "Base.entity", "extendsId": "base-entity", "name": "Record",
            "fields": [], "persistence": "runtime", "collection": "records"}),
    ] {
        let entity: EntityRef = serde_json::from_value(value.clone()).unwrap();
        assert!(entity.is_reference());
        assert_eq!(serde_json::to_value(entity).unwrap(), value);
    }
}
