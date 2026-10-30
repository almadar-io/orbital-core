// An emit the host already ran (`source.dispatched`) reached its listeners on the host; the fold runs it only on the client-only ones, whose host run is dropped (twin of `applyOrbitalEventResponse`'s `hostRan`, `almadar-runtime/src/evaluation/client-role.ts`; ui `Chat.realtime-chat`, `GlobalSearch.project-friday`).

use orbital_core_runtime::embed::EmbeddedKernel;
use {orbital_core_base::bus_event::{BusEventSource, EmittedEvent}, orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::json;
use std::collections::HashSet;

fn kernel() -> EmbeddedKernel {
    let listener = |name: &str, entity: &str| {
        json!({ "name": name, "linkedEntity": entity, "dispatch": "client",
            "listens": [{ "event": "LOADED", "source": { "kind": "orbital", "orbital": "Chat", "trait": "Loader" }, "triggers": "SHOW" }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }],
                "transitions": [{ "from": "idle", "to": "shown", "event": "SHOW", "effects": [] }] } })
    };
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "chat",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Chat",
            "entity": { "name": "Message", "persistence": "persistent", "collection": "messages",
                "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Loader", "linkedEntity": "Message", "emits": [{ "event": "LOADED", "scope": "external" }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } },
                {
                  "name": "HostBacked", "linkedEntity": "Message",
                  "listens": [{ "event": "LOADED", "source": { "kind": "trait", "trait": "Loader" }, "triggers": "SHOW" }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }],
                    "transitions": [{ "from": "idle", "to": "shown", "event": "SHOW", "effects": [["fetch", "Message", {}]] }] } }
            ]
        }, {
            "name": "Desk",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [listener("ClientOnly", "Note")]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c1").unwrap()
}

fn loaded(dispatched: Option<bool>) -> EventResponse {
    EventResponse {
        success: true,
        emitted_events: vec![EmittedEvent {
            event: "LOADED".to_string(),
            payload: None,
            source: Some(BusEventSource {
                orbital: Some("Chat".to_string()),
                r#trait: Some("Loader".to_string()),
                dispatched,
                ..Default::default()
            }),
        }],
        ..Default::default()
    }
}

fn state(k: &EmbeddedKernel, t: &str) -> String {
    k.get_state(t, None)
}

#[test]
fn an_emit_the_host_ran_runs_only_on_the_client_only_listeners() {
    let k = kernel();
    k.kernel().set_host_fans_out(true);
    k.apply_server_response(&loaded(Some(true)), &HashSet::new())
        .unwrap();
    assert_eq!(state(&k, "ClientOnly"), "shown");
    assert_eq!(
        state(&k, "HostBacked"),
        "",
        "the host's own run is what the client folds"
    );
}

#[test]
fn control_an_emit_the_host_did_not_run_reaches_every_listener() {
    let k = kernel();
    k.apply_server_response(&loaded(None), &HashSet::new())
        .unwrap();
    assert_eq!(state(&k, "ClientOnly"), "shown");
    assert_eq!(state(&k, "HostBacked"), "shown");
}

#[test]
fn control_an_explicit_not_dispatched_flag_is_not_a_host_run() {
    let k = kernel();
    k.apply_server_response(&loaded(Some(false)), &HashSet::new())
        .unwrap();
    assert_eq!(state(&k, "HostBacked"), "shown");
}

#[test]
fn control_a_host_that_does_not_fan_out_has_every_listener_run_by_the_fold() {
    let k = kernel();
    k.apply_server_response(&loaded(Some(true)), &HashSet::new())
        .unwrap();
    assert_eq!(
        state(&k, "HostBacked"),
        "shown",
        "a generated server delivers no listener"
    );
}

#[test]
fn an_emit_delivered_to_its_listeners_reaches_every_listener_whatever_the_host_ran() {
    let k = kernel();
    k.kernel().set_host_fans_out(true);
    let source = loaded(Some(true)).emitted_events.remove(0);
    let flight = k.open_flight();
    let progress = k.deliver_emit_to_listeners(flight, source).unwrap();
    assert!(matches!(progress, orbital_core_runtime::embed::FoldProgress::Applied(_)));
    assert_eq!(state(&k, "ClientOnly"), "shown");
    assert_eq!(state(&k, "HostBacked"), "shown", "the delivery is independent of the dispatched stamp and host_fans_out");
}

#[test]
fn control_a_delivered_emit_does_not_run_the_emitters_own_arm() {
    let k = kernel();
    let source = loaded(None).emitted_events.remove(0);
    let flight = k.open_flight();
    k.deliver_emit_to_listeners(flight, source).unwrap();
    assert_eq!(state(&k, "Loader"), "", "an emit with no declared arm for it leaves the emitter's state alone");
}

#[test]
fn a_delivered_emit_queues_behind_an_older_open_flight_and_folds_in_order() {
    let k = kernel();
    let older = k.open_flight();
    let newer = k.open_flight();
    let source = loaded(None).emitted_events.remove(0);
    let progress = k.deliver_emit_to_listeners(newer, source).unwrap();
    assert!(matches!(progress, orbital_core_runtime::embed::FoldProgress::Queued { .. }));
    assert_eq!(state(&k, "HostBacked"), "");
    k.end_flight(older);
    let next = k.next_fold().expect("the queued delivery folds once the older flight ends");
    assert!(next.outcome.is_ok());
    assert_eq!(state(&k, "HostBacked"), "shown");
}

fn self_handling_kernel() -> EmbeddedKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "app", "typeContractVersion": 1, "orbitals": [{
            "name": "O",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Maker", "linkedEntity": "E", "dispatch": "client", "emits": [{ "event": "PROGRESS" }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "ran" }],
                      "transitions": [{ "from": "idle", "to": "ran", "event": "PROGRESS", "effects": [] }] } },
                { "name": "Watcher", "linkedEntity": "E", "dispatch": "client",
                  "listens": [{ "event": "PROGRESS", "triggers": "SEEN", "source": { "kind": "trait", "trait": "Maker" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "seen" }],
                      "transitions": [{ "from": "idle", "to": "seen", "event": "SEEN", "effects": [] }] } }
            ]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c1").unwrap()
}

fn progress() -> EmittedEvent {
    EmittedEvent {
        event: "PROGRESS".to_string(),
        payload: None,
        source: Some(BusEventSource { orbital: Some("O".to_string()), r#trait: Some("Maker".to_string()), ..Default::default() }),
    }
}

#[test]
fn a_delivered_emit_leaves_the_emitters_own_arm_unrun_while_its_listener_runs() {
    let k = self_handling_kernel();
    let flight = k.open_flight();
    k.deliver_emit_to_listeners(flight, progress()).unwrap();
    assert_eq!(state(&k, "Watcher"), "seen");
    assert_eq!(state(&k, "Maker"), "", "the emitter handles PROGRESS, and a push from another client does not run it here");
}

#[test]
fn control_the_fold_of_the_same_emit_runs_the_emitters_own_arm() {
    let k = self_handling_kernel();
    let response = EventResponse { success: true, emitted_events: vec![progress()], ..Default::default() };
    k.apply_server_response(&response, &HashSet::new()).unwrap();
    assert_eq!(state(&k, "Maker"), "ran");
}

fn self_listening_kernel() -> EmbeddedKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "app", "typeContractVersion": 1, "orbitals": [{
            "name": "O",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Caller", "linkedEntity": "E", "dispatch": "client", "emits": [{ "event": "PROGRESS" }],
                  "listens": [{ "event": "PROGRESS", "triggers": "PROGRESS", "source": { "kind": "trait", "trait": "Caller" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "once" }, { "name": "twice" }],
                      "transitions": [
                          { "from": "idle", "to": "once", "event": "PROGRESS", "effects": [] },
                          { "from": "once", "to": "twice", "event": "PROGRESS", "effects": [] }] } },
                { "name": "Watcher", "linkedEntity": "E", "dispatch": "client",
                  "listens": [{ "event": "PROGRESS", "triggers": "SEEN", "source": { "kind": "trait", "trait": "Caller" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "seen" }],
                      "transitions": [{ "from": "idle", "to": "seen", "event": "SEEN", "effects": [] }] } }
            ]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c1").unwrap()
}

fn caller_progress() -> EmittedEvent {
    EmittedEvent {
        event: "PROGRESS".to_string(),
        payload: None,
        source: Some(BusEventSource { orbital: Some("O".to_string()), r#trait: Some("Caller".to_string()), ..Default::default() }),
    }
}

#[test]
fn an_origin_emit_does_not_run_a_self_listening_emitter_a_second_time() {
    let k = self_listening_kernel();
    k.dispatch("Caller", None, "PROGRESS", orbital_core_base::value::Value::Null).unwrap();
    assert_eq!(state(&k, "Caller"), "once", "the call's own progress dispatch ran the arm");
    let flight = k.open_flight();
    k.deliver_origin_emit(flight, caller_progress()).unwrap();
    assert_eq!(state(&k, "Caller"), "once", "the listener route back to the emitter is that arm again");
    assert_eq!(state(&k, "Watcher"), "seen", "every other listener still hears it");
}

#[test]
fn control_a_peer_push_runs_a_self_listening_trait_once() {
    let k = self_listening_kernel();
    let flight = k.open_flight();
    k.deliver_emit_to_listeners(flight, caller_progress()).unwrap();
    assert_eq!(state(&k, "Caller"), "once", "a push from another client did not run the emitter's arm here");
    assert_eq!(state(&k, "Watcher"), "seen");
}

#[test]
fn control_a_peer_delivery_after_the_emitters_own_arm_ran_it_runs_the_self_route_again() {
    let k = self_listening_kernel();
    k.dispatch("Caller", None, "PROGRESS", orbital_core_base::value::Value::Null).unwrap();
    let flight = k.open_flight();
    k.deliver_emit_to_listeners(flight, caller_progress()).unwrap();
    assert_eq!(state(&k, "Caller"), "twice", "the double run the origin delivery removes");
}
