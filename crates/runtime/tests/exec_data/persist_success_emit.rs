// A persist's declared `emit.success` fires with the stored outcome: the row for create/update, `{ id, deleted }` for delete. Twin of almadar-runtime `persist-success-emit.test.ts`.

use orbital_core_base::effect_types::Effect;
use orbital_core_runtime::kernel::RuntimeKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

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
                        arm("DELETE_PICKED", json!([["persist", "delete", "Note", "@payload.id", emit]])),
                        arm("SILENT", json!([["persist", "create", "Note", { "id": "n2", "title": "quiet" }]])),
                        arm("ANON", json!([["persist", "create", "Note", { "title": "no id" }, emit]]))
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
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

#[test]
fn a_create_on_a_taken_id_fires_failure_with_the_duplicate_id_code() {
    let k = kernel();
    send(&k, "CREATE");
    let out = send(&k, "CREATE");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "SAVE_FAILED");
    assert_eq!(out[0].1["code"], json!("duplicate-id"), "{out:?}");
    assert!(out[0].1["error"].as_str().is_some_and(|e| e.contains("already exists")), "{out:?}");
}

#[test]
fn an_update_of_an_absent_row_fires_failure_with_the_not_found_code() {
    let out = send(&kernel(), "UPDATE");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "SAVE_FAILED");
    assert_eq!(out[0].1["code"], json!("not-found"), "{out:?}");
}

#[test]
fn control_a_success_payload_carries_no_code_field() {
    let out = send(&kernel(), "CREATE");
    assert!(out[0].1.get("code").is_none(), "{out:?}");
}

fn guarded_kernel() -> RuntimeKernel {
    let emit = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "persist-guarded",
        "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Note", "persistence": "persistent", "collection": "notes",
                "create_policy": ["=", "@entity.title", "allowed"],
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }]
            },
            "traits": [{
                "name": "Notes", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        arm("CREATE_OK", json!([["persist", "create", "Note", { "id": "n1", "title": "allowed" }, emit]])),
                        arm("CREATE_REFUSED", json!([["persist", "create", "Note", { "id": "n2", "title": "nope" }, emit]]))
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

#[test]
fn a_persist_refused_by_its_policy_fires_failure_with_the_denied_code_from_the_typed_refusal() {
    let k = guarded_kernel();
    let out = send(&k, "CREATE_REFUSED");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "SAVE_FAILED");
    assert_eq!(out[0].1["code"], json!("denied"), "{out:?}");
    let outcomes = k.dispatch("Notes", None, "CREATE_REFUSED", Value::Null).unwrap();
    let persist = outcomes[0].1.effect_results.iter().find(|r| matches!(r.effect, Effect::Persist { .. })).unwrap();
    assert_eq!(persist.code, Some(orbital_core_base::bus_event::ServerEffectCode::Denied));
}

#[test]
fn control_a_policy_that_admits_the_row_carries_no_code() {
    let out = send(&guarded_kernel(), "CREATE_OK");
    assert_eq!(out[0].0, "SAVED");
    assert!(out[0].1.get("code").is_none(), "{out:?}");
}

fn emit_results(k: &RuntimeKernel, event: &str) -> Vec<Json> {
    let outcomes = k.dispatch("Notes", None, event, Value::Null).unwrap();
    let response = orbital_core_runtime::effects::response::EventResponse::from_process_results(&outcomes, "Notes", event);
    response
        .effect_results
        .unwrap_or_default()
        .iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .filter(|r| r["effect"] == json!("emit"))
        .collect()
}

#[test]
fn the_settled_response_lists_each_emit_among_the_effect_results() {
    let out = emit_results(&kernel(), "CREATE");
    assert_eq!(out, vec![json!({ "effect": "emit", "success": true, "target": "SAVED" })]);
}

#[test]
fn control_an_effect_with_no_emit_lists_no_emit_result() {
    assert_eq!(emit_results(&kernel(), "SILENT"), Vec::<Json>::new());
}

fn send_with(k: &RuntimeKernel, event: &str, payload: Json) -> Vec<(String, Json)> {
    k.dispatch("Notes", None, event, serde_json::from_value(payload).unwrap())
        .unwrap()
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } => Some((event.clone(), serde_json::to_value(payload).unwrap())),
            _ => None,
        })
        .collect()
}

// The delete's success carries the id it deleted, never the binding it was declared with.
#[test]
fn a_delete_by_a_bound_id_fires_success_with_that_id() {
    let k = kernel();
    send(&k, "CREATE");
    let emits = send_with(&k, "DELETE_PICKED", json!({ "id": "n1" }));
    assert!(emits.contains(&("SAVED".to_string(), json!({ "id": "n1", "deleted": true }))), "{emits:?}");
}

