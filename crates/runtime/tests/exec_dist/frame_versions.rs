// K5 (EXEC-DIST-7): a response's frames are a versioned delta against the session's view record. The server seals what
// a dispatch wrote, a client applies a delta only at the version it starts from, a repeat is a no-op, a gap asks for a
// snapshot, and an empty diff does not bump the version.

use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::effects::{FrameDelta, FrameOp};
use orbital_core_runtime::embed::{EmbeddedKernel, EmbeddedServer, ServerProgress};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};

fn schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Doc", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" }, { "name": "v", "type": "string" }, { "name": "w", "type": "string" }] },
            "traits": [{ "name": "Writer", "linkedEntity": "Doc",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "SET_V", "effects": [["set", "@entity.v", "@payload.v"]] },
                        { "from": "idle", "to": "idle", "event": "SET_W", "effects": [["set", "@entity.w", "@payload.w"]] }
                    ] } }]
        }]
    }))
    .unwrap()
}

fn request(event: &str, client: &str, field: &str, value: &str) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        target_trait: Some("Writer".to_string()),
        client_id: Some(client.to_string()),
        payload: Some(serde_json::from_value(json!({ field: value })).unwrap()),
        ..Default::default()
    }
}

fn done(progress: ServerProgress) -> EventResponse {
    match progress {
        ServerProgress::Done(response) => response,
        _ => panic!("the dispatch ran to its end"),
    }
}

fn send(server: &EmbeddedServer, request: EventRequest) -> EventResponse {
    done(server.start("Main", request).unwrap())
}

fn delta(response: &EventResponse) -> &FrameDelta {
    response
        .frames
        .as_ref()
        .expect("the dispatch wrote a frame")
}

fn held(frames: &BTreeMap<String, orbital_core_base::value::ValueMap>) -> serde_json::Value {
    serde_json::to_value(frames).unwrap()
}

#[test]
fn a_session_numbers_its_deltas_and_ops_are_absolute() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let first = send(&server, request("SET_V", "tab", "v", "one"));
    let first = delta(&first);
    assert_eq!(
        (first.session.as_deref(), first.from, first.to),
        (Some("tab"), 0, 1)
    );
    assert!(
        first.is_snapshot(),
        "a record's first delta starts from nothing: {first:?}"
    );
    let second = send(&server, request("SET_V", "tab", "v", "two"));
    let second = delta(&second);
    assert_eq!((second.from, second.to), (1, 2));
    assert_eq!(
        second.ops,
        vec![set("v", "two")]
    );
}

#[test]
fn an_empty_diff_is_no_delta_and_no_version_bump() {
    let server = EmbeddedServer::new(schema()).unwrap();
    send(&server, request("SET_V", "tab", "v", "one"));
    let repeat = send(&server, request("SET_V", "tab", "v", "one"));
    assert!(repeat.frames.is_none(), "{:?}", repeat.frames);
    let next = send(&server, request("SET_V", "tab", "v", "two"));
    assert_eq!(
        (delta(&next).from, delta(&next).to),
        (1, 2),
        "the identical write did not bump the version"
    );
}

#[test]
fn frames_since_the_current_version_is_empty_and_any_other_is_a_snapshot() {
    let server = EmbeddedServer::new(schema()).unwrap();
    send(&server, request("SET_V", "tab", "v", "one"));
    send(&server, request("SET_W", "tab", "w", "two"));
    let ask = |since| {
        let answer = send(
            &server,
            EventRequest {
                client_id: Some("tab".into()),
                frames_since: Some(since),
                ..Default::default()
            },
        );
        answer.frames.expect("the answer carries frames")
    };
    let current = ask(2);
    assert!(
        current.ops.is_empty() && (current.from, current.to) == (2, 2),
        "{current:?}"
    );
    for behind in [0, 1, 7] {
        let snapshot = ask(behind);
        assert!(
            snapshot.is_snapshot() && snapshot.to == 2,
            "since {behind}: {snapshot:?}"
        );
        let mut frames = BTreeMap::new();
        snapshot.fold_into(&mut frames);
        assert_eq!(held(&frames)["Writer"]["v"], json!("one"));
        assert_eq!(held(&frames)["Writer"]["w"], json!("two"));
    }
}

#[test]
fn a_fork_keeps_no_record_and_its_delta_has_no_cursor() {
    let fork = EmbeddedServer::new(schema())
        .unwrap()
        .fork_session()
        .unwrap();
    let anonymous = send(&fork, request("SET_V", "tab", "v", "x"));
    assert_eq!(
        delta(&anonymous).session,
        None,
        "a stateless host cannot number its answers"
    );
    assert_eq!(fork.kernel().view_records_held(), 0);
    assert_eq!(
        delta(&anonymous).ops,
        vec![set("v", "x")]
    );
    let asked = send(
        &fork,
        EventRequest {
            client_id: Some("tab".into()),
            frames_since: Some(0),
            ..Default::default()
        },
    );
    assert!(
        !asked.success && asked.frames.is_none(),
        "a snapshot is not available from a host that keeps none"
    );
}

#[test]
fn an_ended_sessions_record_is_freed_with_it() {
    let server = EmbeddedServer::new(schema()).unwrap();
    let stream = server.open_session_stream("tab");
    send(&server, request("SET_V", "tab", "v", "one"));
    assert_eq!(server.kernel().view_records_held(), 1);
    server.close_session_stream(stream);
    assert_eq!(
        server.kernel().view_records_held(),
        0,
        "the record went with its session"
    );
}

fn client() -> EmbeddedKernel {
    EmbeddedKernel::new(schema(), "tab").unwrap()
}

fn answer(frames: FrameDelta) -> EventResponse {
    EventResponse {
        success: true,
        frames: Some(frames),
        ..Default::default()
    }
}

fn frame_of(k: &EmbeddedKernel) -> serde_json::Value {
    serde_json::to_value(k.kernel().snapshot_trait("Writer", None).entity).unwrap()
}

fn versioned(from: u64, to: u64, ops: Vec<FrameOp>) -> FrameDelta {
    FrameDelta {
        session: Some("tab".into()),
        from,
        to,
        ops,
    }
}

fn set(field: &str, value: &str) -> FrameOp {
    FrameOp::Set {
        frames: vec![("Writer".into(), serde_json::from_value(json!({ field: value })).unwrap())],
    }
}

#[test]
fn a_client_applies_a_delta_only_at_the_version_it_starts_from() {
    let k = client();
    let applied = k
        .apply_server_response(
            &answer(versioned(0, 1, vec![set("v", "one")])),
            &HashSet::new(),
        )
        .unwrap();
    assert_eq!(frame_of(&k)["v"], json!("one"));
    assert!(applied.resync.is_none());
    let next = k
        .apply_server_response(
            &answer(versioned(1, 2, vec![set("w", "two")])),
            &HashSet::new(),
        )
        .unwrap();
    assert!(next.resync.is_none());
    assert_eq!(frame_of(&k)["w"], json!("two"));
}

#[test]
fn a_repeat_changes_nothing_and_a_stale_repaint_does_not_overwrite_a_newer_value() {
    let k = client();
    k.apply_server_response(
        &answer(versioned(0, 1, vec![set("v", "one")])),
        &HashSet::new(),
    )
    .unwrap();
    k.apply_server_response(
        &answer(versioned(1, 2, vec![set("v", "two")])),
        &HashSet::new(),
    )
    .unwrap();
    let stale = k
        .apply_server_response(
            &answer(versioned(0, 1, vec![set("v", "one")])),
            &HashSet::new(),
        )
        .unwrap();
    assert!(stale.resync.is_none(), "a late delta is dropped, not a gap");
    assert_eq!(
        frame_of(&k)["v"],
        json!("two"),
        "the stale repaint did not win"
    );
    k.apply_server_response(
        &answer(versioned(1, 2, vec![set("v", "two")])),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(frame_of(&k)["v"], json!("two"));
}

#[test]
fn a_gap_applies_nothing_and_asks_for_the_snapshot_from_the_version_held() {
    let k = client();
    k.apply_server_response(
        &answer(versioned(0, 1, vec![set("v", "one")])),
        &HashSet::new(),
    )
    .unwrap();
    let gap = k
        .apply_server_response(
            &answer(versioned(3, 4, vec![set("v", "four")])),
            &HashSet::new(),
        )
        .unwrap();
    let resync = gap.resync.expect("version 2 and 3 were lost");
    assert_eq!((resync.session.as_str(), resync.since), ("tab", 1));
    assert_eq!(
        frame_of(&k)["v"],
        json!("one"),
        "the delta that skipped ahead was not applied"
    );
    let snapshot = versioned(
        0,
        4,
        vec![
            FrameOp::Clear,
            FrameOp::Put {
                frame: "Writer".into(),
                fields: serde_json::from_value(json!({ "v": "four", "w": "x" })).unwrap(),
            },
        ],
    );
    let healed = k
        .apply_server_response(&answer(snapshot), &HashSet::new())
        .unwrap();
    assert!(healed.resync.is_none());
    assert_eq!(
        (frame_of(&k)["v"].clone(), frame_of(&k)["w"].clone()),
        (json!("four"), json!("x"))
    );
    let after = k
        .apply_server_response(
            &answer(versioned(4, 5, vec![set("v", "five")])),
            &HashSet::new(),
        )
        .unwrap();
    assert!(after.resync.is_none() && frame_of(&k)["v"] == json!("five"));
}

#[test]
fn a_snapshot_forgets_a_frame_the_server_no_longer_holds() {
    let k = client();
    k.apply_server_response(
        &answer(versioned(0, 1, vec![set("v", "one")])),
        &HashSet::new(),
    )
    .unwrap();
    k.apply_server_response(
        &answer(versioned(0, 2, vec![FrameOp::Clear])),
        &HashSet::new(),
    )
    .unwrap();
    assert!(
        frame_of(&k).is_null() || frame_of(&k).get("v").is_none(),
        "{}",
        frame_of(&k)
    );
}

#[test]
fn what_a_client_paints_is_its_own_sealed_delta_of_what_the_server_wrote() {
    let k = client();
    let applied = k
        .apply_server_response_since(
            &answer(versioned(0, 1, vec![set("v", "one")])),
            &HashSet::new(),
            None,
        )
        .unwrap();
    let painted = applied
        .response
        .frames
        .expect("the fold reports the frames it painted");
    assert_eq!(painted.session.as_deref(), Some("$program"));
    assert!(
        matches!(painted.ops.last(), Some(FrameOp::Put { frame, fields }) if frame == "Writer" && fields.get("v") == Some(&"one".into())),
        "{painted:?}"
    );
}

#[test]
fn one_set_op_carries_every_frame_and_field_a_dispatch_changed() {
    let server = EmbeddedServer::new(schema()).unwrap();
    send(&server, request("SET_V", "tab", "v", "one"));
    let first = send(&server, request("SET_V", "tab", "v", "two"));
    assert_eq!(delta(&first).ops.len(), 1, "{:?}", delta(&first).ops);
    let wire = serde_json::to_value(delta(&first)).unwrap();
    assert_eq!(wire["ops"], json!([["set", { "Writer": { "v": "two" } }]]));
    let back: FrameDelta = serde_json::from_value(wire).unwrap();
    assert_eq!(&back, delta(&first));
}

/// A tick the server runs for itself writes the program session's frame; a client's frames are its own session's
/// (EXEC-DIST-9, `kernel/frames.rs` `frame_slot`/`session_eid`), so no client view can miss it as a frame delta.
#[test]
fn a_server_tick_writes_the_program_sessions_frame_and_no_clients_view() {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Doc", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" }, { "name": "n", "type": "number", "default": 0.0 }] },
            "traits": [{ "name": "Ticker", "linkedEntity": "Doc",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "TOUCH", "effects": [["set", "@entity.n", 1.0]] }] },
                "ticks": [{ "name": "beat", "interval": 1000, "effects": [["set", "@entity.n", ["+", "@entity.n", 1.0]]] }] }]
        }]
    }))
    .unwrap();
    let server = EmbeddedServer::new(schema).unwrap();
    send(
        &server,
        EventRequest { event: "TOUCH".into(), target_trait: Some("Ticker".into()), client_id: Some("tab".into()), ..Default::default() },
    );
    server.kernel().process_tick("Ticker", None, "beat").unwrap();
    let tab = send(&server, EventRequest { client_id: Some("tab".into()), frames_since: Some(0), ..Default::default() });
    let mut frames = BTreeMap::new();
    tab.frames.expect("snapshot").fold_into(&mut frames);
    assert_eq!(held(&frames)["Ticker"]["n"], json!(1.0), "the tab keeps its own frame; the tick did not reach it");
    let program = server.kernel().frames_since(0);
    assert!(program.is_some(), "the program session has its own record");
}
