// `@payload` is the payload of the event being processed, and only that (owner ruling 2026-10-12,
// drift D-2): nothing a trait received earlier carries over, an empty payload reads `{}`, and a
// tick has no payload. Twin of `@almadar/runtime` `test/payload-is-the-events-own.test.ts`.

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Picker",
                "linkedEntity": "Item",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "PICK",
                          "effects": [["set", "@entity.picked", "@payload.row"]] },
                        { "from": "idle", "to": "idle", "event": "SHOW",
                          "effects": [["set", "@entity.shown", "@payload.row"], ["set", "@entity.whole", "@payload"]] }
                    ]
                },
                "ticks": [{ "name": "beat", "interval": 1000, "effects": [["set", "@entity.ticked", "@payload.row"]] }]
            }]
        }]
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn payload(value: serde_json::Value) -> Value {
    serde_json::from_value(value).unwrap()
}

fn entity_after(kernel: &RuntimeKernel, event: &str, body: serde_json::Value) -> serde_json::Value {
    let outcomes = kernel
        .dispatch("Picker", None, event, payload(body))
        .unwrap();
    let (_, result) = outcomes
        .iter()
        .find(|(t, r)| t == "Picker" && r.executed)
        .expect("Picker runs");
    serde_json::to_value(&result.entity_data).unwrap()
}

#[test]
fn an_event_reads_its_own_payload() {
    let kernel = kernel();
    let row = entity_after(&kernel, "PICK", json!({ "row": "r1" }));
    assert_eq!(row["picked"], json!("r1"));
}

#[test]
fn an_earlier_events_payload_does_not_carry_over() {
    let kernel = kernel();
    entity_after(&kernel, "PICK", json!({ "row": "r1" }));
    let row = entity_after(&kernel, "SHOW", json!({}));
    assert_ne!(
        row["shown"],
        json!("r1"),
        "the earlier PICK payload must not carry over: {row}"
    );
    assert_eq!(
        row["whole"],
        json!({}),
        "an empty payload reads {{}}: {row}"
    );
}

#[test]
fn a_tick_has_no_payload() {
    let kernel = kernel();
    entity_after(&kernel, "PICK", json!({ "row": "r1" }));
    let tick = kernel.process_tick("Picker", None, "beat").unwrap();
    let row = serde_json::to_value(&tick.entity_data).unwrap();
    assert_ne!(
        row["ticked"],
        json!("r1"),
        "a tick must not read an earlier payload: {row}"
    );
}
