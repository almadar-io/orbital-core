// A delegated server leg replays from the client's circuit state: each carried trait is held at its `from`
// state and each carried row is the trait's `@entity`. A request that declares `traits` without a
// `targetTrait` runs every declared trait. JS twins: `client-kernel-seeded-leg-fold`,
// `client-role-leg-carries-pre-dispatch-row`, `client-role-local-seed-downstream-leg`.

use orbital_core_base::value::ValueMap;
use orbital_core_runtime::effects::TraitFrom;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use serde_json::{json, Value as Json};
use {
    orbital_core_base::schema::types::OrbitalSchema,
    orbital_core_base::value::Value,
    orbital_core_runtime::effects::response::{
        EventRequest, EventResponse, LifecycleEvent, MountSeed, TransitionRejectionCode,
    },
};

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }, { "name": "content", "type": "string" }] },
            "traits": [
                { "name": "Composer", "linkedEntity": "Message",
                  "stateMachine": {
                      "states": [{ "name": "ready", "isInitial": true }],
                      "transitions": [
                          { "from": "ready", "to": "ready", "event": "SEND", "guard": ["validate/required", "@entity.content"],
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
    drive(&EmbeddedServer::new(schema()).unwrap(), request)
}

fn drive(server: &EmbeddedServer, request: EventRequest) -> (EventResponse, Vec<Value>) {
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
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
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
        entity_by_trait: row_json.map(|r| ValueMap::from([("Composer".into(), row(r))])),
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

fn stateful() -> EmbeddedServer {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.set_stateful(true);
    server
}

fn refusal_code(response: &EventResponse) -> TransitionRejectionCode {
    response.rejections.as_ref().expect("a typed rejection")[0].code
}

#[test]
fn a_stateful_host_refuses_carried_traits_without_a_target_or_a_mount() {
    let request = EventRequest {
        event: "NEXT".to_string(),
        traits: Some(vec![carried("Wizard", "second"), carried("Other", "first")]),
        ..Default::default()
    };
    let (response, _) = drive(&stateful(), request);
    assert!(!response.success);
    assert_eq!(refusal_code(&response), TransitionRejectionCode::ClientCircuitStateRefused);
    assert!(response.states.is_empty(), "nothing was seeded or run: {:?}", response.states);
}

#[test]
fn a_stateful_host_refuses_carried_rows_without_a_target_or_a_mount_too() {
    let request = EventRequest {
        event: "SEND".to_string(),
        entity_by_trait: Some(ValueMap::from([("Composer".into(), row(json!({ "content": "x" })))])),
        ..Default::default()
    };
    let (response, created) = drive(&stateful(), request);
    assert_eq!(refusal_code(&response), TransitionRejectionCode::ClientCircuitStateRefused);
    assert!(created.is_empty());
}

#[test]
fn control_a_stateful_host_admits_a_delegated_leg_and_a_request_carrying_nothing() {
    let (leg, created) = drive(&stateful(), send(Some(json!({ "content": "typed" }))));
    assert!(leg.success && created.len() == 1, "a targeted leg is the client role's delegated leg");
    let mount = EventRequest {
        event: "INIT".to_string(),
        client_id: Some("tab-1".to_string()),
        mount: Some(vec![MountSeed { r#trait: "Wizard".to_string(), event: LifecycleEvent::Init }]),
        traits: Some(vec![carried("Wizard", "second")]),
        ..Default::default()
    };
    let (mounted, _) = drive(&stateful(), mount);
    assert!(mounted.rejections.as_ref().is_none_or(|r| r.iter().all(|r| r.code != TransitionRejectionCode::ClientCircuitStateRefused)));
    let plain = EventRequest { event: "NEXT".to_string(), ..Default::default() };
    let (response, _) = drive(&stateful(), plain);
    assert!(response.success, "a request carrying no circuit state is admitted");
}

#[test]
fn control_a_stateless_host_takes_the_clients_copy_on_any_request() {
    let request = EventRequest {
        event: "NEXT".to_string(),
        traits: Some(vec![carried("Wizard", "second")]),
        ..Default::default()
    };
    let (response, _) = drive(&EmbeddedServer::new(schema()).unwrap(), request);
    assert!(response.rejections.as_ref().is_none_or(|r| r.iter().all(|r| r.code != TransitionRejectionCode::ClientCircuitStateRefused)));
}

// A carried row is addressed by its own id (G-CROSS-256, ruled 2026-10-18; JS twin: `evaluateOrbitalEvent`'s per-trait
// address). A `[runtime]` row carried with an id is that row, never the singleton's seeded row; an id-carrying row of a
// `[shared]` entity stays out of the one shared frame.
fn carried_rows_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "CardOrbital",
            "entity": { "name": "Card", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "label", "type": "string" }, { "name": "seen", "type": "object", "properties": { "label": { "name": "label", "type": "string" }, "createdAt": { "name": "createdAt", "type": "string" } } }] },
            "traits": [{ "name": "Viewer", "linkedEntity": "Card",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "PING", "effects": [["set", "@entity.seen", { "label": "@entity.label", "createdAt": ["object/get", "@entity", "createdAt"] }]] }] } }]
        }, {
            "name": "ChatOrbital",
            "entity": { "name": "Draft", "persistence": "persistent", "collection": "drafts", "shared": true, "fields": [{ "name": "id", "type": "string" }, { "name": "channel", "type": "string" }] },
            "traits": [
                { "name": "Writer", "linkedEntity": "Draft",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "SELECT", "effects": [["set", "@entity.channel", "@payload.channel"]] }] } },
                { "name": "Reader", "linkedEntity": "Draft",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } }
            ]
        }, {
            "name": "ScratchOrbital",
            "entity": { "name": "Scratch", "persistence": "runtime", "shared": true, "fields": [{ "name": "id", "type": "string" }, { "name": "channel", "type": "string" }, { "name": "seenChannel", "type": "string" }] },
            "traits": [
                { "name": "ScratchWriter", "linkedEntity": "Scratch",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "SELECT", "effects": [["set", "@entity.seenChannel", "@entity.channel"]] }] } },
                { "name": "ScratchReader", "linkedEntity": "Scratch",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } }
            ]
        }]
    }))
    .unwrap()
}

fn seeded_rows_server() -> EmbeddedServer {
    let server = EmbeddedServer::new(carried_rows_schema()).unwrap();
    server
        .seed_rows("Card", &[row(json!({ "id": "singleton", "label": "seeded", "createdAt": "2026-01-01T00:00:00.000Z" }))])
        .unwrap();
    server
}

fn carried_rows_run(orbital: &str, request: EventRequest) -> EventResponse {
    drive_rows(&seeded_rows_server(), orbital, request)
}

fn drive_rows(server: &EmbeddedServer, orbital: &str, request: EventRequest) -> EventResponse {
    let mut progress = server.start(orbital, request).unwrap();
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
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
        }
    }
}

fn ping(carried_row: Json) -> EventRequest {
    EventRequest {
        event: "PING".to_string(),
        traits: Some(vec![carried("Viewer", "idle")]),
        entity_by_trait: Some(ValueMap::from([("Viewer".into(), row(carried_row))])),
        ..Default::default()
    }
}

fn entity_of(response: &EventResponse, trait_name: &str) -> Json {
    let mut frames = std::collections::BTreeMap::new();
    if let Some(delta) = &response.frames {
        delta.fold_into(&mut frames);
    }
    frames.get(trait_name).map_or(Json::Null, |fields| serde_json::to_value(fields).unwrap())
}

#[test]
fn a_runtime_row_carried_with_its_own_id_is_that_row_not_the_seeded_singleton() {
    let response = carried_rows_run(
        "CardOrbital",
        ping(json!({ "id": "row-1", "label": "hello" })),
    );
    let viewer = entity_of(&response, "Viewer");
    assert_eq!(viewer["id"], json!("row-1"));
    assert_eq!(viewer["seen"]["label"], json!("hello"));
    assert!(
        viewer["seen"]["createdAt"].is_null(),
        "the seeded singleton leaked into row-1: {viewer}"
    );
}

#[test]
fn control_a_runtime_row_carried_without_an_id_folds_over_the_singleton() {
    let response = carried_rows_run("CardOrbital", ping(json!({ "label": "hello" })));
    let viewer = entity_of(&response, "Viewer");
    assert_eq!(viewer["seen"]["label"], json!("hello"));
    assert_eq!(viewer["seen"]["createdAt"], json!("2026-01-01T00:00:00.000Z"));
}

#[test]
fn a_persisted_shared_row_carried_with_an_id_stays_out_of_the_shared_frame() {
    let response = carried_rows_run(
        "ChatOrbital",
        EventRequest {
            event: "SELECT".to_string(),
            payload: Some(row(json!({ "channel": "chan-1" }))),
            traits: Some(vec![carried("Writer", "idle"), carried("Reader", "idle")]),
            entity_by_trait: Some(ValueMap::from([
                ("Writer".into(),
                    row(json!({ "id": "row-1", "channel": "" })),
                ),
                ("Reader".into(), row(json!({ "channel": "" }))),
            ])),
            ..Default::default()
        },
    );
    let reader = entity_of(&response, "$shared::Draft");
    assert!(
        reader.get("id").is_none(),
        "the writer's detail row became the shared frame: {reader}"
    );
    assert_eq!(reader["channel"], json!("chan-1"));
}

#[test]
fn control_a_runtime_shared_row_carried_with_an_id_folds_into_the_shared_frame() {
    let response = carried_rows_run(
        "ScratchOrbital",
        EventRequest {
            event: "SELECT".to_string(),
            traits: Some(vec![
                carried("ScratchWriter", "idle"),
                carried("ScratchReader", "idle"),
            ]),
            entity_by_trait: Some(ValueMap::from([("ScratchWriter".into(),
                row(json!({ "id": "row-1", "channel": "c1" })),
            )])),
            ..Default::default()
        },
    );
    assert_eq!(
        entity_of(&response, "$shared::Scratch")["seenChannel"],
        json!("c1")
    );
}

fn next_other() -> EventRequest {
    EventRequest { event: "NEXT".to_string(), target_trait: Some("Other".to_string()), ..Default::default() }
}

#[test]
fn a_fork_starts_every_trait_at_its_initial_state_whatever_its_template_ran() {
    let template = EmbeddedServer::new(schema()).unwrap();
    let (first, _) = drive(&template, next_other());
    assert_eq!(first.states.get("Other").map(String::as_str), Some("done"));
    let (forked, _) = drive(&template.fork_session().unwrap(), next_other());
    assert!(forked.transitioned, "{forked:?}");
    assert_eq!(forked.states.get("Other").map(String::as_str), Some("done"));
}

#[test]
fn control_a_fork_running_leaves_its_template_where_it_was() {
    let template = EmbeddedServer::new(schema()).unwrap();
    drive(&template, next_other());
    drive(&template.fork_session().unwrap(), next_other());
    let (again, _) = drive(&template, next_other());
    assert!(!again.transitioned, "{again:?}");
}

#[test]
fn a_fork_delivers_the_programs_listens() {
    let fork = EmbeddedServer::new(schema()).unwrap().fork_session().unwrap();
    let (_, created) = drive(&fork, send(Some(json!({ "content": "typed" }))));
    assert_eq!(created.len(), 1);
}

#[test]
fn edge_two_forks_of_one_template_hold_separate_sessions() {
    let template = EmbeddedServer::new(schema()).unwrap();
    let (a, b) = (template.fork_session().unwrap(), template.fork_session().unwrap());
    drive(&a, next_other());
    let (on_b, _) = drive(&b, next_other());
    assert!(on_b.transitioned, "{on_b:?}");
    let (on_a_again, _) = drive(&a, next_other());
    assert!(!on_a_again.transitioned, "{on_a_again:?}");
}

#[test]
fn a_fork_holds_none_of_its_templates_runtime_rows() {
    let fork = seeded_rows_server().fork_session().unwrap();
    let viewer = entity_of(&drive_rows(&fork, "CardOrbital", ping(json!({ "label": "hello" }))), "Viewer");
    assert_eq!(viewer["seen"]["label"], json!("hello"));
    assert!(viewer["seen"]["createdAt"].is_null(), "the template's seeded row reached the fork: {viewer}");
}

#[test]
fn a_fork_answers_exactly_as_a_freshly_registered_server() {
    let request = || ping(json!({ "id": "row-1", "label": "hello" }));
    let template = EmbeddedServer::new(carried_rows_schema()).unwrap();
    drive_rows(&template, "CardOrbital", request());
    let forked = drive_rows(&template.fork_session().unwrap(), "CardOrbital", request());
    let fresh = drive_rows(&EmbeddedServer::new(carried_rows_schema()).unwrap(), "CardOrbital", request());
    let folded = |response: &EventResponse| {
        let mut frames = std::collections::BTreeMap::new();
        response.frames.as_ref().expect("sealed").fold_into(&mut frames);
        frames
    };
    assert_eq!(folded(&forked), folded(&fresh), "the same frames");
    assert_eq!(forked.frames.as_ref().map(|f| f.session.clone()), Some(None), "a fork cannot number its answers");
    let (mut forked, mut fresh) = (serde_json::to_value(&forked).unwrap(), serde_json::to_value(&fresh).unwrap());
    forked.as_object_mut().unwrap().remove("frames");
    fresh.as_object_mut().unwrap().remove("frames");
    assert_eq!(forked, fresh);
}

fn holder_schema() -> OrbitalSchema {
    let items: Vec<Json> = (0..20).map(|i| json!({ "k": format!("item-{i}") })).collect();
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "HolderOrbital",
            "entity": { "name": "Ledger", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "count", "type": "number", "default": 0 },
                { "name": "items", "type": "array", "default": items }
            ] },
            "traits": [{ "name": "Holder", "linkedEntity": "Ledger",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [["set", "@entity.count", 0]] },
                        { "from": "idle", "to": "idle", "event": "BUMP", "effects": [["set", "@entity.count", ["+", "@entity.count", 1]]] }
                    ] } }]
        }]
    }))
    .unwrap()
}

fn holder_mount() -> EventRequest {
    EventRequest {
        event: "INIT".to_string(),
        mount: Some(vec![MountSeed { r#trait: "Holder".to_string(), event: LifecycleEvent::Init }]),
        ..Default::default()
    }
}

#[test]
fn a_mount_seed_returns_the_whole_row() {
    let server = EmbeddedServer::new(holder_schema()).unwrap();
    let mounted = drive_rows(&server, "HolderOrbital", holder_mount());
    let holder = entity_of(&mounted, "Holder");
    assert_eq!(holder["items"].as_array().map(Vec::len), Some(20), "{holder}");
    assert!(holder.get("count").is_some(), "{holder}");
}

#[test]
fn control_a_dispatch_after_the_mount_returns_only_what_it_wrote() {
    let server = EmbeddedServer::new(holder_schema()).unwrap();
    drive_rows(&server, "HolderOrbital", holder_mount());
    let bump = EventRequest { event: "BUMP".to_string(), target_trait: Some("Holder".to_string()), ..Default::default() };
    let holder = entity_of(&drive_rows(&server, "HolderOrbital", bump), "Holder");
    assert!(holder.get("count").is_some(), "{holder}");
    assert!(holder.get("items").is_none(), "the unchanged list rode the dispatch: {holder}");
}

fn topic_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "TopicOrbital",
            "entity": { "name": "Topic", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "topic", "type": "string" },
                { "name": "seen", "type": "string" }
            ] },
            "traits": [{ "name": "Viewer", "linkedEntity": "Topic",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "SET_TOPIC", "effects": [["set", "@entity.topic", "@payload.topic"]] },
                        { "from": "idle", "to": "idle", "event": "READ_BACK", "effects": [["set", "@entity.seen", "@entity.topic"]] }
                    ] } }]
        }]
    }))
    .unwrap()
}

fn topic_request(event: &str, topic: Option<&str>, carried: Option<&str>) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        target_trait: Some("Viewer".to_string()),
        payload: topic.map(|t| row(json!({ "topic": t }))),
        entity_by_trait: carried.map(|t| ValueMap::from([("Viewer".into(), row(json!({ "topic": t })))])),
        ..Default::default()
    }
}

fn seen_after(server: &EmbeddedServer, request: EventRequest) -> Json {
    entity_of(&drive_rows(server, "TopicOrbital", request), "Viewer")["seen"].clone()
}

#[test]
fn a_viewers_server_frame_is_not_overwritten_by_another_viewers_session() {
    let template = EmbeddedServer::new(topic_schema()).unwrap();
    let (a, b) = (template.fork_session().unwrap(), template.fork_session().unwrap());
    drive_rows(&a, "TopicOrbital", topic_request("SET_TOPIC", Some("app"), None));
    drive_rows(&b, "TopicOrbital", topic_request("SET_TOPIC", Some("cli"), None));
    assert_eq!(seen_after(&a, topic_request("READ_BACK", None, None)), json!("app"));
    assert_eq!(seen_after(&b, topic_request("READ_BACK", None, None)), json!("cli"));
}

#[test]
fn control_one_viewer_reads_back_its_own_last_write() {
    let server = EmbeddedServer::new(topic_schema()).unwrap().fork_session().unwrap();
    drive_rows(&server, "TopicOrbital", topic_request("SET_TOPIC", Some("app"), None));
    drive_rows(&server, "TopicOrbital", topic_request("SET_TOPIC", Some("cli"), None));
    assert_eq!(seen_after(&server, topic_request("READ_BACK", None, None)), json!("cli"));
}

#[test]
fn a_stateless_request_reads_back_the_frame_its_client_carries_not_another_viewers() {
    let template = EmbeddedServer::new(topic_schema()).unwrap();
    drive_rows(&template.fork_session().unwrap(), "TopicOrbital", topic_request("SET_TOPIC", Some("cli"), None));
    let fresh = template.fork_session().unwrap();
    assert_eq!(seen_after(&fresh, topic_request("READ_BACK", None, Some("app"))), json!("app"));
}
