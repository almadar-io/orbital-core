// EXEC-IDENTITY-1, -3, -4 on the embedded server: `identity/sign-in` binds a sealed Verified to the session, `@user` is
// then the program's `[identity]` row of its subject, `identity/sign-out` returns the session to the anonymous viewer, and
// a session that never signed in is the anonymous viewer (no host or request supplies a viewer).

use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest, PendingRequest};
use serde_json::json;

const FIXTURE: &str = include_str!("../../../../tests/fixtures/security/jws-verify.json");

fn fixture() -> serde_json::Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn schema(persistence: &str) -> OrbitalSchema {
    let sign_in = |extra: serde_json::Value| {
        let mut effects = vec![
            json!(["let", [["v", ["jws/verify", "@payload.token", "@payload.keys", { "issuer": "https://issuer.test", "audience": "app-1" }]]],
            ["identity/sign-in", "@v", { "emit": { "success": "IN_OK", "failure": "IN_FAIL" } }]]),
        ];
        if let serde_json::Value::Array(more) = extra {
            effects.extend(more);
        }
        effects
    };
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Person", "identity": true, "persistence": persistence,
                "fields": [{ "name": "id", "type": "string" }, { "name": "email", "type": "string" },
                    { "name": "role", "type": "string", "default": "member" }] },
            "traits": [
                { "name": "Gate", "linkedEntity": "Person",
                    "emits": [
                        { "event": "VERIFIED", "payloadSchema": [
                            { "name": "verified", "type": "Verified", "required": true },
                            { "name": "note", "type": "string", "required": false }] },
                        { "event": "PLAIN_DONE", "payloadSchema": [{ "name": "note", "type": "string", "required": false }] }],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "SIGN_IN", "effects": sign_in(json!([])) },
                            { "from": "idle", "to": "idle", "event": "SIGN_IN_SLOW", "effects": sign_in(json!([["call-service", "svc", "work", {}]])) },
                            { "from": "idle", "to": "idle", "event": "SIGN_IN_FAKE", "effects": [
                                ["identity/sign-in", "@payload.fake", { "emit": { "success": "IN_OK", "failure": "IN_FAIL" } }]] },
                            { "from": "idle", "to": "idle", "event": "RELAY", "effects": [
                                ["let", [["v", ["jws/verify", "@payload.token", "@payload.keys", { "issuer": "https://issuer.test", "audience": "app-1" }]]],
                                    ["emit", "VERIFIED", { "verified": "@v", "note": "kept" }]]] },
                            { "from": "idle", "to": "idle", "event": "PLAIN", "effects": [["emit", "PLAIN_DONE", { "note": "plain" }]] },
                            { "from": "idle", "to": "idle", "event": "SIGN_OUT", "effects": [
                                ["identity/sign-out", { "emit": { "success": "OUT_OK" } }]] },
                            { "from": "idle", "to": "idle", "event": "READ", "effects": [
                                ["set", "@entity.who", "@user.id"], ["set", "@entity.role", "@user.role"], ["set", "@entity.email", "@user.email"]] }
                        ] } },
                { "name": "Sink", "linkedEntity": "Person",
                    "listens": [
                        { "event": "IN_OK", "triggers": "GOT_OK", "source": { "kind": "trait", "trait": "Gate" } },
                        { "event": "IN_FAIL", "triggers": "GOT_FAIL", "source": { "kind": "trait", "trait": "Gate" } },
                        { "event": "OUT_OK", "triggers": "GOT_OUT", "source": { "kind": "trait", "trait": "Gate" } },
                        { "event": "VERIFIED", "triggers": "RELAYED", "source": { "kind": "trait", "trait": "Gate" }, "payloadMapping": { "verified": "@payload.verified" } }],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GOT_OK", "effects": [["set", "@entity.outcome", "@payload.subject"]] },
                            { "from": "idle", "to": "idle", "event": "GOT_FAIL", "effects": [["set", "@entity.outcome", "@payload.kind"]] },
                            { "from": "idle", "to": "idle", "event": "RELAYED", "effects": [
                                ["identity/sign-in", "@payload.verified", { "emit": { "success": "IN_OK", "failure": "IN_FAIL" } }]] },
                            { "from": "idle", "to": "idle", "event": "GOT_OUT", "effects": [["set", "@entity.outcome", "@payload.subject"]] }
                        ] } }
            ]
        }]
    }))
    .unwrap()
}

fn request(event: &str, client: &str, payload: serde_json::Value) -> EventRequest {
    EventRequest {
        event: event.to_string(),
        target_trait: Some("Gate".to_string()),
        client_id: Some(client.to_string()),
        payload: Some(serde_json::from_value(payload).unwrap()),
        ..Default::default()
    }
}

fn credential(token: &str) -> serde_json::Value {
    json!({ "token": fixture()["tokens"][token].clone(), "keys": fixture()["keys"].clone() })
}

fn answer(pending: Vec<PendingRequest>, store: &[serde_json::Value]) -> Vec<IoReply> {
    pending
        .into_iter()
        .map(|p| {
            let answer = match &p.request {
                IoRequest::ReadRow { id, .. } => IoAnswer::Row(Ok(store
                    .iter()
                    .find(|row| row["id"] == json!(id))
                    .map(|row| serde_json::from_value(row.clone()).unwrap())
                    .unwrap_or(Value::Null))),
                IoRequest::CallService { .. } => IoAnswer::Service(Ok(Value::Null)),
                other => panic!("an unexpected ask: {other:?}"),
            };
            IoReply { id: p.id, answer }
        })
        .collect()
}

/// Run `request` to its end, answering the host's reads from `store`.
fn finish(server: &EmbeddedServer, request: EventRequest, store: &[serde_json::Value]) -> EventResponse {
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return response,
            ServerProgress::Await { flight, requests } => progress = server.resume(flight, answer(requests, store)).unwrap(),
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
        }
    }
}

fn frames(response: &EventResponse) -> serde_json::Value {
    let mut frames = std::collections::BTreeMap::new();
    if let Some(delta) = &response.frames {
        delta.fold_into(&mut frames);
    }
    serde_json::to_value(frames).unwrap()
}

struct Rig {
    server: EmbeddedServer,
    store: Vec<serde_json::Value>,
}

impl Rig {
    fn new(persistence: &str, rows: Vec<serde_json::Value>) -> Rig {
        let server = EmbeddedServer::new(schema(persistence)).unwrap();
        Rig { server, store: rows }
    }

    fn send(&self, request: EventRequest) -> serde_json::Value {
        frames(&finish(&self.server, request, &self.store))
    }

    fn run(&self, event: &str, client: &str, payload: serde_json::Value) -> serde_json::Value {
        self.send(request(event, client, payload))
    }

    fn outcome(&self, event: &str, client: &str, payload: serde_json::Value) -> serde_json::Value {
        self.run(event, client, payload)["Sink"]["outcome"].clone()
    }

    fn viewer(&self, client: &str) -> (serde_json::Value, serde_json::Value) {
        let read = self.run("READ", client, json!({}));
        (read["Gate"]["who"].clone(), read["Gate"]["role"].clone())
    }
}

fn person(id: &str, role: &str) -> serde_json::Value {
    json!({ "id": id, "role": role, "email": "row@example.com" })
}

#[test]
fn exec_identity_3_signing_in_binds_the_session_and_user_is_the_persistent_identity_row() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    assert_eq!(rig.outcome("SIGN_IN", "tab-a", credential("rs256_valid")), json!("user-1"));
    let read = rig.run("READ", "tab-a", json!({}));
    assert_eq!(read["Gate"]["who"], json!("user-1"));
    assert_eq!(read["Gate"]["role"], json!("admin"), "the row's own field");
    assert_eq!(read["Gate"]["email"], json!("row@example.com"));
}

#[test]
fn exec_identity_3_a_persistent_identity_with_no_row_answers_no_identity_and_binds_nothing() {
    let rig = Rig::new("persistent", vec![person("someone-else", "admin")]);
    assert_eq!(rig.outcome("SIGN_IN", "tab-a", credential("rs256_valid")), json!("no-identity"));
    assert_eq!(
        rig.viewer("tab-a").0,
        json!("anonymous"),
        "control: a session that bound nothing is the anonymous viewer"
    );
}

#[test]
fn exec_identity_3_a_runtime_identity_row_is_made_from_the_declared_claims() {
    let rig = Rig::new("runtime", vec![]);
    assert_eq!(rig.outcome("SIGN_IN", "tab-a", credential("rs256_valid")), json!("user-1"));
    let read = rig.run("READ", "tab-a", json!({}));
    assert_eq!(read["Gate"]["who"], json!("user-1"));
    assert_eq!(
        read["Gate"]["email"],
        json!("u@example.com"),
        "the declared field `email` takes the claim of that name"
    );
    assert_eq!(
        read["Gate"]["role"],
        json!("member"),
        "the declared default: no claim is named role (the token carries roles)"
    );
}

#[test]
fn exec_identity_3_a_value_that_is_not_a_sealed_verified_is_refused_as_unsealed() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    let fake = json!({ "fake": { "subject": "user-1", "issuer": "x", "audience": "y", "claims": [], "expiresAt": 1 } });
    assert_eq!(rig.outcome("SIGN_IN_FAKE", "tab-a", fake), json!("unsealed"));
    assert_eq!(rig.viewer("tab-a").0, json!("anonymous"));
    assert_eq!(
        rig.outcome("SIGN_IN", "tab-a", credential("rs256_valid")),
        json!("user-1"),
        "control: the sealed value signs in"
    );
}

#[test]
fn exec_identity_3_signing_out_returns_the_session_to_the_anonymous_viewer() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    rig.run("SIGN_IN", "tab-a", credential("rs256_valid"));
    assert_eq!(rig.viewer("tab-a").0, json!("user-1"));
    rig.run("SIGN_OUT", "tab-a", json!({}));
    assert_eq!(rig.viewer("tab-a"), (json!("anonymous"), json!("anonymous")));
    assert_eq!(
        rig.outcome("SIGN_OUT", "tab-b", json!({})),
        json!("anonymous"),
        "a session never signed in signs out as the anonymous viewer"
    );
    assert_eq!(rig.viewer("tab-b"), (json!("anonymous"), json!("anonymous")));
}

#[test]
fn exec_identity_1_two_sessions_hold_their_own_identity() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    rig.run("SIGN_IN", "tab-a", credential("rs256_valid"));
    assert_eq!(rig.viewer("tab-b").0, json!("anonymous"), "another session is not signed in");
    rig.run("SIGN_IN", "tab-b", credential("rs256_valid"));
    rig.run("SIGN_OUT", "tab-a", json!({}));
    assert_eq!(rig.viewer("tab-a").0, json!("anonymous"));
    assert_eq!(
        rig.viewer("tab-b").0,
        json!("user-1"),
        "signing one session out leaves the other signed in"
    );
}

#[test]
fn exec_identity_1_an_unbound_session_reads_the_anonymous_viewer_and_a_bound_one_its_own_identity() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    assert_eq!(rig.viewer("tab-a"), (json!("anonymous"), json!("anonymous")), "no sign-in, no host viewer: anonymous");
    rig.run("SIGN_IN", "tab-b", credential("rs256_valid"));
    assert_eq!(rig.viewer("tab-b"), (json!("user-1"), json!("admin")), "control: the signed-in session reads its identity row");
    assert_eq!(rig.viewer("tab-c").0, json!("anonymous"), "control: signing in one session leaves another anonymous");
}

#[test]
fn exec_identity_3_a_replayed_step_binds_the_same_identity_as_a_straight_one() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    let replayed = rig.run("SIGN_IN_SLOW", "tab-a", credential("rs256_valid"));
    let straight = rig.run("SIGN_IN", "tab-b", credential("rs256_valid"));
    assert_eq!(replayed["Sink"]["outcome"], straight["Sink"]["outcome"]);
    let (a, b) = (rig.viewer("tab-a"), rig.viewer("tab-b"));
    assert_eq!(a, b);
    assert_eq!(a.0, json!("user-1"));
}

#[test]
fn exec_identity_4_a_client_kernel_never_binds_an_identity_it_hands_the_sign_in_to_the_server() {
    let client = crate::support::TestKernel::client().register(schema("persistent"));
    let payload: Value = serde_json::from_value(credential("rs256_valid")).unwrap();
    let dispatch = client
        .dispatch_with_server_leg("Main", "Gate", None, "SIGN_IN", payload, Some("c1"))
        .unwrap();
    assert_eq!(
        dispatch.server_legs.len(),
        1,
        "the sign-in is the server's: {:?}",
        dispatch.server_legs
    );
    assert_eq!(dispatch.server_legs[0].event, "SIGN_IN");
    assert_eq!(client.session_identity(), None, "the client kernel holds no identity of its own");
    let read = client
        .dispatch_with_server_leg("Main", "Gate", None, "READ", Value::Null, Some("c1"))
        .unwrap();
    assert!(
        read.server_legs.is_empty(),
        "control: a step with no identity effect stays on the client"
    );
}

#[test]
fn exec_identity_1_a_freed_session_forgets_its_identity() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    let stream = rig.server.open_session_stream("tab-a");
    rig.run("SIGN_IN", "tab-a", credential("rs256_valid"));
    assert_eq!(rig.viewer("tab-a").0, json!("user-1"));
    assert!(rig.server.close_session_stream(stream));
    assert!(rig.server.kernel().session_ended("tab-a"), "the quiet session was freed");
    rig.server.kernel().revive_session("tab-a");
    assert_eq!(rig.viewer("tab-a").0, json!("anonymous"), "the revived session starts unbound");
}

#[test]
fn exec_identity_3_an_internal_emit_carries_a_verified_to_a_listener_that_signs_in_with_it() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    rig.run("RELAY", "tab-a", credential("rs256_valid"));
    assert_eq!(rig.viewer("tab-a"), (json!("user-1"), json!("admin")));
}

fn emitted(response: &EventResponse, event: &str) -> serde_json::Value {
    let found = response.emitted_events.iter().find(|emitted| emitted.event == event);
    serde_json::to_value(found.unwrap_or_else(|| panic!("{event} is emitted: {:?}", response.emitted_events))).unwrap()["payload"].clone()
}

#[test]
fn exec_identity_2_a_verified_payload_field_reaches_a_host_as_a_sealed_stub_and_the_other_fields_stay() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    let response = finish(&rig.server, request("RELAY", "tab-a", credential("rs256_valid")), &rig.store);
    let payload = emitted(&response, "VERIFIED");
    assert_eq!(payload["verified"], json!({ "sealed": "Verified" }), "no subject, no claims: {payload}");
    assert_eq!(payload["note"], json!("kept"), "control: another field of the same payload is intact");
    assert_eq!(rig.viewer("tab-a"), (json!("user-1"), json!("admin")), "control: the listener inside the kernel still signed in with the real value");
    let plain = finish(&rig.server, request("PLAIN", "tab-b", json!({})), &rig.store);
    assert_eq!(emitted(&plain, "PLAIN_DONE"), json!({ "note": "plain" }), "control: an emit declaring no sealed field is untouched");
}

fn self_cascade_schema() -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Person", "identity": true, "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "email", "type": "string" },
                    { "name": "role", "type": "string", "default": "member" }] },
            "traits": [{ "name": "Gate", "linkedEntity": "Person",
                "emits": [
                    { "event": "TOKEN_VERIFIED", "payloadSchema": [{ "name": "verified", "type": "Verified", "required": true }] },
                    { "event": "CHECKED", "payloadSchema": [{ "name": "subject", "type": "string", "required": true }] }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "CHECK", "effects": [
                            ["let", [["v", ["jws/verify", "@payload.token", "@payload.keys", { "issuer": "https://issuer.test", "audience": "app-1" }]]],
                                ["emit", "TOKEN_VERIFIED", { "verified": "@v" }]]] },
                        { "from": "idle", "to": "idle", "event": "TOKEN_VERIFIED", "effects": [
                            ["emit", "CHECKED", { "subject": "@payload.verified.subject" }]] }
                    ] } }]
        }]
    }))
    .unwrap()
}

#[test]
fn exec_queue_2_a_trait_own_emit_carrying_a_verified_reaches_its_own_arm_with_the_sealed_value() {
    let server = EmbeddedServer::new(self_cascade_schema()).unwrap();
    let response = finish(&server, request("CHECK", "tab-a", credential("rs256_valid")), &[]);
    assert_eq!(emitted(&response, "CHECKED"), json!({ "subject": "user-1" }), "the arm read the real subject, not a redacted stub");
    assert_eq!(emitted(&response, "TOKEN_VERIFIED")["verified"], json!({ "sealed": "Verified" }), "control: a host still sees only the stub");
}

#[test]
fn exec_identity_2_the_dispatch_stream_redacts_a_verified_field_too() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    finish(&rig.server, request("RELAY", "tab-a", credential("rs256_valid")), &rig.store);
    let stream = rig.server.dispatch_stream_json().unwrap();
    assert!(stream.contains("\"sealed\":\"Verified\""), "{stream}");
    assert!(!stream.contains("issuer.test") && !stream.contains("expiresAt"), "no claim of the credential is in the stream: {stream}");
}

#[test]
fn exec_identity_1_a_runtime_identity_row_belongs_to_its_session_and_is_freed_with_it() {
    let rig = Rig::new("runtime", vec![]);
    let stream_a = rig.server.open_session_stream("tab-a");
    rig.run("SIGN_IN", "tab-a", credential("rs256_valid"));
    rig.run("SIGN_IN", "tab-b", credential("rs256_valid"));
    assert_eq!(rig.server.kernel().entity_source().list("Person").unwrap().len(), 0, "no kernel-wide row keyed by the subject");
    assert_eq!(rig.viewer("tab-a").0, json!("user-1"));
    assert!(rig.server.close_session_stream(stream_a));
    assert!(rig.server.kernel().session_ended("tab-a"));
    rig.server.kernel().revive_session("tab-a");
    assert_eq!(rig.viewer("tab-a").0, json!("anonymous"), "the freed session's identity row is gone");
    assert_eq!(rig.viewer("tab-b").0, json!("user-1"), "control: another session of the same subject keeps its own row");
}

// EXEC-STORE-4 + EXEC-IDENTITY-1: a persistent identity bound to a `.lolo` store driver. Every step reads `@user`, and
// that read is a store ask; the driver answering it steps in the program session, so it never asks for the identity
// it is reading (a viewer-session driver re-asked for it without end: the launchpad's stack overflow on std-kflow
// sign-in, 2026-11-10).
fn store_bound_identity_schema(driver_page: Option<&str>) -> OrbitalSchema {
    let mut schema = serde_json::to_value(schema("persistent")).unwrap();
    schema["orbitals"][0]["entity"]["store"] = json!("Rows");
    let row = json!({ "id": "@payload.id", "role": "admin", "email": "row@example.com" });
    let mut rows = json!({ "name": "Rows",
        "entity": { "name": "RowsRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
        "traits": [{ "name": "RowsDriver", "linkedEntity": "RowsRun",
            "stateMachine": { "states": [{ "name": "ready", "isInitial": true }, { "name": "busy" }],
                "events": [{ "key": "ROW_READ", "name": "ROW_READ" }, { "key": "RD_NEXT", "name": "RD_NEXT" }],
                "transitions": [
                    { "from": "ready", "to": "busy", "event": "ROW_READ", "effects": [["emit", "RD_NEXT", { "key": "@payload.key", "id": "@payload.id" }]] },
                    { "from": "busy", "to": "ready", "event": "RD_NEXT", "effects": [["emit", "ROW_FOUND", { "key": "@payload.key", "row": row }]] }
                ] } }] });
    if let Some(protocol) = driver_page {
        rows["pages"] = json!([{ "name": "Bg", "path": "", "protocol": protocol, "traits": [{ "ref": "RowsDriver" }] }]);
    }
    schema["orbitals"].as_array_mut().unwrap().push(rows);
    serde_json::from_value(schema).unwrap()
}

fn signed_in_read(driver_page: Option<&str>) -> serde_json::Value {
    let server = EmbeddedServer::new(store_bound_identity_schema(driver_page)).unwrap();
    let outcome = frames(&finish(&server, request("SIGN_IN", "tab-a", credential("rs256_valid")), &[]))["Sink"]["outcome"].clone();
    assert_eq!(outcome, json!("user-1"), "the driver answered the sign-in's identity read");
    frames(&finish(&server, request("READ", "tab-a", json!({})), &[]))
}

#[test]
fn a_store_bound_identity_is_read_by_a_driver_that_never_asks_for_it_again() {
    let read = signed_in_read(None);
    assert_eq!(read["Gate"]["who"], json!("user-1"), "{read}");
    assert_eq!(read["Gate"]["role"], json!("admin"), "{read}");
}

#[test]
fn control_a_boot_mounted_driver_answers_the_same_identity_read() {
    let read = signed_in_read(Some("boot"));
    assert_eq!(read["Gate"]["who"], json!("user-1"), "{read}");
}

// A guest is signed in through `program/dispatch` with no client session, so its identity is the program session's
// own: the store driver answering its identity read must still read no viewer (the launchpad crash of 07:27).
#[test]
fn a_program_session_identity_bound_to_a_store_is_read_by_a_driver_that_reads_no_viewer() {
    let server = EmbeddedServer::new(store_bound_identity_schema(None)).unwrap();
    let programwide = |event: &str, payload: serde_json::Value| EventRequest { client_id: None, ..request(event, "unused", payload) };
    let outcome = frames(&finish(&server, programwide("SIGN_IN", credential("rs256_valid")), &[]))["Sink"]["outcome"].clone();
    assert_eq!(outcome, json!("user-1"), "the driver answered the sign-in's identity read");
    let read = frames(&finish(&server, programwide("READ", json!({})), &[]));
    assert_eq!(read["Gate"]["who"], json!("user-1"), "{read}");
}

// EXEC-IDENTITY-1: a persistent identity's row is read once and held in the session's binding, read again only after
// the kernel writes that identity entity (owner ruling 2026-11-10: a store-bound row read per step flooded Firestore).
// A tick in the program session belongs to no viewer and reads the anonymous viewer.
fn held_identity_schema() -> OrbitalSchema {
    let mut schema = serde_json::to_value(schema("persistent")).unwrap();
    let gate = &mut schema["orbitals"][0]["traits"][0];
    gate["stateMachine"]["transitions"].as_array_mut().unwrap().push(json!(
        { "from": "idle", "to": "idle", "event": "PROMOTE", "effects": [["persist", "update", "Person", "@user.id", { "role": "owner" }]] }));
    schema["orbitals"].as_array_mut().unwrap().push(json!({ "name": "Probes",
        "entity": { "name": "Seen", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "who", "type": "string" }, { "name": "role", "type": "string" }] },
        "traits": [{ "name": "Probe", "linkedEntity": "Seen",
            "ticks": [{ "name": "beat", "interval": 1000, "effects": [["set", "@entity.who", "@user.id"]] }],
            "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "LOOK", "effects": [["set", "@entity.who", "@user.id"], ["set", "@entity.role", "@user.role"]] }] } }] }));
    serde_json::from_value(schema).unwrap()
}

struct Counted {
    kernel: orbital_core_runtime::kernel::RuntimeKernel,
    rows: orbital_core_runtime::entity_source::InMemoryEntityCache,
    reads: std::sync::atomic::AtomicUsize,
}

impl Counted {
    fn new() -> Counted {
        use orbital_core_runtime::entity_source::EntitySource;
        let kernel = orbital_core_runtime::kernel::RuntimeKernel::new(
            orbital_core_base::effect_types::RuntimeEnvironment::Server,
            std::sync::Arc::new(orbital_core_runtime::entity_source::InMemoryEntityCache::new()) as std::sync::Arc<dyn EntitySource>,
            orbital_core_runtime::config::KernelConfig::default(),
        );
        kernel.register_schema(held_identity_schema()).unwrap();
        let rows = orbital_core_runtime::entity_source::InMemoryEntityCache::new();
        rows.create("Person", &serde_json::from_value(person("user-1", "admin")).unwrap()).unwrap();
        Counted { kernel, rows, reads: Default::default() }
    }

    fn run(&self, event: &str, payload: serde_json::Value) {
        let target = if event == "LOOK" { "Probe" } else { "Gate" };
        let progress = self.kernel.start_dispatch(target, None, event, serde_json::from_value(payload).unwrap(), None, None).unwrap();
        orbital_core_runtime::resumable::drive_blocking(&self.kernel, progress, &|request| {
            if matches!(request, IoRequest::ReadRow { entity_type, .. } if entity_type == "Person") {
                self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            orbital_core_runtime::resumable::BlockingHost { source: &self.rows, service: None, substrate: None }.answer(request)
        })
        .unwrap();
    }

    fn tick(&self) {
        let progress = self.kernel.start_dispatch_tick("Probe", None, "beat").unwrap();
        orbital_core_runtime::resumable::drive_blocking(&self.kernel, progress, &|request| {
            orbital_core_runtime::resumable::BlockingHost { source: &self.rows, service: None, substrate: None }.answer(request)
        })
        .unwrap();
    }

    fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn gate(&self) -> serde_json::Value {
        serde_json::to_value(&self.kernel.snapshot_trait("Probe", None).entity).unwrap()
    }
}

#[test]
fn a_persistent_identity_row_is_held_after_sign_in_and_not_read_again_per_step() {
    let held = Counted::new();
    let _tab = held.kernel.enter_session("tab-a");
    held.run("SIGN_IN", credential("rs256_valid"));
    let after_sign_in = held.reads();
    for _ in 0..3 {
        held.run("LOOK", json!({}));
    }
    assert_eq!(held.reads(), after_sign_in, "three steps read the held row, not the store");
    assert_eq!(held.gate()["role"], json!("admin"));
}

#[test]
fn control_a_write_to_the_identity_entity_reads_the_row_again() {
    let held = Counted::new();
    let _tab = held.kernel.enter_session("tab-a");
    held.run("SIGN_IN", credential("rs256_valid"));
    held.run("PROMOTE", json!({}));
    let after_write = held.reads();
    held.run("LOOK", json!({}));
    assert_eq!(held.gate()["role"], json!("owner"), "the step reads the written row");
    assert!(held.reads() > after_write, "the held row was dropped by the write and read once more");
}

#[test]
fn a_program_session_tick_reads_the_anonymous_viewer_though_the_program_session_signed_in() {
    let held = Counted::new();
    held.run("SIGN_IN", credential("rs256_valid"));
    held.tick();
    assert_eq!(held.gate()["who"], json!("anonymous"), "a program-session tick belongs to no viewer");
}

#[test]
fn control_a_program_session_event_reads_the_signed_in_identity() {
    let held = Counted::new();
    held.run("SIGN_IN", credential("rs256_valid"));
    held.run("LOOK", json!({}));
    assert_eq!(held.gate()["who"], json!("user-1"));
}

// EXEC-IDENTITY-3: `identity/sign-in verified session` and `identity/sign-out session` name the client session they bind; only a
// program-session step may name one. Without a session they bind the session the step runs in.
fn boot_signer_schema() -> OrbitalSchema {
    let routes = || json!({ "emit": { "success": "IN_OK", "failure": "IN_FAIL" } });
    let verify = json!(["jws/verify", "@payload.token", [], { "issuer": "firebase-auth-emulator@example.com", "audience": "test", "allowUnsigned": true }]);
    serde_json::from_value(json!({
        "name": "App", "typeContractVersion": 1,
        "orbitals": [{
            "name": "Ui",
            "entity": { "name": "Seen", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "who", "type": "string" }, { "name": "kind", "type": "string" }] },
            "traits": [{
                "name": "Viewer", "linkedEntity": "Seen",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "ASK", "effects": [["emit", "SIGN", { "token": "@payload.token", "session": "@payload.session" }]] },
                    { "from": "idle", "to": "idle", "event": "OUT", "effects": [["emit", "SIGN_OUT", { "session": "@payload.session" }]] },
                    { "from": "idle", "to": "idle", "event": "SELF", "effects": [["let", [["v", verify.clone()]], ["identity/sign-in", "@v", "@payload.session", routes()]]] },
                    { "from": "idle", "to": "idle", "event": "IN_FAIL", "effects": [["set", "@entity.kind", "@payload.kind"]] },
                    { "from": "idle", "to": "idle", "event": "WHO", "effects": [["set", "@entity.who", "@user.id"]] }] },
                "emits": [{ "event": "SIGN", "scope": "external" }, { "event": "SIGN_OUT", "scope": "external" }]
            }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Viewer" }] }]
        }, {
            "name": "Auth",
            "entity": { "name": "Person", "identity": true, "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "kind", "type": "string" }, { "name": "failedFor", "type": "string" }] },
            "traits": [{
                "name": "Signer", "linkedEntity": "Person",
                "listens": [
                    { "event": "SIGN", "triggers": "SIGN_ASKED", "source": { "kind": "orbital", "orbital": "Ui", "trait": "Viewer" } },
                    { "event": "SIGN_OUT", "triggers": "OUT_ASKED", "source": { "kind": "orbital", "orbital": "Ui", "trait": "Viewer" } }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "SIGN_ASKED", "guard": ["!=", "@payload.session", ""],
                      "effects": [["let", [["v", verify.clone()]], ["identity/sign-in", "@v", "@payload.session", routes()]]] },
                    { "from": "idle", "to": "idle", "event": "SIGN_ASKED", "guard": ["=", "@payload.session", ""],
                      "effects": [["let", [["v", verify]], ["identity/sign-in", "@v", routes()]]] },
                    { "from": "idle", "to": "idle", "event": "OUT_ASKED", "effects": [["identity/sign-out", "@payload.session", routes()]] },
                    { "from": "idle", "to": "idle", "event": "IN_FAIL", "effects": [["set", "@entity.kind", "@payload.kind"], ["set", "@entity.failedFor", "@payload.session"]] }] }
            }],
            "pages": [{ "name": "Boot", "path": "", "protocol": "boot", "traits": [{ "ref": "Signer" }] }]
        }]
    }))
    .unwrap()
}

fn boot_signer() -> orbital_core_runtime::kernel::RuntimeKernel {
    let kernel = orbital_core_runtime::kernel::RuntimeKernel::new(
        orbital_core_base::effect_types::RuntimeEnvironment::Server,
        std::sync::Arc::new(orbital_core_runtime::entity_source::InMemoryEntityCache::new()),
        Default::default(),
    );
    kernel.register_schema(boot_signer_schema()).unwrap();
    kernel
}

fn ask_in(kernel: &orbital_core_runtime::kernel::RuntimeKernel, from: &str, event: &str, session: &str) {
    let _session = kernel.enter_session(from);
    let payload: Value = serde_json::from_value(json!({ "token": crate::support::identity::token("ada", json!({})), "session": session })).unwrap();
    kernel.dispatch("Viewer", None, event, payload).unwrap();
}

fn who(kernel: &orbital_core_runtime::kernel::RuntimeKernel, session: Option<&str>) -> serde_json::Value {
    let _session = session.map(|s| kernel.enter_session(s));
    kernel.dispatch("Viewer", None, "WHO", Value::Null).unwrap();
    serde_json::to_value(&kernel.snapshot_trait("Viewer", None).entity).unwrap()["who"].clone()
}

fn refusal(kernel: &orbital_core_runtime::kernel::RuntimeKernel, trait_name: &str, session: Option<&str>) -> serde_json::Value {
    let _session = session.map(|s| kernel.enter_session(s));
    serde_json::to_value(&kernel.snapshot_trait(trait_name, None).entity).unwrap()["kind"].clone()
}

#[test]
fn exec_identity_3_a_program_session_step_naming_a_session_binds_that_session() {
    let kernel = boot_signer();
    ask_in(&kernel, "tab-1", "ASK", "tab-1");
    assert_eq!(who(&kernel, Some("tab-1")), json!("ada"), "the named session is signed in");
    assert_eq!(who(&kernel, Some("tab-2")), json!("anonymous"), "control: another session is not");
    assert_eq!(who(&kernel, None), json!("anonymous"), "control: the program session is not");
}

#[test]
fn exec_identity_3_a_program_session_step_may_bind_a_session_other_than_the_one_that_asked() {
    let kernel = boot_signer();
    ask_in(&kernel, "tab-1", "ASK", "tab-2");
    assert_eq!(who(&kernel, Some("tab-2")), json!("ada"));
    assert_eq!(who(&kernel, Some("tab-1")), json!("anonymous"));
}

#[test]
fn control_a_sign_in_naming_no_session_binds_the_session_the_step_runs_in_never_the_asker() {
    let kernel = boot_signer();
    ask_in(&kernel, "tab-1", "ASK", "");
    assert_eq!(who(&kernel, Some("tab-1")), json!("anonymous"), "nothing implicit: the asker is not bound");
    assert_eq!(who(&kernel, None), json!("ada"), "the program session the step ran in is");
}

#[test]
fn exec_identity_3_a_viewers_own_step_naming_a_session_is_refused() {
    let kernel = boot_signer();
    ask_in(&kernel, "tab-1", "SELF", "tab-2");
    assert_eq!(refusal(&kernel, "Viewer", Some("tab-1")), json!("not-program-session"));
    assert_eq!(who(&kernel, Some("tab-2")), json!("anonymous"));
    assert_eq!(who(&kernel, Some("tab-1")), json!("anonymous"));
}

#[test]
fn edge_an_ended_session_is_refused_and_bound_to_nothing() {
    let kernel = boot_signer();
    kernel.free_session("tab-2");
    ask_in(&kernel, "tab-1", "ASK", "tab-2");
    assert_eq!(refusal(&kernel, "Signer", None), json!("session-ended"));
    let entity = { serde_json::to_value(&kernel.snapshot_trait("Signer", None).entity).unwrap() };
    assert_eq!(entity["failedFor"], json!("tab-2"), "the failure names the session it was for");
    assert!(kernel.session_ended("tab-2"), "the ended session stays ended");
}

#[test]
fn exec_identity_3_a_sign_out_naming_a_session_signs_that_session_out() {
    let kernel = boot_signer();
    ask_in(&kernel, "tab-1", "ASK", "tab-1");
    ask_in(&kernel, "tab-3", "ASK", "tab-3");
    ask_in(&kernel, "tab-3", "OUT", "tab-1");
    assert_eq!(who(&kernel, Some("tab-1")), json!("anonymous"));
    assert_eq!(who(&kernel, Some("tab-3")), json!("ada"), "control: the session that asked stays signed in");
}
