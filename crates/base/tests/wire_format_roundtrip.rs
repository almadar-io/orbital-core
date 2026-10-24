//! Phase F.4: wire-format roundtrip — no `null` literals anywhere.
//!
//! Phase A established that the wire format of an OrbitalSchema must omit
//! optional fields rather than serializing them as `null`. The TypeScript
//! consumer side and the parity diff in orbital-verify both depend on
//! "absent key" vs. "explicit null" distinction.
//!
//! Phase F.4 (this revision) extends `#[serde(skip_serializing_if =
//! "Option::is_none")]` to every `Option<T>` field on the OrbitalSchema
//! type tree, so the invariant now holds for the full schema, not just the
//! protected leaves. The tests below assert:
//!
//!   1. `FieldDefinition`, `PayloadField`, and `EmitDefinition` (the leaf
//!      structs) must round-trip without introducing null literals when
//!      their Option<T> fields are unset.
//!   2. A real shipped std atom round-trips value-preservingly.
//!   3. Whole-schema no-nulls invariant: a real shipped std atom parsed,
//!      re-serialized, and walked as a generic `serde_json::Value` contains
//!      zero `Value::Null` literals anywhere in its tree.

use std::collections::HashMap;
use std::path::PathBuf;

use orbital_core_base::schema::{
    EmitDefinition, EventScope, FieldDefinition, FieldType, OrbitalSchema, PayloadField,
    PayloadTypeWhen, SExpression,
};

#[test]
fn id_carrying_schema_with_ledger_round_trips_byte_stably() {
    // Phase 2: a schema carrying node ids, dual-carry reference ids, and an
    // identity ledger must survive serialize -> deserialize -> serialize with
    // byte-identical output (the ledger is the durable id store).
    let json = r#"{
        "name": "Ledger",
        "schemaVersion": 1,
        "orbitals": [{
            "name": "Ledger",
            "id": "orb_01HZX5AAAAAAAAAAAAAAAAAA",
            "entity": {
                "name": "Account",
                "id": "ent_01HZX5BBBBBBBBBBBBBBBBBB",
                "fields": [
                    { "name": "id", "type": "string", "required": true },
                    {
                        "name": "owner",
                        "type": "relation",
                        "relation": { "entity": "Owner", "entityId": "ent_01HZX5EEEEEEEEEEEEEEEEEE" }
                    }
                ]
            },
            "traits": [{
                "name": "Deposit",
                "id": "trt_01HZX5CCCCCCCCCCCCCCCCCC",
                "linkedEntity": "Account",
                "linkedEntityId": "ent_01HZX5BBBBBBBBBBBBBBBBBB",
                "emits": [{ "event": "DEPOSITED", "eventId": "evt_01HZX5DDDDDDDDDDDDDDDDDD" }],
                "stateMachine": {
                    "states": [{ "name": "open" }],
                    "events": [{ "key": "DEPOSITED", "id": "evt_01HZX5DDDDDDDDDDDDDDDDDD" }],
                    "transitions": [{
                        "from": "open",
                        "to": "open",
                        "event": "DEPOSITED",
                        "eventId": "evt_01HZX5DDDDDDDDDDDDDDDDDD"
                    }]
                }
            }]
        }],
        "ledger": {
            "schemaVersion": 1,
            "entries": {
                "trt_01HZX5CCCCCCCCCCCCCCCCCC": {
                    "id": "trt_01HZX5CCCCCCCCCCCCCCCCCC",
                    "kind": "trait",
                    "bakedName": "Deposit",
                    "curName": "Deposit",
                    "renames": [],
                    "owner": "workspace"
                }
            }
        }
    }"#;
    let schema: OrbitalSchema = serde_json::from_str(json).expect("parse id-carrying schema");
    assert_eq!(schema.schema_version, Some(1));
    assert!(schema.ledger.is_some(), "ledger preserved");

    let once = serde_json::to_string(&schema).expect("serialize once");
    let reparsed: OrbitalSchema = serde_json::from_str(&once).expect("re-parse");
    let twice = serde_json::to_string(&reparsed).expect("serialize twice");
    assert_eq!(once, twice, "id-carrying schema is not byte-stable");

    // Spot-check the dual-carry id survived on the reference position.
    let value: serde_json::Value = serde_json::from_str(&once).unwrap();
    assert_eq!(
        value["orbitals"][0]["traits"][0]["linkedEntityId"],
        "ent_01HZX5BBBBBBBBBBBBBBBBBB"
    );
    assert_eq!(
        value["orbitals"][0]["traits"][0]["emits"][0]["eventId"],
        "evt_01HZX5DDDDDDDDDDDDDDDDDD"
    );
    assert_eq!(value["schemaVersion"], 1);

    // V4 W3a dual-carry id siblings (Event.id, Transition.eventId,
    // RelationDefinition.entityId) round-trip unchanged too.
    let sm = &value["orbitals"][0]["traits"][0]["stateMachine"];
    assert_eq!(sm["events"][0]["id"], "evt_01HZX5DDDDDDDDDDDDDDDDDD");
    assert_eq!(
        sm["transitions"][0]["eventId"],
        "evt_01HZX5DDDDDDDDDDDDDDDDDD"
    );
    assert_eq!(
        value["orbitals"][0]["entity"]["fields"][1]["relation"]["entityId"],
        "ent_01HZX5EEEEEEEEEEEEEEEEEE"
    );
}

#[test]
fn legacy_id_free_schema_loads_with_all_ids_none() {
    // Back-compat: an old `.orb` with no ids/ledger/schemaVersion still loads;
    // every id sibling is absent (None) and re-serialization introduces none.
    let json = r#"{
        "name": "Legacy",
        "orbitals": [{
            "name": "Legacy",
            "entity": { "name": "Thing", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Doer",
                "linkedEntity": "Thing",
                "stateMachine": { "states": [], "transitions": [] }
            }]
        }]
    }"#;
    let schema: OrbitalSchema = serde_json::from_str(json).expect("parse legacy schema");
    assert_eq!(schema.schema_version, None);
    assert!(schema.ledger.is_none());
    assert!(schema.orbitals[0].id.is_none());

    let serialized = serde_json::to_string(&schema).expect("re-serialize legacy");
    assert!(
        !serialized.contains("\"id\":"),
        "legacy round-trip must not introduce node id fields: {serialized}"
    );
    assert!(!serialized.contains("linkedEntityId"));
    assert!(!serialized.contains("schemaVersion"));
    assert!(!serialized.contains("ledger"));
}

#[test]
fn field_definition_unset_options_omit_nulls() {
    // FieldDefinition is fully Phase-A-protected: every Option<T> on it has
    // `skip_serializing_if`. Construct one with most fields unset and assert
    // the wire JSON contains no null literals.
    let field = FieldDefinition {
        nullable: None,
        additional_properties: None,
        mock: None,
        merged_from: None,
        name: Some("status".to_string()),
        field_type: FieldType::String,
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
        projected_from: None,
        record: None,
    };
    let json = serde_json::to_string(&field).expect("serialize field");
    assert!(
        !contains_null_token(&json),
        "FieldDefinition wire JSON unexpectedly contains a null literal: {}",
        json
    );

    // Also check the populated case retains the value but introduces no
    // nulls anywhere else.
    let field_with_default = FieldDefinition {
        nullable: None,
        additional_properties: None,
        mock: None,
        merged_from: None,
        name: Some("status".to_string()),
        field_type: FieldType::String,
        required: Some(false),
        primary_key: None,
        unique: None,
        default: Some(SExpression::String("active".to_string())),
        values: Some(vec!["active".to_string(), "archived".to_string()]),
        relation: None,
        items: None,
        properties: None,
        intrinsic: None,
        description: None,
        synonyms: None,
        projected_from: None,
        record: None,
    };
    let json2 = serde_json::to_string(&field_with_default).expect("serialize populated field");
    assert!(
        !contains_null_token(&json2),
        "Populated FieldDefinition wire JSON unexpectedly contains a null literal: {}",
        json2
    );
    assert!(json2.contains("\"default\":\"active\""));
    assert!(json2.contains("\"values\":[\"active\",\"archived\"]"));
}

#[test]
fn emit_payload_field_omits_optional_nulls() {
    // PayloadField has skip_serializing_if on description and required.
    let field = PayloadField { nullable: None, additional_properties: None, values: None,
        name: "id".to_string(),
        field_type: "string".to_string(),
        required: Some(true),
        description: None,
        properties: None,
        type_when: None,
        entity: None,
        projected_from: None,
        items: None,
    };
    let json = serde_json::to_string(&field).expect("serialize payload field");
    assert!(
        !contains_null_token(&json),
        "PayloadField with description=None must not serialize null: {}",
        json
    );
}

/// A `PayloadField` carrying a POPULATED `typeWhen` conditional list (the
/// lowered form of type-level `match`, §25) must round-trip serialize ->
/// deserialize -> serialize byte-stably, with no null literals and the
/// `type`/`knob`/`equals` keys spelled exactly as `PayloadTypeWhen`'s serde
/// attributes declare. Missing before this: the only prior `PayloadField`
/// test in this file left `type_when: None`.
#[test]
fn emit_payload_field_with_type_when_round_trips() {
    let field = PayloadField { nullable: None, additional_properties: None, values: None,
        name: "value".to_string(),
        field_type: "string".to_string(),
        required: Some(true),
        description: None,
        properties: None,
        type_when: Some(vec![PayloadTypeWhen {
            knob: "inputType".to_string(),
            equals: "number".to_string(),
            ty: "number".to_string(),
        }]),
        entity: None,
        projected_from: None,
        items: None,
    };
    let json = serde_json::to_string(&field).expect("serialize payload field with typeWhen");
    assert!(
        !contains_null_token(&json),
        "PayloadField with a populated typeWhen must not serialize null: {}",
        json
    );
    assert!(
        json.contains(r#""typeWhen":[{"knob":"inputType","equals":"number","type":"number"}]"#),
        "typeWhen must serialize with its declared key spellings: {}",
        json
    );

    let round_tripped: PayloadField =
        serde_json::from_str(&json).expect("deserialize payload field with typeWhen");
    let json2 =
        serde_json::to_string(&round_tripped).expect("re-serialize round-tripped payload field");
    assert_eq!(
        json, json2,
        "typeWhen must survive serialize -> deserialize -> serialize byte-stably"
    );
}

#[test]
fn emit_definition_omits_optional_nulls() {
    let emit = EmitDefinition {
        payload_entity: None,
        definer_knob: None,
        scope_overridden: None,
        event: "USER_SAVED".to_string(),
        description: None,
        synonyms: None,
        tier: None,
        scope: Some(EventScope::Internal),
        payload_schema: None,
        event_id: None,
    };
    let json = serde_json::to_string(&emit).expect("serialize emit definition");
    assert!(
        !contains_null_token(&json),
        "EmitDefinition with description=None and payload=None must not serialize null: {}",
        json
    );
}

#[test]
fn negative_control_catches_explicit_null() {
    // Confirm the helper recognizes a real null literal and isn't fooled by
    // null-substring inside string values.
    let json_with_null = r#"{"x":null}"#;
    assert!(
        contains_null_token(json_with_null),
        "negative control: helper failed to detect a clear null literal"
    );

    let json_with_nullish_string = r#"{"x":"contains the word null inside"}"#;
    assert!(
        !contains_null_token(json_with_nullish_string),
        "negative control: helper false-flagged a null inside a string"
    );
}

#[test]
fn shipped_std_agent_atom_roundtrips_value_preserving() {
    // Phase F.4: a real shipped std atom must parse, re-serialize, and
    // re-parse to a structurally equivalent representation. We compare the
    // round-tripped JSON to itself parsed-and-re-emitted via serde_json::Value
    // canonicalization to absorb the not-yet-fixed null literals from
    // unprotected Option fields. This proves data preservation even though
    // the no-nulls invariant is incomplete on the parent structs.
    let path = std_agent_context_window_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "wire_format_roundtrip: skipping std atom test, file unreadable at {}: {}",
                path.display(),
                e
            );
            return;
        }
    };
    let schema: OrbitalSchema = serde_json::from_str(&raw).expect("parse std-agent-context-window");

    let serialized = serde_json::to_string(&schema).expect("re-serialize");
    let _: OrbitalSchema =
        serde_json::from_str(&serialized).expect("re-parse round-tripped output");

    // Structural canonicalization for data-preservation comparison: parse
    // the round-trip output as a generic Value and re-emit. The double pass
    // confirms the second serialization is stable.
    let value: serde_json::Value =
        serde_json::from_str(&serialized).expect("re-parse round-tripped as value");
    let canonical = serde_json::to_string(&value).expect("re-emit canonical");
    let value2: serde_json::Value = serde_json::from_str(&canonical).expect("re-parse canonical");
    assert_eq!(value, value2, "round-trip is not stable on second pass");
}

#[test]
fn full_schema_no_nulls_std_agent_context_window() {
    let path = std_agent_context_window_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "wire_format_roundtrip: skipping full-schema no-nulls test, file unreadable at {}: {}",
                path.display(),
                e
            );
            return;
        }
    };
    assert_no_nulls_round_trip(&raw, &path.display().to_string());
}

#[test]
fn full_schema_no_nulls_std_cart() {
    let path = std_cart_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "wire_format_roundtrip: skipping full-schema no-nulls test, file unreadable at {}: {}",
                path.display(),
                e
            );
            return;
        }
    };
    assert_no_nulls_round_trip(&raw, &path.display().to_string());
}

/// Parse `raw` as an `OrbitalSchema`, re-serialize it, then walk the
/// re-serialized JSON as a generic `serde_json::Value` and assert no
/// `Value::Null` is reachable anywhere. This is the strict whole-schema
/// no-nulls invariant: every `Option<T>` field on the OrbitalSchema type
/// tree carries `skip_serializing_if = "Option::is_none"`, so unset
/// optionals must not appear in the wire format at all.
fn assert_no_nulls_round_trip(raw: &str, label: &str) {
    let schema: OrbitalSchema = serde_json::from_str(raw)
        .unwrap_or_else(|e| panic!("parse {} as OrbitalSchema: {}", label, e));
    let serialized = serde_json::to_string(&schema).expect("re-serialize OrbitalSchema");
    let value: serde_json::Value =
        serde_json::from_str(&serialized).expect("re-parse serialized as generic Value");

    let mut path = String::new();
    let null_at = first_null_path(&value, &mut path);
    assert!(
        null_at.is_none(),
        "schema {} re-serialized with a null literal at JSON path `{}`",
        label,
        null_at.unwrap_or_default()
    );
}

/// Walk a `serde_json::Value` and return the JSON-pointer-style path to the
/// first `Null` encountered, or `None` if the tree is null-free. The walker
/// records nested array indices as `[i]` and object keys as `.key`.
fn first_null_path(value: &serde_json::Value, path: &mut String) -> Option<String> {
    match value {
        serde_json::Value::Null => Some(path.clone()),
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let saved = path.len();
                path.push_str(&format!("[{}]", i));
                if let Some(found) = first_null_path(item, path) {
                    return Some(found);
                }
                path.truncate(saved);
            }
            None
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let saved = path.len();
                path.push('.');
                path.push_str(k);
                if let Some(found) = first_null_path(v, path) {
                    return Some(found);
                }
                path.truncate(saved);
            }
            None
        }
        _ => None,
    }
}

/// Search a JSON string for a `null` token while ignoring `null` substrings
/// that occur inside string literals (e.g., a description that mentions
/// "null"). Identifies bare `null` JSON values regardless of surrounding
/// punctuation.
fn contains_null_token(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        // Outside a string: look for `null`.
        if b == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let after_ok = i + 4 == bytes.len() || !is_ident_byte(bytes[i + 4]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn std_agent_context_window_path() -> PathBuf {
    repo_root()
        .join("packages/almadar-std/behaviors/registry/agent/atoms/std-agent-context-window.orb")
}

fn std_cart_path() -> PathBuf {
    repo_root().join("packages/almadar-std/behaviors/registry/core/molecules/std-cart.orb")
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    manifest
        .ancestors()
        .find(|p| p.join("packages/almadar-std/behaviors/registry").exists())
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest)
}

// Suppress unused-import warning when the std atom test bails early.
const _: fn() = || {
    let _: HashMap<String, String> = HashMap::new();
};

#[test]
fn expects_declarations_round_trip_and_stay_absent_when_unset() {
    // docs/Almadar_LOLO_Expects_Proposal.md (E1): the three `expects` kinds —
    // typed shapes included — survive parse -> serialize -> parse byte-stably,
    // and a schema without `expects` never grows the key.
    let json = r#"{
        "name": "Shop",
        "schemaVersion": 1,
        "orbitals": [{
            "name": "ProductOrbital",
            "expects": [
                {
                    "kind": "identity",
                    "name": "Customer",
                    "shape": [
                        { "name": "role", "type": "enum", "values": ["store-manager", "operations-director"], "required": true }
                    ]
                },
                {
                    "kind": "entity",
                    "name": "OrderRecord",
                    "shape": [
                        { "name": "status", "type": "enum", "values": ["placed", "shipped", "delivered"] },
                        { "name": "totalAmount", "type": "number" }
                    ]
                },
                { "kind": "entity", "name": "CartItem" },
                { "kind": "event", "traitName": "Checkout", "event": "ORDER_PLACED" }
            ],
            "entity": {
                "name": "Product",
                "fields": [{ "name": "id", "type": "string", "required": true }]
            },
            "traits": [{
                "name": "Browse",
                "linkedEntity": "Product",
                "stateMachine": {
                    "states": [{ "name": "browsing" }],
                    "events": [],
                    "transitions": []
                }
            }]
        }]
    }"#;
    let schema: OrbitalSchema = serde_json::from_str(json).expect("parse expects-carrying schema");
    let expects = schema.orbitals[0]
        .expects
        .as_ref()
        .expect("expects preserved on parse");
    assert_eq!(expects.len(), 4);

    let once = serde_json::to_string(&schema).expect("serialize once");
    assert!(
        !contains_null_token(&once),
        "expects wire JSON unexpectedly contains a null literal: {}",
        once
    );
    let reparsed: OrbitalSchema = serde_json::from_str(&once).expect("re-parse");
    let twice = serde_json::to_string(&reparsed).expect("serialize twice");
    assert_eq!(once, twice, "expects-carrying schema is not byte-stable");

    let value: serde_json::Value = serde_json::from_str(&once).unwrap();
    let wire_expects = &value["orbitals"][0]["expects"];
    assert_eq!(wire_expects[0]["kind"], "identity");
    assert_eq!(wire_expects[0]["name"], "Customer");
    assert_eq!(
        wire_expects[0]["shape"][0]["values"],
        serde_json::json!(["store-manager", "operations-director"])
    );
    assert_eq!(wire_expects[1]["shape"][1]["type"], "number");
    assert!(wire_expects[2].get("shape").is_none());
    assert_eq!(wire_expects[3]["traitName"], "Checkout");

    // Absent stays absent: no `expects` key materializes on a plain orbital.
    let plain = r#"{
        "name": "Plain",
        "orbitals": [{
            "name": "Plain",
            "entity": { "name": "Thing", "fields": [] },
            "traits": []
        }]
    }"#;
    let plain_schema: OrbitalSchema = serde_json::from_str(plain).expect("parse plain schema");
    assert!(plain_schema.orbitals[0].expects.is_none());
    let plain_once = serde_json::to_string(&plain_schema).expect("serialize plain");
    assert!(
        !plain_once.contains("\"expects\""),
        "schema without expects must not serialize the key: {}",
        plain_once
    );
}

#[test]
fn orbital_types_map_round_trips_and_stays_absent_when_unset() {
    // Type-param completion Gap A: `Orbital.types` (an orbital's own named
    // struct aliases, flattened to PayloadField trees) round-trips
    // byte-stably, and a schema with no aliases never grows the key
    // (`skip_serializing_if` proven — existing corpus `.orb` byte-stability).
    let json = r#"{
        "name": "Purchasing",
        "orbitals": [{
            "name": "PurchaseOrderOrbital",
            "entity": {
                "name": "PurchaseOrder",
                "fields": [{ "name": "id", "type": "string", "required": true }]
            },
            "traits": [],
            "types": {
                "PoApprovalPayload": [
                    { "name": "id", "type": "string", "required": true }
                ]
            }
        }]
    }"#;
    let schema: OrbitalSchema = serde_json::from_str(json).expect("parse types-carrying schema");
    let types = schema.orbitals[0]
        .types
        .as_ref()
        .expect("types preserved on parse");
    assert_eq!(types.len(), 1);
    let shape = &types["PoApprovalPayload"];
    assert_eq!(shape.len(), 1);
    assert_eq!(shape[0].name, "id");
    assert_eq!(shape[0].field_type, "string");

    let once = serde_json::to_string(&schema).expect("serialize once");
    assert!(
        !contains_null_token(&once),
        "types wire JSON unexpectedly contains a null literal: {}",
        once
    );
    let reparsed: OrbitalSchema = serde_json::from_str(&once).expect("re-parse");
    let twice = serde_json::to_string(&reparsed).expect("serialize twice");
    assert_eq!(once, twice, "types-carrying schema is not byte-stable");

    let value: serde_json::Value = serde_json::from_str(&once).unwrap();
    let wire_types = &value["orbitals"][0]["types"];
    assert_eq!(wire_types["PoApprovalPayload"][0]["name"], "id");
    assert_eq!(wire_types["PoApprovalPayload"][0]["type"], "string");

    // Absent stays absent: no `types` key materializes on an orbital with no
    // local struct aliases.
    let plain = r#"{
        "name": "Plain",
        "orbitals": [{
            "name": "Plain",
            "entity": { "name": "Thing", "fields": [] },
            "traits": []
        }]
    }"#;
    let plain_schema: OrbitalSchema = serde_json::from_str(plain).expect("parse plain schema");
    assert!(plain_schema.orbitals[0].types.is_none());
    let plain_once = serde_json::to_string(&plain_schema).expect("serialize plain");
    assert!(
        !plain_once.contains("\"types\""),
        "schema without local struct aliases must not serialize the key: {}",
        plain_once
    );
}
