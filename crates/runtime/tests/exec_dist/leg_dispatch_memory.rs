// A server leg carries the target trait's dispatch memory (EXEC-QUEUE-5) only when that trait reads it: a `[client]`
// front that folds a large answer and then forwards its next ask (std-kflow's knowledge fronts) never names
// `@prevEvents` or `@prevStates`, so its leg posts no `dispatchLog` and the answer is not sent back up (G-CROSS-643).

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::kernel::RuntimeKernel;
use serde_json::json;

fn kernel(next_guard: Option<serde_json::Value>) -> RuntimeKernel {
    let mut next =
        json!({ "from": "idle", "to": "idle", "event": "POKE", "effects": [["emit", "FWD", {}]] });
    if let Some(guard) = next_guard {
        next["guard"] = guard;
    }
    let schema: OrbitalSchema = from_json_value(&json!({
        "name": "App",
        "typeContractVersion": 1,
        "_metadata": { "serverListens": [{ "orbital": "Worker", "listen": {
            "event": "FWD", "triggers": "LIST", "source": { "kind": "any" }
        } }] },
        "orbitals": [{
            "name": "Dash",
            "entity": { "name": "Board", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Map", "linkedEntity": "Board", "dispatch": "client",
                "emits": [{ "event": "ANSWERED", "scope": "external" }, { "event": "POKE", "scope": "external" }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                        "effects": [["emit", "ANSWERED", { "rows": ["an answer the host already sent"] }], ["emit", "POKE", {}]] }]
                }
            }]
        }, {
            "name": "Relay",
            "entity": { "name": "FrontRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Front", "linkedEntity": "FrontRun", "dispatch": "client",
                "emits": [{ "event": "FWD", "scope": "external" }],
                "listens": [{ "event": "ANSWERED", "triggers": "DONE", "source": { "kind": "any" } },
                    { "event": "POKE", "triggers": "POKE", "source": { "kind": "any" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "DONE", "effects": [] },
                        next
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
}

fn front_leg(k: &RuntimeKernel) -> orbital_core_runtime::effects::EventRequest {
    let d = k
        .dispatch_with_server_leg("Dash", "Map", None, "GO", Value::Null, Some("c1"))
        .unwrap();
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    let leg = d.server_legs.into_iter().next().unwrap();
    assert_eq!(leg.target_trait.as_deref(), Some("Front"));
    assert_eq!(leg.event, "POKE");
    leg
}

#[test]
fn a_leg_to_a_trait_that_never_reads_its_dispatch_memory_carries_none() {
    assert!(front_leg(&kernel(None)).dispatch_log.is_none());
}

#[test]
fn control_a_leg_to_a_trait_whose_guard_reads_prev_events_carries_them() {
    let log = front_leg(&kernel(Some(json!([">", ["array/len", "@prevEvents"], 0]))))
        .dispatch_log
        .expect("the dispatch memory");
    assert_eq!(log.prev_events.len(), 1);
    assert_eq!(log.prev_events[0].event, "ANSWERED");
}

#[test]
fn control_a_leg_to_a_trait_whose_guard_reads_prev_states_carries_them() {
    assert!(
        front_leg(&kernel(Some(json!(["=", ["array/len", "@prevStates"], 0]))))
            .dispatch_log
            .is_some()
    );
}
