// A `persist` effect's `emit.success` envelope is marked on the response (`source.persistSuccess`), so a
// live-push host can fan exactly those emits out to the other connected clients. A failure envelope and a
// plain `emit` are not marked. JS twin: `deliverEmit(..., fromPersistSuccess)` -> `liveBroadcastSink`
// (`target: 'peers'`).

use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::runtime::resumable::{IoAnswer, IoReply, IoRequest};
use orbital_core::{EventRequest, EventResponse, OrbitalSchema};
use serde_json::json;

fn schema() -> OrbitalSchema {
    let emit = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "Notes", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "SAVE",
                          "effects": [["persist", "create", "Note", { "id": "n1", "title": "t" }, emit], ["emit", "PLAIN", {}]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

fn save(create: IoAnswer) -> EventResponse {
    let server = EmbeddedServer::new(schema()).unwrap();
    let request = EventRequest {
        event: "SAVE".to_string(),
        target_trait: Some("Notes".to_string()),
        ..Default::default()
    };
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
                            IoRequest::CreateRow { data, .. } => match &create {
                                IoAnswer::Row(Ok(_)) => IoAnswer::Row(Ok(data)),
                                other => other.clone(),
                            },
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

fn marked(response: &EventResponse, event: &str) -> Option<bool> {
    response
        .emitted_events
        .iter()
        .find(|e| e.event == event)
        .map(|e| {
            e.source
                .as_ref()
                .and_then(|s| s.persist_success)
                .unwrap_or(false)
        })
}

#[test]
fn the_success_envelope_of_a_persist_is_marked() {
    let response = save(IoAnswer::Row(
        Ok(serde_json::from_value(json!({})).unwrap()),
    ));
    assert_eq!(marked(&response, "SAVED"), Some(true));
}

#[test]
fn a_plain_emit_beside_it_is_not() {
    let response = save(IoAnswer::Row(
        Ok(serde_json::from_value(json!({})).unwrap()),
    ));
    assert_eq!(marked(&response, "PLAIN"), Some(false));
}

#[test]
fn control_a_failed_persists_failure_envelope_is_not() {
    let response = save(IoAnswer::Row(Err("disk full".to_string())));
    assert_eq!(marked(&response, "SAVE_FAILED"), Some(false));
    assert_eq!(marked(&response, "SAVED"), None);
}
