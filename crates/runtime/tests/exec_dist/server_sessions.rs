// EXEC-DIST-9 on the embedded server (the JS hosts' kernel): a request naming a client runs in that client's session,
// and the kernel frees a session once its live streams have all dropped and nothing of its own runs. Native twins:
// the session tests of orbital-shell-rust `host::runtime`.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::resumable::{IoAnswer, IoReply};
use orbital_core_base::schema::types::OrbitalSchema;
use serde_json::json;

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Draft", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "name", "type": "string" }, { "name": "seen", "type": "string" }] },
            "traits": [{ "name": "Editor", "linkedEntity": "Draft",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [["set", "@entity.name", "fresh"]] },
                        { "from": "idle", "to": "idle", "event": "SET", "effects": [["set", "@entity.name", "@payload.name"]] },
                        { "from": "idle", "to": "idle", "event": "LOOK", "effects": [["set", "@entity.seen", "@entity.name"]] },
                        { "from": "idle", "to": "idle", "event": "ASK", "effects": [["call-service", "svc", "work", {}]] }
                    ] } }]
        }]
    }))
    .unwrap()
}

fn request(event: &str, client: &str, name: Option<&str>) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        target_trait: Some("Editor".to_string()),
        client_id: Some(client.to_string()),
        payload: name.map(|name| serde_json::from_value(json!({ "name": name })).unwrap()),
        ..Default::default()
    }
}

fn done(progress: ServerProgress) -> EventResponse {
    match progress {
        ServerProgress::Done(response) => response,
        ServerProgress::Await { .. } => panic!("the dispatch asked the host for something"),
        ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
    }
}

fn name_in(response: &EventResponse) -> serde_json::Value {
    let rows = serde_json::to_value(response.entity_by_trait.clone().unwrap_or_default()).unwrap();
    rows["Editor"]["seen"].clone()
}

#[test]
fn exec_dist_9_a_clients_frame_is_its_own_session_and_another_client_does_not_overwrite_it() {
    let server = EmbeddedServer::new(schema()).unwrap();
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    done(server.start("Main", request("SET", "tab-b", Some("cli"))).unwrap());
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("app"));
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-b", None)).unwrap())), json!("cli"));
}

#[test]
fn exec_dist_9_a_session_is_freed_once_its_live_stream_drops_and_a_later_request_is_refused() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()));
    assert!(server.close_session_stream(stream));
    assert!(!server.kernel().frame_sessions().contains(&"tab-a".to_string()), "the quiet session was freed");
    let refused = server.start("Main", request("LOOK", "tab-a", None)).expect_err("an ended session refuses a request");
    assert!(refused.starts_with("SESSION_ENDED"), "{refused}");
}

#[test]
fn exec_dist_9_control_a_session_with_an_open_stream_keeps_its_frame() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let _stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()));
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("app"));
}

#[test]
fn exec_dist_9_a_parked_dispatch_keeps_its_session_until_it_ends() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    let ServerProgress::Await { flight, requests } = server.start("Main", request("ASK", "tab-a", None)).unwrap() else {
        panic!("the unkeyed call parks its dispatch");
    };
    server.close_session_stream(stream);
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()), "a running request keeps its session");
    let replies = requests
        .into_iter()
        .map(|p| IoReply { id: p.id, answer: IoAnswer::Service(Ok(orbital_core_base::value::Value::Null)) })
        .collect();
    done(server.resume(flight, replies).unwrap());
    assert!(!server.kernel().frame_sessions().contains(&"tab-a".to_string()), "freed once its last request ended");
}

#[test]
fn exec_dist_9_control_a_session_whose_stream_never_opened_is_never_freed() {
    let server = EmbeddedServer::new(schema()).unwrap();
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()));
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("app"));
}

fn mount(client: &str, carried: Option<&str>) -> EventRequest {
    use orbital_core_runtime::effects::response::{LifecycleEvent, MountSeed};
    EventRequest {
        event: "INIT".to_string(),
        client_id: Some(client.to_string()),
        mount: Some(vec![MountSeed { r#trait: "Editor".to_string(), event: LifecycleEvent::Init }]),
        entity_by_trait: carried.map(|name| serde_json::from_value(json!({ "Editor": { "name": name } })).unwrap()),
        ..Default::default()
    }
}

#[test]
fn exec_dist_9_a_mount_reviving_an_ended_session_keeps_the_carried_copy_and_runs_no_lifecycle_for_it() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    server.close_session_stream(stream);
    let revived = done(server.start("Main", mount("tab-a", Some("kept"))).unwrap());
    let rows = serde_json::to_value(revived.entity_by_trait.clone().unwrap_or_default()).unwrap();
    assert_eq!(rows["Editor"]["name"], json!("kept"), "the carried row is echoed");
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("kept"));
}

#[test]
fn exec_dist_9_control_a_mount_reviving_with_no_carried_copy_runs_the_lifecycle_from_defaults() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    server.close_session_stream(stream);
    done(server.start("Main", mount("tab-a", None)).unwrap());
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("fresh"));
}

#[test]
fn exec_dist_9_a_request_waiting_for_the_lane_keeps_its_session() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    let ServerProgress::Await { flight, requests } = server.start("Main", request("ASK", "tab-b", None)).unwrap() else {
        panic!("tab-b's unkeyed call parks holding the lane");
    };
    let ServerProgress::Queued { ticket } = server.start("Main", request("LOOK", "tab-a", None)).unwrap() else {
        panic!("tab-a's request waits for the lane");
    };
    server.close_session_stream(stream);
    let replies = requests
        .into_iter()
        .map(|p| IoReply { id: p.id, answer: IoAnswer::Service(Ok(orbital_core_base::value::Value::Null)) })
        .collect();
    done(server.resume(flight, replies).unwrap());
    let (ran, progress) = server.next().expect("the lane is free");
    assert_eq!(ran, ticket);
    assert_eq!(name_in(&done(progress.expect("the waiting request kept its session"))), json!("app"));
    assert!(!server.kernel().frame_sessions().contains(&"tab-a".to_string()), "freed once its waiting request ended");
}
