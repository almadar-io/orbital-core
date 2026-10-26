// EXEC-RENDER-8: a composing transition repaints its callsite-capture children on both roles.
// `@callsitePayload.<field>` is the payload of the transition that composed a child trait into its embedder
// (`@trait.Child` in that transition's `render-ui`). After the composing transition the server re-fires the
// child's lifecycle arm under that payload, the capture is sticky for the child's own later runs, and a
// repaint never moves a child. JS twins: `callsite-payload-embedded-child.test.ts`,
// `callsite-capture-composed-by-event.test.ts`.

use orbital_core_runtime::embed::{EmbeddedKernel, EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::{json, Value as Json};

const ENTITY: &str = r#"{ "name": "Ticket", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] }"#;

fn trait_of(name: &str, config: Json, states: Json, arms: Json) -> Json {
    json!({ "name": name, "linkedEntity": "Ticket", "config": config,
        "stateMachine": { "states": states, "transitions": arms } })
}

fn idle() -> Json {
    json!([{ "name": "idle", "isInitial": true }])
}

fn schema_of(traits: Vec<Json>) -> OrbitalSchema {
    let entity: Json = serde_json::from_str(ENTITY).unwrap();
    serde_json::from_value(json!({
        "name": "App", "orbitals": [{ "name": "Main", "entity": entity, "traits": traits }]
    }))
    .unwrap()
}

fn app(traits: Vec<Json>) -> EmbeddedServer {
    EmbeddedServer::new(schema_of(traits)).unwrap()
}

/// The client role: the same program in a browser's kernel, dispatched locally.
fn client_send(client: &EmbeddedKernel, target: &str, event: &str, payload: Json) -> EventResponse {
    client
        .dispatch(target, None, event, serde_json::from_value(payload).unwrap())
        .unwrap()
        .response
}

fn send(server: &EmbeddedServer, target: &str, event: &str, payload: Json) -> EventResponse {
    let request = EventRequest {
        event: event.to_string(),
        target_trait: Some(target.to_string()),
        payload: Some(serde_json::from_value(payload).unwrap()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|p| IoReply {
                        id: p.id,
                        answer: IoAnswer::Row(Err("no row".into())),
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

/// The last render-ui pattern `trait_name` painted in `response`.
fn pattern(response: &EventResponse, trait_name: &str) -> Option<Json> {
    let frames = response.client_effects_by_trait.as_ref()?;
    let frame = frames.iter().rev().find(|f| f.trait_name == trait_name)?;
    serde_json::to_value(&frame.effect).ok()?.get(2).cloned()
}

fn frame_payload(response: &EventResponse, trait_name: &str) -> Option<Json> {
    let frames = response.client_effects_by_trait.as_ref()?;
    let frame = frames.iter().rev().find(|f| f.trait_name == trait_name)?;
    frame
        .callsite_payload
        .as_ref()
        .map(|p| serde_json::to_value(p).unwrap())
}

fn render_stack(children: Json) -> Json {
    json!(["render-ui", "main", { "type": "stack", "children": children }])
}

fn synthetic() -> EmbeddedServer {
    app(synthetic_traits())
}

fn synthetic_traits() -> Vec<Json> {
    let loaded = |from: &str| json!({ "from": from, "to": "viewing", "event": "LOADED", "effects": [render_stack(json!(["@trait.Child"]))] });
    let host = trait_of(
        "Host",
        json!(null),
        json!([{ "name": "loading", "isInitial": true }, { "name": "viewing" }]),
        json!([loaded("loading"), loaded("viewing")]),
    );
    let child = trait_of(
        "Child",
        json!({
            "content": { "type": "string", "default": "@callsitePayload.error" },
            "disabled": { "type": "boolean", "default": ["if", ["and", ["=", "@callsitePayload.data.status", "resolved"], true], false, true] }
        }),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "INIT",
            "effects": [["render-ui", "main", { "type": "button", "label": "@config.content", "disabled": "@config.disabled" }]] }]),
    );
    vec![host, child]
}

#[test]
fn on_the_client_role_the_composing_payload_re_renders_the_child() {
    let client = EmbeddedKernel::new(schema_of(synthetic_traits()), "c1").unwrap();
    let resolved = client_send(
        &client,
        "Host",
        "LOADED",
        json!({ "error": "Boom", "data": { "status": "resolved" } }),
    );
    let child = pattern(&resolved, "Child").expect("the client repaints the child it composed");
    assert_eq!(child["label"], json!("Boom"));
    assert_eq!(child["disabled"], json!(false));
    assert_eq!(
        frame_payload(&resolved, "Child"),
        Some(json!({ "error": "Boom", "data": { "status": "resolved" } }))
    );
}

#[test]
fn control_on_the_client_role_a_dispatch_that_composes_no_child_repaints_none() {
    let client = EmbeddedKernel::new(schema_of(synthetic_traits()), "c1").unwrap();
    let own = client_send(&client, "Child", "INIT", json!({}));
    assert!(pattern(&own, "Host").is_none());
    let child = pattern(&own, "Child").expect("the child's own run paints it");
    assert!(child.get("label").is_none(), "nothing composed it yet: {child}");
}

#[test]
fn the_composing_payload_re_renders_the_child_whole_value_and_nested() {
    let server = synthetic();
    let resolved = send(
        &server,
        "Host",
        "LOADED",
        json!({ "error": "Boom", "data": { "status": "resolved" } }),
    );
    let child = pattern(&resolved, "Child").expect("the child is repainted");
    assert_eq!(child["label"], json!("Boom"));
    assert_eq!(child["disabled"], json!(false));
    let open = send(
        &server,
        "Host",
        "LOADED",
        json!({ "error": "Still broken", "data": { "status": "open" } }),
    );
    let child = pattern(&open, "Child").unwrap();
    assert_eq!(child["label"], json!("Still broken"));
    assert_eq!(child["disabled"], json!(true));
}

#[test]
fn a_childs_own_lifecycle_run_keeps_the_payload_it_was_composed_with() {
    let server = synthetic();
    send(
        &server,
        "Host",
        "LOADED",
        json!({ "error": "Boom", "data": { "status": "resolved" } }),
    );
    let own = send(&server, "Child", "INIT", json!({}));
    assert_eq!(pattern(&own, "Child").unwrap()["label"], json!("Boom"));
    assert_eq!(
        frame_payload(&own, "Child"),
        Some(json!({ "error": "Boom", "data": { "status": "resolved" } }))
    );
}

#[test]
fn control_before_any_composition_the_childs_own_run_has_nothing_to_capture() {
    let server = synthetic();
    let own = send(&server, "Child", "INIT", json!({}));
    assert!(
        pattern(&own, "Child").unwrap().get("label").is_none(),
        "{:?}",
        pattern(&own, "Child")
    );
    assert_eq!(frame_payload(&own, "Child"), None);
}

#[test]
fn a_later_composition_replaces_the_sticky_payload() {
    let server = synthetic();
    send(
        &server,
        "Host",
        "LOADED",
        json!({ "error": "First", "data": { "status": "open" } }),
    );
    send(
        &server,
        "Host",
        "LOADED",
        json!({ "error": "Second", "data": { "status": "open" } }),
    );
    let own = send(&server, "Child", "INIT", json!({}));
    assert_eq!(pattern(&own, "Child").unwrap()["label"], json!("Second"));
}

fn deals() -> EmbeddedServer {
    let content = trait_of(
        "Content",
        json!(null),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "INIT",
        "effects": [render_stack(json!(["@trait.Panel"]))] }]),
    );
    let panel = trait_of(
        "Panel",
        json!(null),
        idle(),
        json!([
            { "from": "idle", "to": "idle", "event": "INIT", "effects": [render_stack(json!([]))] },
            { "from": "idle", "to": "idle", "event": "DEALS_LOADED", "effects": [render_stack(json!(["@trait.Won"]))] }
        ]),
    );
    let won = trait_of(
        "Won",
        json!(null),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "INIT",
        "effects": [["render-ui", "main", { "type": "stat-display", "label": "Won",
            "value": ["array/len", ["array/filter", "@callsitePayload.data", ["fn", "d", ["=", "@d.status", "won"]]]] }]] }]),
    );
    app(vec![content, panel, won])
}

#[test]
fn only_the_children_the_firing_transition_composes_are_repainted() {
    let server = deals();
    let init = send(&server, "Panel", "INIT", json!({}));
    assert!(init.success);
    assert!(pattern(&init, "Won").is_none(), "INIT composes no child");
    let loaded = send(
        &server,
        "Panel",
        "DEALS_LOADED",
        json!({ "data": [{ "status": "won" }, { "status": "lost" }, { "status": "won" }] }),
    );
    assert_eq!(pattern(&loaded, "Won").unwrap()["value"], json!(2.0));
}

fn pass_through() -> EmbeddedServer {
    let host = trait_of(
        "Host",
        json!(null),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "PING",
        "effects": [render_stack(json!(["@trait.Ledger"]))] }]),
    );
    let ledger = trait_of(
        "Ledger",
        json!(null),
        json!([{ "name": "loading", "isInitial": true }, { "name": "browsing" }]),
        json!([
            { "from": "loading", "to": "loading", "event": "INIT", "effects": [render_stack(json!(["@trait.Child"]))] },
            { "from": "loading", "to": "browsing", "event": "LOADED", "effects": [render_stack(json!(["@trait.Child"]))] },
            { "from": "browsing", "to": "loading", "event": "INIT", "effects": [render_stack(json!(["@trait.Child"]))] }
        ]),
    );
    let child = trait_of(
        "Child",
        json!({ "content": { "type": "string", "default": "@callsitePayload.label" } }),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "button", "label": "@config.content" }]] }]),
    );
    app(vec![host, ledger, child])
}

#[test]
fn a_loaded_pass_through_child_stays_loaded_and_its_grandchild_keeps_what_the_child_composed_it_with(
) {
    let server = pass_through();
    send(&server, "Ledger", "INIT", json!({}));
    send(&server, "Ledger", "LOADED", json!({ "label": "Ledger" }));
    let ping = send(&server, "Host", "PING", json!({ "label": "Fresh" }));
    assert_eq!(
        ping.states.get("Ledger").map(String::as_str),
        Some("browsing")
    );
    assert!(ping
        .client_effects_by_trait
        .iter()
        .flatten()
        .all(|f| f.trait_name != "Ledger"));
    assert_eq!(pattern(&ping, "Child").unwrap()["label"], json!("Ledger"));
}

#[test]
fn control_a_child_whose_lifecycle_arm_is_a_self_loop_is_repainted_under_the_payload() {
    let server = pass_through();
    send(&server, "Ledger", "INIT", json!({}));
    let ping = send(&server, "Host", "PING", json!({ "label": "Fresh" }));
    assert_eq!(
        ping.states.get("Ledger").map(String::as_str),
        Some("loading")
    );
    assert!(ping
        .client_effects_by_trait
        .iter()
        .flatten()
        .any(|f| f.trait_name == "Ledger"));
    assert_eq!(pattern(&ping, "Child").unwrap()["label"], json!("Fresh"));
}

#[test]
fn a_pattern_object_inside_the_childs_children_receives_the_payload() {
    let feed = trait_of(
        "Feed",
        json!(null),
        json!([{ "name": "loading", "isInitial": true }, { "name": "browsing" }]),
        json!([{ "from": "loading", "to": "browsing", "event": "LOADED", "effects": [render_stack(json!(["@trait.Body"]))] }]),
    );
    let body = trait_of(
        "Body",
        json!({ "children": { "type": "unknown", "default": ["@trait.Title", { "type": "timeline", "entity": "@callsitePayload.data", "fields": ["title"] }] } }),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "stack", "children": "@config.children" }]] }]),
    );
    let title = trait_of(
        "Title",
        json!(null),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "INIT",
        "effects": [["render-ui", "main", { "type": "typography", "content": "Log" }]] }]),
    );
    let server = app(vec![feed, body, title]);
    let rows = json!([{ "id": "t1", "title": "Pour" }, { "id": "t2", "title": "Frame" }]);
    let loaded = send(&server, "Feed", "LOADED", json!({ "data": rows }));
    let stack = pattern(&loaded, "Body").unwrap();
    let timeline = stack["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == json!("timeline"))
        .unwrap();
    assert_eq!(timeline["entity"], rows);
}

fn walk_through() -> EmbeddedServer {
    let host = trait_of(
        "Host",
        json!(null),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "PING",
        "effects": [render_stack(json!(["@trait.Feed"]))] }]),
    );
    let feed = trait_of(
        "Feed",
        json!(null),
        json!([{ "name": "browsing", "isInitial": true }]),
        json!([
            { "from": "browsing", "to": "browsing", "event": "INIT", "effects": [["set", "@entity.id", "reload"], ["render-ui", "main", { "type": "spinner" }]] },
            { "from": "browsing", "to": "browsing", "event": "LOADED", "effects": [render_stack(json!(["@trait.Leaf"]))] }
        ]),
    );
    let leaf = trait_of(
        "Leaf",
        json!({ "content": { "type": "string", "default": "@callsitePayload.label" } }),
        idle(),
        json!([{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["render-ui", "main", { "type": "button", "label": "@config.content" }]] }]),
    );
    app(vec![host, feed, leaf])
}

#[test]
fn walking_through_a_reloading_child_the_grandchild_keeps_the_payload_its_own_composer_gave_it() {
    let server = walk_through();
    send(&server, "Feed", "LOADED", json!({ "label": "Mine" }));
    let ping = send(&server, "Host", "PING", json!({ "label": "Host" }));
    assert_eq!(pattern(&ping, "Leaf").unwrap()["label"], json!("Mine"));
}

#[test]
fn control_a_grandchild_never_composed_yet_takes_the_ancestors_payload() {
    let server = walk_through();
    let ping = send(&server, "Host", "PING", json!({ "label": "Host" }));
    assert_eq!(pattern(&ping, "Leaf").unwrap()["label"], json!("Host"));
}
