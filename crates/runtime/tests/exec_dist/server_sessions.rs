// EXEC-DIST-9 on the embedded server (the JS hosts' kernel): a request naming a client runs in that client's session,
// and the kernel frees a session once its live streams have all dropped and nothing of its own runs. Native twins:
// the session tests of orbital-shell-rust `host::runtime`.

use orbital_core_base::json_value::from_json_value;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::resumable::{IoAnswer, IoReply};
use orbital_core_base::schema::types::OrbitalSchema;
use serde_json::json;

fn schema() -> OrbitalSchema {
    from_json_value(&json!({
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
                        { "from": "idle", "to": "idle", "event": "ASK", "effects": [["call-service", "svc", "work", {}]] },
                        { "from": "idle", "to": "idle", "event": "ASK_LATER", "effects": [["call-service", "svc", "work", {}, { "key": "work", "emit": { "success": "WORKED" } }]] },
                        { "from": "idle", "to": "idle", "event": "WORKED", "effects": [] }
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
        payload: name.map(|name| from_json_value(&json!({ "name": name })).unwrap()),
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
    let mut frames = std::collections::BTreeMap::new();
    if let Some(delta) = &response.frames {
        delta.fold_into(&mut frames);
    }
    serde_json::to_value(frames).unwrap()["Editor"]["seen"].clone()
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
fn exec_dist_9_control_a_session_whose_stream_never_opened_is_never_freed_without_a_host_expiry() {
    let server = EmbeddedServer::new(schema()).unwrap();
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(40));
    server.tick_pass(1000);
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()));
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("app"));
}

const SHORT_EXPIRY: std::time::Duration = std::time::Duration::from_millis(30);

fn past_the_expiry(server: &EmbeddedServer) {
    std::thread::sleep(SHORT_EXPIRY * 2);
    server.tick_pass(1000);
}

/// G-CROSS-590: a session that never opened its live stream is freed once quiet past the expiry its host declared, on
/// the host's tick pass; its next request is refused as an ended session's.
#[test]
fn exec_dist_9_a_session_whose_stream_never_opened_is_freed_once_quiet_past_the_hosts_expiry() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.kernel().set_unconnected_session_expiry(Some(SHORT_EXPIRY));
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()), "kept while inside the expiry");
    past_the_expiry(&server);
    assert!(!server.kernel().frame_sessions().contains(&"tab-a".to_string()), "freed past the expiry");
    let refused = server.start("Main", request("LOOK", "tab-a", None)).expect_err("an expired session refuses a request");
    assert!(refused.starts_with("SESSION_ENDED"), "{refused}");
}

#[test]
fn exec_dist_9_control_a_session_with_an_open_stream_outlives_the_unconnected_expiry() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.kernel().set_unconnected_session_expiry(Some(SHORT_EXPIRY));
    let _stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    past_the_expiry(&server);
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("app"));
}

#[test]
fn edge_a_session_whose_stream_opened_and_dropped_keeps_the_stream_rule_not_the_expiry() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.kernel().set_unconnected_session_expiry(Some(std::time::Duration::from_secs(3600)));
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    server.close_session_stream(stream);
    assert!(!server.kernel().frame_sessions().contains(&"tab-a".to_string()), "freed when its stream dropped, an hour early");
}

#[test]
fn edge_a_request_inside_the_expiry_restarts_its_quiet_time() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.kernel().set_unconnected_session_expiry(Some(std::time::Duration::from_millis(120)));
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(80));
    done(server.start("Main", request("SET", "tab-a", Some("again"))).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(80));
    server.tick_pass(1000);
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("again"));
}

#[test]
fn edge_a_parked_request_keeps_its_unconnected_session_past_the_expiry() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.kernel().set_unconnected_session_expiry(Some(SHORT_EXPIRY));
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    let ServerProgress::Await { flight, requests } = server.start("Main", request("ASK", "tab-a", None)).unwrap() else {
        panic!("the unkeyed call parks its dispatch");
    };
    past_the_expiry(&server);
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()), "a running request keeps its session");
    let replies = requests
        .into_iter()
        .map(|p| IoReply { id: p.id, answer: IoAnswer::Service(Ok(orbital_core_base::value::Value::Null)) })
        .collect();
    done(server.resume(flight, replies).unwrap());
    assert!(server.kernel().frame_sessions().contains(&"tab-a".to_string()), "its quiet time starts when the request ends");
}

#[test]
fn edge_an_expired_session_is_revived_by_a_mount_carrying_its_copy() {
    let server = EmbeddedServer::new(schema()).unwrap();
    server.kernel().set_unconnected_session_expiry(Some(SHORT_EXPIRY));
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    past_the_expiry(&server);
    done(server.start("Main", mount("tab-a", Some("kept"))).unwrap());
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("kept"));
}

fn mount(client: &str, carried: Option<&str>) -> EventRequest {
    use orbital_core_runtime::effects::response::{LifecycleEvent, MountSeed};
    EventRequest {
        event: "INIT".to_string(),
        client_id: Some(client.to_string()),
        mount: Some(vec![MountSeed { r#trait: "Editor".to_string(), event: LifecycleEvent::Init }]),
        entity_by_trait: carried.map(|name| from_json_value(&json!({ "Editor": { "name": name } })).unwrap()),
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
    let mut frames = std::collections::BTreeMap::new();
    revived.frames.as_ref().expect("the revived mount is sealed").fold_into(&mut frames);
    let rows = serde_json::to_value(frames).unwrap();
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

/// G-CROSS-388 (owner 2026-11-12): a trait the ended session left waiting on a deferred ask never hears its answer, so
/// the mount that revives the session runs its lifecycle event from the restored state.
#[test]
fn exec_dist_9_a_revived_trait_whose_deferred_ask_died_with_its_session_hears_its_lifecycle() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", request("SET", "tab-a", Some("app"))).unwrap());
    let ServerProgress::Await { flight, requests } = server.start("Main", request("ASK_LATER", "tab-a", None)).unwrap() else {
        panic!("the keyed call asks the host");
    };
    let deferred = requests.into_iter().map(|p| IoReply { id: p.id, answer: IoAnswer::ServiceDeferred }).collect();
    done(server.resume(flight, deferred).unwrap());
    server.close_session_stream(stream);
    done(server.start("Main", mount("tab-a", Some("kept"))).unwrap());
    assert_eq!(
        name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())),
        json!("fresh"),
        "INIT ran from the restored state, so the chain restarts instead of waiting forever"
    );
}

/// G-CROSS-357: a page's first stream opened and dropped before its first mount, so the session ended holding nothing; the
/// mount's carried copy is the client's own optimistic step, not a state this host held, so the lifecycle runs here.
#[test]
fn exec_dist_9_a_mount_reviving_a_session_that_held_no_state_runs_the_lifecycle_for_its_carried_traits() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    server.close_session_stream(stream);
    done(server.start("Main", mount("tab-a", Some("client-made"))).unwrap());
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

#[test]
fn exec_tick_4_a_freed_session_releases_the_traits_its_client_mounted() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    done(server.start("Main", mount("tab-a", None)).unwrap());
    assert!(server.kernel().trait_mounted("Editor"));
    assert!(server.close_session_stream(stream));
    assert!(!server.kernel().trait_mounted("Editor"), "the freed session's mounts left with it");
}

#[test]
fn exec_tick_4_control_another_clients_mount_outlives_a_freed_session() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab-a");
    let _other = server.open_session_stream("tab-b");
    done(server.start("Main", mount("tab-a", None)).unwrap());
    done(server.start("Main", mount("tab-b", None)).unwrap());
    server.close_session_stream(stream);
    assert!(server.kernel().trait_mounted("Editor"), "tab-b still holds it");
}

/// G-ORB-605: a fresh mount carrying the client's state for a trait it does not restore runs that trait's lifecycle
/// from the trait's own state; the carried state is the client's step and never seeds the server (the native shell's
/// twin: `a_reconnect_mount_into_a_session_that_held_no_state_runs_init_over_the_carried_frame`).
#[test]
fn exec_dist_9_a_fresh_mount_never_seeds_the_carried_state_of_a_trait_it_does_not_restore() {
    use orbital_core_runtime::effects::response::TraitFrom;
    let schema: OrbitalSchema = from_json_value(&json!({
        "name": "App", "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Draft", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "name", "type": "string" }, { "name": "seen", "type": "string" }] },
            "traits": [{ "name": "Editor", "linkedEntity": "Draft",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [["set", "@entity.name", "fresh"]] },
                        { "from": "busy", "to": "busy", "event": "INIT", "effects": [["set", "@entity.name", "seeded"]] },
                        { "from": "idle", "to": "idle", "event": "LOOK", "effects": [["set", "@entity.seen", "@entity.name"]] },
                        { "from": "busy", "to": "busy", "event": "LOOK", "effects": [["set", "@entity.seen", "@entity.name"]] }
                    ] } }]
        }]
    }))
    .unwrap();
    let server = EmbeddedServer::new(schema).unwrap();
    let mut carried = mount("tab-a", Some("client-made"));
    carried.traits = Some(vec![TraitFrom { r#trait: "Editor".to_string(), from: "busy".to_string(), event: Some("INIT".to_string()) }]);
    done(server.start("Main", carried).unwrap());
    assert_eq!(name_in(&done(server.start("Main", request("LOOK", "tab-a", None)).unwrap())), json!("fresh"));
}
