//! A Server-env kernel over a seeded `InMemoryEntityCache`: a `fetch` returns the seeded rows,
//! ordered and unordered.

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

fn run(entity_extra: serde_json::Value, fetch_cfg: serde_json::Value) -> usize {
    let mut entity = json!({ "name": "Ticket", "persistence": "persistent",
        "fields": [{ "name": "id", "type": "string" }, { "name": "score", "type": "number", "required": true }] });
    for (k, v) in entity_extra.as_object().unwrap() {
        entity[k] = v.clone();
    }
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "sf", "typeContractVersion": 1, "orbitals": [{ "name": "Main", "entity": entity,
            "traits": [{ "name": "T", "linkedEntity": "Ticket", "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "LIST",
                    "effects": [["fetch", "Ticket", fetch_cfg]] }] } }] }]
    }))
    .unwrap();
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache.clone() as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    // Seed AFTER registering: the cache keys its store by the entity's bound `collection`, which
    // `register_schema` binds. Rows written under the entity name earlier sit in a store the entity
    // no longer reads.
    for i in 0..3 {
        let row: Value =
            serde_json::from_value(json!({ "id": format!("t{i}"), "score": i })).unwrap();
        cache.write("Ticket", &format!("t{i}"), &row).unwrap();
    }
    let out = k.dispatch("T", None, "LIST", Value::Null).unwrap();
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| er.return_value.as_ref()?.as_array().map(|a| a.len()))
        .sum()
}

#[test]
fn unordered_fetch_returns_the_seeded_rows() {
    assert_eq!(run(json!({}), json!({ "emit": { "success": "L" } })), 3);
}

#[test]
fn ordered_fetch_returns_the_seeded_rows() {
    assert_eq!(
        run(
            json!({}),
            json!({ "orderBy": "score:desc", "emit": { "success": "L" } })
        ),
        3
    );
}

#[test]
fn collection_declared_entity_reads_the_entity_name_key() {
    assert_eq!(
        run(
            json!({ "collection": "tickets" }),
            json!({ "emit": { "success": "L" } })
        ),
        3
    );
}

/// The fetch's own result for a by-id miss: what the verifier's judge reads as handled or not (G-ORBITAL-CORE-035).
fn missed_fetch(fetch_cfg: serde_json::Value) -> orbital_core_base::effect_types::EffectResult {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "sf", "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Ticket", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "Ticket", "stateMachine": {
                "states": [{ "name": "loading", "isInitial": true }, { "name": "error" }],
                "transitions": [
                    { "from": "loading", "to": "loading", "event": "OPEN", "effects": [["fetch", "Ticket", fetch_cfg]] },
                    { "from": "loading", "to": "error", "event": "LOAD_FAILED" }
                ] } }] }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(RuntimeEnvironment::Server, Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>, Default::default());
    k.register_schema(schema).unwrap();
    let out = k.dispatch("T", None, "OPEN", Value::Null).unwrap();
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .find(|er| matches!(er.effect, orbital_core_base::effect_types::Effect::Fetch { .. }))
        .cloned()
        .expect("the fetch ran")
}

#[test]
fn a_missed_fetch_reports_the_failure_event_it_was_routed_to() {
    let result = missed_fetch(json!({ "id": "absent", "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }));
    assert!(result.is_failed(), "{result:?}");
    assert_eq!(result.routed_failure().as_deref(), Some("LOAD_FAILED"));
}

#[test]
fn control_a_missed_fetch_with_no_failure_route_reports_none() {
    let result = missed_fetch(json!({ "id": "absent", "emit": { "success": "LOADED" } }));
    assert!(result.is_failed(), "{result:?}");
    assert_eq!(result.routed_failure(), None);
}

/// The ids a `fetch` with `filter` returns over `rows`, in the fetch's order, for a LIST dispatched with `payload`.
fn fetched_ids(rows: serde_json::Value, filter: serde_json::Value, payload: serde_json::Value) -> Vec<String> {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "sf", "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Ticket", "persistence": "persistent", "fields": [
                { "name": "id", "type": "string" }, { "name": "projectId", "type": "string" },
                { "name": "status", "type": "string" }, { "name": "score", "type": "number", "required": true }] },
            "traits": [{ "name": "T", "linkedEntity": "Ticket", "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "LIST",
                    "effects": [["fetch", "Ticket", { "filter": filter, "emit": { "success": "L" } }]] }] } }] }]
    }))
    .unwrap();
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache.clone() as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    for row in rows.as_array().unwrap() {
        let id = row["id"].as_str().unwrap().to_string();
        let row: Value = serde_json::from_value(row.clone()).unwrap();
        cache.write("Ticket", &id, &row).unwrap();
    }
    let payload: Value = serde_json::from_value(payload).unwrap();
    let out = k.dispatch("T", None, "LIST", payload).unwrap();
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| er.return_value.as_ref()?.as_array().cloned())
        .flatten()
        .map(|row| row.get_path("id").and_then(|v| v.as_str().map(str::to_string)).unwrap())
        .collect()
}

fn tickets() -> serde_json::Value {
    json!([
        { "id": "t1", "projectId": "p1", "status": "open", "score": 1 },
        { "id": "t2", "projectId": "p1", "status": "done", "score": 2 },
        { "id": "t3", "projectId": "p2", "status": "open", "score": 3 },
        { "id": "t4", "projectId": "p2", "status": "done", "score": 4 }
    ])
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[test]
fn control_an_s_expression_filter_returns_only_the_matching_rows() {
    let filter = json!(["=", ["object/get", "@entity", "projectId"], "p1"]);
    assert_eq!(sorted(fetched_ids(tickets(), filter, json!({}))), ["t1", "t2"]);
}

#[test]
fn an_object_filter_returns_the_rows_the_s_expression_form_returns() {
    let object = fetched_ids(tickets(), json!({ "projectId": "p1" }), json!({}));
    let sexpr = fetched_ids(tickets(), json!(["=", ["object/get", "@entity", "projectId"], "p1"]), json!({}));
    assert_eq!(sorted(object.clone()), ["t1", "t2"]);
    assert_eq!(object, sexpr);
}

#[test]
fn an_object_filter_value_that_is_a_payload_binding_resolves_before_comparing() {
    let ids = fetched_ids(tickets(), json!({ "projectId": "@payload.id" }), json!({ "id": "p2" }));
    assert_eq!(sorted(ids), ["t3", "t4"]);
}

#[test]
fn an_object_filter_value_that_is_an_expression_is_evaluated_before_comparing() {
    let ids = fetched_ids(tickets(), json!({ "score": ["+", "@payload.n", 1] }), json!({ "n": 2 }));
    assert_eq!(ids, ["t3"]);
}

#[test]
fn an_object_filters_keys_and_together() {
    let ids = fetched_ids(tickets(), json!({ "projectId": "p2", "status": "open" }), json!({}));
    assert_eq!(ids, ["t3"]);
}

#[test]
fn an_object_filter_that_matches_nothing_returns_no_rows() {
    assert!(fetched_ids(tickets(), json!({ "projectId": "nope" }), json!({})).is_empty());
}

#[test]
fn an_object_filter_with_no_keys_is_the_empty_conjunction_and_admits_every_row() {
    assert_eq!(sorted(fetched_ids(tickets(), json!({}), json!({}))), ["t1", "t2", "t3", "t4"]);
}

/// The ids a `fetch` lists after a `persist create` of `row`, on a Server kernel that keeps `[runtime]` rows in the kernel.
fn persisted_then_fetched(persistence: &str, row: serde_json::Value) -> Vec<String> {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "rt", "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Card", "persistence": persistence, "fields": [
                { "name": "id", "type": "string" }, { "name": "label", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "Card", "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [
                    { "from": "idle", "to": "idle", "event": "SAVE", "effects": [["persist", "create", "Card", row]] },
                    { "from": "idle", "to": "idle", "event": "LIST", "effects": [["fetch", "Card", { "emit": { "success": "L" } }]] }] } }] }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        orbital_core_runtime::config::KernelConfig { runtime_rows_in_kernel: true, ..Default::default() },
    );
    k.register_schema(schema).unwrap();
    k.dispatch("T", None, "SAVE", Value::Null).unwrap();
    let out = k.dispatch("T", None, "LIST", Value::Null).unwrap();
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| er.return_value.as_ref()?.as_array().cloned())
        .flatten()
        .map(|row| row.get_path("id").and_then(|v| v.as_str().map(str::to_string)).unwrap())
        .collect()
}

#[test]
fn a_runtime_row_persisted_with_an_id_is_fetched() {
    assert_eq!(persisted_then_fetched("runtime", json!({ "id": "c1", "label": "x" })), ["c1"]);
}

#[test]
fn control_a_persistent_row_persisted_with_an_id_is_fetched() {
    assert_eq!(persisted_then_fetched("persistent", json!({ "id": "c1", "label": "x" })), ["c1"]);
}

#[test]
fn control_a_runtime_row_persisted_without_an_id_is_fetched() {
    assert_eq!(persisted_then_fetched("runtime", json!({ "label": "x" })).len(), 1);
}

/// The completion events a fetch dispatch fired, as `(event, payload)`, with the fetch declared by `fetch_cfg`
/// and OPEN dispatched with `payload` over one seeded row `t0`.
fn fetch_completions(fetch_cfg: serde_json::Value, payload: serde_json::Value) -> Vec<(String, serde_json::Value)> {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "sf", "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Ticket", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "Ticket", "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "OPEN", "effects": [["fetch", "Ticket", fetch_cfg]] }] } }] }]
    }))
    .unwrap();
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = RuntimeKernel::new(RuntimeEnvironment::Server, cache.clone() as Arc<dyn EntitySource>, Default::default());
    k.register_schema(schema).unwrap();
    cache.write("Ticket", "t0", &serde_json::from_value(json!({ "id": "t0" })).unwrap()).unwrap();
    let out = k.dispatch("T", None, "OPEN", serde_json::from_value(payload).unwrap()).unwrap();
    out.iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            orbital_core_base::effect_types::Effect::Emit { event, payload } => {
                Some((event.clone(), serde_json::to_value(payload).unwrap()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_keyed_fetch_echoes_its_key_in_the_success_payload() {
    let emitted = fetch_completions(
        json!({ "key": "http", "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }),
        json!({}),
    );
    assert_eq!(emitted.len(), 1, "{emitted:?}");
    assert_eq!(emitted[0].0, "LOADED");
    assert_eq!(emitted[0].1["key"], json!("http"));
    assert_eq!(emitted[0].1["data"], json!([{ "id": "t0" }]));
    assert_eq!(emitted[0].1["totalCount"], json!(1.0));
}

#[test]
fn a_keyed_fetch_echoes_its_key_in_the_failure_payload() {
    let emitted = fetch_completions(
        json!({ "id": "absent", "key": "picker", "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }),
        json!({}),
    );
    assert_eq!(emitted.len(), 1, "{emitted:?}");
    assert_eq!(emitted[0].0, "LOAD_FAILED");
    assert_eq!(emitted[0].1["key"], json!("picker"));
    assert_eq!(emitted[0].1["code"], json!("not-found"));
    assert!(emitted[0].1["error"].is_string());
}

#[test]
fn a_fetch_key_is_an_expression_evaluated_against_the_step() {
    let emitted = fetch_completions(
        json!({ "key": "@payload.kind", "emit": { "success": "LOADED" } }),
        json!({ "kind": "hook" }),
    );
    assert_eq!(emitted[0].1["key"], json!("hook"));
}

#[test]
fn control_an_unkeyed_fetch_payloads_are_unchanged() {
    let success = fetch_completions(json!({ "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }), json!({}));
    assert_eq!(success[0].1, json!({ "data": [{ "id": "t0" }], "totalCount": 1.0 }));
    let failure = fetch_completions(
        json!({ "id": "absent", "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }),
        json!({}),
    );
    let fields: Vec<&str> = failure[0].1.as_object().unwrap().keys().map(String::as_str).collect();
    assert!(!fields.contains(&"key"), "{fields:?}");
}

#[test]
fn edge_a_key_that_is_not_a_non_empty_string_fails_the_fetch_with_no_completion_row() {
    for key in [json!(""), json!(7)] {
        let emitted = fetch_completions(json!({ "key": key, "emit": { "success": "LOADED", "failure": "LOAD_FAILED" } }), json!({}));
        assert!(emitted.is_empty(), "{emitted:?}");
    }
}
