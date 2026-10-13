// The server role ends a dispatch by bringing each trait's row to its frame as it stands, and echoing every
// non-empty frame to every trait bound to it: a bound trait that never ran still converges its row. JS twin:
// the final-row and frame-echo passes of `evaluateOrbitalEvent` (`riya-level-seed-reaches-the-body.test.ts`,
// `evaluate-shared-frame-final-row.test.ts`).

use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::{EventRequest, EventResponse, OrbitalSchema};
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
                    .map(|p| orbital_core::runtime::resumable::IoReply {
                        id: p.id,
                        answer: orbital_core::runtime::resumable::IoAnswer::Row(Err(
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
