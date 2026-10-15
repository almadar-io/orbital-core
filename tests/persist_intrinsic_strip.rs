//! `@intrinsic` entity fields are trait-owned frame state, never a persisted column: a persist that
//! carries one (a bare `@entity`, or any data naming it) writes the row without it. JS twin:
//! `almadar-runtime/test/orbital-server-runtime-persist-intrinsic-strip.test.ts`; the compiled
//! server strips the same set (`OirEntity::frame_fields`).

use orbital_core::evaluator::ValueMap;
use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::json;
use std::sync::Arc;

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Message",
                "persistence": "persistent",
                "collection": "messages",
                "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "text", "type": "string" },
                    { "name": "draft", "type": "string", "intrinsic": true }
                ]
            },
            "traits": [{
                "name": "Composer",
                "linkedEntity": "Message",
                "stateMachine": {
                    "states": [{ "name": "editing", "isInitial": true }],
                    "transitions": [
                        { "from": "editing", "to": "editing", "event": "SEND", "effects": [
                            ["set", "@entity.id", "m1"],
                            ["set", "@entity.draft", "unsent"],
                            ["set", "@entity.text", "hello"],
                            ["persist", "create", "Message", "@entity"]
                        ] },
                        { "from": "editing", "to": "editing", "event": "SEND_LITERAL", "effects": [
                            ["persist", "create", "Message", { "id": "m2", "text": "lit", "draft": "scratch" }]
                        ] },
                        { "from": "editing", "to": "editing", "event": "EDIT", "effects": [
                            ["persist", "update", "Message", { "id": "m1", "text": "edited", "draft": "scratch" }]
                        ] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn run(events: &[&str]) -> Vec<Value> {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache.clone() as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema()).unwrap();
    for event in events {
        kernel
            .process_event(
                "Composer",
                Some("singleton"),
                event,
                Value::Object(ValueMap::new()),
            )
            .unwrap();
    }
    cache.list("Message").unwrap().iter().map(|r| (**r).clone()).collect()
}

fn has(row: &Value, field: &str) -> bool {
    row.as_object().is_some_and(|o| o.contains_key(field))
}

#[test]
fn a_bare_entity_persist_leaves_the_intrinsic_field_out_of_the_row() {
    let rows = run(&["SEND"]);
    assert_eq!(rows.len(), 1);
    assert!(has(&rows[0], "text"), "{rows:?}");
    assert!(!has(&rows[0], "draft"), "{rows:?}");
}

#[test]
fn a_literal_naming_the_intrinsic_field_is_stripped_the_same_way() {
    let rows = run(&["SEND_LITERAL"]);
    assert_eq!(rows.len(), 1);
    assert!(has(&rows[0], "text"), "{rows:?}");
    assert!(!has(&rows[0], "draft"), "{rows:?}");
}

#[test]
fn an_update_leaves_the_intrinsic_field_out_too() {
    let rows = run(&["SEND", "EDIT"]);
    let edited = rows.iter().find(|r| {
        r.as_object()
            .and_then(|o| o.get("text"))
            .is_some_and(|t| format!("{t:?}").contains("edited"))
    });
    assert!(edited.is_some(), "the update must land: {rows:?}");
    assert!(rows.iter().all(|r| !has(r, "draft")), "{rows:?}");
}

#[test]
fn control_a_declared_column_is_kept() {
    let rows = run(&["SEND"]);
    assert!(
        rows.iter().all(|r| has(r, "text") && has(r, "id")),
        "{rows:?}"
    );
}
