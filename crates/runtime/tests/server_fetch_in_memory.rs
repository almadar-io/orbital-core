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
