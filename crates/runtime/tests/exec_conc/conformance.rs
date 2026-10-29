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

mod client_fold_order {
    use orbital_core_base::bus_event::{BusEventSource, EmittedEvent};
    use orbital_core_base::schema::types::OrbitalSchema;
    use orbital_core_base::value::Value;
    use orbital_core_runtime::effects::response::EventResponse;
    use orbital_core_runtime::embed::{EmbeddedKernel, FoldProgress};
    use serde_json::json;
    use std::collections::HashSet;

    fn client() -> EmbeddedKernel {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "books", "typeContractVersion": 1, "orbitals": [{
                "name": "Books",
                "entity": { "name": "Invoice", "persistence": "persistent", "collection": "invoices",
                    "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "InvoiceList", "linkedEntity": "Invoice",
                    "emits": [{ "event": "ADDED", "scope": "external" }],
                    "stateMachine": {
                        "states": [{ "name": "browsing", "isInitial": true }],
                        "transitions": [
                            { "from": "browsing", "to": "browsing", "event": "ADD",
                              "effects": [["persist", "create", "Invoice", {}, { "emit": { "success": "ADDED" } }]] },
                            { "from": "browsing", "to": "browsing", "event": "LOOK", "effects": [] }] } }]
            }, {
                "name": "Desk",
                "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Badge", "linkedEntity": "Note", "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "off", "isInitial": true }, { "name": "on" }],
                        "transitions": [{ "from": "off", "to": "on", "event": "FLIP", "effects": [] }] } }]
            }]
        }))
        .unwrap();
        EmbeddedKernel::new(schema, "c1").unwrap()
    }

    fn answer() -> EventResponse {
        EventResponse {
            success: true,
            transitioned: true,
            emitted_events: vec![EmittedEvent {
                event: "ADDED".to_string(),
                payload: None,
                source: Some(BusEventSource { r#trait: Some("InvoiceList".to_string()), ..Default::default() }),
            }],
            ..Default::default()
        }
    }

    fn flight(kernel: &EmbeddedKernel, event: &str) -> u64 {
        kernel.dispatch("InvoiceList", None, event, Value::Null).unwrap().flight.expect("the dispatch owes a leg")
    }

    fn fold(kernel: &EmbeddedKernel, flight: u64) -> FoldProgress {
        kernel.fold_in_order(flight, answer(), HashSet::new(), None).unwrap()
    }

    #[test]
    fn exec_conc_8_a_later_dispatch_whose_post_resolves_first_folds_after_the_earlier_one() {
        let kernel = client();
        let first = flight(&kernel, "ADD");
        let second = flight(&kernel, "ADD");
        assert!(first < second, "flights are numbered in dispatch order");
        let FoldProgress::Queued { ticket } = fold(&kernel, second) else { panic!("the later answer folded ahead of the earlier dispatch") };
        assert!(kernel.next_fold().is_none(), "nothing folds while the earlier flight is open");
        assert!(matches!(fold(&kernel, first), FoldProgress::Applied(_)));
        assert!(kernel.next_fold().is_none(), "the earlier flight is open until the host ends it");
        assert!(kernel.end_flight(first));
        let next = kernel.next_fold().expect("the queued answer folds once the earlier flight ends");
        assert_eq!(next.ticket, ticket);
        next.outcome.unwrap();
        assert!(kernel.next_fold().is_none());
    }

    #[test]
    fn exec_conc_8_control_answers_arriving_in_dispatch_order_fold_at_once() {
        let kernel = client();
        let first = flight(&kernel, "ADD");
        let second = flight(&kernel, "ADD");
        assert!(matches!(fold(&kernel, first), FoldProgress::Applied(_)));
        assert!(kernel.end_flight(first));
        assert!(matches!(fold(&kernel, second), FoldProgress::Applied(_)));
        assert!(kernel.next_fold().is_none());
    }

    #[test]
    fn exec_conc_8_a_second_answer_of_one_flight_waits_behind_its_own_queued_answer() {
        let kernel = client();
        let first = flight(&kernel, "ADD");
        let second = flight(&kernel, "ADD");
        assert!(matches!(fold(&kernel, second), FoldProgress::Queued { .. }));
        assert!(kernel.end_flight(first));
        assert!(matches!(fold(&kernel, second), FoldProgress::Queued { .. }), "the flight's earlier answer folds first");
        assert!(kernel.next_fold().is_some());
        assert!(kernel.next_fold().is_some());
        assert!(kernel.next_fold().is_none());
    }

    #[test]
    fn exec_conc_8_a_flight_the_host_gave_up_on_frees_the_ones_behind_it() {
        let kernel = client();
        let first = flight(&kernel, "ADD");
        let second = flight(&kernel, "ADD");
        assert!(matches!(fold(&kernel, second), FoldProgress::Queued { .. }));
        assert!(kernel.end_flight(first), "a failed post ends its flight without folding");
        assert!(kernel.next_fold().is_some());
    }

    #[test]
    fn exec_conc_8_a_synchronous_host_s_fold_ends_the_flights_it_answered() {
        let kernel = client();
        let first = flight(&kernel, "ADD");
        let second = flight(&kernel, "ADD");
        kernel.apply_server_response(&answer(), &HashSet::new()).unwrap();
        assert!(!kernel.end_flight(first) && !kernel.end_flight(second), "the synchronous fold already ended both");
        let third = flight(&kernel, "ADD");
        assert!(matches!(fold(&kernel, third), FoldProgress::Applied(_)), "no stale flight blocks the next dispatch");
    }

    #[test]
    fn exec_conc_8_a_dispatch_that_owes_no_leg_opens_no_flight_and_an_unknown_flight_is_refused() {
        let kernel = client();
        let looked = kernel.dispatch("Badge", None, "FLIP", Value::Null).unwrap();
        assert!(looked.flight.is_none());
        assert!(kernel.fold_in_order(99, answer(), HashSet::new(), None).is_err());
        assert!(!kernel.end_flight(99));
    }

    #[test]
    fn exec_conc_8_an_opened_flight_orders_work_the_kernel_did_not_wire() {
        let kernel = client();
        let dispatched = flight(&kernel, "ADD");
        let host = kernel.open_flight();
        assert!(matches!(fold(&kernel, host), FoldProgress::Queued { .. }));
        assert!(kernel.end_flight(dispatched));
        assert!(kernel.next_fold().is_some());
    }
}

fn tick_pair(rival_first: bool, surge: serde_json::Value) -> orbital_core_base::schema::types::OrbitalSchema {
    let driver = json!({ "name": "Driver", "linkedEntity": "Board", "dispatch": "client",
        "listens": [{ "event": "SURGE", "source": { "kind": "any" }, "triggers": "SURGE_IN" }],
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [], "transitions": [
            { "from": "idle", "to": "idle", "event": "INIT", "effects": [] },
            { "from": "idle", "to": "idle", "event": "SURGE_IN", "effects": [surge] }] },
        "ticks": [{ "name": "beat", "interval": 1000, "effects": [["set", "@entity.score", ["+", "@entity.score", 1]]] }] });
    let rival = json!({ "name": "Rival", "linkedEntity": "Board", "dispatch": "client", "emits": [{ "event": "SURGE", "scope": "external" }],
        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [], "transitions": [] },
        "ticks": [{ "name": "surge", "interval": 1000, "effects": [["emit", "SURGE", {}]] }] });
    let traits = if rival_first { json!([rival, driver]) } else { json!([driver, rival]) };
    serde_json::from_value(json!({
        "name": "x", "typeContractVersion": 1, "orbitals": [{
            "name": "M", "entity": { "name": "Board", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "score", "type": "number", "default": 0 }] },
            "traits": traits }]})).unwrap()
}

fn driver_score(k: &orbital_core_runtime::embed::EmbeddedKernel) -> Option<Value> {
    k.frames_since(0).unwrap().ops.iter().find_map(|op| match op {
        orbital_core_runtime::effects::FrameOp::Put { frame, fields } if frame == "Driver" => fields.get("score").cloned(),
        _ => None,
    })
}

#[test]
fn exec_tick_a_frame_seals_every_tick_s_own_writes_before_the_cascades_that_read_them() {
    let k = orbital_core_runtime::embed::EmbeddedKernel::new(tick_pair(false, json!(["set", "@entity.score", ["+", "@entity.score", 100]])), "c").unwrap();
    k.kernel().remount_traits(["Driver", "Rival"]);
    k.dispatch("Driver", None, "INIT", Value::Null).unwrap();
    assert!(k.frame(0).unwrap().is_none());
    let frame = k.frame(1000).unwrap().expect("both ticks fire");
    let frames = frame.response.frames.expect("the frame's writes");
    let cascade = frame.dispatches[0].dispatch.response.frames.clone().expect("the surge cascade's write");
    assert!(frames.from <= cascade.from && cascade.to <= frames.to, "the response's delta spans the cascade's");
    assert_eq!(driver_score(&k), Some(Value::from(101.0)), "the beat's +1 then the surge's +100, not the beat's stale 1");
}

/// The client embed frame runs each tick as the server-role tick path does: own effects, then its cascade, then the
/// next tick. Here Rival's cascade writes Driver's row and Driver's own tick reads it after.
#[test]
fn exec_tick_a_frame_orders_two_ticks_as_the_server_role_tick_path_does() {
    let surge = json!(["set", "@entity.score", 10]);
    let frame_kernel = orbital_core_runtime::embed::EmbeddedKernel::new(tick_pair(true, surge.clone()), "c").unwrap();
    frame_kernel.kernel().remount_traits(["Driver", "Rival"]);
    frame_kernel.dispatch("Driver", None, "INIT", Value::Null).unwrap();
    frame_kernel.frame(0).unwrap();
    frame_kernel.frame(1000).unwrap().expect("both ticks fire");

    let server = orbital_core_runtime::embed::EmbeddedKernel::new(tick_pair(true, surge), "c").unwrap();
    server.kernel().remount_traits(["Driver", "Rival"]);
    server.dispatch("Driver", None, "INIT", Value::Null).unwrap();
    for (trait_name, tick) in [("Rival", "surge"), ("Driver", "beat")] {
        server.kernel().dispatch_tick_unrepainted("M", trait_name, None, tick, Some("c")).unwrap();
    }
    let held = |k: &orbital_core_runtime::embed::EmbeddedKernel| k.kernel().snapshot_trait("Driver", None).entity.as_object().and_then(|row| row.get("score").cloned());
    assert_eq!(held(&server), Some(Value::from(11.0)), "Rival's cascade sets 10, then Driver's beat adds 1");
    assert_eq!(held(&frame_kernel), held(&server));
    assert_eq!(driver_score(&frame_kernel), Some(Value::from(11.0)), "and the view record the client paints from agrees");
}
