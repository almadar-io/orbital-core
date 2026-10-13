// The API-boundary payload check is scoped to the traits a request dispatches to: the target when named,
// else the traits whose held state handles the event; each against its own declaration of the event.
// JS twin: `PayloadValidator.validateEventPayload` + `evaluateOrbitalEvent` step 3
// (`payload-validation-scope.test.ts`, `payload-validation-container.test.ts`).

use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::runtime::resumable::{IoAnswer, IoReply, IoRequest};
use orbital_core::{EventRequest, EventResponse, OrbitalSchema};
use serde_json::{json, Value as Json};

fn schema() -> OrbitalSchema {
    let trait_with = |name: &str, field: Json, arms: Json| {
        json!({
            "name": name, "linkedEntity": "Message",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "events": [{ "key": "SEND", "name": "Send", "payloadSchema": [field] }],
                "transitions": arms
            }
        })
    };
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                trait_with("ChatComposer", json!({ "name": "value", "type": "string", "required": false }),
                    json!([{ "from": "idle", "to": "idle", "event": "SEND" }])),
                trait_with("InlineRender", json!({ "name": "value", "type": "string", "required": true }), json!([])),
                trait_with("Burst", json!({ "name": "particles", "type": "[object]" }),
                    json!([{ "from": "idle", "to": "idle", "event": "SEND" }]))
            ]
        }]
    }))
    .unwrap()
}

fn send(target: Option<&str>, payload: Json) -> EventResponse {
    let server = EmbeddedServer::new(schema()).unwrap();
    let request = EventRequest {
        event: "SEND".to_string(),
        target_trait: target.map(str::to_string),
        payload: Some(serde_json::from_value(payload).unwrap()),
        ..Default::default()
    };
    let mut progress = server.start("ChatOrbital", request).unwrap();
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

#[test]
fn a_dispatch_targeted_at_the_optional_trait_ignores_a_siblings_required_field() {
    assert!(send(Some("ChatComposer"), json!({})).success);
}

#[test]
fn the_same_dispatch_targeted_at_the_required_trait_is_rejected() {
    let response = send(Some("InlineRender"), json!({}));
    assert!(!response.success);
    assert!(!response.transitioned);
    assert_eq!(
        response.error.as_deref(),
        Some("Payload validation failed for event 'SEND': value (missing, expected string)")
    );
}

#[test]
fn a_null_required_field_is_reported_as_null() {
    let response = send(Some("InlineRender"), json!({ "value": null }));
    assert_eq!(
        response.error.as_deref(),
        Some("Payload validation failed for event 'SEND': value (null, expected string)")
    );
}

#[test]
fn control_the_required_field_present_passes() {
    assert!(send(Some("InlineRender"), json!({ "value": "hi" })).success);
}

#[test]
fn an_untargeted_dispatch_validates_only_against_the_traits_that_handle_the_event() {
    let response = send(None, json!({ "particles": [] }));
    assert!(response.success, "{:?}", response.error);
}

#[test]
fn a_declared_container_is_enforced() {
    let rejected = send(Some("Burst"), json!({ "particles": { "id": "p" } }));
    assert!(!rejected.success);
    assert_eq!(
        rejected.error.as_deref(),
        Some(
            "Payload validation failed for event 'SEND': particles (wrong-type, expected [object])"
        )
    );
    assert!(send(Some("Burst"), json!({ "particles": [{ "id": "p" }] })).success);
    assert!(
        send(Some("Burst"), json!({})).success,
        "an optional field may be absent"
    );
}
