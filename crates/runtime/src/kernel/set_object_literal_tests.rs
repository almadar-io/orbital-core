//! `set` evaluates the members of an object literal; a literal with none is stored as written.
use super::*;
use crate::entity_source::InMemoryEntityCache;

fn row(effect: serde_json::Value) -> Value {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "SetObject",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Page",
            "entity": { "name": "View", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Holder",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [effect] }]
                }
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    let payload: Value =
        serde_json::from_value(serde_json::json!({ "data": { "id": "abc", "n": 3 } })).unwrap();
    let outcomes = kernel.dispatch("Holder", None, "GO", payload).unwrap();
    outcomes
        .into_iter()
        .find(|(t, _)| t == "Holder")
        .expect("Holder ran")
        .1
        .entity_data
        .to_value()
}

fn prof(row: &Value) -> Value {
    row.as_object()
        .and_then(|o| o.get("prof"))
        .cloned()
        .expect("prof is set")
}

#[test]
fn a_literal_with_computed_members_is_stored_evaluated() {
    let stored = prof(&row(serde_json::json!(["set", "@entity.prof", {
        "id": ["object/get", "@payload.data", "id"],
        "n": ["object/get", "@payload.data", "n"],
        "label": ["str/concat", "p-", ["object/get", "@payload.data", "id"]],
        "nested": { "again": ["object/get", "@payload.data", "n"] }
    }])));
    let expected: Value = serde_json::from_value(serde_json::json!({
        "id": "abc", "n": 3, "label": "p-abc", "nested": { "again": 3 }
    }))
    .unwrap();
    assert_eq!(stored, expected);
}

#[test]
fn control_a_literal_with_no_computed_member_is_stored_as_written() {
    let stored = prof(&row(
        serde_json::json!(["set", "@entity.prof", { "id": "fixed", "n": 7, "tags": ["a", "b"] }]),
    ));
    let expected: Value = serde_json::from_value(
        serde_json::json!({ "id": "fixed", "n": 7, "tags": ["a", "b"] }),
    )
    .unwrap();
    assert_eq!(stored, expected);
}

#[test]
fn control_a_bound_member_resolves() {
    let stored = prof(&row(
        serde_json::json!(["set", "@entity.prof", { "id": "@payload.data.id" }]),
    ));
    let expected: Value = serde_json::from_value(serde_json::json!({ "id": "abc" })).unwrap();
    assert_eq!(stored, expected);
}

// G-ORBITAL-CORE-019: an unresolved bare binding reads as null, as every other read of it does.
#[test]
fn a_bare_binding_that_resolves_to_nothing_stores_null_not_its_text() {
    assert_eq!(
        prof(&row(serde_json::json!([
            "set",
            "@entity.prof",
            "@payload.status"
        ]))),
        Value::Null
    );
}

#[test]
fn control_a_bare_binding_that_resolves_stores_its_value() {
    assert_eq!(
        prof(&row(serde_json::json!([
            "set",
            "@entity.prof",
            "@payload.data.n"
        ]))),
        Value::from(3)
    );
}

#[test]
fn edge_text_after_a_binding_interpolates_it() {
    assert_eq!(
        prof(&row(serde_json::json!([
            "set",
            "@entity.prof",
            "@payload.data.id done"
        ]))),
        Value::String("abc done".into())
    );
}

#[test]
fn edge_a_plain_string_is_stored_as_written() {
    assert_eq!(
        prof(&row(serde_json::json!(["set", "@entity.prof", "open"]))),
        Value::String("open".into())
    );
}
