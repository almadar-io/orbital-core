// A client kernel addresses each server leg to the trait whose own delivery needs the server (G-ORB-158), and exposes every executed delivery (G-ORB-169).

use orbital_core_base::json_value::from_json_value;
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

/// `Browse` as a `[client]` trait: the host runs no copy of it, so its server listeners are the legs.
fn client_browse() -> Json {
    let mut trait_json = browse(json!([]));
    trait_json["dispatch"] = json!("client");
    trait_json
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
    from_json_value(&json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
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
    from_json_value(&json!({ "id": id })).unwrap()
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
            client_browse(),
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
            client_browse(),
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
            client_browse(),
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
        schema(vec![client_browse(), relay, downstream]),
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

/// EXEC-DIST-10: `Hop` is a server trait whose emitter ran in the client and posted none, so it is the leg; `Sink`, whose
/// emitter's leg posts, is the host's cascade from it (a stateful host never sees a carried state, so it must run `Hop`).
#[test]
fn a_server_hop_that_needs_no_server_itself_is_the_leg_and_its_downstream_rides_it() {
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
        schema(vec![client_browse(), hop, sink]),
        "Browse",
        "EDIT_ROW",
    );
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Hop"));
    assert_eq!(d.server_legs[0].event, "HOP");
}

fn hop() -> Json {
    json!({
        "name": "Hop", "linkedEntity": "Note",
        "listens": [{ "event": "EDIT", "triggers": "HOP", "source": { "kind": "trait", "trait": "Browse" } }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [{ "from": "ready", "to": "ready", "event": "HOP", "effects": [["emit", "HOPPED", {}]] }]
        }
    })
}

fn opening(name: &str, heard: &str, source: &str, effects: Json) -> Json {
    json!({
        "name": name, "linkedEntity": "Note",
        "listens": [{ "event": heard, "triggers": "OPEN", "source": { "kind": "trait", "trait": source } }],
        "stateMachine": {
            "states": [{ "name": "closed", "isInitial": true }, { "name": "open" }],
            "transitions": [{ "from": "closed", "to": "open", "event": "OPEN", "effects": effects }]
        }
    })
}

fn carried_from(d: &ClientDispatch, name: &str) -> String {
    d.server_legs[0]
        .traits
        .as_ref()
        .expect("carried traits")
        .iter()
        .find(|t| t.r#trait == name)
        .unwrap_or_else(|| panic!("{name} is carried: {:?}", d.server_legs[0].traits))
        .from
        .clone()
}

/// EXEC-DIST-10: the host runs `Form` in its cascade from the leg's target, so the leg carries the state that step starts
/// from; carried where the client left it, the host finds no arm and the form's server work never runs.
#[test]
fn a_listener_riding_a_delivery_leg_is_carried_from_the_state_its_step_starts_in() {
    let form = opening("Form", "HOPPED", "Hop", json!([["persist", "create", "Note", { "title": "s" }]]));
    let d = dispatch(schema(vec![client_browse(), hop(), form]), "Browse", "EDIT_ROW");
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Hop"));
    assert_eq!(carried_from(&d, "Form"), "closed");
}

#[test]
fn control_a_moved_trait_outside_the_legs_cascade_is_carried_where_the_client_left_it() {
    let form = opening("Form", "HOPPED", "Hop", json!([["persist", "create", "Note", { "title": "s" }]]));
    let bystander = opening("Bystander", "EDIT", "Browse", json!([["set", "@entity.title", "x"]]));
    let d = dispatch(schema(vec![client_browse(), bystander, hop(), form]), "Browse", "EDIT_ROW");
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Hop"));
    assert_eq!(carried_from(&d, "Bystander"), "open");
    assert_eq!(carried_from(&d, "Form"), "closed");
}

#[test]
fn edge_a_client_only_listener_of_the_legs_target_is_carried_where_the_client_left_it() {
    let form = opening("Form", "HOPPED", "Hop", json!([["persist", "create", "Note", { "title": "s" }]]));
    let mut panel = opening("Panel", "HOPPED", "Hop", json!([["set", "@entity.title", "x"]]));
    panel["dispatch"] = json!("client");
    let d = dispatch(schema(vec![client_browse(), hop(), panel, form]), "Browse", "EDIT_ROW");
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(carried_from(&d, "Panel"), "open");
    assert_eq!(carried_from(&d, "Form"), "closed");
}

#[test]
fn edge_a_listener_two_hops_below_the_legs_target_is_carried_from_its_starting_state() {
    let form = opening("Form", "HOPPED", "Hop", json!([["emit", "FORM_OPENED", {}]]));
    let loader = opening("Loader", "FORM_OPENED", "Form", json!([["persist", "create", "Note", { "title": "s" }]]));
    let d = dispatch(schema(vec![client_browse(), hop(), form, loader]), "Browse", "EDIT_ROW");
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(carried_from(&d, "Form"), "closed");
    assert_eq!(carried_from(&d, "Loader"), "closed");
}

#[test]
fn already_delivered_names_the_seed_and_each_leg_target() {
    let d = dispatch(
        schema(vec![
            client_browse(),
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

/// EXEC-DIST-10 / EXEC-DIST-2: a server seed is the first server trait of its own cascade, so it is the leg and its
/// listeners ride the host's cascade from it; a stateful host is carried no states and must run the seed's step.
#[test]
fn a_server_seed_whose_listener_persists_is_the_leg() {
    let d = dispatch(
        schema(vec![
            browse(json!([])),
            listener("NoteEdit", "EDIT", "NOTE_EDIT", persist_update()),
        ]),
        "Browse",
        "EDIT_ROW",
    );
    let shapes: Vec<(&str, Option<&str>)> = d.server_legs.iter().map(|l| (l.event.as_str(), l.target_trait.as_deref())).collect();
    assert_eq!(shapes, vec![("EDIT_ROW", Some("Browse"))]);
}

fn relation_form_schema(person: Json, form: Json) -> OrbitalSchema {
    from_json_value(&json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Task", "persistence": "persistent", "fields": [
                { "name": "id", "type": "string" },
                { "name": "title", "type": "string" },
                { "name": "assignee", "type": "relation", "relation": { "entity": "Person", "cardinality": "one", "display": "name" } }
            ] },
            "auxiliaryEntities": [person],
            "traits": [form]
        }]
    }))
    .unwrap()
}

fn person(extra: Json) -> Json {
    let mut entity = json!({ "name": "Person", "persistence": "persistent", "fields": [
        { "name": "id", "type": "string" }, { "name": "name", "type": "string" }] });
    for (k, v) in extra.as_object().unwrap() {
        entity[k] = v.clone();
    }
    entity
}

fn form_trait(node: Json) -> Json {
    json!({
        "name": "Form", "linkedEntity": "Task",
        "stateMachine": {
            "states": [{ "name": "closed", "isInitial": true }, { "name": "open" }],
            "transitions": [{ "from": "closed", "to": "open", "event": "OPEN", "effects": [["render-ui", "modal", node]] }]
        }
    })
}

fn open_form(schema: OrbitalSchema) -> ClientDispatch {
    client(schema)
        .dispatch_with_server_leg("App", "Form", None, "OPEN", Value::Null, None)
        .unwrap()
}

/// The browser holds no rows of a server-stored entity, so the options of a relation field are the host's to give
/// (`relationsData`): a step that paints a form naming one is posted, and the host paints it.
#[test]
fn a_step_that_paints_a_relation_field_of_server_rows_is_posted_to_the_host() {
    let node = json!({ "type": "form-section", "fields": ["title", "assignee"] });
    let d = open_form(relation_form_schema(person(json!({})), form_trait(node)));
    let shapes: Vec<(&str, Option<&str>)> = d.server_legs.iter().map(|l| (l.event.as_str(), l.target_trait.as_deref())).collect();
    assert_eq!(shapes, vec![("OPEN", Some("Form"))]);
}

#[test]
fn control_a_form_with_no_relation_field_stays_in_the_browser() {
    let node = json!({ "type": "form-section", "fields": ["title"] });
    let d = open_form(relation_form_schema(person(json!({})), form_trait(node)));
    assert!(d.server_legs.is_empty(), "{:?}", d.server_legs);
}

#[test]
fn control_a_relation_to_browser_stored_rows_stays_in_the_browser() {
    let node = json!({ "type": "form-section", "fields": ["title", "assignee"] });
    let d = open_form(relation_form_schema(person(json!({ "browser": true })), form_trait(node)));
    assert!(d.server_legs.is_empty(), "{:?}", d.server_legs);
}

#[test]
fn edge_authored_relation_options_need_no_host() {
    let node = json!({ "type": "form-section", "fields": ["title", "assignee"],
        "relationsData": { "assignee": [{ "value": "p1", "label": "Ana" }] } });
    let d = open_form(relation_form_schema(person(json!({})), form_trait(node)));
    assert!(d.server_legs.is_empty(), "{:?}", d.server_legs);
}

#[test]
fn edge_a_client_trait_painting_a_relation_field_posts_nothing() {
    let node = json!({ "type": "form-section", "fields": ["title", "assignee"] });
    let mut form = form_trait(node);
    form["dispatch"] = json!("client");
    let d = open_form(relation_form_schema(person(json!({})), form));
    assert!(d.server_legs.is_empty(), "{:?}", d.server_legs);
}

fn stashing_listener() -> Json {
    json!({
        "name": "NoteEdit", "linkedEntity": "Note",
        "listens": [{ "event": "EDIT", "triggers": "NOTE_EDIT", "source": { "kind": "trait", "trait": "Browse" } }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [
                { "from": "ready", "to": "ready", "event": "STASH", "effects": [["set", "@entity.title", "kept"]] },
                { "from": "ready", "to": "ready", "event": "NOTE_EDIT", "effects": persist_update() }
            ]
        }
    })
}

fn carried_title(d: &ClientDispatch, name: &str) -> Option<String> {
    d.server_legs[0]
        .entity_by_trait
        .as_ref()
        .and_then(|rows| rows.get(name))
        .and_then(|row| row.get_path("title"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// EXEC-DIST-10: the host runs `NoteEdit` in its cascade from the seed's leg, so the leg carries the row that step
/// starts from; without it the host runs the step over defaults and a guard on what the browser stashed refuses.
#[test]
fn a_seed_leg_carries_the_row_of_a_listener_the_host_runs_in_its_cascade() {
    let k = client(schema(vec![browse(json!([])), stashing_listener()]));
    k.dispatch_with_server_leg("App", "NoteEdit", None, "STASH", Value::Null, None).unwrap();
    let d = k.dispatch_with_server_leg("App", "Browse", None, "EDIT_ROW", payload("n1"), None).unwrap();
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Browse"));
    assert_eq!(carried_title(&d, "NoteEdit").as_deref(), Some("kept"));
}

#[test]
fn a_delivery_leg_carries_the_row_of_a_listener_the_host_runs_below_its_target() {
    let relay = json!({
        "name": "Relay", "linkedEntity": "Note",
        "listens": [{ "event": "EDIT", "triggers": "RELAY", "source": { "kind": "trait", "trait": "Browse" } }],
        "stateMachine": {
            "states": [{ "name": "ready", "isInitial": true }],
            "transitions": [{ "from": "ready", "to": "ready", "event": "RELAY", "effects": [["emit", "RELAYED", {}]] }]
        }
    });
    let mut sink = stashing_listener();
    sink["listens"] = json!([{ "event": "RELAYED", "triggers": "NOTE_EDIT", "source": { "kind": "trait", "trait": "Relay" } }]);
    let k = client(schema(vec![client_browse(), relay, sink]));
    k.dispatch_with_server_leg("App", "NoteEdit", None, "STASH", Value::Null, None).unwrap();
    let d = k.dispatch_with_server_leg("App", "Browse", None, "EDIT_ROW", payload("n1"), None).unwrap();
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Relay"));
    assert_eq!(carried_title(&d, "NoteEdit").as_deref(), Some("kept"));
}

#[test]
fn control_a_listener_with_no_row_of_its_own_adds_no_carried_row() {
    let d = dispatch(
        schema(vec![browse(json!([])), listener("NoteEdit", "EDIT", "NOTE_EDIT", persist_update())]),
        "Browse",
        "EDIT_ROW",
    );
    assert_eq!(d.server_legs.len(), 1);
    assert_eq!(carried_title(&d, "NoteEdit"), None);
}
