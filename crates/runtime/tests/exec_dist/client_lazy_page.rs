// EXEC-LOAD-2: a browser loads a lazy page into the one client kernel of its program, so the frames its session sends
// for that page's traits are the kernel's own; a fresh kernel per page refused them.

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::embed::{EmbeddedKernel, EmbeddedServer, ServerProgress};
use serde_json::{json, Value};
use std::collections::HashSet;

fn orbital(name: &str, entity: &str, trait_name: &str) -> Value {
    json!({
        "name": name,
        "entity": { "name": entity, "persistence": "runtime", "fields": [
            { "name": "id", "type": "string" }, { "name": "v", "type": "string" }] },
        "traits": [{ "name": trait_name, "linkedEntity": entity,
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "SET_V", "effects": [["set", "@entity.v", "@payload.v"]] }] } }]
    })
}

fn program(name: &str, orbitals: Vec<Value>) -> OrbitalSchema {
    from_json_value(&json!({ "name": name, "typeContractVersion": 1, "orbitals": orbitals })).unwrap()
}

fn site() -> OrbitalSchema {
    program("Site", vec![orbital("Home", "HomeDoc", "HomeWriter")])
}

fn quickstart() -> OrbitalSchema {
    program("Quickstart", vec![orbital("QuickstartPage", "QuickstartDoc", "QuickstartWriter")])
}

fn introduction() -> OrbitalSchema {
    program("Introduction", vec![orbital("IntroductionPage", "IntroductionDoc", "IntroductionWriter")])
}

fn server() -> EmbeddedServer {
    EmbeddedServer::new(program(
        "Site",
        vec![
            orbital("Home", "HomeDoc", "HomeWriter"),
            orbital("QuickstartPage", "QuickstartDoc", "QuickstartWriter"),
            orbital("IntroductionPage", "IntroductionDoc", "IntroductionWriter"),
        ],
    ))
    .unwrap()
}

fn send(server: &EmbeddedServer, orbital: &str, trait_name: &str) -> EventResponse {
    let request = EventRequest {
        event: "SET_V".to_string(),
        target_trait: Some(trait_name.to_string()),
        client_id: Some("tab".to_string()),
        payload: Some(from_json_value(&json!({ "v": trait_name })).unwrap()),
        ..Default::default()
    };
    match server.start(orbital, request).unwrap() {
        ServerProgress::Done(response) => response,
        _ => panic!("the dispatch ran to its end"),
    }
}

#[test]
fn a_lazy_page_loaded_into_the_programs_kernel_folds_the_frames_of_the_page_left_and_the_page_opened() {
    let server = server();
    let left = send(&server, "QuickstartPage", "QuickstartWriter");
    let opened = send(&server, "IntroductionPage", "IntroductionWriter");
    let client = EmbeddedKernel::new(site(), "tab").unwrap();
    client.register_lazy(quickstart()).unwrap();
    client.apply_server_response(&left, &HashSet::new()).unwrap();
    client.register_lazy(introduction()).unwrap();
    client
        .apply_server_response(&opened, &HashSet::new())
        .expect("the frame of the page left belongs to this kernel");
    assert_eq!(client.owning_orbital("IntroductionWriter").unwrap(), "IntroductionPage");
    assert_eq!(client.owning_orbital("QuickstartWriter").unwrap(), "QuickstartPage");
}

#[test]
fn control_a_kernel_built_from_one_lazy_page_refuses_the_frame_of_the_page_left() {
    let server = server();
    let session = send(&server, "QuickstartPage", "QuickstartWriter");
    assert!(session.frames.as_ref().is_some_and(|d| d.is_snapshot()), "a fresh kernel's first fold is the session's snapshot");
    let client = EmbeddedKernel::new(introduction(), "tab").unwrap();
    let refused = client
        .apply_server_response(&session, &HashSet::new())
        .expect_err("a fresh kernel per page does not declare the page left");
    assert!(refused.contains("QuickstartWriter"), "{refused}");
}

fn program_kernel() -> EmbeddedKernel {
    let client = EmbeddedKernel::new(site(), "tab").unwrap();
    client.register_lazy(quickstart()).unwrap();
    client.register_lazy(introduction()).unwrap();
    client
}

#[test]
fn the_page_left_closing_after_the_next_page_entered_leaves_nothing() {
    let client = program_kernel();
    client.enter(None, "quickstart", vec!["QuickstartWriter".into()]).unwrap();
    client.enter(None, "introduction", vec!["IntroductionWriter".into()]).unwrap();
    assert!(client.leave_mount("quickstart").is_empty(), "the quickstart page already left when introduction entered");
    let left = client.leave_mount("introduction");
    assert_eq!(left.iter().flat_map(|l| l.traits.clone()).collect::<Vec<_>>(), vec!["IntroductionWriter".to_string()], "introduction was still entered");
}

#[test]
fn control_the_page_entered_closing_leaves_its_traits() {
    let client = program_kernel();
    client.enter(None, "quickstart", vec!["QuickstartWriter".into()]).unwrap();
    let left = client.leave_mount("quickstart");
    assert_eq!(left.iter().flat_map(|l| l.traits.clone()).collect::<Vec<_>>(), vec!["QuickstartWriter".to_string()]);
}

#[test]
fn edge_loading_the_same_lazy_page_twice_registers_it_once() {
    let client = EmbeddedKernel::new(site(), "tab").unwrap();
    client.register_lazy(quickstart()).unwrap();
    client.register_lazy(quickstart()).unwrap();
    assert_eq!(client.owning_orbital("QuickstartWriter").unwrap(), "QuickstartPage");
    let response = client.dispatch("QuickstartWriter", None, "SET_V", from_json_value(&json!({ "v": "x" })).unwrap()).unwrap();
    assert_eq!(response.response.transitions.as_ref().map(Vec::len), Some(1), "one registration, one transition");
}
