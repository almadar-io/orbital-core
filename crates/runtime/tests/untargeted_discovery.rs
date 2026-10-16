// A request that names no trait is handed to every trait whose held state has an arm for the event, not to the orbital's first trait (twin of `evaluateOrbitalEvent` discovery, `almadar-runtime/src/evaluation/evaluateOrbitalEvent.ts`).

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse, TransitionRejectionCode}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn tr(name: &str, arms: Json) -> Json {
    json!({
        "name": name, "linkedEntity": "Row",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
            "transitions": arms
        }
    })
}

fn go(to: &str, text: &str) -> Json {
    json!({ "from": "idle", "to": to, "event": "GO",
        "effects": [["render-ui", "main", { "type": "typography", "content": text }]] })
}

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Row", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                tr("First", json!([{ "from": "idle", "to": "idle", "event": "OTHER" }])),
                tr("Second", json!([go("busy", "second")])),
                tr("Third", json!([go("idle", "third")])),
            ]
        }]
    }))
    .unwrap()
}

fn names() -> Vec<String> {
    ["First", "Second", "Third"].map(String::from).to_vec()
}

fn kernel() -> RuntimeKernel {
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema()).unwrap();
    k
}

#[test]
fn discovery_lists_every_trait_whose_held_state_handles_the_event() {
    assert_eq!(
        kernel().discover_handlers(&names(), None, "GO"),
        ["Second", "Third"]
    );
}

#[test]
fn control_a_trait_moved_past_the_arm_is_not_discovered() {
    let k = kernel();
    k.dispatch("Second", None, "GO", Value::Null).unwrap();
    assert_eq!(k.discover_handlers(&names(), None, "GO"), ["Third"]);
}

#[test]
fn edge_no_trait_handling_the_event_discovers_nothing() {
    assert!(kernel()
        .discover_handlers(&names(), None, "NOPE")
        .is_empty());
}

#[test]
fn dispatch_each_runs_every_discovered_trait_and_only_those() {
    let k = kernel();
    let handlers = k.discover_handlers(&names(), None, "GO");
    let ran: Vec<String> = k
        .dispatch_each(&handlers, None, "GO", Value::Null)
        .unwrap()
        .into_iter()
        .filter(|(_, r)| r.executed)
        .map(|(t, _)| t)
        .collect();
    assert_eq!(ran, ["Second", "Third"]);
}

fn server() -> EmbeddedServer {
    EmbeddedServer::new(schema()).unwrap()
}

fn start(server: &EmbeddedServer, request: EventRequest) -> EventResponse {
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match pending.request {
                            IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
                            other => panic!("the fixture asks for {other:?}"),
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn request(event: &str, target: Option<&str>) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        target_trait: target.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn an_untargeted_request_reaches_every_handler_and_echoes_the_held_states() {
    let response = start(&server(), request("GO", None));
    assert!(response.transitioned);
    let tagged: Vec<&str> = response
        .client_effects_by_trait
        .iter()
        .flatten()
        .map(|e| e.trait_name.as_str())
        .collect();
    assert_eq!(tagged, ["Second", "Third"]);
    assert_eq!(
        response.states.get("Second").map(String::as_str),
        Some("busy")
    );
    assert_eq!(
        response.states.get("Third").map(String::as_str),
        Some("idle")
    );
    assert_eq!(
        response.states.get("First").map(String::as_str),
        Some("idle")
    );
}

#[test]
fn control_a_targeted_request_runs_only_its_trait() {
    let response = start(&server(), request("GO", Some("Third")));
    let tagged: Vec<&str> = response
        .client_effects_by_trait
        .iter()
        .flatten()
        .map(|e| e.trait_name.as_str())
        .collect();
    assert_eq!(tagged, ["Third"]);
}

#[test]
fn an_untargeted_request_nothing_handles_is_a_no_dispatchable_traits_rejection() {
    let response = start(&server(), request("NOPE", None));
    assert!(response.success);
    assert!(!response.transitioned);
    let rejections = response.rejections.expect("a rejection");
    assert_eq!(rejections.len(), 1);
    assert_eq!(
        rejections[0].code,
        TransitionRejectionCode::NoDispatchableTraits
    );
    assert_eq!(rejections[0].event.as_deref(), Some("NOPE"));
    assert_eq!(
        response.states.len(),
        3,
        "the held states of the whole orbital are echoed"
    );
}

#[test]
fn the_in_process_bridge_discovers_handlers_for_an_untargeted_event() {
    use orbital_core_runtime::client::bridge::{KernelBridge, ServerBridge};
    let bridge = KernelBridge::new(schema()).unwrap();
    let response = bridge
        .send_event_with_options("Main", "GO", None, None, Value::Null)
        .unwrap();
    assert_eq!(
        response.states.get("Second").map(String::as_str),
        Some("busy")
    );
    assert_eq!(
        response.states.get("Third").map(String::as_str),
        Some("idle")
    );
    let none = bridge
        .send_event_with_options("Main", "NOPE", None, None, Value::Null)
        .unwrap();
    assert_eq!(
        none.rejections.expect("rejection")[0].code,
        TransitionRejectionCode::NoDispatchableTraits
    );
}

#[test]
fn control_the_in_process_bridge_runs_a_named_trait_alone() {
    use orbital_core_runtime::client::bridge::{KernelBridge, ServerBridge};
    let bridge = KernelBridge::new(schema()).unwrap();
    let response = bridge
        .send_event_with_options("Main", "GO", None, Some("Third"), Value::Null)
        .unwrap();
    assert_eq!(response.states.get("Second"), None);
}
