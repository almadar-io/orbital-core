// A persist's declared `emit.success` fires with the stored outcome: the row for create/update, `{ id, deleted }` for delete. Twin of almadar-runtime `persist-success-emit.test.ts`.

use orbital_core::effects::Effect;
use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn arm(event: &str, effects: Json) -> Json {
    json!({ "from": "idle", "to": "idle", "event": event, "effects": effects })
}

fn kernel() -> RuntimeKernel {
    let emit = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "persist-success",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "Notes", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        arm("CREATE", json!([["persist", "create", "Note", { "id": "n1", "title": "first" }, emit]])),
                        arm("UPDATE", json!([["persist", "update", "Note", { "id": "n1", "title": "second" }, emit]])),
                        arm("DELETE", json!([["persist", "delete", "Note", "n1", emit]])),
                        arm("SILENT", json!([["persist", "create", "Note", { "id": "n2", "title": "quiet" }]])),
                        arm("ANON", json!([["persist", "create", "Note", { "title": "no id" }, emit]]))
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    k
}

fn send(k: &RuntimeKernel, event: &str) -> Vec<(String, Json)> {
    k.dispatch("Notes", None, event, Value::Null)
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } => {
                Some((event.clone(), serde_json::to_value(payload).unwrap()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn create_fires_success_with_the_stored_row() {
    let out = send(&kernel(), "CREATE");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "SAVED");
    assert_eq!(out[0].1["id"], json!("n1"));
    assert_eq!(out[0].1["title"], json!("first"));
}

#[test]
fn update_fires_success_with_the_updated_row() {
    let k = kernel();
    send(&k, "CREATE");
    let out = send(&k, "UPDATE");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "SAVED");
    assert_eq!(out[0].1["title"], json!("second"));
}

#[test]
fn delete_fires_success_with_the_deleted_id() {
    let k = kernel();
    send(&k, "CREATE");
    assert_eq!(
        send(&k, "DELETE"),
        vec![("SAVED".to_string(), json!({ "id": "n1", "deleted": true }))]
    );
}

#[test]
fn control_a_persist_that_declares_no_emit_fires_nothing() {
    assert!(send(&kernel(), "SILENT").is_empty());
}

#[test]
fn an_id_less_create_mints_a_distinct_id_each_time() {
    let k = kernel();
    let a = send(&k, "ANON");
    let b = send(&k, "ANON");
    let ids = [a[0].1["id"].clone(), b[0].1["id"].clone()];
    assert!(
        ids.iter()
            .all(|id| id.as_str().is_some_and(|s| !s.is_empty())),
        "{ids:?}"
    );
    assert_ne!(ids[0], ids[1]);
}
