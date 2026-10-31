// EXEC-IDENTITY-1, -3, -4 on the embedded server: `identity/sign-in` binds a sealed Verified to the session, `@user` is
// then the program's `[identity]` row of its subject, `identity/sign-out` returns the session to the anonymous viewer, and
// a bound session ignores a per-dispatch viewer.

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
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "SIGN_IN", "effects": sign_in(json!([])) },
                            { "from": "idle", "to": "idle", "event": "SIGN_IN_SLOW", "effects": sign_in(json!([["call-service", "svc", "work", {}]])) },
                            { "from": "idle", "to": "idle", "event": "SIGN_IN_FAKE", "effects": [
                                ["identity/sign-in", "@payload.fake", { "emit": { "success": "IN_OK", "failure": "IN_FAIL" } }]] },
                            { "from": "idle", "to": "idle", "event": "RELAY", "effects": [
                                ["let", [["v", ["jws/verify", "@payload.token", "@payload.keys", { "issuer": "https://issuer.test", "audience": "app-1" }]]],
                                    ["emit", "VERIFIED", { "verified": "@v" }]]] },
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
        server.set_default_user(serde_json::from_value(json!({ "id": "legacy", "role": "legacy" })).unwrap());
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
    assert_eq!(read["Gate"]["role"], json!("admin"), "the row's own field, not the legacy viewer's");
    assert_eq!(read["Gate"]["email"], json!("row@example.com"));
}

#[test]
fn exec_identity_3_a_persistent_identity_with_no_row_answers_no_identity_and_binds_nothing() {
    let rig = Rig::new("persistent", vec![person("someone-else", "admin")]);
    assert_eq!(rig.outcome("SIGN_IN", "tab-a", credential("rs256_valid")), json!("no-identity"));
    assert_eq!(
        rig.viewer("tab-a").0,
        json!("legacy"),
        "control: an unbound session keeps the transitional viewer"
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
    assert_eq!(rig.viewer("tab-a").0, json!("legacy"));
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
    assert_eq!(rig.viewer("tab-b").0, json!("legacy"), "another session is not signed in");
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
fn exec_identity_1_a_bound_session_ignores_the_dispatch_viewer() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    rig.run("SIGN_IN", "tab-a", credential("rs256_valid"));
    let as_other = |client: &str| {
        let mut read = request("READ", client, json!({}));
        read.user = Some(serde_json::from_value(json!({ "id": "other", "role": "other" })).unwrap());
        rig.send(read)["Gate"]["who"].clone()
    };
    assert_eq!(as_other("tab-a"), json!("user-1"));
    assert_eq!(
        as_other("tab-b"),
        json!("other"),
        "control: an unbound session takes the dispatch viewer"
    );
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
    assert_eq!(rig.viewer("tab-a").0, json!("legacy"), "the revived session starts unbound");
}

#[test]
fn exec_identity_3_an_internal_emit_carries_a_verified_to_a_listener_that_signs_in_with_it() {
    let rig = Rig::new("persistent", vec![person("user-1", "admin")]);
    rig.run("RELAY", "tab-a", credential("rs256_valid"));
    assert_eq!(rig.viewer("tab-a"), (json!("user-1"), json!("admin")));
}
