// A server response is the full picture of the orbital: every trait's held state, the dispatched traits'
// new ones over them. JS twin: `allStates` at the end of `evaluateOrbitalEvent`.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::json;

fn schema() -> OrbitalSchema {
    let trait_of = |name: &str, arms: serde_json::Value| {
        json!({ "name": name, "linkedEntity": "Item",
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }], "transitions": arms } })
    };
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [
            { "name": "Main",
              "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [
                  trait_of("Mover", json!([{ "from": "idle", "to": "busy", "event": "GO" }])),
                  trait_of("Still", json!([]))
              ] },
            { "name": "Other",
              "entity": { "name": "Thing", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
              "traits": [trait_of("Elsewhere", json!([]))] }
        ]
    }))
    .unwrap()
}

fn go() -> EventResponse {
    let server = EmbeddedServer::new(schema()).unwrap();
    let request = EventRequest {
        event: "GO".to_string(),
        target_trait: Some("Mover".to_string()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|p| orbital_core_runtime::resumable::IoReply {
                        id: p.id,
                        answer: orbital_core_runtime::resumable::IoAnswer::Row(Err(
                            "no row".into()
                        )),
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

#[test]
fn the_response_holds_every_traits_state_of_the_orbital() {
    let response = go();
    assert_eq!(
        response.states.get("Mover").map(String::as_str),
        Some("busy")
    );
    assert_eq!(
        response.states.get("Still").map(String::as_str),
        Some("idle")
    );
}

#[test]
fn control_another_orbitals_traits_are_not_echoed() {
    assert!(!go().states.contains_key("Elsewhere"));
}
