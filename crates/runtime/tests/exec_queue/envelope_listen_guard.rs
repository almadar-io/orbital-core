// EXEC-QUEUE-9 (owner 2026-11-12, G-ORB-325): an envelope carries the emit its listener heard, and the receiving kernel
// judges that listener's `when` guard against it before it dispatches, as it would a local listen.

use orbital_core_base::envelope::{Envelope, EnvelopeListen, EnvelopeVerdict, InstanceRef};
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::kernel::RuntimeKernel;
use serde_json::{json, Value as Json};

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Sender", "linkedEntity": "Note", "emits": [{ "event": "PING" }], "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "PING", { "n": 1 }]] }]
                } },
                { "name": "Receiver", "linkedEntity": "Note",
                  "listens": [{ "event": "PING", "triggers": "RECEIVE", "guard": ["=", "@payload.n", 1], "source": { "kind": "trait", "trait": "Sender" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                    "transitions": [{ "from": "idle", "to": "heard", "event": "RECEIVE" }]
                  } }
            ]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

fn envelope(id: &str, heard: Option<Json>) -> Envelope {
    Envelope {
        id: id.into(),
        from: InstanceRef { instance: "api".into(), orbital: "Main".into(), trait_name: Some("Sender".into()) },
        to: InstanceRef { instance: "web".into(), orbital: "Main".into(), trait_name: Some("Receiver".into()) },
        event: "RECEIVE".into(),
        payload: serde_json::from_value(json!({})).unwrap(),
        causal_id: "c".into(),
        hop: 1,
        listen: heard.map(|payload| EnvelopeListen { event: "PING".into(), payload: serde_json::from_value(payload).unwrap() }),
    }
}

#[test]
fn a_listener_whose_guard_fails_on_the_heard_emit_takes_the_envelope_and_runs_nothing() {
    let kernel = kernel();
    assert_eq!(kernel.dispatch_envelope(envelope("e1", Some(json!({ "n": 2 })))), EnvelopeVerdict::Accepted);
    assert_ne!(kernel.snapshot_trait("Receiver", None).state, "heard", "the receiver never ran");
}

#[test]
fn control_a_listener_whose_guard_passes_runs_its_trigger() {
    let kernel = kernel();
    assert_eq!(kernel.dispatch_envelope(envelope("e1", Some(json!({ "n": 1 })))), EnvelopeVerdict::Accepted);
    assert_eq!(kernel.snapshot_trait("Receiver", None).state, "heard");
}
