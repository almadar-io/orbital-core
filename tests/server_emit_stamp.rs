// The server role stamps each emitted event with its emitter's identity (orbital, the ids the schema
// carries, the emit contract's id), so a client matches `listens` sources the way the JS host's stamp lets it.
// JS twin: `stampEmitSource` (`almadar-runtime/src/events/emit-stamp.ts`).

use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::{EventRequest, EventResponse, OrbitalSchema};
use serde_json::json;

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "id": "orb_A", "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "id": "trt_A", "name": "Emitter", "linkedEntity": "Item",
                "emits": [{ "event": "PING", "eventId": "evt_PING" }, { "event": "PONG" }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                        "effects": [["emit", "PING", {}], ["emit", "PONG", {}]] }]
                }
            }]
        }]
    }))
    .unwrap()
}

fn go() -> EventResponse {
    let server = EmbeddedServer::new(schema()).unwrap();
    let request = EventRequest {
        event: "GO".to_string(),
        target_trait: Some("Emitter".to_string()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|p| orbital_core::runtime::resumable::IoReply {
                        id: p.id,
                        answer: orbital_core::runtime::resumable::IoAnswer::Row(Err(
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
fn an_emit_carries_its_emitters_orbital_and_ids() {
    let response = go();
    let source = response.emitted_events[0].source.as_ref().unwrap();
    assert_eq!(source.r#trait.as_deref(), Some("Emitter"));
    assert_eq!(source.orbital.as_deref(), Some("Main"));
    assert_eq!(source.orbital_id.as_deref(), Some("orb_A"));
    assert_eq!(source.trait_id.as_deref(), Some("trt_A"));
}

#[test]
fn an_emit_carries_the_id_of_its_contract_only_when_it_declares_one() {
    let response = go();
    assert_eq!(
        response.emitted_events[0]
            .source
            .as_ref()
            .unwrap()
            .event_id
            .as_deref(),
        Some("evt_PING")
    );
    assert_eq!(
        response.emitted_events[1].source.as_ref().unwrap().event_id,
        None
    );
}
