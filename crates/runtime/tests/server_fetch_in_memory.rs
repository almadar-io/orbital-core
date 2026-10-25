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
        "name": "sf", "orbitals": [{ "name": "Main", "entity": entity,
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
        "name": "sf", "orbitals": [{ "name": "Main",
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
