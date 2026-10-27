//! EXEC-CONC conformance: what the straight-through dispatch may answer in place.


use orbital_core_base::value::Value;
use serde_json::json;
use crate::support::TestKernel;

fn fetch_errors(store: Option<&str>) -> Vec<String> {
    let mut ticket = json!({ "name": "Ticket", "persistence": "persistent", "collection": "tickets",
        "fields": [{ "name": "id", "type": "string" }] });
    if let Some(driver) = store {
        ticket["store"] = json!(driver);
    }
    let kernel = TestKernel::server().build(json!({
        "name": "Desk",
        "orbitals": [{ "name": "Tickets", "entity": ticket,
            "traits": [{ "name": "Desk", "linkedEntity": "Ticket",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "LOOK",
                        "effects": [["fetch", "Ticket", { "id": "t1" }]] }] } }] }]
    }));
    let outcomes = kernel.dispatch("Desk", None, "LOOK", Value::Null).unwrap();
    outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter(|e| e.is_failed())
        .filter_map(|e| e.error.clone())
        .collect()
}

#[test]
fn exec_conc_4_the_straight_through_dispatch_refuses_a_store_bound_row_ask() {
    let errors = fetch_errors(Some("Rows"));
    assert!(errors.iter().any(|e| e.contains("bound to store driver Rows")), "{errors:?}");
}

#[test]
fn exec_conc_4_control_an_unbound_entity_is_answered_in_place() {
    let errors = fetch_errors(None);
    assert!(errors.iter().all(|e| !e.contains("store driver")), "answered in place, not refused: {errors:?}");
}

use orbital_core_runtime::config::KernelConfig;
use orbital_core_runtime::entity_source::InMemoryEntityCache;
use orbital_core_runtime::kernel::RuntimeKernel;
use std::sync::Arc;

fn emit_chain(listener_listens: bool) -> (RuntimeKernel, usize) {
    let mut listener = json!({ "name": "B", "linkedEntity": "Note",
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
            "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD", "effects": [] }] } });
    if listener_listens {
        listener["listens"] = json!([{ "event": "PING", "triggers": "HEARD", "source": { "kind": "trait", "trait": "A" } }]);
    }
    let kernel = TestKernel::server().build(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "A", "linkedEntity": "Note",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "PING", {}]] }] } },
                listener
            ]
        }]
    }));
    let outcomes = kernel.dispatch("A", None, "GO", Value::Null).unwrap();
    let count = outcomes.len();
    (kernel, count)
}

/// EXEC-CONC-2: a plain emit stays in the current dispatch (the inner cascade worklist), not a new host event.
#[test]
fn exec_conc_2_a_plain_emit_is_delivered_inside_the_same_dispatch() {
    let (kernel, steps) = emit_chain(true);
    assert_eq!(kernel.snapshot_trait("B", None).state, "heard", "listener ran within the one dispatch call");
    assert!(steps >= 2, "the one dispatch result carries the emitter's and the listener's step: {steps}");
}

#[test]
fn exec_conc_2_control_without_a_listener_the_emit_reaches_no_other_trait() {
    let (kernel, steps) = emit_chain(false);
    assert_ne!(kernel.snapshot_trait("B", None).state, "heard", "no listens, nothing delivered");
    assert_eq!(steps, 1, "only the emitter's step ran");
}

fn session_kernel() -> RuntimeKernel {
    TestKernel::server().build(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "Note",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                    "transitions": [{ "from": "idle", "to": "busy", "event": "GO", "effects": [] }] } }]
        }]
    }))
}

/// EXEC-CONC-5: a fork builds a fresh session over the registered program and shares no mutable state.
#[test]
fn exec_conc_5_a_fork_starts_a_fresh_session_and_shares_no_mutable_state() {
    let parent = session_kernel();
    parent.dispatch("T", None, "GO", Value::Null).unwrap();
    assert_eq!(parent.snapshot_trait("T", None).state, "busy");

    let fork = parent
        .fork_session(Arc::new(InMemoryEntityCache::new()), KernelConfig::default())
        .expect("forks");
    assert_ne!(fork.snapshot_trait("T", None).state, "busy", "the fork does not inherit the parent's trait state");
    assert!(fork.dispatch("T", None, "GO", Value::Null).is_ok(), "the fork runs the registered program");
}

#[test]
fn exec_conc_5_control_a_dispatch_on_a_fork_leaves_the_parent_session_alone() {
    let parent = session_kernel();
    let fork = parent
        .fork_session(Arc::new(InMemoryEntityCache::new()), KernelConfig::default())
        .expect("forks");
    fork.dispatch("T", None, "GO", Value::Null).unwrap();
    assert_eq!(fork.snapshot_trait("T", None).state, "busy", "control: the fork's own dispatch moved it");
    assert_ne!(parent.snapshot_trait("T", None).state, "busy", "the parent shares no mutable state with the fork");
}
