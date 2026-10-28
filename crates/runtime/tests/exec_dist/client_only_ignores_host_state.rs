// A host's state and row for a client-only trait never overwrite the client's (twin of `withoutClientOnlyRuns`, `almadar-runtime/src/evaluation/client-role.ts`; ui `BrowserPlayground.cross-orbital-answer`).

use {orbital_core_runtime::kernel::{already_delivered_from, RuntimeKernel}, orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig};
use orbital_core_runtime::ServerLegCollector;
use {orbital_core_runtime::effects::executor::EffectDelegate, orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "split",
        "typeContractVersion": 1, "orbitals": [
            { "name": "SearchOrbital",
              "entity": { "name": "Query", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [{ "name": "Searcher", "linkedEntity": "Query", "dispatch": "client",
                  "emits": [{ "event": "REQUESTED", "scope": "external" }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "searching" }],
                      "transitions": [{ "from": "idle", "to": "searching", "event": "GO", "effects": [["emit", "REQUESTED", {}]] }] } }] },
            { "name": "TaskOrbital",
              "entity": { "name": "Task", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [{ "name": "Responder", "linkedEntity": "Task", "dispatch": "optimistic",
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "answering" }],
                      "transitions": [{ "from": "idle", "to": "answering", "event": "ASK", "effects": [["fetch", "Task", {}]] }] } }] }
        ]
    }))
    .unwrap();
    let collector = Arc::new(ServerLegCollector::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(collector as Arc<dyn EffectDelegate>),
            ..KernelConfig::default()
        },
    );
    k.register_schema(schema).unwrap();
    k
}

fn host_answer() -> EventResponse {
    EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([
            ("Responder".to_string(), "answering".to_string()),
            ("Searcher".to_string(), "idle".to_string()),
        ]),
        ..EventResponse::default()
    }
}

fn host_row() -> EventResponse {
    EventResponse {
        success: true,
        entity_by_trait: Some(orbital_core_base::value::ValueMap::from([(
            "Searcher".to_string(),
            serde_json::from_value::<Value>(json!({ "id": "q1", "text": "from the host" }))
                .unwrap(),
        )])),
        ..EventResponse::default()
    }
}

#[test]
fn a_host_state_for_a_client_only_trait_is_ignored() {
    let k = kernel();
    let d = k
        .dispatch_with_server_leg("split", "Searcher", None, "GO", Value::Null, None)
        .unwrap();
    k.apply_server_response(&host_answer(), &already_delivered_from(&d))
        .unwrap();
    let state = |t: &str| k.get_state(t, None).map(|s| s.current_state);
    assert_eq!(state("Searcher").as_deref(), Some("searching"));
}

#[test]
fn a_host_state_for_a_trait_that_awaits_the_host_is_still_folded() {
    let k = kernel();
    let d = k
        .dispatch_with_server_leg("split", "Searcher", None, "GO", Value::Null, None)
        .unwrap();
    k.apply_server_response(&host_answer(), &already_delivered_from(&d))
        .unwrap();
    let state = k.get_state("Responder", None).map(|s| s.current_state);
    assert_eq!(state.as_deref(), Some("answering"));
}

#[test]
fn a_host_row_for_a_client_only_trait_is_ignored() {
    let k = kernel();
    k.apply_server_response(&host_row(), &Default::default())
        .unwrap();
    assert_eq!(k.snapshot_trait("Searcher", Some("q1")).entity, Value::Null);
}

#[test]
fn a_host_row_for_a_trait_that_awaits_the_host_is_still_folded() {
    let k = kernel();
    let row =
        serde_json::from_value::<Value>(json!({ "id": "t1", "text": "from the host" })).unwrap();
    let response = EventResponse {
        success: true,
        entity_by_trait: Some(orbital_core_base::value::ValueMap::from([("Responder".to_string(), row.clone())])),
        ..EventResponse::default()
    };
    k.apply_server_response(&response, &Default::default())
        .unwrap();
    assert_eq!(k.snapshot_trait("Responder", Some("t1")).entity, row);
}

// std-confirmation-shaped: Delete runs CONFIRM in the browser, a listener persists through the host. The leg carries
// Delete at its pre-dispatch `from`, and the host echoes that held state back without running Delete.
fn confirm_kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "crm",
        "typeContractVersion": 1, "orbitals": [
            { "name": "ContactOrbital",
              "entity": { "name": "Contact", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [
                { "name": "Delete", "linkedEntity": "Contact",
                  "emits": [{ "event": "CONFIRM", "scope": "external" }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }, { "name": "confirming" }],
                      "transitions": [
                          { "from": "idle", "to": "idle", "event": "INIT", "effects": [["fetch", "Contact", {}]] },
                          { "from": "idle", "to": "confirming", "event": "REQUEST", "effects": [] },
                          { "from": "confirming", "to": "idle", "event": "CONFIRM", "effects": [["emit", "CONFIRM", { "id": "@payload.id" }]] }] } },
                { "name": "Persistor", "linkedEntity": "Contact",
                  "listens": [{ "event": "CONFIRM", "triggers": "DO_DELETE", "scope": "external", "source": { "kind": "trait", "trait": "Delete" } }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "DO_DELETE", "effects": [["persist", "delete", "Contact", "@payload.id"]] }] } }
              ] }
        ]
    }))
    .unwrap();
    let collector = Arc::new(ServerLegCollector::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(collector as Arc<dyn EffectDelegate>),
            ..KernelConfig::default()
        },
    );
    k.register_schema(schema).unwrap();
    k
}

fn persistor_hop() -> orbital_core_runtime::effects::response::TransitionHop {
    orbital_core_runtime::effects::response::TransitionHop {
        trait_name: "Persistor".to_string(),
        from: "idle".to_string(),
        to: "idle".to_string(),
        event: "DO_DELETE".to_string(),
        fired_arm: None,
        guard_result: None,
        effects: Vec::new(),
        observed: None,
    }
}

fn echoed_answer(hops: Vec<orbital_core_runtime::effects::response::TransitionHop>) -> EventResponse {
    EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([
            ("Delete".to_string(), "confirming".to_string()),
            ("Persistor".to_string(), "idle".to_string()),
        ]),
        transitions: Some(hops),
        ..EventResponse::default()
    }
}

fn confirmed(k: &RuntimeKernel) -> orbital_core_runtime::kernel::ClientDispatch {
    let payload: Value = serde_json::from_value(json!({ "id": "c1" })).unwrap();
    k.dispatch_with_server_leg("crm", "Delete", None, "REQUEST", payload.clone(), None).unwrap();
    let d = k.dispatch_with_server_leg("crm", "Delete", None, "CONFIRM", payload, None).unwrap();
    assert!(!d.server_legs.is_empty(), "the listener's persist must leave as a server leg");
    d
}

fn carried(d: &orbital_core_runtime::kernel::ClientDispatch, target: &str) -> HashMap<String, String> {
    let leg = d.server_legs.iter().find(|l| l.target_trait.as_deref() == Some(target)).expect("a leg for the listener");
    leg.traits.iter().flatten().map(|c| (c.r#trait.clone(), c.from.clone())).collect()
}

#[test]
fn a_listener_leg_carries_the_finished_emitter_where_the_client_left_it() {
    let k = confirm_kernel();
    let d = confirmed(&k);
    assert_eq!(carried(&d, "Persistor").get("Delete").map(String::as_str), Some("idle"));
}

#[test]
fn control_a_listener_leg_carries_its_target_at_the_state_it_replays_from() {
    let k = confirm_kernel();
    let d = confirmed(&k);
    let leg = d.server_legs.iter().find(|l| l.target_trait.as_deref() == Some("Persistor")).unwrap();
    let target = leg.traits.iter().flatten().find(|c| c.r#trait == "Persistor").expect("the target is carried");
    assert_eq!((target.from.as_str(), target.event.as_deref()), ("idle", Some("DO_DELETE")));
}

#[test]
fn the_hosts_echo_of_a_listener_leg_does_not_revert_the_emitter() {
    let k = confirm_kernel();
    let d = confirmed(&k);
    let echo = EventResponse {
        success: true,
        transitioned: true,
        states: carried(&d, "Persistor"),
        transitions: Some(vec![persistor_hop()]),
        ..EventResponse::default()
    };
    k.apply_server_response(&echo, &already_delivered_from(&d)).unwrap();
    assert_eq!(k.get_state("Delete", None).map(|s| s.current_state).as_deref(), Some("idle"));
}

#[test]
fn control_a_host_state_for_a_delivered_trait_the_host_ran_is_folded() {
    let k = confirm_kernel();
    let d = confirmed(&k);
    let host_ran_delete = orbital_core_runtime::effects::response::TransitionHop {
        trait_name: "Delete".to_string(),
        from: "idle".to_string(),
        to: "confirming".to_string(),
        event: "CONFIRM".to_string(),
        fired_arm: None,
        guard_result: None,
        effects: Vec::new(),
        observed: None,
    };
    k.apply_server_response(&echoed_answer(vec![persistor_hop(), host_ran_delete]), &already_delivered_from(&d)).unwrap();
    assert_eq!(k.get_state("Delete", None).map(|s| s.current_state).as_deref(), Some("confirming"));
}

#[test]
fn control_a_host_state_outside_the_clients_own_dispatch_is_folded() {
    let k = confirm_kernel();
    k.apply_server_response(&echoed_answer(vec![persistor_hop()]), &Default::default()).unwrap();
    assert_eq!(k.get_state("Delete", None).map(|s| s.current_state).as_deref(), Some("confirming"));
}

fn server_kernel(view_dispatch: &str, view_page: serde_json::Value) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "loader",
        "typeContractVersion": 1, "orbitals": [{ "name": "Play",
            "entity": { "name": "Load", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Loader", "linkedEntity": "Load",
                  "emits": [{ "event": "LOADED", "scope": "external" }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "LOADED", {}]] }] } },
                { "name": "View", "linkedEntity": "Load", "dispatch": view_dispatch,
                  "listens": [{ "event": "LOADED", "triggers": "LOADED", "source": { "kind": "trait", "trait": "Loader" } }],
                  "stateMachine": { "states": [{ "name": "opening", "isInitial": true }, { "name": "open" }],
                      "transitions": [{ "from": "opening", "to": "open", "event": "LOADED" }] } }
            ],
            "pages": [view_page, { "name": "Work", "path": "", "protocol": "boot", "traits": [{ "ref": "Loader" }] }] }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig { environment: RuntimeEnvironment::Server, ..KernelConfig::default() },
    );
    k.register_schema(schema).unwrap();
    k
}

#[test]
fn a_server_never_runs_a_client_only_listener_a_route_page_places() {
    let k = server_kernel("client", json!({ "name": "Home", "path": "/", "traits": [{ "ref": "View" }] }));
    let outcomes = k.dispatch("Loader", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().all(|(t, _)| t != "View"), "the browser runs View's steps (EXEC-ENDPOINT-3a)");
    let response = EventResponse::from_process_results(&outcomes, "Loader", "GO");
    let loaded = response.emitted_events.iter().find(|e| e.event == "LOADED").expect("LOADED is answered");
    assert_ne!(loaded.source.as_ref().and_then(|s| s.dispatched), Some(true), "no listener ran it on the host, so the client delivers it");
}

#[test]
fn control_a_server_runs_an_optimistic_listener_of_its_emit() {
    let k = server_kernel("optimistic", json!({ "name": "Home", "path": "/", "traits": [{ "ref": "View" }] }));
    let outcomes = k.dispatch("Loader", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().any(|(t, r)| t == "View" && r.new_state == "open"), "{outcomes:?}");
}

#[test]
fn control_a_host_runs_a_client_only_listener_no_route_places_as_a_cli_host_runs_std_args() {
    let k = server_kernel("client", json!({ "name": "Cli", "path": "", "protocol": "boot", "traits": [{ "ref": "View" }] }));
    let outcomes = k.dispatch("Loader", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().any(|(t, r)| t == "View" && r.new_state == "open"), "no browser runs it, so the host does: {outcomes:?}");
}
