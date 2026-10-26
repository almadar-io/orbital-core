// A client kernel addresses each server leg to the trait whose own delivery needs the server (G-ORB-158), and exposes every executed delivery (G-ORB-169).

use {orbital_core_runtime::kernel::{already_delivered_from, RuntimeKernel}, orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig};
use orbital_core_runtime::ServerLegCollector;
use {orbital_core_runtime::ClientDispatch, orbital_core_runtime::effects::executor::EffectDelegate, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn browse(extra_transitions: Json) -> Json {
    let mut transitions = vec![
        json!({ "from": "browsing", "to": "browsing", "event": "EDIT_ROW",
            "effects": [["emit", "EDIT", { "id": "@payload.id" }]] }),
        json!({ "from": "browsing", "to": "browsing", "event": "DELETE_ROW",
            "effects": [["emit", "DELETE", { "id": "@payload.id" }]] }),
    ];
    transitions.extend(extra_transitions.as_array().unwrap().iter().cloned());
    json!({
        "name": "Browse", "linkedEntity": "Note",
        "stateMachine": { "states": [{ "name": "browsing", "isInitial": true }], "transitions": transitions }
    })
}

fn listener(name: &str, heard: &str, trigger: &str, effects: Json) -> Json {
    json!({
        "name": name, "linkedEntity": "Note",
        "listens": [{
            "event": heard, "triggers": trigger,
            "source": { "kind": "trait", "trait": "Browse" },
            "payloadMapping": { "noteId": "@payload.id" }
        }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [{ "from": "ready", "to": "ready", "event": trigger, "effects": effects }]
        }
    })
}

fn schema(traits: Vec<Json>) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [
                { "name": "id", "type": "string" },
                { "name": "title", "type": "string" }
            ] },
            "traits": traits
        }]
    }))
    .unwrap()
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

fn payload(id: &str) -> Value {
    serde_json::from_value(json!({ "id": id })).unwrap()
}

fn dispatch(schema: OrbitalSchema, trait_name: &str, event: &str) -> ClientDispatch {
    client(schema)
        .dispatch_with_server_leg("App", trait_name, None, event, payload("n1"), None)
        .unwrap()
}

fn persist_update() -> Json {
    json!([["persist", "update", "Note", "@payload.noteId", { "title": "x" }]])
}

fn persist_delete() -> Json {
    json!([["persist", "delete", "Note", "@payload.noteId"]])
}

#[test]
fn an_emit_only_seed_whose_listener_persists_yields_a_leg_to_the_listener() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener("NoteEdit", "EDIT", "NOTE_EDIT", persist_update()),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    let leg = &d.server_legs[0];
    assert_eq!(leg.target_trait.as_deref(), Some("NoteEdit"));
    assert_eq!(leg.event, "NOTE_EDIT");
    assert_eq!(leg.source_trait.as_deref(), Some("Browse"));
    let mapped = leg
        .payload
        .as_ref()
        .and_then(|p| p.get_path("noteId"))
        .and_then(Value::as_str);
    assert_eq!(
        mapped,
        Some("n1"),
        "the leg carries the mapped payload: {leg:?}"
    );
    let delivery = leg
        .delivery
        .as_ref()
        .expect("a continuation carries its delivery");
    assert_eq!(delivery.event, "EDIT");
    assert_eq!(delivery.source.r#trait.as_deref(), Some("Browse"));
    let traits = leg.traits.as_ref().expect("executed deliveries");
    assert!(traits
        .iter()
        .any(|t| t.r#trait == "NoteEdit" && t.from == "ready"));
    assert!(traits
        .iter()
        .any(|t| t.r#trait == "Browse" && t.from == "browsing"));
}

#[test]
fn control_a_seed_with_its_own_server_bound_arm_keeps_its_own_leg() {
    let own = json!([{ "from": "browsing", "to": "browsing", "event": "SAVE_ROW",
        "effects": [["persist", "create", "Note", { "title": "x" }], ["emit", "EDIT", { "id": "n1" }]] }]);
    let d = dispatch(
        schema(vec![
            browse(own),
            listener("NoteEdit", "EDIT", "NOTE_EDIT", persist_update()),
        ]),
        "Browse",
        "SAVE_ROW",
    );
    assert_eq!(
        d.server_legs.len(),
        1,
        "the server cascades the listener itself"
    );
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Browse"));
    assert_eq!(d.server_legs[0].event, "SAVE_ROW");
}

#[test]
fn two_server_bound_listeners_of_one_emit_yield_two_legs() {
    let both = |name: &str, trigger: &str, effects: Json| listener(name, "EDIT", trigger, effects);
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            both("NoteEdit", "NOTE_EDIT", persist_update()),
            both(
                "NoteAudit",
                "NOTE_AUDIT",
                json!([["persist", "create", "Note", { "title": "audit" }]]),
            ),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    let mut targets: Vec<(&str, &str)> = d
        .server_legs
        .iter()
        .map(|l| (l.target_trait.as_deref().unwrap(), l.event.as_str()))
        .collect();
    targets.sort();
    assert_eq!(
        targets,
        vec![("NoteAudit", "NOTE_AUDIT"), ("NoteEdit", "NOTE_EDIT")]
    );
}

#[test]
fn control_a_listener_that_stays_on_the_client_owes_no_leg() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener(
                "NoteEdit",
                "EDIT",
                "NOTE_EDIT",
                json!([["set", "@entity.title", "x"]]),
            ),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    assert!(d.server_legs.is_empty());
}

#[test]
fn control_only_the_listener_that_heard_the_event_gets_a_leg() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener("NoteEdit", "EDIT", "NOTE_EDIT", persist_update()),
            listener("NoteDelete", "DELETE", "NOTE_DELETE", persist_delete()),
        ]),
        "Browse",
        "DELETE_ROW",
    );
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("NoteDelete"));
    assert_eq!(d.server_legs[0].event, "NOTE_DELETE");
}

#[test]
fn a_listener_reached_through_another_legs_server_cascade_rides_that_leg() {
    let relay = json!({
        "name": "Relay", "linkedEntity": "Note",
        "listens": [{ "event": "EDIT", "triggers": "RELAY", "source": { "kind": "trait", "trait": "Browse" } }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [{ "from": "ready", "to": "ready", "event": "RELAY",
                "effects": [["persist", "create", "Note", { "title": "r" }], ["emit", "RELAYED", {}]] }]
        }
    });
    let downstream = json!({
        "name": "Mirror", "linkedEntity": "Note",
        "listens": [{ "event": "RELAYED", "triggers": "MIRROR", "source": { "kind": "trait", "trait": "Relay" } }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [{ "from": "ready", "to": "ready", "event": "MIRROR",
                "effects": [["persist", "create", "Note", { "title": "m" }]] }]
        }
    });
    let d = dispatch(
        schema(vec![browse(json!([])), relay, downstream]),
        "Browse",
        "EDIT_ROW",
    );
    let targets: Vec<&str> = d
        .server_legs
        .iter()
        .map(|l| l.target_trait.as_deref().unwrap())
        .collect();
    assert_eq!(
        targets,
        vec!["Relay"],
        "Mirror runs in Relay's server cascade, not a second time"
    );
}

#[test]
fn control_a_downstream_server_trait_behind_a_client_only_hop_still_gets_its_own_leg() {
    let hop = json!({
        "name": "Hop", "linkedEntity": "Note",
        "listens": [{ "event": "EDIT", "triggers": "HOP", "source": { "kind": "trait", "trait": "Browse" } }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [{ "from": "ready", "to": "ready", "event": "HOP", "effects": [["emit", "HOPPED", {}]] }]
        }
    });
    let sink = json!({
        "name": "Sink", "linkedEntity": "Note",
        "listens": [{ "event": "HOPPED", "triggers": "SINK", "source": { "kind": "trait", "trait": "Hop" } }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [{ "from": "ready", "to": "ready", "event": "SINK",
                "effects": [["persist", "create", "Note", { "title": "s" }]] }]
        }
    });
    let d = dispatch(
        schema(vec![browse(json!([])), hop, sink]),
        "Browse",
        "EDIT_ROW",
    );
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Sink"));
    assert_eq!(d.server_legs[0].event, "SINK");
}

#[test]
fn already_delivered_names_the_seed_and_each_leg_target() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener("NoteEdit", "EDIT", "NOTE_EDIT", persist_update()),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    let delivered = already_delivered_from(&d);
    assert!(delivered.contains(&("Browse".to_string(), "EDIT_ROW".to_string())));
    assert!(delivered.contains(&("NoteEdit".to_string(), "NOTE_EDIT".to_string())));
}

#[test]
fn control_without_a_leg_the_seed_is_not_claimed_as_delivered() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener(
                "NoteEdit",
                "EDIT",
                "NOTE_EDIT",
                json!([["set", "@entity.title", "x"]]),
            ),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    assert!(!already_delivered_from(&d).contains(&("Browse".to_string(), "EDIT_ROW".to_string())));
}

#[test]
fn a_self_loop_listen_delivery_is_among_the_dispatch_outcomes() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener(
                "NoteEdit",
                "EDIT",
                "NOTE_EDIT",
                json!([["set", "@entity.title", "x"]]),
            ),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    let (_, delivery) = d
        .outcomes
        .iter()
        .find(|(t, r)| t == "NoteEdit" && r.executed)
        .expect("the listener's ready -> ready delivery ran");
    assert_eq!(delivery.event.as_deref(), Some("NOTE_EDIT"));
    assert_eq!(
        (
            delivery.previous_state.as_str(),
            delivery.new_state.as_str()
        ),
        ("ready", "ready")
    );
    assert_eq!(
        delivery.payload.get_path("noteId").and_then(Value::as_str),
        Some("n1")
    );
}

#[test]
fn control_a_listener_of_an_event_nobody_emitted_has_no_outcome() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener(
                "NoteDelete",
                "DELETE",
                "NOTE_DELETE",
                json!([["set", "@entity.title", "x"]]),
            ),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    assert!(d.outcomes.iter().all(|(t, _)| t != "NoteDelete"));
    assert_eq!(d.seed, ("Browse".to_string(), "EDIT_ROW".to_string()));
}
