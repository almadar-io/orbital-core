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
