// A host's state and row for a client-only trait never overwrite the client's (twin of `withoutClientOnlyRuns`, `almadar-runtime/src/evaluation/client-role.ts`; ui `BrowserPlayground.cross-orbital-answer`).

use orbital_core_base::json_value::from_json_value;
use {orbital_core_runtime::kernel::{already_delivered_from, RuntimeKernel}, orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig};
use orbital_core_runtime::ServerLegCollector;
use {orbital_core_runtime::effects::executor::EffectDelegate, orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = from_json_value(&json!({
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
              "traits": [{ "name": "Responder", "linkedEntity": "Task", "dispatch": "server",
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
            from_json_value(&json!({ "id": "q1", "text": "from the host" })).unwrap(),
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
    assert_eq!(k.snapshot_trait("Searcher", None).entity, Value::Null);
    assert_eq!(k.snapshot_trait("Searcher", Some("q1")).entity, Value::Null);
}

#[test]
fn a_host_row_for_a_trait_that_awaits_the_host_is_still_folded() {
    let k = kernel();
    let row =
        from_json_value::<Value>(&json!({ "id": "t1", "text": "from the host" })).unwrap();
    let response = EventResponse {
        success: true,
        frames: Some(orbital_core_runtime::effects::FrameDelta::absolute([("Responder".to_string(), row.as_object().unwrap().clone())])),
        ..EventResponse::default()
    };
    k.apply_server_response(&response, &Default::default())
        .unwrap();
    assert_eq!(k.snapshot_trait("Responder", None).entity, row);
}

// std-confirmation-shaped: Delete runs CONFIRM in the browser, a listener persists through the host. The leg carries
// Delete at its pre-dispatch `from`, and the host echoes that held state back without running Delete.
fn confirm_kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = from_json_value(&json!({
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
    let payload: Value = from_json_value(&json!({ "id": "c1" })).unwrap();
    k.dispatch_with_server_leg("crm", "Delete", None, "REQUEST", payload.clone(), None).unwrap();
    let d = k.dispatch_with_server_leg("crm", "Delete", None, "CONFIRM", payload, None).unwrap();
    assert!(!d.server_legs.is_empty(), "the persist must leave as a server leg");
    d
}

fn carried(d: &orbital_core_runtime::kernel::ClientDispatch, target: &str) -> HashMap<String, String> {
    let leg = d.server_legs.iter().find(|l| l.target_trait.as_deref() == Some(target)).expect("a leg for that trait");
    leg.traits.iter().flatten().map(|c| (c.r#trait.clone(), c.from.clone())).collect()
}

/// EXEC-DIST-10 / EXEC-DIST-2: the server emitter that moved is the leg, so a stateful host, which is carried no states,
/// runs its step too; its listener rides the host's cascade from it.
#[test]
fn a_server_emitter_that_moved_is_the_leg_and_replays_from_where_it_was() {
    let k = confirm_kernel();
    let d = confirmed(&k);
    let shapes: Vec<(&str, Option<&str>)> = d.server_legs.iter().map(|l| (l.event.as_str(), l.target_trait.as_deref())).collect();
    assert_eq!(shapes, vec![("CONFIRM", Some("Delete"))]);
    assert_eq!(carried(&d, "Delete").get("Delete").map(String::as_str), Some("confirming"));
}

#[test]
fn control_the_emitter_leg_carries_its_listener_at_the_state_the_host_cascades_it_from() {
    let k = confirm_kernel();
    let d = confirmed(&k);
    let leg = d.server_legs.iter().find(|l| l.target_trait.as_deref() == Some("Delete")).unwrap();
    let listener = leg.traits.iter().flatten().find(|c| c.r#trait == "Persistor").expect("the listener is carried");
    assert_eq!((listener.from.as_str(), listener.event.as_deref()), ("idle", Some("DO_DELETE")));
}

#[test]
fn the_hosts_echo_of_the_emitter_leg_does_not_revert_the_emitter() {
    let k = confirm_kernel();
    let d = confirmed(&k);
    let host_ran_delete = orbital_core_runtime::effects::response::TransitionHop {
        trait_name: "Delete".to_string(),
        from: "confirming".to_string(),
        to: "idle".to_string(),
        event: "CONFIRM".to_string(),
        fired_arm: None,
        guard_result: None,
        effects: Vec::new(),
        observed: None,
    };
    let echo = EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([("Delete".to_string(), "idle".to_string()), ("Persistor".to_string(), "idle".to_string())]),
        transitions: Some(vec![host_ran_delete, persistor_hop()]),
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
    let schema: OrbitalSchema = from_json_value(&json!({
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
    let schema: OrbitalSchema = from_json_value(&json!({
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
    from_json_value(&json!({
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
    let guest: Value = from_json_value(&json!({ "program": {
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
    from_json_value(&schema).unwrap()
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
    let browser = orbital_core_runtime::embed::EmbeddedKernel::new(from_json_value(&schema).unwrap(), "c1").unwrap();
    let seeds = browser.route_mounts("/");
    assert_eq!(seeds.iter().filter(|s| s.r#trait == "Stage").count(), 1, "{seeds:?}");
}

// EXEC-DIST-4/8: the browser runs a client-only trait and paints it itself; a host's paint of it never reaches the browser.
#[test]
fn the_browser_view_carries_no_render_of_a_client_only_trait_and_keeps_the_others() {
    let server = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig { environment: RuntimeEnvironment::Server, ..KernelConfig::default() },
    );
    server.register_schema(embedded_stage_schema()).unwrap();
    let mut outcomes = server.dispatch("Frame", None, "INIT", Value::Null).unwrap();
    outcomes.extend(server.dispatch("Stage", None, "PAINTED", from_json_value(&json!({ "guest": "g" })).unwrap()).unwrap());
    let full = EventResponse::from_process_results(&outcomes, "Frame", "INIT");
    let painted = |r: &EventResponse| r.client_effects_by_trait.iter().flatten().map(|e| e.trait_name.clone()).collect::<Vec<_>>();
    assert!(painted(&full).iter().any(|t| t == "Frame"), "control: the host painted the client trait in process: {:?}", painted(&full));
    let view = server.client_view(full);
    assert!(painted(&view).iter().all(|t| t != "Frame" && t != "Stage"), "{:?}", painted(&view));
}

// EXEC-DIST-8: the rows a server-run trait fetched are the host's; the browser view carries them only when a browser-run
// trait fetched them or is bound to their entity (G-CROSS-565).
fn fetched_rows_schema(reader_dispatch: &str, view_entity: &str) -> OrbitalSchema {
    from_json_value(&json!({
        "name": "rows",
        "typeContractVersion": 1, "orbitals": [
            { "name": "Events",
              "entity": { "name": "Event", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }],
                  "instances": [{ "id": "e1" }, { "id": "e2" }] },
              "traits": [{ "name": "Reader", "linkedEntity": "Event", "dispatch": reader_dispatch,
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "READ", "effects": [["fetch", "Event", {}]] }] } }] },
            { "name": "Board",
              "entity": { "name": "BoardRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [{ "name": "View", "linkedEntity": view_entity, "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }] } }] }
        ]
    }))
    .unwrap()
}

fn read_view(reader_dispatch: &str, view_entity: &str) -> EventResponse {
    let server = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig { environment: RuntimeEnvironment::Server, ..KernelConfig::default() },
    );
    server.register_schema(fetched_rows_schema(reader_dispatch, view_entity)).unwrap();
    let outcomes = server.dispatch("Reader", None, "READ", Value::Null).unwrap();
    let full = EventResponse::from_process_results(&outcomes, "Reader", "READ");
    assert_eq!(full.data.as_ref().and_then(|d| d.get("Event")).map(Vec::len), Some(2), "the host's own response keeps the rows: {full:?}");
    server.client_view(full)
}

fn fetch_rows_sent(view: &EventResponse) -> usize {
    view.effect_results.iter().flatten().filter_map(|r| r.data.as_ref()).filter_map(Value::as_array).map(Vec::len).sum()
}

#[test]
fn rows_only_a_server_trait_fetched_stay_off_the_browser() {
    let view = read_view("server", "BoardRun");
    assert!(view.data.as_ref().is_none_or(|d| !d.contains_key("Event")), "{view:?}");
    assert_eq!(fetch_rows_sent(&view), 0, "the fetch result carries no rows either: {view:?}");
}

#[test]
fn control_rows_a_browser_run_trait_fetched_reach_the_browser() {
    let view = read_view("optimistic", "BoardRun");
    assert_eq!(view.data.as_ref().and_then(|d| d.get("Event")).map(Vec::len), Some(2), "{view:?}");
}

#[test]
fn control_rows_of_an_entity_a_browser_run_trait_is_bound_to_reach_the_browser() {
    let view = read_view("server", "Event");
    assert_eq!(view.data.as_ref().and_then(|d| d.get("Event")).map(Vec::len), Some(2), "{view:?}");
}

// A page remount on a host that keeps the session (EXEC-DIST-9): a browser-run trait's host copy goes stale once a leg ran
// it there and the browser took the rest of its steps itself, so its lifecycle event runs from the state the mount
// carries, the browser's. KFlow /home → path detail → /home: the paths front came back `busy` with no request in flight
// and its asks waited forever (2026-11-16).
fn front_schema() -> OrbitalSchema {
    from_json_value(&json!({
        "name": "fronted", "typeContractVersion": 1,
        "orbitals": [{ "name": "Main",
            "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Front", "linkedEntity": "Run", "dispatch": "client",
                  "emits": [{ "event": "FWD", "scope": "external" }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }], "transitions": [
                      { "from": "idle", "to": "idle", "event": "INIT" },
                      { "from": "busy", "to": "busy", "event": "INIT" },
                      { "from": "idle", "to": "busy", "event": "ASK", "effects": [["emit", "FWD", {}]] },
                      { "from": "busy", "to": "idle", "event": "DONE" }] } },
                { "name": "Worker", "linkedEntity": "Run",
                  "listens": [{ "event": "FWD", "triggers": "WORK", "source": { "kind": "trait", "trait": "Front" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                      { "from": "idle", "to": "idle", "event": "WORK" }] } }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Front" }] },
                      { "name": "Work", "path": "", "protocol": "boot", "traits": [{ "ref": "Worker" }] }] }]
    }))
    .unwrap()
}

fn front_server() -> orbital_core_runtime::embed::EmbeddedServer {
    orbital_core_runtime::embed::EmbeddedServer::new(front_schema()).unwrap()
}

fn front_send(server: &orbital_core_runtime::embed::EmbeddedServer, request: serde_json::Value) -> EventResponse {
    let request: orbital_core_runtime::effects::response::EventRequest = from_json_value(&request).unwrap();
    match server.start("Main", request).unwrap() {
        orbital_core_runtime::embed::ServerProgress::Done(response) => response,
        _ => panic!("the request ran to its end"),
    }
}

#[test]
fn a_remount_runs_a_browser_run_traits_lifecycle_from_the_state_the_browser_carries() {
    let server = front_server();
    let asked = front_send(&server, json!({ "event": "ASK", "targetTrait": "Front", "clientId": "tab",
        "traits": [{ "trait": "Front", "from": "idle", "event": "ASK" }] }));
    assert_eq!(asked.states.get("Front").map(String::as_str), Some("busy"), "the leg ran Front on the host: {asked:?}");
    let remounted = front_send(&server, json!({ "event": "INIT", "clientId": "tab", "route": "/",
        "mount": [{ "trait": "Front", "event": "INIT" }], "traits": [{ "trait": "Front", "from": "idle" }] }));
    assert_eq!(remounted.states.get("Front").map(String::as_str), Some("idle"), "{remounted:?}");
}

#[test]
fn control_a_remount_runs_a_server_traits_lifecycle_from_the_host_state() {
    let server = front_server();
    front_send(&server, json!({ "event": "ASK", "targetTrait": "Front", "clientId": "tab",
        "traits": [{ "trait": "Front", "from": "idle", "event": "ASK" }] }));
    let remounted = front_send(&server, json!({ "event": "INIT", "clientId": "tab", "route": "",
        "mount": [{ "trait": "Worker", "event": "INIT" }], "traits": [{ "trait": "Worker", "from": "elsewhere" }] }));
    assert_ne!(remounted.states.get("Worker").map(String::as_str), Some("elsewhere"), "a server trait's carried copy is ignored: {remounted:?}");
}

fn front_client() -> RuntimeKernel {
    let collector = Arc::new(ServerLegCollector::new());
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig { environment: RuntimeEnvironment::Client, delegate: Some(collector as Arc<dyn EffectDelegate>), ..KernelConfig::default() },
    );
    k.register_schema(front_schema()).unwrap();
    k
}

fn front_hop(from: &str, to: &str, event: &str) -> orbital_core_runtime::effects::response::TransitionHop {
    orbital_core_runtime::effects::response::TransitionHop {
        trait_name: "Front".to_string(), from: from.to_string(), to: to.to_string(), event: event.to_string(),
        fired_arm: None, guard_result: None, effects: Vec::new(), observed: None,
    }
}

fn front_reply(state: &str, hop: orbital_core_runtime::effects::response::TransitionHop) -> EventResponse {
    EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([("Front".to_string(), state.to_string())]),
        transitions: Some(vec![hop]),
        ..EventResponse::default()
    }
}

#[test]
fn a_host_hop_the_browser_has_moved_past_does_not_roll_a_browser_run_trait_back() {
    let k = front_client();
    k.dispatch_with_server_leg("fronted", "Front", None, "ASK", Value::Null, None).unwrap();
    assert_eq!(k.get_state("Front", None).map(|s| s.current_state).as_deref(), Some("busy"));
    k.apply_server_response(&front_reply("idle", front_hop("idle", "idle", "INIT")), &Default::default()).unwrap();
    assert_eq!(k.get_state("Front", None).map(|s| s.current_state).as_deref(), Some("busy"), "the INIT echo is older than the browser's ASK");
}

#[test]
fn control_a_host_hop_from_the_browsers_state_is_folded() {
    let k = front_client();
    k.apply_server_response(&front_reply("busy", front_hop("idle", "busy", "ASK")), &Default::default()).unwrap();
    assert_eq!(k.get_state("Front", None).map(|s| s.current_state).as_deref(), Some("busy"));
}

// A boot-mounted worker steps in the program session (EXEC-FIELD-3), so the row it writes is never the viewer's: a leg's
// answer carries no frame for it, keyed or not. KFlow path detail: the presence ask's keyed social row came back as a
// frame the browser does not declare, and the page refused the answer (2026-11-16).
fn worker_schema(keyed: bool) -> OrbitalSchema {
    let listen = if keyed {
        json!({ "event": "FWD", "triggers": "WORK", "source": { "kind": "trait", "trait": "Front" }, "key": "@payload.key" })
    } else {
        json!({ "event": "FWD", "triggers": "WORK", "source": { "kind": "trait", "trait": "Front" } })
    };
    from_json_value(&json!({
        "name": "worked", "typeContractVersion": 1,
        "orbitals": [{ "name": "Main",
            "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "note", "type": "string", "default": "" }] },
            "traits": [
                { "name": "Front", "linkedEntity": "Run", "dispatch": "client",
                  "emits": [{ "event": "FWD", "scope": "external", "payloadSchema": [{ "name": "key", "type": "string" }] }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }], "transitions": [
                      { "from": "idle", "to": "idle", "event": "INIT" },
                      { "from": "idle", "to": "busy", "event": "ASK", "effects": [["emit", "FWD", { "key": "k1" }]] }] } },
                { "name": "Worker", "linkedEntity": "Run",
                  "listens": [listen],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                      { "from": "idle", "to": "idle", "event": "WORK", "effects": [["set", "@entity.note", "worked"]] }] } }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Front" }] },
                      { "name": "Work", "path": "", "protocol": "boot", "traits": [{ "ref": "Worker" }] }] }]
    }))
    .unwrap()
}

fn leg_frames(keyed: bool) -> Vec<String> {
    let server = orbital_core_runtime::embed::EmbeddedServer::new(worker_schema(keyed)).unwrap();
    let request: orbital_core_runtime::effects::response::EventRequest = from_json_value(&json!({ "event": "ASK", "targetTrait": "Front", "clientId": "tab",
        "traits": [{ "trait": "Front", "from": "idle", "event": "ASK" }] })).unwrap();
    let response = match server.start("Main", request).unwrap() {
        orbital_core_runtime::embed::ServerProgress::Done(response) => response,
        _ => panic!("the request ran to its end"),
    };
    response.frames.iter().flat_map(|delta| delta.ops.iter()).flat_map(|op| match op {
        orbital_core_runtime::effects::frame_delta::FrameOp::Put { frame, .. } | orbital_core_runtime::effects::frame_delta::FrameOp::Del { frame } => vec![frame.clone()],
        orbital_core_runtime::effects::frame_delta::FrameOp::Set { frames } => frames.iter().map(|(f, _)| f.clone()).collect(),
        orbital_core_runtime::effects::frame_delta::FrameOp::Clear => Vec::new(),
    }).collect()
}

#[test]
fn a_leg_answer_carries_no_frame_of_a_keyed_boot_worker_row() {
    let frames = leg_frames(true);
    assert!(frames.iter().all(|f| !f.starts_with("Worker")), "{frames:?}");
}

#[test]
fn a_leg_answer_carries_no_frame_of_an_unkeyed_boot_worker() {
    let frames = leg_frames(false);
    assert!(frames.iter().all(|f| !f.starts_with("Worker")), "{frames:?}");
}

#[test]
fn control_a_leg_answer_still_carries_the_asking_traits_own_frame() {
    let frames = leg_frames(true);
    assert!(frames.iter().any(|f| f.starts_with("Front") || f.starts_with("$shared::Run")), "{frames:?}");
}

/// An undeclared trait whose step owed the host nothing ran here (std-approval-request's pipeline mounts in the
/// browser and posts no leg); a host that never ran it reports its never-run default, which must not undo the step.
#[test]
fn a_host_state_for_an_awaited_trait_the_client_stepped_and_the_host_did_not_run_is_ignored() {
    let k = confirm_kernel();
    let payload: Value = from_json_value(&json!({ "id": "c1" })).unwrap();
    let d = k.dispatch_with_server_leg("crm", "Delete", None, "REQUEST", payload, None).unwrap();
    assert!(d.server_legs.is_empty(), "REQUEST owes the host nothing");
    let report = EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([("Delete".to_string(), "idle".to_string())]),
        transitions: Some(vec![persistor_hop()]),
        ..EventResponse::default()
    };
    k.apply_server_response(&report, &Default::default()).unwrap();
    assert_eq!(k.get_state("Delete", None).map(|s| s.current_state).as_deref(), Some("confirming"));
}

#[test]
fn control_a_host_state_for_an_awaited_trait_the_host_ran_from_the_clients_state_is_folded() {
    let k = confirm_kernel();
    let payload: Value = from_json_value(&json!({ "id": "c1" })).unwrap();
    k.dispatch_with_server_leg("crm", "Delete", None, "REQUEST", payload, None).unwrap();
    let host_ran_delete = orbital_core_runtime::effects::response::TransitionHop {
        trait_name: "Delete".to_string(),
        from: "confirming".to_string(),
        to: "idle".to_string(),
        event: "CONFIRM".to_string(),
        fired_arm: None,
        guard_result: None,
        effects: Vec::new(),
        observed: None,
    };
    let report = EventResponse {
        success: true,
        transitioned: true,
        states: HashMap::from([("Delete".to_string(), "idle".to_string())]),
        transitions: Some(vec![host_ran_delete]),
        ..EventResponse::default()
    };
    k.apply_server_response(&report, &Default::default()).unwrap();
    assert_eq!(k.get_state("Delete", None).map(|s| s.current_state).as_deref(), Some("idle"));
}
