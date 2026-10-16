//! `persist batch` on the kernel: every op runs in order through the same
//! checks as the single forms; success emits `{ operations, completedCount,
//! totalCount }`, the first failure emits `{ error }` and stops. Twin of the
//! JS runtime's `EffectExecutor` / `ServerEffectHandlers` batch branch.

use orbital_core_base::effect_types::Effect;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn arm(event: &str, effects: Json) -> Json {
    json!({ "from": "idle", "to": "idle", "event": event, "effects": effects })
}

fn kernel() -> RuntimeKernel {
    let emit = json!({ "emit": { "success": "SAVED", "failure": "FAILED" } });
    let list = json!({ "emit": { "success": "LISTED" } });
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "persist-batch",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string", "required": true }, { "name": "status", "type": "string" }] },
            "traits": [{
                "name": "Notes", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        arm("SEED", json!([["persist", "batch", [
                            ["create", "Note", { "id": "a", "title": "A", "status": "new" }],
                            ["create", "Note", { "id": "b", "title": "B", "status": "new" }],
                            ["update", "Note", "a", { "status": "done" }],
                            ["delete", "Note", "b"]
                        ], emit]])),
                        arm("MAPPED", json!([["persist", "batch",
                            ["array/map", "@payload.ids", ["fn", "i", ["create", "Note", { "id": ["str/concat", "n-", "@i"], "title": "t" }]]],
                            emit]])),
                        arm("BAD_SECOND", json!([["persist", "batch", [
                            ["create", "Note", { "id": "ok", "title": "fine" }],
                            ["create", "Note", { "id": "nope" }],
                            ["create", "Note", { "id": "never", "title": "x" }]
                        ], emit]])),
                        arm("BAD_ACTION", json!([["persist", "batch", [["upsert", "Note", { "id": "z", "title": "z" }]], emit]])),
                        arm("NOT_ARRAY", json!([["persist", "batch", "@payload.nothing", emit]])),
                        arm("EMPTY", json!([["persist", "batch", [], emit]])),
                        arm("SILENT", json!([["persist", "batch", [["create", "Note", { "id": "q", "title": "q" }]]]])),
                        arm("LIST", json!([["fetch", "Note", list]]))
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

fn send(k: &RuntimeKernel, event: &str, payload: Json) -> Vec<(String, Json)> {
    k.dispatch(
        "Notes",
        None,
        event,
        serde_json::from_value::<Value>(payload).unwrap(),
    )
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

fn ids(k: &RuntimeKernel) -> Vec<String> {
    let out = send(k, "LIST", Json::Null);
    let mut ids: Vec<String> = out[0].1["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn every_op_runs_in_order_and_success_carries_the_summary() {
    let k = kernel();
    let out = send(&k, "SEED", Json::Null);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "SAVED");
    assert_eq!(out[0].1["completedCount"].as_f64(), Some(4.0));
    assert_eq!(out[0].1["totalCount"].as_f64(), Some(4.0));
    assert_eq!(out[0].1["operations"].as_array().unwrap().len(), 4);
    assert_eq!(out[0].1["operations"][2]["action"], json!("update"));
    assert_eq!(out[0].1["operations"][3]["deleted"], json!(true));
    assert_eq!(ids(&k), vec!["a".to_string()], "b was created then deleted");
}

#[test]
fn an_array_map_of_op_tuples_writes_one_row_per_element() {
    let k = kernel();
    let out = send(&k, "MAPPED", json!({ "ids": ["1", "2", "3"] }));
    assert_eq!(out[0].0, "SAVED", "{out:?}");
    assert_eq!(out[0].1["completedCount"].as_f64(), Some(3.0));
    assert_eq!(ids(&k), vec!["n-1", "n-2", "n-3"]);
}

#[test]
fn a_failing_op_fails_the_whole_batch_and_routes_failure() {
    let k = kernel();
    let out = send(&k, "BAD_SECOND", Json::Null);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "FAILED");
    let error = out[0].1["error"].as_str().unwrap();
    assert!(
        error.contains("[create, Note]") && error.contains("required"),
        "{error}"
    );
    assert!(
        ids(&k).is_empty(),
        "a batch is all or nothing (owner ruling Q2, 2026-10-13): the op before the failure did not land either"
    );
}

#[test]
fn an_unknown_action_fails_the_batch() {
    let out = send(&kernel(), "BAD_ACTION", Json::Null);
    assert_eq!(out[0].0, "FAILED");
    assert!(
        out[0].1["error"]
            .as_str()
            .unwrap()
            .contains("Unknown batch operation action: upsert"),
        "{out:?}"
    );
}

#[test]
fn ops_that_do_not_evaluate_to_an_array_fail_the_batch() {
    let out = send(&kernel(), "NOT_ARRAY", Json::Null);
    assert_eq!(out[0].0, "FAILED");
    assert!(
        out[0].1["error"].as_str().unwrap().contains("array"),
        "{out:?}"
    );
}

#[test]
fn an_empty_batch_fails_rather_than_reporting_success() {
    let out = send(&kernel(), "EMPTY", Json::Null);
    assert_eq!(out[0].0, "FAILED");
    assert!(
        out[0].1["error"].as_str().unwrap().contains("non-empty"),
        "{out:?}"
    );
}

#[test]
fn control_a_batch_that_declares_no_emit_fires_nothing_but_still_writes() {
    let k = kernel();
    assert!(send(&k, "SILENT", Json::Null).is_empty());
    assert_eq!(ids(&k), vec!["q".to_string()]);
}
