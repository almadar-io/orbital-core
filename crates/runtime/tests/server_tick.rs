// A server tick is a resumable dispatch (`EventRequest.run_tick`): the host owns the timer, the kernel owns which
// tick fires — its trait's mount scope, `appliesTo`, the rows it runs over. JS twins:
// `almadar-runtime/test/{tick-mount-scope,tick-applies-to-states,pause-ticks}.test.ts`.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use serde_json::{json, Value as Json};
use std::sync::Arc;
use {orbital_core_base::bus_event::tick_event_key, orbital_core_runtime::effects::RunTick};
use {
    orbital_core_base::schema::types::OrbitalSchema,
    orbital_core_base::value::Value,
    orbital_core_runtime::effects::response::{EventRequest, EventResponse},
};

fn trait_with_tick(name: &str, emit: &str, background: bool, applies_to: Option<Json>) -> Json {
    let mut tick = json!({ "name": "beat", "interval": 10, "effects": [["emit", emit, {}]] });
    if background {
        tick["runsInBackground"] = json!(true);
    }
    if let Some(applies_to) = applies_to {
        tick["appliesTo"] = applies_to;
    }
    json!({
        "name": name, "linkedEntity": "Cell",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "running" }],
            "events": [{ "key": "START", "name": "START" }],
            "transitions": [{ "from": "idle", "to": "running", "event": "START" }]
        },
        "ticks": [tick],
        "emits": [{ "event": emit }]
    })
}

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Board",
            "entity": { "name": "Cell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                trait_with_tick("Scoped", "SCOPED_PULSE", false, None),
                trait_with_tick("Background", "BG_PULSE", true, None),
                trait_with_tick("InRunning", "RUNNING_PULSE", true, Some(json!(["running"]))),
                trait_with_tick("EmptyList", "EMPTY_PULSE", true, Some(json!([]))),
            ]
        }]
    }))
    .unwrap()
}

/// `Cell` is a `[runtime]` entity: its `rows` are seeded into the kernel, and the host is asked for nothing.
fn drive(server: &EmbeddedServer, request: EventRequest, rows: &[&str]) -> EventResponse {
    let mut asked: Vec<IoRequest> = Vec::new();
    let cells: Vec<Value> = rows.iter().map(|id| row(id)).collect();
    server.seed_rows("Cell", &cells).unwrap();
    let mut progress = server.start("Board", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| {
                        asked.push(pending.request.clone());
                        IoReply {
                            id: pending.id,
                            answer: match pending.request {
                                IoRequest::ListRows { .. } => IoAnswer::Rows(Ok(rows
                                    .iter()
                                    .map(|id| Arc::new(row(id)))
                                    .collect())),
                                IoRequest::ReadRow { id, .. } => IoAnswer::Row(Ok(row(&id))),
                                other => panic!("the fixture asks for {other:?}"),
                            },
                        }
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn row(id: &str) -> Value {
    serde_json::from_value(json!({ "id": id })).unwrap()
}

fn tick(trait_name: &str, entity_id: Option<&str>) -> EventRequest {
    EventRequest {
        event: tick_event_key("beat"),
        run_tick: Some(RunTick {
            trait_name: trait_name.to_string(),
            tick: "beat".to_string(),
        }),
        target_trait: Some(trait_name.to_string()),
        entity_id: entity_id.map(str::to_string),
        ..Default::default()
    }
}

fn start(trait_name: &str) -> EventRequest {
    EventRequest {
        event: "START".to_string(),
        target_trait: Some(trait_name.to_string()),
        ..Default::default()
    }
}

#[test]
fn runtime_singleton_and_literal_singleton_row_keep_distinct_frames() {
    let server = EmbeddedServer::new(schema()).unwrap();
    assert!(matches!(server.start("Board", start("Scoped")).unwrap(), ServerProgress::Done(_)));
    assert_eq!(server.get_state("Scoped", None), "running");
    assert_eq!(server.get_state("Scoped", Some("singleton")), "idle");
}

fn emitted(response: &EventResponse) -> Vec<&str> {
    response
        .emitted_events
        .iter()
        .map(|e| e.event.as_str())
        .collect()
}

#[test]
fn a_background_tick_fires_with_no_client_mounted() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let response = drive(&server, tick("Background", None), &["a"]);
    assert_eq!(emitted(&response), ["BG_PULSE"]);
}

#[test]
fn an_unmounted_trait_s_tick_is_paused() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let response = drive(&server, tick("Scoped", None), &["a"]);
    assert!(emitted(&response).is_empty());
    assert!(response.success);
}

#[test]
fn mounting_the_trait_starts_its_tick_and_unmounting_pauses_it_again() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.set_tick_mount(vec!["Scoped".to_string()]);
    assert_eq!(
        emitted(&drive(&server, tick("Scoped", None), &["a"])),
        ["SCOPED_PULSE"]
    );
    server.set_tick_mount(Vec::new());
    assert!(emitted(&drive(&server, tick("Scoped", None), &["a"])).is_empty());
}

#[test]
fn control_mounting_another_trait_does_not_start_this_tick() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.set_tick_mount(vec!["Background".to_string()]);
    assert!(emitted(&drive(&server, tick("Scoped", None), &["a"])).is_empty());
}

#[test]
fn a_tick_over_a_runtime_entity_with_no_rows_runs_once_on_the_singleton_frame() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let response = drive(&server, tick("Background", None), &[]);
    assert_eq!(emitted(&response), ["BG_PULSE"]);
}

#[test]
fn control_a_runtime_entity_with_rows_ticks_once_per_row_and_not_also_on_the_singleton() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let response = drive(&server, tick("Background", None), &["a", "b"]);
    assert_eq!(emitted(&response), ["BG_PULSE", "BG_PULSE"]);
}

#[test]
fn control_an_unmounted_trait_s_tick_stays_paused_when_the_entity_has_no_rows() {
    let server = EmbeddedServer::new(schema()).unwrap();
    assert!(emitted(&drive(&server, tick("Scoped", None), &[])).is_empty());
}

#[test]
fn a_tick_runs_over_every_row_of_its_entity() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let response = drive(&server, tick("Background", None), &["a", "b", "c"]);
    assert_eq!(emitted(&response), ["BG_PULSE", "BG_PULSE", "BG_PULSE"]);
}

#[test]
fn an_entity_id_runs_the_tick_for_that_row_alone() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let response = drive(&server, tick("Background", Some("b")), &["a", "b", "c"]);
    assert_eq!(emitted(&response), ["BG_PULSE"]);
}

#[test]
fn edge_a_tick_over_a_persistent_entity_with_no_rows_fires_nothing() {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Board",
            "entity": { "name": "Cell", "persistence": "persistent", "collection": "cells", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [trait_with_tick("Background", "BG_PULSE", true, None)]
        }]
    }))
    .unwrap();
    let server = EmbeddedServer::new(schema).unwrap();
    assert!(emitted(&drive(&server, tick("Background", None), &[])).is_empty());
}

#[test]
fn a_tick_runs_only_in_a_listed_state() {
    let server = EmbeddedServer::new(schema()).unwrap();
    assert!(emitted(&drive(&server, tick("InRunning", None), &["a"])).is_empty());
    drive(&server, start("InRunning"), &["a"]);
    assert_eq!(
        emitted(&drive(&server, tick("InRunning", None), &["a"])),
        ["RUNNING_PULSE"]
    );
}

#[test]
fn control_an_empty_applies_to_list_runs_in_every_state() {
    let server = EmbeddedServer::new(schema()).unwrap();
    assert_eq!(
        emitted(&drive(&server, tick("EmptyList", None), &["a"])),
        ["EMPTY_PULSE"]
    );
}

#[test]
fn a_tick_the_trait_does_not_declare_is_an_inert_rejection() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let mut request = tick("Background", None);
    request.run_tick = Some(RunTick {
        trait_name: "Background".to_string(),
        tick: "nope".to_string(),
    });
    let response = drive(&server, request, &["a"]);
    assert!(response.success);
    assert!(!response.transitioned);
    assert_eq!(
        response.rejections.expect("a rejection")[0].code,
        orbital_core_runtime::effects::response::TransitionRejectionCode::NoDispatchableTraits
    );
}

// EXEC-STEP-5 with EXEC-RENDER-4: a step that does not fire re-renders what the slot holds, not the first arm that enters the state.
mod unfired_step_render {
    use super::*;

    fn painter() -> OrbitalSchema {
        let say =
            |text: &str| json!(["render-ui", "main", { "type": "typography", "content": text }]);
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Board",
                "entity": { "name": "Cell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Painter", "linkedEntity": "Cell",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [{ "key": "INIT", "name": "Init" }, { "key": "GO", "name": "Go" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT", "effects": [say("first")] },
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [say("second")] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    fn send(server: &EmbeddedServer, event: &str) -> EventResponse {
        drive(
            server,
            EventRequest {
                event: event.to_string(),
                target_trait: Some("Painter".to_string()),
                ..Default::default()
            },
            &[],
        )
    }

    fn painted(response: &EventResponse) -> Vec<String> {
        response
            .client_effects
            .iter()
            .flatten()
            .map(|effect| {
                serde_json::to_value(effect).unwrap()[2]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn an_unfired_step_repaints_what_the_slot_holds_not_the_first_arm_that_enters_the_state() {
        let server = EmbeddedServer::new(painter()).unwrap();
        assert_eq!(painted(&send(&server, "INIT")), ["first"]);
        assert_eq!(painted(&send(&server, "GO")), ["second"]);
        let unfired = send(&server, "NOPE");
        assert!(!unfired.transitioned);
        assert_eq!(painted(&unfired), ["second"]);
    }

    #[test]
    fn control_an_unfired_step_on_a_trait_that_has_painted_nothing_shows_the_state_s_entering_render(
    ) {
        let server = EmbeddedServer::new(painter()).unwrap();
        assert_eq!(painted(&send(&server, "NOPE")), ["first"]);
    }

    #[test]
    fn control_a_later_fired_arm_replaces_what_an_unfired_step_repaints() {
        let server = EmbeddedServer::new(painter()).unwrap();
        send(&server, "GO");
        send(&server, "INIT");
        assert_eq!(painted(&send(&server, "NOPE")), ["first"]);
    }
}
