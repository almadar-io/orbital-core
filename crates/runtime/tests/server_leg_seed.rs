// A delegated server leg replays from the client's circuit state: each carried trait is held at its `from`
// state and each carried row is the trait's `@entity`. A request that declares `traits` without a
// `targetTrait` runs every declared trait. JS twins: `client-kernel-seeded-leg-fold`,
// `client-role-leg-carries-pre-dispatch-row`, `client-role-local-seed-downstream-leg`.

use orbital_core_base::value::ValueMap;
use orbital_core_runtime::effects::TraitFrom;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse, TransitionRejectionCode}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }, { "name": "content", "type": "string" }] },
            "traits": [
                { "name": "Composer", "linkedEntity": "Message",
                  "stateMachine": {
                      "states": [{ "name": "ready", "isInitial": true }],
                      "transitions": [
                          { "from": "ready", "to": "ready", "event": "SEND", "guard": ["!=", "@entity.content", ""],
                            "effects": [["emit", "SAVE", { "content": "@entity.content" }]] }
                      ]
                  } },
                { "name": "Persistor", "linkedEntity": "Message",
                  "listens": [{ "event": "SAVE", "triggers": "DO_CREATE", "source": { "kind": "trait", "trait": "Composer" } }],
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "DO_CREATE",
                          "effects": [["persist", "create", "Message", { "content": "@payload.content" }]] }]
                  } },
                { "name": "Wizard", "linkedEntity": "Message",
                  "stateMachine": {
                      "states": [{ "name": "first", "isInitial": true }, { "name": "second" }, { "name": "done" }],
                      "transitions": [{ "from": "second", "to": "done", "event": "NEXT" }]
                  } },
                { "name": "Other", "linkedEntity": "Message",
                  "stateMachine": {
                      "states": [{ "name": "first", "isInitial": true }, { "name": "done" }],
                      "transitions": [{ "from": "first", "to": "done", "event": "NEXT" }]
                  } }
            ]
        }]
    }))
    .unwrap()
}

fn row(json: Json) -> Value {
    serde_json::from_value(json).unwrap()
}

/// Run `request`; the persistence host records every create it is asked for.
fn run(request: EventRequest) -> (EventResponse, Vec<Value>) {
    let server = EmbeddedServer::new(schema()).unwrap();
    let mut created = Vec::new();
    let mut progress = server.start("ChatOrbital", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return (response, created),
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match pending.request {
                            IoRequest::CreateRow { data, .. } => {
                                created.push(data.clone());
                                IoAnswer::Row(Ok(data))
                            }
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

fn carried(trait_name: &str, from: &str) -> TraitFrom {
    TraitFrom {
        r#trait: trait_name.to_string(),
        from: from.to_string(),
        event: None,
    }
}

fn send(row_json: Option<Json>) -> EventRequest {
    EventRequest {
        event: "SEND".to_string(),
        target_trait: Some("Composer".to_string()),
        traits: Some(vec![
            carried("Composer", "ready"),
            carried("Persistor", "idle"),
        ]),
        entity_by_trait: row_json.map(|r| ValueMap::from([("Composer".to_string(), row(r))])),
        ..Default::default()
    }
}

#[test]
fn the_leg_replays_the_row_the_client_dispatched_against() {
    let (_, created) = run(send(Some(json!({ "content": "typed" }))));
    assert_eq!(created.len(), 1);
    assert_eq!(
        created[0].get_path("content").and_then(Value::as_str),
        Some("typed")
    );
}

#[test]
fn control_without_the_carried_row_the_guard_blocks_the_send() {
    let (response, created) = run(send(None));
    assert!(created.is_empty());
    assert_eq!(response.guard_failed.as_deref(), Some("Composer.SEND"));
}

#[test]
fn a_carried_from_state_replaces_the_servers_held_state() {
    let request = EventRequest {
        event: "NEXT".to_string(),
        target_trait: Some("Wizard".to_string()),
        traits: Some(vec![carried("Wizard", "second")]),
        ..Default::default()
    };
    let (response, _) = run(request);
    assert_eq!(
        response.states.get("Wizard").map(String::as_str),
        Some("done")
    );
}

#[test]
fn control_without_a_carried_state_the_held_initial_state_has_no_arm() {
    let request = EventRequest {
        event: "NEXT".to_string(),
        target_trait: Some("Wizard".to_string()),
        ..Default::default()
    };
    let (response, _) = run(request);
    assert_eq!(
        response.states.get("Wizard").map(String::as_str),
        Some("first")
    );
    assert!(!response.transitioned);
}

#[test]
fn declared_traits_without_a_target_all_run() {
    let request = EventRequest {
        event: "NEXT".to_string(),
        traits: Some(vec![carried("Wizard", "second"), carried("Other", "first")]),
        ..Default::default()
    };
    let (response, _) = run(request);
    assert_eq!(
        response.states.get("Wizard").map(String::as_str),
        Some("done")
    );
    assert_eq!(
        response.states.get("Other").map(String::as_str),
        Some("done")
    );
}

#[test]
fn an_empty_declared_list_is_an_inert_rejection() {
    let request = EventRequest {
        event: "NEXT".to_string(),
        traits: Some(Vec::new()),
        ..Default::default()
    };
    let (response, _) = run(request);
    assert!(response.success);
    assert!(!response.transitioned);
    assert_eq!(
        response.rejections.expect("rejection")[0].code,
        TransitionRejectionCode::NoDispatchableTraits
    );
}
