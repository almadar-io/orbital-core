//! A fetch's `emit.success` payload is the TS wire's (`EffectExecutor.ts`):
//! `{ data, totalCount }`, `totalCount` counted after `@read` + `filter:` and
//! before offset/limit; a by-id hit's `data` is the row itself.
use super::*;
use crate::entity_source::InMemoryEntityCache;

fn loaded(fetch: serde_json::Value) -> Option<Value> {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "Fetches",
        "orbitals": [{
            "name": "Items",
            "entity": {
                "name": "Item",
                "fields": [{ "name": "id", "type": "string" }, { "name": "kind", "type": "string" }],
                "instances": [
                    { "id": "a", "kind": "x" },
                    { "id": "b", "kind": "y" },
                    { "id": "c", "kind": "x" }
                ]
            },
            "traits": [{
                "name": "Loader",
                "linkedEntity": "Item",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [fetch] }]
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
    let out = kernel.dispatch("Loader", None, "GO", Value::Null).unwrap();
    out.iter().find_map(|(_, r)| {
        r.effect_results.iter().find_map(|er| match &er.effect {
            orbital_core_base::effect_types::Effect::Emit { event, payload }
                if event == "LOADED" =>
            {
                Some(payload.clone())
            }
            _ => None,
        })
    })
}

fn field<'a>(payload: &'a Option<Value>, key: &str) -> Option<&'a Value> {
    match payload {
        Some(Value::Object(m)) => m.get(key),
        _ => None,
    }
}

fn rows(payload: &Option<Value>) -> usize {
    field(payload, "data")
        .and_then(Value::as_array)
        .map_or(usize::MAX, Vec::len)
}

#[test]
fn a_collection_fetch_reports_the_total_before_the_page() {
    let p = loaded(
        serde_json::json!(["fetch", "Item", { "limit": 2, "emit": { "success": "LOADED" } }]),
    );
    assert_eq!(rows(&p), 2);
    assert_eq!(field(&p, "totalCount"), Some(&Value::Number(3.0)));
}

#[test]
fn the_total_counts_the_filtered_rows() {
    let p = loaded(serde_json::json!(["fetch", "Item", {
        "filter": ["=", ["object/get", "@entity", "kind"], "x"], "emit": { "success": "LOADED" }
    }]));
    assert_eq!(rows(&p), 2);
    assert_eq!(field(&p, "totalCount"), Some(&Value::Number(2.0)));
}

#[test]
fn an_offset_past_the_end_still_reports_the_total() {
    let p = loaded(
        serde_json::json!(["fetch", "Item", { "offset": 5, "emit": { "success": "LOADED" } }]),
    );
    assert_eq!(rows(&p), 0);
    assert_eq!(field(&p, "totalCount"), Some(&Value::Number(3.0)));
}

#[test]
fn a_by_id_hit_carries_the_row_itself() {
    let p = loaded(
        serde_json::json!(["fetch", "Item", { "id": "b", "emit": { "success": "LOADED" } }]),
    );
    let row = field(&p, "data").and_then(|d| match d {
        Value::Object(m) => m.get("id").cloned(),
        _ => None,
    });
    assert_eq!(row, Some(Value::string("b")));
    assert_eq!(field(&p, "totalCount"), Some(&Value::Number(1.0)));
}

#[test]
fn control_a_by_id_miss_emits_no_success() {
    let p = loaded(
        serde_json::json!(["fetch", "Item", { "id": "zz", "emit": { "success": "LOADED" } }]),
    );
    assert_eq!(p, None);
}
