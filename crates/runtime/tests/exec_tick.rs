//! EXEC-TICK conformance: where a tick's emits go.

mod support;

use orbital_core_base::value::Value;
use serde_json::json;
use support::TestKernel;

#[test]
fn exec_tick_2_a_sibling_with_a_same_named_arm_and_no_listens_does_not_hear_a_tick_emit() {
    let kernel = TestKernel::client().build(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }, { "name": "beats", "type": "number" }] },
            "traits": [
                { "name": "Ticker", "linkedEntity": "Note", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "running", "isInitial": true }],
                      "transitions": [{ "from": "running", "to": "running", "event": "INIT", "effects": [] }] },
                  "ticks": [{ "name": "beat", "interval": 1000, "effects": [["emit", "BEAT", { "n": 1 }]] }] },
                { "name": "Listener", "linkedEntity": "Note", "dispatch": "client",
                  "listens": [{ "event": "BEAT", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Ticker" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                      "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD", "effects": [] }] } },
                { "name": "Sibling", "linkedEntity": "Note", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "overheard" }],
                      "transitions": [{ "from": "idle", "to": "overheard", "event": "BEAT", "effects": [] }] } }
            ]
        }]
    }));
    kernel.dispatch_with_server_leg("App", "Ticker", None, "INIT", Value::Null, None).unwrap();

    let tick = kernel.dispatch_tick("Main", "Ticker", None, "beat", None).unwrap();
    assert_eq!(tick.emits.len(), 1);
    let states = &tick.emits[0].dispatch.response.states;
    assert_eq!(states.get("Listener").map(String::as_str), Some("heard"), "control: the declared listener hears it");
    assert_ne!(states.get("Sibling").map(String::as_str), Some("overheard"), "no bare-key broadcast of a tick emit");
    assert_ne!(kernel.snapshot_trait("Sibling", None).state, "overheard");
}
