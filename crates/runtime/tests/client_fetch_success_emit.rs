// A client kernel whose seed delegated a `fetch` to the server must leave `loading` once the server's response (carrying the fetch's success emit) is applied.

use {orbital_core_base::bus_event::{BusEventSource, EmittedEvent}, orbital_core_runtime::effects::{EffectDelegate, EventResponse}};
use {orbital_core_runtime::kernel::{already_delivered_from, RuntimeKernel}, orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig};
use orbital_core_runtime::ServerLegCollector;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

fn schema(fetching: bool) -> OrbitalSchema {
    let init_effects = if fetching {
        json!([["fetch", "Item", { "emit": { "success": "Loaded", "failure": "LoadFailed" } }]])
    } else {
        json!([])
    };
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Browse",
                    "linkedEntity": "Item",
                    "stateMachine": {
                        "states": [
                            { "name": "loading", "isInitial": true },
                            { "name": "browsing" }
                        ],
                        "transitions": [
                            { "from": "loading", "to": "loading", "event": "INIT", "effects": init_effects },
                            { "from": "loading", "to": "browsing", "event": "Loaded" },
                            { "from": "browsing", "to": "browsing", "event": "Loaded" }
                        ]
                    }
                },
                {
                    "name": "Relay",
                    "linkedEntity": "Item",
                    "listens": [{
                        "event": "Loaded", "triggers": "RelayLoaded",
                        "source": { "kind": "trait", "trait": "Browse" },
                        "payloadMapping": { "rows": "@payload.data" }
                    }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }],
                        "transitions": [
                            { "from": "idle", "to": "shown", "event": "RelayLoaded" },
                            { "from": "shown", "to": "shown", "event": "RelayLoaded" }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn row(id: &str) -> Value {
    serde_json::from_value(json!({ "id": id })).unwrap()
}

fn server(schema: OrbitalSchema) -> RuntimeKernel {
    let cache = Arc::new(InMemoryEntityCache::new());
    cache.write("Item", "i-1", &row("i-1")).unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        cache as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn client(schema: OrbitalSchema) -> RuntimeKernel {
    let collector = Arc::new(ServerLegCollector::new());
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(collector as Arc<dyn EffectDelegate>),
            ..KernelConfig::default()
        },
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn state(kernel: &RuntimeKernel, t: &str) -> String {
    kernel
        .get_state(t, Some("singleton"))
        .unwrap()
        .current_state
}

fn server_answer(fetching: bool) -> EventResponse {
    let s = server(schema(fetching));
    let outcomes = s.dispatch("Browse", None, "INIT", Value::Null).unwrap();
    EventResponse::from_process_results(&outcomes, "Browse", "INIT")
}

fn generated_server_answer(dispatched: bool) -> EventResponse {
    let source = BusEventSource {
        r#trait: Some("Browse".to_string()),
        orbital: Some("Main".to_string()),
        dispatched: dispatched.then_some(true),
        ..Default::default()
    };
    EventResponse {
        success: true,
        states: HashMap::from([("Browse".to_string(), "loading".to_string())]),
        emitted_events: vec![EmittedEvent {
            event: "Loaded".to_string(),
            payload: Some(
                serde_json::from_value(json!({ "data": [{ "id": "i-1" }], "totalCount": 1 }))
                    .unwrap(),
            ),
            source: Some(source),
        }],
        ..Default::default()
    }
}

fn apply(fetching: bool, response: &EventResponse) -> RuntimeKernel {
    let c = client(schema(fetching));
    c.remount_traits(["Browse"]);
    let dispatch = c
        .dispatch_with_server_leg("App", "Browse", None, "INIT", Value::Null, None)
        .unwrap();
    assert_eq!(state(&c, "Browse"), "loading");
    let already = already_delivered_from(&dispatch);
    c.apply_server_response(response, &already).unwrap();
    c
}

#[test]
fn the_wire_carries_the_success_emit_from_the_emitting_trait() {
    let r = server_answer(true);
    assert!(
        r.emitted_events.iter().any(|e| e.event == "Loaded"
            && e.source.as_ref().and_then(|s| s.r#trait.as_deref()) == Some("Browse")),
        "{r:?}"
    );
}

#[test]
fn applying_the_servers_fetch_response_advances_the_emitting_trait() {
    let c = apply(true, &server_answer(true));
    assert_eq!(state(&c, "Browse"), "browsing");
}

#[test]
fn the_relay_listener_still_receives_the_emit() {
    let c = apply(true, &server_answer(true));
    assert_eq!(state(&c, "Relay"), "shown");
}

#[test]
fn control_a_trait_with_no_fetch_is_unaffected() {
    let c = apply(false, &server_answer(false));
    assert_eq!(state(&c, "Browse"), "loading");
}

#[test]
fn edge_the_echo_of_the_delivered_init_is_skipped_but_the_emit_is_not() {
    let c = client(schema(true));
    let dispatch = c
        .dispatch_with_server_leg("App", "Browse", None, "INIT", Value::Null, None)
        .unwrap();
    let mut r = generated_server_answer(false);
    r.emitted_events.insert(
        0,
        EmittedEvent {
            event: "INIT".to_string(),
            payload: None,
            source: Some(BusEventSource::from_trait("Browse")),
        },
    );
    let outcomes = c
        .apply_server_response(&r, &already_delivered_from(&dispatch))
        .unwrap();
    assert_eq!(state(&c, "Browse"), "browsing");
    assert!(!outcomes
        .iter()
        .any(|(_, o)| o.event.as_deref() == Some("INIT")));
}

#[test]
fn a_generated_servers_unconsumed_emit_advances_the_emitting_trait() {
    let c = apply(true, &generated_server_answer(false));
    assert_eq!(state(&c, "Browse"), "browsing");
    assert_eq!(state(&c, "Relay"), "shown");
}

#[test]
fn control_an_emit_the_host_already_ran_is_not_delivered_again() {
    let c = apply(true, &generated_server_answer(true));
    assert_eq!(state(&c, "Browse"), "loading");
    assert_eq!(state(&c, "Relay"), "shown");
}

#[test]
fn reconciled_states_land_on_the_instance_a_dispatch_addresses() {
    let c = client(schema(false));
    let response = EventResponse {
        success: true,
        states: HashMap::from([("Relay".to_string(), "shown".to_string())]),
        ..Default::default()
    };
    c.apply_server_response(&response, &Default::default())
        .unwrap();
    assert_eq!(state(&c, "Relay"), "shown");
}

#[test]
fn a_relayed_outcome_carries_the_mapped_payload() {
    let outcomes = server(schema(true))
        .dispatch("Browse", None, "INIT", Value::Null)
        .unwrap();
    let relay = outcomes
        .iter()
        .find(|(t, o)| t == "Relay" && o.executed)
        .map(|(_, o)| o)
        .expect("Relay ran");
    let expected: Value = serde_json::from_value(json!({ "rows": [{ "id": "i-1" }] })).unwrap();
    assert_eq!(*relay.payload, expected);
}

#[test]
fn control_a_payloadless_seed_outcome_carries_null() {
    let outcomes = server(schema(true))
        .dispatch("Browse", None, "INIT", Value::Null)
        .unwrap();
    let seed = outcomes
        .iter()
        .find(|(t, o)| t == "Browse" && o.event.as_deref() == Some("INIT"))
        .map(|(_, o)| o)
        .expect("INIT ran");
    assert_eq!(*seed.payload, Value::Null);
}

fn guarded_seed_schema(with_listener: bool) -> OrbitalSchema {
    let listener = if with_listener {
        json!([{
            "name": "Saver",
            "linkedEntity": "Item",
            "listens": [{ "event": "VIEW", "triggers": "SAVE", "source": { "kind": "trait", "trait": "Browse" } }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "SAVE",
                    "effects": [["persist", "create", "Item", {}]] }]
            }
        }])
    } else {
        json!([])
    };
    let mut traits = vec![json!({
        "name": "Browse",
        "linkedEntity": "Item",
        "stateMachine": {
            "states": [{ "name": "browsing", "isInitial": true }],
            "transitions": [{ "from": "browsing", "to": "browsing", "event": "VIEW",
                "guard": ["=", "@payload.mode", "detail"] }]
        }
    })];
    traits.extend(listener.as_array().unwrap().iter().cloned());
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": traits
        }]
    }))
    .unwrap()
}

fn view_leg(with_listener: bool) -> Option<orbital_core_runtime::effects::response::EventRequest> {
    let c = client(guarded_seed_schema(with_listener));
    let payload: Value = serde_json::from_value(json!({ "mode": "table" })).unwrap();
    c.dispatch_with_server_leg("App", "Browse", None, "VIEW", payload, None)
        .unwrap()
        .server_legs
        .into_iter()
        .next()
}

#[test]
fn a_leg_names_the_seeds_state_even_when_its_own_guard_rejected_locally() {
    let leg = view_leg(true).expect("the listener's persist needs a leg");
    let traits = leg.traits.unwrap_or_default();
    assert!(
        traits
            .iter()
            .any(|t| t.r#trait == "Browse" && t.from == "browsing"),
        "{traits:?}"
    );
}

#[test]
fn control_a_guard_rejected_seed_with_no_server_work_posts_no_leg() {
    assert!(view_leg(false).is_none());
}
