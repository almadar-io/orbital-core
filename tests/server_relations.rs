// `fetch … { include }` hydrates relations (a self-relation as a whole tree that stops at a repeated
// ancestor) and a server `render-ui` ships `relationsData` for the relation fields its form / detail /
// column-bearing nodes list. JS twins: `populate-self-relation-depth`, `populate-relations-auxiliary-entity`,
// `relation-options-injection` in `almadar-runtime/test`.

use std::sync::Arc;
use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::runtime::resumable::{IoAnswer, IoReply, IoRequest};
use orbital_core::{EventRequest, EventResponse, OrbitalSchema, Value};
use serde_json::{json, Value as Json};
use std::collections::HashMap;

fn note_entity() -> Json {
    json!({
        "name": "Note", "persistence": "persistent",
        "fields": [
            { "name": "id", "type": "string", "primaryKey": true },
            { "name": "content", "type": "string" },
            { "name": "replies", "type": "relation", "relation": { "entity": "Note", "cardinality": "many" } }
        ]
    })
}

fn list_trait() -> Json {
    json!({
        "name": "NoteList", "linkedEntity": "Note",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "LOAD",
                "effects": [["fetch", "Note", { "include": ["replies"], "emit": { "success": "NotesLoaded" } }]] }]
        }
    })
}

fn notes_schema(note_is_primary: bool) -> OrbitalSchema {
    let task = json!({ "name": "Task", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }] });
    let mut orbital = json!({ "name": "NotesOrbital", "traits": [list_trait()] });
    if note_is_primary {
        orbital["entity"] = note_entity();
    } else {
        orbital["entity"] = task;
        orbital["auxiliaryEntities"] = json!([note_entity()]);
    }
    serde_json::from_value(json!({ "name": "App", "orbitals": [orbital] })).unwrap()
}

type Store = HashMap<String, Vec<Json>>;

fn drive(
    server: &EmbeddedServer,
    orbital: &str,
    request: EventRequest,
    store: &Store,
) -> EventResponse {
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
                            IoRequest::ListRows { entity_type } => IoAnswer::Rows(Ok(store
                                .get(&entity_type)
                                .into_iter()
                                .flatten()
                                .map(|r| Arc::new(serde_json::from_value(r.clone()).unwrap()))
                                .collect())),
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

fn load(note_is_primary: bool, notes: Vec<Json>) -> Json {
    let server = EmbeddedServer::new(notes_schema(note_is_primary)).unwrap();
    let store = Store::from([("Note".to_string(), notes)]);
    let request = EventRequest {
        event: "LOAD".to_string(),
        target_trait: Some("NoteList".to_string()),
        ..Default::default()
    };
    let response = drive(&server, "NotesOrbital", request, &store);
    let loaded = response
        .emitted_events
        .iter()
        .find(|e| e.event == "NotesLoaded")
        .and_then(|e| e.payload.clone())
        .expect("NotesLoaded");
    serde_json::to_value(loaded.get_path("data").unwrap()).unwrap()
}

/// `row` as `{ content, replies: [tree…] }`.
fn tree(row: &Json) -> Json {
    let replies: Vec<Json> = row["replies"]
        .as_array()
        .map(|rs| rs.iter().filter(|r| r.is_object()).map(tree).collect())
        .unwrap_or_default();
    json!({ "content": row["content"], "replies": replies })
}

fn top(rows: &Json, content: &str) -> Json {
    tree(
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["content"] == json!(content))
            .unwrap(),
    )
}

fn note(id: &str, content: &str, replies: &[&str]) -> Json {
    json!({ "id": id, "content": content, "replies": replies })
}

#[test]
fn a_reply_to_a_reply_is_loaded_under_its_parent() {
    let rows = load(
        true,
        vec![
            note("g", "grandchild", &[]),
            note("c", "child", &["g"]),
            note("t", "top", &["c"]),
        ],
    );
    assert_eq!(
        top(&rows, "top"),
        json!({ "content": "top", "replies": [{ "content": "child", "replies": [{ "content": "grandchild", "replies": [] }] }] })
    );
}

#[test]
fn control_a_one_level_reply_is_loaded_as_before() {
    let rows = load(
        true,
        vec![note("c", "child", &[]), note("t", "top", &["c"])],
    );
    assert_eq!(
        top(&rows, "top"),
        json!({ "content": "top", "replies": [{ "content": "child", "replies": [] }] })
    );
}

#[test]
fn a_cycle_stops_at_the_repeated_ancestor_and_stays_serializable() {
    let rows = load(true, vec![note("a", "a", &["b"]), note("b", "b", &["a"])]);
    assert!(serde_json::to_string(&rows).is_ok());
    assert_eq!(
        top(&rows, "a"),
        json!({ "content": "a", "replies": [{ "content": "b", "replies": [] }] })
    );
}

#[test]
fn a_post_listing_itself_as_a_reply_hydrates_no_replies() {
    let rows = load(true, vec![note("a", "a", &["a"])]);
    assert_eq!(top(&rows, "a"), json!({ "content": "a", "replies": [] }));
}

#[test]
fn an_auxiliary_entitys_self_relation_is_hydrated_to_rows() {
    let rows = load(
        false,
        vec![note("c", "a reply", &[]), note("t", "top", &["c"])],
    );
    assert_eq!(
        top(&rows, "top"),
        json!({ "content": "top", "replies": [{ "content": "a reply", "replies": [] }] })
    );
}

#[test]
fn control_a_fetch_without_include_leaves_ids() {
    let mut schema = notes_schema(true);
    let mut value = serde_json::to_value(&mut schema).unwrap();
    value["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["effects"] =
        json!([["fetch", "Note", { "emit": { "success": "NotesLoaded" } }]]);
    let schema: OrbitalSchema = serde_json::from_value(value).unwrap();
    let server = EmbeddedServer::new(schema).unwrap();
    let store = Store::from([(
        "Note".to_string(),
        vec![note("c", "child", &[]), note("t", "top", &["c"])],
    )]);
    let request = EventRequest {
        event: "LOAD".to_string(),
        target_trait: Some("NoteList".to_string()),
        ..Default::default()
    };
    let response = drive(&server, "NotesOrbital", request, &store);
    let data = response.emitted_events[0].payload.clone().unwrap();
    let rows = serde_json::to_value(data.get_path("data").unwrap()).unwrap();
    let t = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["content"] == json!("top"))
        .unwrap();
    assert_eq!(t["replies"], json!(["c"]));
}

fn editor_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [
            { "name": "StaffOrbital",
              "entity": { "name": "Staff", "persistence": "persistent", "fields": [{ "name": "id", "type": "string" }, { "name": "name", "type": "string" }] },
              "traits": [] },
            { "name": "TaskOrbital",
              "entity": { "name": "Task", "persistence": "persistent", "fields": [
                  { "name": "id", "type": "string" }, { "name": "title", "type": "string" },
                  { "name": "assignee", "type": "relation", "relation": { "entity": "Staff", "cardinality": "one" } }
              ] },
              "traits": [{
                  "name": "editor", "linkedEntity": "Task",
                  "stateMachine": {
                      "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "OPEN",
                          "effects": [["render-ui", "modal", { "type": "stack", "children": [
                              { "type": "form-section", "mode": "edit", "fields": ["title", "assignee"] },
                              { "type": "detail-panel", "fields": [{ "key": "title" }, { "key": "assignee" }] },
                              { "type": "table-view", "columns": [{ "key": "title" }, { "key": "assigneeCol", "field": "assignee" }] }
                          ] }]]}]
                  }
              }] }
        ]
    }))
    .unwrap()
}

fn staff() -> Store {
    Store::from([(
        "Staff".to_string(),
        vec![
            json!({ "id": "s1", "name": "Ada" }),
            json!({ "id": "s2", "title": "Grace" }),
        ],
    )])
}

fn open() -> Json {
    let server = EmbeddedServer::new(editor_schema()).unwrap();
    let request = EventRequest {
        event: "OPEN".to_string(),
        target_trait: Some("editor".to_string()),
        ..Default::default()
    };
    let response = drive(&server, "TaskOrbital", request, &staff());
    serde_json::to_value(response.client_effects.unwrap()[0].clone()).unwrap()
}

#[test]
fn form_detail_and_column_nodes_carry_the_relation_options() {
    let render = open();
    let children = render[2]["children"].as_array().unwrap();
    for node in children {
        assert_eq!(
            node["relationsData"]["assignee"],
            json!([{ "value": "s1", "label": "Ada" }, { "value": "s2", "label": "Grace" }]),
            "{node}"
        );
    }
    assert!(children[2]["relationsData"].get("assigneeCol").is_none());
}

#[test]
fn control_a_node_without_relation_fields_and_the_wrapper_stay_untouched() {
    let render = open();
    assert!(render[2].get("relationsData").is_none());
    let _: Value = serde_json::from_value(render).unwrap();
}
