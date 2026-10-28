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
        "typeContractVersion": 1, "orbitals": [{ "name": "Tickets", "entity": ticket,
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
        "typeContractVersion": 1, "orbitals": [{
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
        "typeContractVersion": 1, "orbitals": [{
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

fn lane_schema() -> orbital_core_base::schema::types::OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Asker", "linkedEntity": "Job",
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "asked" }],
                      "transitions": [{ "from": "idle", "to": "asked", "event": "ASK", "effects": [["call-service", "svc", "work", {}]] }]
                  } },
                { "name": "Counter", "linkedEntity": "Job",
                  "stateMachine": {
                      "states": [{ "name": "zero", "isInitial": true }, { "name": "one" }, { "name": "two" }],
                      "transitions": [
                          { "from": "zero", "to": "one", "event": "TOUCH" },
                          { "from": "one", "to": "two", "event": "TOUCH" }
                      ]
                  } }
            ]
        }]
    }))
    .unwrap()
}

fn targeted(event: &str, target: &str) -> orbital_core_runtime::effects::response::EventRequest {
    orbital_core_runtime::effects::response::EventRequest {
        event: event.to_string(),
        target_trait: Some(target.to_string()),
        ..Default::default()
    }
}

fn answered(requests: Vec<orbital_core_runtime::resumable::PendingRequest>) -> Vec<orbital_core_runtime::resumable::IoReply> {
    use orbital_core_runtime::resumable::{IoAnswer, IoReply};
    requests
        .into_iter()
        .map(|pending| IoReply {
            id: pending.id,
            answer: IoAnswer::Service(Ok(serde_json::from_value(serde_json::json!({ "ok": true })).unwrap())),
        })
        .collect()
}

#[test]
fn exec_conc_2_the_kernel_queues_a_request_behind_a_dispatch_parked_on_an_unkeyed_ask() {
    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    let server = EmbeddedServer::new(lane_schema()).unwrap();
    let ServerProgress::Await { flight, requests } = server.start("Main", targeted("ASK", "Asker")).unwrap() else {
        panic!("the unkeyed call parks its dispatch");
    };
    assert!(server.holds_lane(flight), "an unkeyed ask keeps the lane");
    let ServerProgress::Queued { ticket: first } = server.start("Main", targeted("TOUCH", "Counter")).unwrap() else {
        panic!("a request arriving while the lane is held waits");
    };
    let ServerProgress::Queued { ticket: second } = server.start("Main", targeted("TOUCH", "Counter")).unwrap() else {
        panic!("a second request queues behind the first");
    };
    assert!(server.next().is_none(), "nothing starts while the parked dispatch holds the lane");
    assert_eq!(server.get_state("Counter", None), "zero", "a queued request has not run");
    let ServerProgress::Done(_) = server.resume(flight, answered(requests)).unwrap() else {
        panic!("the answered dispatch commits");
    };
    let (ticket, started) = server.next().expect("the lane is free, so the oldest queued request starts");
    assert_eq!(ticket, first);
    assert!(matches!(started, Ok(ServerProgress::Done(_))));
    assert_eq!(server.get_state("Counter", None), "one");
    let (ticket, _) = server.next().expect("then the next one, in arrival order");
    assert_eq!(ticket, second);
    assert_eq!(server.get_state("Counter", None), "two");
    assert!(server.next().is_none(), "the queue is empty");
}

#[test]
fn exec_conc_2_control_a_request_with_the_lane_free_starts_at_once() {
    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    let server = EmbeddedServer::new(lane_schema()).unwrap();
    assert!(matches!(server.start("Main", targeted("TOUCH", "Counter")).unwrap(), ServerProgress::Done(_)));
    assert!(server.next().is_none());
}

#[test]
fn exec_conc_2_control_an_abandoned_dispatch_frees_the_lane_for_the_queue() {
    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    let server = EmbeddedServer::new(lane_schema()).unwrap();
    let ServerProgress::Await { flight, .. } = server.start("Main", targeted("ASK", "Asker")).unwrap() else {
        panic!("the unkeyed call parks its dispatch");
    };
    let ServerProgress::Queued { ticket } = server.start("Main", targeted("TOUCH", "Counter")).unwrap() else {
        panic!("queued behind the parked dispatch");
    };
    assert!(server.abandon(flight));
    assert!(!server.holds_lane(flight));
    let (started, _) = server.next().expect("the abandoned dispatch released the lane");
    assert_eq!(started, ticket);
}
