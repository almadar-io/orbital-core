// EXEC-TICK-6 with EXEC-ENDPOINT-3: a trait on a protocol endpoint (`boot`, `http`, `stdio`) is mounted by the kernel for
// the program's life, so its mount-scoped tick fires with no client; a route trait's waits for a client (G-CROSS-538).

use orbital_core_base::bus_event::tick_event_key;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::effects::RunTick;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use serde_json::json;

fn ticker(name: &str) -> serde_json::Value {
    json!({
        "name": name, "linkedEntity": "Clock",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "events": [{ "key": "INIT", "name": "INIT" }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
        },
        "ticks": [{ "name": "beat", "interval": 10, "effects": [["emit", format!("{name}_PULSE"), {}]] }],
        "emits": [{ "event": format!("{name}_PULSE") }]
    })
}

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App", "typeContractVersion": 1,
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Clock", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [ticker("Booted"), ticker("Routed"), ticker("Orphan")],
            "pages": [
                { "name": "Bg", "path": "", "protocol": "boot", "traits": [{ "ref": "Booted" }] },
                { "name": "Home", "path": "/", "traits": [{ "ref": "Routed" }] }
            ]
        }]
    }))
    .unwrap()
}

fn tick(trait_name: &str) -> EventRequest {
    EventRequest {
        event: tick_event_key("beat"),
        run_tick: Some(RunTick { trait_name: trait_name.to_string(), tick: "beat".to_string() }),
        target_trait: Some(trait_name.to_string()),
        ..Default::default()
    }
}

fn emitted(server: &EmbeddedServer, trait_name: &str) -> Vec<String> {
    let response: EventResponse = match server.start("Main", tick(trait_name)).unwrap() {
        ServerProgress::Done(response) => response,
        other => panic!("the fixture asks the host for nothing: {}", matches!(other, ServerProgress::Await { .. })),
    };
    response.emitted_events.iter().map(|e| e.event.clone()).collect()
}

fn fired(server: &EmbeddedServer) -> Vec<String> {
    server.tick_pass(0);
    let mut names: Vec<String> = server.tick_pass(1_000).into_values().flatten().map(|t| t.trait_name).collect();
    names.sort();
    names
}

#[test]
fn a_boot_trait_s_tick_fires_with_no_client_mounted() {
    let server = EmbeddedServer::new(schema()).unwrap();
    assert_eq!(emitted(&server, "Booted"), ["Booted_PULSE"]);
    assert_eq!(fired(&server), ["Booted"]);
}

#[test]
fn control_a_route_trait_s_tick_waits_for_a_client_and_a_trait_on_no_page_never_ticks() {
    let server = EmbeddedServer::new(schema()).unwrap();
    assert!(emitted(&server, "Routed").is_empty());
    assert!(emitted(&server, "Orphan").is_empty());
    server.mount_client("tab", "Main", vec!["Routed".to_string()]);
    assert_eq!(emitted(&server, "Routed"), ["Routed_PULSE"]);
}
