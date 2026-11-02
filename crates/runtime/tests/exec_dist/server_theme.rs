// G-CROSS-395 on the identity path: `theme/set` binds a declared theme key to the session, `@currentTheme` is that key
// ahead of every declared theme, `theme/clear` returns the session to its declared theme, the server owns the binding and
// the client mirrors it from the frame that carries it.

use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::effects::response::{EventRequest, EventResponse, LifecycleEvent, MountSeed};
use orbital_core_runtime::embed::{EmbeddedKernel, EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};

fn schema() -> OrbitalSchema {
    let set = |extra: serde_json::Value| {
        let mut effects = vec![json!(["theme/set", "@payload.key", { "emit": { "success": "SET_OK", "failure": "SET_FAIL" } }])];
        if let serde_json::Value::Array(more) = extra {
            effects.extend(more);
        }
        effects
    };
    serde_json::from_value(json!({
        "name": "App",
        "theme": "gazette-light",
        "themes": ["ink-wash-dark", "scandi-light"],
        "typeContractVersion": 1, "orbitals": [
            { "name": "Main",
              "entity": { "name": "Pick", "persistence": "runtime",
                  "fields": [{ "name": "id", "type": "string" }, { "name": "seen", "type": "string" }] },
              "traits": [
                { "name": "Picker", "linkedEntity": "Pick",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "SET", "effects": set(json!([])) },
                            { "from": "idle", "to": "idle", "event": "SET_SLOW", "effects": set(json!([["call-service", "svc", "work", {}]])) },
                            { "from": "idle", "to": "idle", "event": "CLEAR", "effects": [
                                ["theme/clear", { "emit": { "success": "CLEAR_OK" } }]] },
                            { "from": "idle", "to": "idle", "event": "READ", "effects": [["set", "@entity.seen", "@currentTheme"]] }
                        ] } },
                { "name": "Restorer", "linkedEntity": "Pick",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["theme/set", "scandi-light"]] }] } },
                { "name": "Sink", "linkedEntity": "Pick",
                    "listens": [
                        { "event": "SET_OK", "triggers": "GOT_OK", "source": { "kind": "trait", "trait": "Picker" } },
                        { "event": "SET_FAIL", "triggers": "GOT_FAIL", "source": { "kind": "trait", "trait": "Picker" } },
                        { "event": "CLEAR_OK", "triggers": "GOT_CLEAR", "source": { "kind": "trait", "trait": "Picker" } }],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GOT_OK", "effects": [["set", "@entity.seen", "@payload.theme"]] },
                            { "from": "idle", "to": "idle", "event": "GOT_FAIL", "effects": [["set", "@entity.seen", "@payload.kind"]] },
                            { "from": "idle", "to": "idle", "event": "GOT_CLEAR", "effects": [["set", "@entity.seen", "@payload.theme"]] }
                        ] } }] },
            { "name": "Pinned", "theme": "clay-light",
              "entity": { "name": "PinnedRow", "persistence": "runtime",
                  "fields": [{ "name": "id", "type": "string" }, { "name": "seen", "type": "string" }] },
              "traits": [{ "name": "PinnedReader", "linkedEntity": "PinnedRow",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "READ", "effects": [["set", "@entity.seen", "@currentTheme"]] }] } }] }
        ]
    }))
    .unwrap()
}

fn request(orbital_trait: &str, event: &str, client: &str, payload: serde_json::Value) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        target_trait: Some(orbital_trait.to_string()),
        client_id: Some(client.to_string()),
        payload: Some(serde_json::from_value(payload).unwrap()),
        ..Default::default()
    }
}

fn finish(server: &EmbeddedServer, orbital: &str, request: EventRequest) -> EventResponse {
    let mut progress = server.start(orbital, request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => {
                let replies = requests.into_iter().map(|p| IoReply { id: p.id, answer: IoAnswer::Service(Ok(Value::Null)) }).collect();
                progress = server.resume(flight, replies).unwrap();
            }
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
        }
    }
}

fn frames(response: &EventResponse) -> serde_json::Value {
    let mut frames = BTreeMap::new();
    if let Some(delta) = &response.frames {
        delta.fold_into(&mut frames);
    }
    serde_json::to_value(frames).unwrap()
}

struct Rig {
    server: EmbeddedServer,
}

impl Rig {
    fn new() -> Rig {
        Rig { server: EmbeddedServer::new(schema()).unwrap() }
    }

    fn respond(&self, event: &str, client: &str, payload: serde_json::Value) -> EventResponse {
        finish(&self.server, "Main", request("Picker", event, client, payload))
    }

    fn outcome(&self, event: &str, client: &str, payload: serde_json::Value) -> serde_json::Value {
        frames(&self.respond(event, client, payload))["Sink"]["seen"].clone()
    }

    fn theme(&self, client: &str) -> serde_json::Value {
        frames(&self.respond("READ", client, json!({})))["Picker"]["seen"].clone()
    }

    fn pinned_theme(&self, client: &str) -> serde_json::Value {
        frames(&finish(&self.server, "Pinned", request("PinnedReader", "READ", client, json!({}))))["PinnedReader"]["seen"].clone()
    }
}

#[test]
fn theme_set_binds_a_declared_key_and_current_theme_reads_it() {
    let rig = Rig::new();
    assert_eq!(rig.theme("tab-a"), json!("gazette-light"), "control: an unbound session reads the app theme");
    assert_eq!(rig.outcome("SET", "tab-a", json!({ "key": "ink-wash-dark" })), json!("ink-wash-dark"));
    assert_eq!(rig.theme("tab-a"), json!("ink-wash-dark"));
}

#[test]
fn theme_set_accepts_the_app_theme_and_an_orbital_theme_as_declared_keys() {
    let rig = Rig::new();
    assert_eq!(rig.outcome("SET", "tab-a", json!({ "key": "gazette-light" })), json!("gazette-light"));
    assert_eq!(rig.outcome("SET", "tab-b", json!({ "key": "clay-light" })), json!("clay-light"));
}

#[test]
fn theme_set_refuses_an_undeclared_key_and_binds_nothing() {
    let rig = Rig::new();
    assert_eq!(rig.outcome("SET", "tab-a", json!({ "key": "neon-dark" })), json!("undeclared-theme"));
    assert_eq!(rig.theme("tab-a"), json!("gazette-light"), "the refused key bound nothing");
    assert_eq!(rig.outcome("SET", "tab-b", json!({ "key": "" })), json!("undeclared-theme"), "an empty key is not declared");
}

#[test]
fn theme_set_refuses_a_value_that_is_not_a_string() {
    let rig = Rig::new();
    assert_eq!(rig.outcome("SET", "tab-a", json!({ "key": 3 })), json!("invalid"));
    assert_eq!(rig.theme("tab-a"), json!("gazette-light"));
}

#[test]
fn the_session_key_overrides_an_orbitals_own_theme() {
    let rig = Rig::new();
    assert_eq!(rig.pinned_theme("tab-a"), json!("clay-light"), "control: unbound, the orbital's own theme wins over the app's");
    rig.respond("SET", "tab-a", json!({ "key": "scandi-light" }));
    assert_eq!(rig.pinned_theme("tab-a"), json!("scandi-light"), "the pick overrides all (owner 2026-11-02)");
}

#[test]
fn theme_clear_returns_the_session_to_its_declared_theme() {
    let rig = Rig::new();
    rig.respond("SET", "tab-a", json!({ "key": "ink-wash-dark" }));
    assert_eq!(rig.outcome("CLEAR", "tab-a", json!({})), json!("gazette-light"), "the success payload names the theme now in effect");
    assert_eq!(rig.theme("tab-a"), json!("gazette-light"));
    assert_eq!(rig.pinned_theme("tab-a"), json!("clay-light"), "and an orbital's own theme is back");
    assert_eq!(rig.outcome("CLEAR", "tab-b", json!({})), json!("gazette-light"), "clearing an unbound session is a no-op");
}

#[test]
fn two_sessions_hold_their_own_theme() {
    let rig = Rig::new();
    rig.respond("SET", "tab-a", json!({ "key": "ink-wash-dark" }));
    assert_eq!(rig.theme("tab-b"), json!("gazette-light"), "another session is untouched");
    rig.respond("SET", "tab-b", json!({ "key": "scandi-light" }));
    assert_eq!((rig.theme("tab-a"), rig.theme("tab-b")), (json!("ink-wash-dark"), json!("scandi-light")));
}

#[test]
fn a_replayed_step_binds_the_same_theme_as_a_straight_one() {
    let rig = Rig::new();
    rig.respond("SET_SLOW", "tab-a", json!({ "key": "ink-wash-dark" }));
    rig.respond("SET", "tab-b", json!({ "key": "ink-wash-dark" }));
    let (replayed, straight) = (rig.theme("tab-a"), rig.theme("tab-b"));
    assert_eq!(replayed, straight);
    assert_eq!(replayed, json!("ink-wash-dark"));
}

#[test]
fn a_freed_session_forgets_its_theme() {
    let rig = Rig::new();
    let stream = rig.server.open_session_stream("tab-a");
    rig.respond("SET", "tab-a", json!({ "key": "ink-wash-dark" }));
    assert!(rig.server.close_session_stream(stream));
    assert!(rig.server.kernel().session_ended("tab-a"));
    rig.server.kernel().revive_session("tab-a");
    assert_eq!(rig.theme("tab-a"), json!("gazette-light"), "the revived session starts with its declared theme");
}

#[test]
fn the_frame_delta_carries_a_changed_theme_only_and_a_snapshot_always() {
    let rig = Rig::new();
    let set = rig.respond("SET", "tab-a", json!({ "key": "ink-wash-dark" }));
    let carried = set.frames.as_ref().and_then(|d| d.theme.clone()).expect("the delta that changed the theme carries it");
    assert_eq!(carried.key.as_deref(), Some("ink-wash-dark"));
    let read = rig.respond("READ", "tab-a", json!({}));
    assert!(read.frames.as_ref().is_some_and(|d| d.theme.is_none()), "an unchanged theme is not resent: {:?}", read.frames);
    let in_session = rig.server.kernel().enter_session("tab-a");
    let snapshot = rig.server.kernel().frames_since(0).expect("a record");
    drop(in_session);
    assert_eq!(snapshot.theme.and_then(|t| t.key).as_deref(), Some("ink-wash-dark"));
    let cleared = rig.respond("CLEAR", "tab-a", json!({}));
    let carried = cleared.frames.as_ref().and_then(|d| d.theme.clone()).expect("a clear is a change");
    assert_eq!(carried.key, None, "a cleared theme is carried as no key");
}

#[test]
fn a_client_reads_the_servers_bound_theme_from_the_frame_that_carries_it() {
    let rig = Rig::new();
    let client = EmbeddedKernel::new(schema(), "tab-a").unwrap();
    assert_eq!(client.kernel().session_theme(), None);
    let set = rig.respond("SET", "tab-a", json!({ "key": "scandi-light" }));
    client.apply_server_response(&set, &HashSet::new()).unwrap();
    assert_eq!(client.kernel().session_theme().as_deref(), Some("scandi-light"));
    assert_eq!(client.kernel().resolved_theme().key, "scandi-light", "the client resolves the app theme through the mirror");
    let cleared = rig.respond("CLEAR", "tab-a", json!({}));
    client.apply_server_response(&cleared, &HashSet::new()).unwrap();
    assert_eq!(client.kernel().session_theme(), None);
    assert_eq!(client.kernel().resolved_theme().key, "gazette-light");
}

#[test]
fn a_client_trait_binds_the_theme_on_its_own_kernel_with_no_server_leg() {
    let client = crate::support::TestKernel::client().register(schema());
    let payload: Value = serde_json::from_value(json!({ "key": "ink-wash-dark" })).unwrap();
    let dispatch = client.dispatch_with_server_leg("Main", "Picker", None, "SET", payload, Some("c1")).unwrap();
    assert!(dispatch.server_legs.is_empty(), "runsOn any: the client runs it: {:?}", dispatch.server_legs);
    assert_eq!(client.session_theme().as_deref(), Some("ink-wash-dark"));
    assert_eq!(client.resolved_theme().key, "ink-wash-dark");
    let refused: Value = serde_json::from_value(json!({ "key": "neon-dark" })).unwrap();
    client.dispatch_with_server_leg("Main", "Picker", None, "SET", refused, Some("c1")).unwrap();
    assert_eq!(client.session_theme().as_deref(), Some("ink-wash-dark"), "an undeclared key is refused on the client too");
    client.dispatch_with_server_leg("Main", "Picker", None, "CLEAR", Value::Null, Some("c1")).unwrap();
    assert_eq!(client.session_theme(), None);
}

#[test]
fn a_server_frame_replaces_a_theme_the_client_bound_itself() {
    let rig = Rig::new();
    let client = EmbeddedKernel::new(schema(), "tab-a").unwrap();
    let payload: Value = serde_json::from_value(json!({ "key": "ink-wash-dark" })).unwrap();
    client.kernel().dispatch_with_server_leg("Main", "Picker", None, "SET", payload, Some("tab-a")).unwrap();
    assert_eq!(client.kernel().session_theme().as_deref(), Some("ink-wash-dark"));
    let set = rig.respond("SET", "tab-a", json!({ "key": "scandi-light" }));
    client.apply_server_response(&set, &HashSet::new()).unwrap();
    assert_eq!(client.kernel().session_theme().as_deref(), Some("scandi-light"), "the later binding wins");
}

#[test]
fn a_theme_bound_while_a_page_mounts_reaches_the_client_in_the_mount_response() {
    let rig = Rig::new();
    let mount = EventRequest {
        event: "INIT".to_string(),
        client_id: Some("tab-m".to_string()),
        mount: Some(vec![MountSeed { r#trait: "Restorer".to_string(), event: LifecycleEvent::Init }]),
        ..Default::default()
    };
    let response = finish(&rig.server, "Main", mount);
    let carried = response.frames.as_ref().and_then(|d| d.theme.clone());
    assert_eq!(carried.and_then(|t| t.key).as_deref(), Some("scandi-light"), "the mount's frames: {:?}", response.frames);
    assert_eq!(rig.theme("tab-m"), json!("scandi-light"), "control: the server session holds it");
}

#[test]
fn a_theme_carried_by_a_delta_that_lands_after_a_newer_one_still_reaches_the_client() {
    let rig = Rig::new();
    let client = EmbeddedKernel::new(schema(), "tab-a").unwrap();
    let set = rig.respond("SET", "tab-a", json!({ "key": "scandi-light" }));
    let later = rig.respond("READ", "tab-a", json!({}));
    assert!(later.frames.as_ref().is_some_and(|d| d.theme.is_none()), "the newer delta does not resend the theme");
    client.apply_server_response(&later, &HashSet::new()).unwrap();
    client.apply_server_response(&set, &HashSet::new()).unwrap();
    assert_eq!(client.kernel().session_theme().as_deref(), Some("scandi-light"), "the older delta's theme is still the newest theme the server sent");
}

#[test]
fn control_deltas_folded_in_order_leave_the_newest_theme() {
    let rig = Rig::new();
    let client = EmbeddedKernel::new(schema(), "tab-a").unwrap();
    for key in ["scandi-light", "ink-wash-dark"] {
        let set = rig.respond("SET", "tab-a", json!({ "key": key }));
        client.apply_server_response(&set, &HashSet::new()).unwrap();
    }
    assert_eq!(client.kernel().session_theme().as_deref(), Some("ink-wash-dark"));
}
