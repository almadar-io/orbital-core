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
        frames: Some(orbital_core_runtime::effects::FrameDelta::absolute([(
            "Searcher".to_string(),
            serde_json::from_value(json!({ "id": "q1", "text": "from the host" })).unwrap(),
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
        frames: Some(orbital_core_runtime::effects::FrameDelta::absolute([("Responder".to_string(), row.as_object().unwrap().clone())])),
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

/// `View` (`[client]`) hears `Loader.LOADED`; it is on no page itself, but `Frame` embeds it with `@trait.View`, and
/// `frame_page` places `Frame`.
fn embedded_view_kernel(frame_page: serde_json::Value) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "loader",
        "typeContractVersion": 1, "orbitals": [{ "name": "Play",
            "entity": { "name": "Load", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Loader", "linkedEntity": "Load",
                  "emits": [{ "event": "LOADED", "scope": "external" }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "LOADED", {}]] }] } },
                { "name": "Frame", "linkedEntity": "Load", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [
                          ["render-ui", "main", { "type": "stack", "children": ["@trait.View"] }]
                      ] }] } },
                { "name": "View", "linkedEntity": "Load", "dispatch": "client",
                  "listens": [{ "event": "LOADED", "triggers": "LOADED", "source": { "kind": "trait", "trait": "Loader" } }],
                  "stateMachine": { "states": [{ "name": "opening", "isInitial": true }, { "name": "open" }],
                      "transitions": [{ "from": "opening", "to": "open", "event": "LOADED" }] } }
            ],
            "pages": [frame_page, { "name": "Work", "path": "", "protocol": "boot", "traits": [{ "ref": "Loader" }] }] }]
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
fn a_server_never_runs_a_client_only_listener_a_route_page_trait_embeds() {
    let k = embedded_view_kernel(json!({ "name": "Home", "path": "/", "traits": [{ "ref": "Frame" }] }));
    let outcomes = k.dispatch("Loader", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().all(|(t, _)| t != "View"), "the browser runs the embedded View (EXEC-DIST-1, EXEC-EVAL-9): {outcomes:?}");
}

#[test]
fn control_a_host_runs_a_client_only_listener_only_a_boot_trait_embeds() {
    let k = embedded_view_kernel(json!({ "name": "Cli", "path": "", "protocol": "boot", "traits": [{ "ref": "Frame" }] }));
    let outcomes = k.dispatch("Loader", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().any(|(t, r)| t == "View" && r.new_state == "open"), "no browser shows it, so the host runs it: {outcomes:?}");
}

/// One program on both sides: `Session` (server) emits `PAINTED`; `Frame` (`[client]`, on the route page) embeds
/// `@trait.Stage`; `Stage` (`[client]`, on no page) hears `Session.PAINTED`.
fn embedded_stage_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "stage",
        "typeContractVersion": 1, "orbitals": [{ "name": "Play",
            "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Session", "linkedEntity": "Run",
                  "emits": [{ "event": "PAINTED", "scope": "external" }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [
                          { "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "PAINTED", { "guest": "guest-1" }]] },
                          { "from": "idle", "to": "idle", "event": "OPEN", "effects": [
                              ["program/open", "@payload.program", {}, { "emit": { "success": "OPENED", "failure": "OPEN_FAILED" } }]
                          ] },
                          { "from": "idle", "to": "idle", "event": "OPENED", "effects": [["emit", "PAINTED", { "guest": "@payload.result.guest" }]] },
                          { "from": "idle", "to": "idle", "event": "OPEN_FAILED" }
                      ] } },
                { "name": "Frame", "linkedEntity": "Run", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [
                          ["render-ui", "main", { "type": "stack", "children": ["@trait.Stage"] }]
                      ] }] } },
                { "name": "Stage", "linkedEntity": "Run", "dispatch": "client",
                  "listens": [{ "event": "PAINTED", "triggers": "PAINTED", "source": { "kind": "trait", "trait": "Session" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }],
                      "transitions": [{ "from": "idle", "to": "shown", "event": "PAINTED", "effects": [
                          ["set", "@entity.guest", "@payload.guest"],
                          ["render-ui", "main", { "type": "typography", "content": "@payload.guest" }]
                      ] }] } }
            ],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Frame" }, { "ref": "Session" }] }] }]
    }))
    .unwrap()
}

#[test]
fn twin_a_server_emit_reaches_the_browsers_embedded_client_listener_which_runs_it_there() {
    let server = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig { environment: RuntimeEnvironment::Server, ..KernelConfig::default() },
    );
    server.register_schema(embedded_stage_schema()).unwrap();
    let outcomes = server.dispatch("Session", None, "GO", Value::Null).unwrap();
    assert!(outcomes.iter().all(|(t, _)| t != "Stage"), "the server leaves Stage to the browser: {outcomes:?}");
    let answer = server.client_view(EventResponse::from_process_results(&outcomes, "Session", "GO"));
    assert!(answer.emitted_events.iter().any(|e| e.event == "PAINTED"), "the browser is told PAINTED: {answer:?}");

    let browser = orbital_core_runtime::embed::EmbeddedKernel::new(embedded_stage_schema(), "c1").unwrap();
    browser.apply_server_response(&answer, &std::collections::HashSet::new()).unwrap();
    assert_eq!(browser.get_state("Stage", None), "shown", "the browser's Stage ran PAINTED");
}

#[test]
fn twin_a_server_legs_emit_reaches_the_browsers_embedded_client_listener_when_the_browser_folds_its_own_leg() {
    let server = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig { environment: RuntimeEnvironment::Server, ..KernelConfig::default() },
    );
    server.register_schema(embedded_stage_schema()).unwrap();
    let browser = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::for_environment(RuntimeEnvironment::Client),
    );
    browser.register_schema(embedded_stage_schema()).unwrap();
    browser.set_host_fans_out(true);
    let guest: Value = serde_json::from_value(json!({ "program": {
        "name": "Tiny", "typeContractVersion": 1,
        "orbitals": [{ "name": "Tiny", "entity": { "name": "T", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "TinyView", "linkedEntity": "T", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "typography", "content": "tiny" }]] }] } }],
            "pages": [{ "name": "TinyPage", "path": "/tiny", "traits": [{ "ref": "TinyView" }] }] }]
    } }))
    .unwrap();
    let d = browser.dispatch_with_server_leg("stage", "Session", None, "OPEN", guest.clone(), None).unwrap();
    let outcomes = server.dispatch("Session", None, "OPEN", guest).unwrap();
    assert!(outcomes.iter().all(|(t, _)| t != "Stage"), "the server leaves Stage to the browser: {outcomes:?}");
    let answer = server.client_view(EventResponse::from_process_results(&outcomes, "Session", "OPEN"));
    assert!(answer.emitted_events.iter().any(|e| e.event == "PAINTED"), "the browser is told PAINTED: {answer:?}");
    browser.apply_server_response(&answer, &already_delivered_from(&d)).unwrap();
    assert_eq!(
        browser.get_state("Stage", None).map(|s| s.current_state).as_deref(),
        Some("shown"),
        "the browser's Stage ran the leg's PAINTED"
    );
}

fn stage_with_init_schema() -> OrbitalSchema {
    let mut schema = serde_json::to_value(embedded_stage_schema()).unwrap();
    let stage = schema["orbitals"][0]["traits"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|t| t["name"] == "Stage")
        .unwrap();
    stage["stateMachine"]["transitions"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "from": "idle", "to": "idle", "event": "INIT", "effects": [["set", "@entity.guest", ""]] }));
    serde_json::from_value(schema).unwrap()
}

fn entered_browser(schema: OrbitalSchema) -> (orbital_core_runtime::embed::EmbeddedKernel, Vec<String>) {
    let browser = orbital_core_runtime::embed::EmbeddedKernel::new(schema, "c1").unwrap();
    let circuit = browser.page_circuit(None, Some("/")).unwrap();
    let mut traits: Vec<String> = circuit.mounts.iter().map(|s| s.r#trait.clone()).collect();
    traits.extend(circuit.embedded.iter().filter(|t| !traits.contains(t)).cloned().collect::<Vec<_>>());
    let entered = browser.enter(Some("/"), "c1\u{0}/", traits).unwrap();
    let seeded: Vec<String> = entered.seeds.iter().map(|s| s.r#trait.clone()).collect();
    browser.dispatch_mount(&entered.seeds, None, Value::Null).unwrap();
    (browser, seeded)
}

fn painted_answer() -> EventResponse {
    let server = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig { environment: RuntimeEnvironment::Server, ..KernelConfig::default() },
    );
    server.register_schema(stage_with_init_schema()).unwrap();
    let outcomes = server.dispatch("Session", None, "GO", Value::Null).unwrap();
    server.client_view(EventResponse::from_process_results(&outcomes, "Session", "GO"))
}

#[test]
fn a_route_mount_seeds_the_lifecycle_of_a_trait_a_page_trait_embeds_so_its_deliveries_are_not_held() {
    let (browser, seeded) = entered_browser(stage_with_init_schema());
    assert!(seeded.contains(&"Stage".to_string()), "the embedded Stage is seeded with the page: {seeded:?}");
    browser.apply_server_response(&painted_answer(), &std::collections::HashSet::new()).unwrap();
    assert_eq!(browser.get_state("Stage", None), "shown", "the browser's embedded Stage ran PAINTED");
}

#[test]
fn control_an_embedded_trait_with_no_lifecycle_event_is_not_seeded_and_still_hears_its_listens() {
    let (browser, seeded) = entered_browser(embedded_stage_schema());
    assert!(!seeded.contains(&"Stage".to_string()), "Stage handles no lifecycle event: {seeded:?}");
    browser.apply_server_response(&painted_answer(), &std::collections::HashSet::new()).unwrap();
    assert_eq!(browser.get_state("Stage", None), "shown");
}

#[test]
fn edge_a_trait_both_listed_and_embedded_is_seeded_once() {
    let mut schema = serde_json::to_value(stage_with_init_schema()).unwrap();
    schema["orbitals"][0]["pages"][0]["traits"].as_array_mut().unwrap().push(json!({ "ref": "Stage" }));
    let browser = orbital_core_runtime::embed::EmbeddedKernel::new(serde_json::from_value(schema).unwrap(), "c1").unwrap();
    let seeds = browser.route_mounts("/");
    assert_eq!(seeds.iter().filter(|s| s.r#trait == "Stage").count(), 1, "{seeds:?}");
}
