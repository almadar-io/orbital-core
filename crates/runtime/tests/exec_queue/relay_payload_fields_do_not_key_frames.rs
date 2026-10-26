// EXEC-STEP-1: a listener reached across orbitals steps the dispatch's frame. A payload field is data, so a field
// that happens to be called `orbitalName` or `entityId` never picks another frame: the listener's new state is the
// one the next delivery sees. JS twin: `almadar-runtime/test/cross-orbital-relay-payload-fields-do-not-key-frames.test.ts`.

use orbital_core_runtime::kernel::RuntimeKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn kernel(field: &str) -> RuntimeKernel {
    let schema: Json = json!({
        "name": "App",
        "orbitals": [
            {
                "name": "StarterOrbital",
                "entity": { "name": "Start", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Starter", "linkedEntity": "Start",
                    "stateMachine": { "states": [{ "name": "ready", "isInitial": true }],
                        "transitions": [{ "from": "ready", "to": "ready", "event": "GO", "effects": [["emit", "ASK", { field: "card-1" }]] }] }
                }]
            },
            {
                "name": "AskerOrbital",
                "entity": { "name": "Ask", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Asker", "linkedEntity": "Ask",
                    "listens": [{ "event": "ASK", "triggers": "ASKED", "source": { "kind": "orbital", "orbital": "StarterOrbital", "trait": "Starter" } }],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "asked" }, { "name": "pinged" }],
                        "transitions": [
                            { "from": "idle", "to": "asked", "event": "ASKED" },
                            { "from": "asked", "to": "pinged", "event": "PING" }
                        ] }
                }]
            }
        ]
    });
    let schema: OrbitalSchema = serde_json::from_value(schema).unwrap();
    crate::support::TestKernel::server().register(schema)
}

fn next_delivery_sees_the_relayed_state(field: &str) -> bool {
    let k = kernel(field);
    let go = k.dispatch("Starter", None, "GO", Value::Null).unwrap();
    assert!(
        go.iter().any(|(t, r)| t == "Asker" && r.executed),
        "the relay reached the listener"
    );
    k.dispatch("Asker", None, "PING", Value::Null)
        .unwrap()
        .iter()
        .any(|(t, r)| t == "Asker" && r.executed)
}

#[test]
fn a_payload_field_named_orbital_name_does_not_key_the_listener_frame() {
    assert!(next_delivery_sees_the_relayed_state("orbitalName"));
}

#[test]
fn a_payload_field_named_entity_id_does_not_key_the_listener_frame() {
    assert!(next_delivery_sees_the_relayed_state("entityId"));
}

#[test]
fn control_a_neutral_payload_field_keeps_the_listener_frame() {
    assert!(next_delivery_sees_the_relayed_state("request"));
}
