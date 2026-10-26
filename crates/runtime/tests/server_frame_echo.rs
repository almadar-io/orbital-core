// The server role ends a dispatch by bringing each trait's row to its frame as it stands, and echoing every
// non-empty frame to every trait bound to it: a bound trait that never ran still converges its row. JS twin:
// the final-row and frame-echo passes of `evaluateOrbitalEvent` (`riya-level-seed-reaches-the-body.test.ts`,
// `evaluate-shared-frame-final-row.test.ts`).

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse, LifecycleEvent, MountSeed}, orbital_core_base::schema::types::OrbitalSchema};
use serde_json::{json, Value as Json};

fn schema() -> OrbitalSchema {
    let trait_of = |name: &str, entity: &str, arms: Json| {
        json!({ "name": name, "linkedEntity": entity,
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": arms } })
    };
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Doc", "shared": true, "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "v", "type": "string" }] },
            "auxiliaryEntities": [{ "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] }],
            "traits": [
                trait_of("Writer", "Doc", json!([{ "from": "idle", "to": "idle", "event": "WRITE", "effects": [["set", "@entity.v", "written"]] }])),
                trait_of("Sibling", "Doc", json!([])),
                trait_of("Private", "Note", json!([{ "from": "idle", "to": "idle", "event": "COUNT", "effects": [["set", "@entity.n", 3]] }])),
                trait_of("Bystander", "Note", json!([]))
            ]
        }]
    }))
    .unwrap()
}

fn run(server: &EmbeddedServer, event: &str, target: &str) -> EventResponse {
    let request = EventRequest {
        event: event.to_string(),
        target_trait: Some(target.to_string()),
        ..Default::default()
    };
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|p| orbital_core_runtime::resumable::IoReply {
                        id: p.id,
                        answer: orbital_core_runtime::resumable::IoAnswer::Row(Err(
                            "no row".into()
                        )),
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn rows(response: &EventResponse) -> Json {
    serde_json::to_value(response.entity_by_trait.clone().unwrap_or_default()).unwrap()
}

#[test]
fn a_shared_frame_is_echoed_to_every_trait_bound_to_it() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let rows = rows(&run(&server, "WRITE", "Writer"));
    assert_eq!(rows["Writer"]["v"], json!("written"));
    assert_eq!(rows["Sibling"]["v"], json!("written"));
}

#[test]
fn a_frame_written_by_an_earlier_request_reaches_a_request_that_changes_nothing() {
    let server = EmbeddedServer::new(schema()).unwrap();
    run(&server, "WRITE", "Writer");
    let later = run(&server, "NOTHING_HANDLES_THIS", "Sibling");
    assert_eq!(rows(&later)["Sibling"]["v"], json!("written"));
    assert_eq!(rows(&later)["Writer"]["v"], json!("written"));
}

#[test]
fn a_private_frame_stays_with_its_trait_and_its_entitys_other_traits_do_not_see_it() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let rows = rows(&run(&server, "COUNT", "Private"));
    assert_eq!(rows["Private"]["n"], json!(3.0));
    assert!(rows.get("Bystander").is_none(), "{rows}");
}

#[test]
fn control_with_no_frame_written_there_are_no_rows_to_echo() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let response = run(&server, "NOTHING_HANDLES_THIS", "Writer");
    assert!(
        response.entity_by_trait.is_none()
            || rows(&response).as_object().is_some_and(|m| m
                .values()
                .all(|r| r.as_object().is_some_and(|o| o.is_empty())))
    );
}

// A dispatch row is the trait's id plus only what its steps wrote in that dispatch; only a mount carries whole rows.

fn row_keys(rows: &Json, trait_name: &str) -> Vec<String> {
    rows[trait_name].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

fn only_id_and(rows: &Json, trait_name: &str, written: &[&str]) -> bool {
    row_keys(rows, trait_name).iter().all(|k| k == "id" || written.contains(&k.as_str()))
}

fn mount_of(trait_name: &str) -> EventRequest {
    EventRequest {
        event: "INIT".to_string(),
        mount: Some(vec![MountSeed { r#trait: trait_name.to_string(), event: LifecycleEvent::Init }]),
        ..Default::default()
    }
}

fn start(server: &EmbeddedServer, request: EventRequest) -> EventResponse {
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|p| orbital_core_runtime::resumable::IoReply {
                        id: p.id,
                        answer: orbital_core_runtime::resumable::IoAnswer::Row(Err("no row".into())),
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
        }
    }
}

fn list_schema(bump_resets_the_list: bool) -> OrbitalSchema {
    let items: Vec<Json> = (0..150).map(|i| json!({ "k": format!("item-{i:04}"), "pad": "x".repeat(80) })).collect();
    let mut bump = vec![json!(["set", "@entity.count", ["+", "@entity.count", 1]])];
    if bump_resets_the_list {
        bump.push(json!(["set", "@entity.items", "@entity.items"]));
    }
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Ledger", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "count", "type": "number", "default": 0 },
                { "name": "items", "type": "array", "default": items }
            ] },
            "traits": [{ "name": "Holder", "linkedEntity": "Ledger",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [["set", "@entity.count", 0]] },
                        { "from": "idle", "to": "idle", "event": "BUMP", "effects": bump }
                    ] } }]
        }]
    }))
    .unwrap()
}

fn bump_rows_bytes(server: &EmbeddedServer, requests: usize) -> (usize, Vec<Json>) {
    let mut bytes = 0;
    let mut all = Vec::new();
    for _ in 0..requests {
        let rows = rows(&run(server, "BUMP", "Holder"));
        bytes += serde_json::to_string(&rows).unwrap().len();
        all.push(rows);
    }
    (bytes, all)
}

#[test]
fn a_counter_only_request_carries_the_counter_and_never_the_held_list() {
    let server = EmbeddedServer::new(list_schema(false)).unwrap();
    start(&server, mount_of("Holder"));
    let (bytes, all) = bump_rows_bytes(&server, 50);
    for rows in &all {
        assert!(rows["Holder"].get("count").is_some(), "{rows}");
        assert!(rows["Holder"].get("items").is_none(), "the unchanged list rode the response");
    }
    assert!(bytes < 50 * 300, "{bytes} bytes over 50 requests");
}

#[test]
fn control_a_request_that_rewrites_the_list_carries_it_and_the_size_shows() {
    let server = EmbeddedServer::new(list_schema(true)).unwrap();
    start(&server, mount_of("Holder"));
    let (bytes, all) = bump_rows_bytes(&server, 50);
    assert!(all.iter().all(|rows| rows["Holder"].get("items").is_some()));
    assert!(bytes > 50 * 10_000, "{bytes} bytes over 50 requests");
}

#[test]
fn a_mount_request_returns_the_whole_row_with_the_list() {
    let server = EmbeddedServer::new(list_schema(false)).unwrap();
    let rows = rows(&start(&server, mount_of("Holder")));
    assert_eq!(rows["Holder"]["items"].as_array().map(Vec::len), Some(150), "{rows}");
    assert!(rows["Holder"].get("count").is_some());
}

#[test]
fn control_the_request_after_a_mount_returns_only_what_it_wrote() {
    let server = EmbeddedServer::new(list_schema(false)).unwrap();
    start(&server, mount_of("Holder"));
    let after = rows(&run(&server, "BUMP", "Holder"));
    assert!(only_id_and(&after, "Holder", &["count"]), "{after}");
}

fn doc_schema(extra_traits: Vec<Json>) -> OrbitalSchema {
    let mut traits = vec![
        json!({ "name": "WriterA", "linkedEntity": "Doc3", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "WRITE_V", "effects": [["set", "@entity.v", "written"]] }] } }),
        json!({ "name": "ReaderB", "linkedEntity": "Doc3", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } }),
    ];
    traits.extend(extra_traits);
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Doc3", "shared": true, "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "v", "type": "string", "default": "v0" },
                { "name": "w", "type": "string", "default": "w0" },
                { "name": "x", "type": "string", "default": "x0" }
            ] },
            "traits": traits
        }]
    }))
    .unwrap()
}

#[test]
fn a_shared_frames_written_field_alone_reaches_every_bound_trait() {
    let server = EmbeddedServer::new(doc_schema(vec![])).unwrap();
    let rows = rows(&run(&server, "WRITE_V", "WriterA"));
    assert_eq!(rows["ReaderB"]["v"], json!("written"), "{rows}");
    assert!(only_id_and(&rows, "ReaderB", &["v"]), "{rows}");
    assert!(only_id_and(&rows, "WriterA", &["v"]), "{rows}");
}

#[test]
fn control_a_shared_field_no_trait_wrote_is_absent_from_every_row() {
    let server = EmbeddedServer::new(doc_schema(vec![])).unwrap();
    let rows = rows(&run(&server, "WRITE_V", "WriterA"));
    for trait_name in ["WriterA", "ReaderB"] {
        assert!(rows[trait_name].get("w").is_none() && rows[trait_name].get("x").is_none(), "{rows}");
    }
}

fn cascade_schema(second_writes: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Doc3", "shared": true, "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "v", "type": "string", "default": "v0" },
                { "name": "w", "type": "string", "default": "w0" },
                { "name": "x", "type": "string", "default": "x0" }
            ] },
            "traits": [
                { "name": "First", "linkedEntity": "Doc3", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                        "effects": [["set", "@entity.v", "one"], ["emit", "HOP", {}]] }] } },
                { "name": "Second", "linkedEntity": "Doc3",
                  "listens": [{ "event": "HOP", "triggers": "ON_HOP", "source": { "kind": "trait", "trait": "First" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "ON_HOP", "effects": second_writes }] } }
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn cascade_steps_writing_the_same_field_leave_the_last_value() {
    let server = EmbeddedServer::new(cascade_schema(json!([["set", "@entity.v", "two"]]))).unwrap();
    let rows = rows(&run(&server, "GO", "First"));
    assert_eq!(rows["First"]["v"], json!("two"), "{rows}");
    assert_eq!(rows["Second"]["v"], json!("two"), "{rows}");
}

#[test]
fn cascade_steps_writing_different_fields_leave_both() {
    let server = EmbeddedServer::new(cascade_schema(json!([["set", "@entity.w", "second"]]))).unwrap();
    let rows = rows(&run(&server, "GO", "First"));
    for trait_name in ["First", "Second"] {
        assert_eq!(rows[trait_name]["v"], json!("one"), "{rows}");
        assert_eq!(rows[trait_name]["w"], json!("second"), "{rows}");
        assert!(rows[trait_name].get("x").is_none(), "{rows}");
    }
}

fn quiet_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "n", "type": "number", "default": 7 }
            ] },
            "traits": [
                { "name": "Pinger", "linkedEntity": "Note", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "PING", "effects": [] }] } },
                { "name": "Idle", "linkedEntity": "Note", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [] } }
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn a_step_that_wrote_nothing_still_has_its_row_with_no_fields() {
    let server = EmbeddedServer::new(quiet_schema()).unwrap();
    let rows = rows(&run(&server, "PING", "Pinger"));
    assert!(rows.get("Pinger").is_some(), "the stepped trait has no row: {rows}");
    assert!(only_id_and(&rows, "Pinger", &[]), "{rows}");
}

#[test]
fn control_a_trait_that_did_not_step_and_whose_frame_was_not_written_has_no_row() {
    let server = EmbeddedServer::new(quiet_schema()).unwrap();
    let rows = rows(&run(&server, "PING", "Pinger"));
    assert!(rows.get("Idle").is_none(), "{rows}");
}

// Replaces `a_frame_written_by_an_earlier_request_reaches_a_request_that_changes_nothing` in K2.
#[test]
fn a_request_that_changes_nothing_carries_no_earlier_fields() {
    let server = EmbeddedServer::new(schema()).unwrap();
    run(&server, "WRITE", "Writer");
    let later = rows(&run(&server, "NOTHING_HANDLES_THIS", "Sibling"));
    for trait_name in ["Sibling", "Writer"] {
        assert!(later[trait_name].get("v").is_none(), "{later}");
    }
}
