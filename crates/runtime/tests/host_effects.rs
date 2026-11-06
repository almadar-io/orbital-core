// `random-bytes`, `credential-status` and `credential-forget` through the kernel (EXEC-HTTP-2): one journaled
// host ask each, the principal comes from the dispatch's @user and never the program, and a failure fires
// the declared route with `{ error }` and no ask.

#[path = "support/mod.rs"]
mod support;

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::credential_admin::CredentialStanding;
use orbital_core_runtime::resumable::{completion_event, program_call_key, IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn schema(effect: Json) -> OrbitalSchema {
    schema_with(effect, json!({ "credentials": ["admin"] }))
}

fn schema_with(effect: Json, grants: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "osGrants": grants,
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Host", "linkedEntity": "Job",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "GO", "effects": [effect] },
                        { "from": "idle", "to": "done", "event": "OK" },
                        { "from": "idle", "to": "failed", "event": "BAD" }
                    ]
                } }]
        }]
    }))
    .unwrap()
}

/// The client session a signed-in request runs in; `run_on` signs it in as [`signed_in`] before the request starts.
const VIEWER_SESSION: &str = "viewer";

fn go(user: Option<Json>) -> EventRequest {
    assert!(user.is_none() || user == signed_in(), "a request runs as the one signed-in viewer or as nobody");
    EventRequest {
        event: "GO".to_string(),
        target_trait: Some("Host".to_string()),
        payload: Some(serde_json::from_value(json!({ "principal": "evil" })).unwrap()),
        client_id: user.map(|_| VIEWER_SESSION.to_string()),
        ..Default::default()
    }
}

fn signed_in() -> Option<Json> {
    Some(json!({ "id": "u1", "role": "member" }))
}

fn run(
    effect: Json,
    request: EventRequest,
    host: impl FnMut(&IoRequest) -> IoAnswer,
) -> (EventResponse, Vec<IoRequest>) {
    run_on(EmbeddedServer::new(schema(effect)).unwrap(), request, host)
}

fn run_on(
    server: EmbeddedServer,
    request: EventRequest,
    host: impl FnMut(&IoRequest) -> IoAnswer,
) -> (EventResponse, Vec<IoRequest>) {
    if request.client_id.as_deref() == Some(VIEWER_SESSION) {
        support::identity::enable(server.kernel(), &["role"]);
        support::identity::sign_in_server(&server, VIEWER_SESSION, "u1", json!({ "role": "member" }));
    }
    drive(&server, request, host)
}

fn drive(
    server: &EmbeddedServer,
    request: EventRequest,
    mut host: impl FnMut(&IoRequest) -> IoAnswer,
) -> (EventResponse, Vec<IoRequest>) {
    let mut seen = Vec::new();
    let mut progress = server.start("Main", request).unwrap();
    loop {
        match progress {
            ServerProgress::Done(response) => return (response, seen),
            ServerProgress::Await { flight, requests } => {
                let replies = requests
                    .into_iter()
                    .map(|pending| IoReply {
                        id: pending.id,
                        answer: match &pending.request {
                            IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
                            other => {
                                seen.push(other.clone());
                                host(other)
                            }
                        },
                    })
                    .collect();
                progress = server.resume(flight, replies).unwrap();
            }
            ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
        }
    }
}

fn emitted(response: &EventResponse) -> Vec<(&str, Option<&Value>)> {
    response
        .emitted_events
        .iter()
        .map(|e| (e.event.as_str(), e.payload.as_ref()))
        .collect()
}

fn payload_of(response: &EventResponse, event: &str) -> Json {
    let found = emitted(response)
        .into_iter()
        .find(|(e, _)| *e == event)
        .unwrap_or_else(|| panic!("{event} was not emitted: {:?}", emitted(response)));
    serde_json::to_value(found.1).unwrap()
}

fn routes() -> Json {
    json!({ "emit": { "success": "OK", "failure": "BAD" } })
}

#[test]
fn random_bytes_asks_the_host_once_and_emits_base64url_without_padding() {
    let (response, seen) =
        run(
            json!(["random-bytes", 2, routes()]),
            go(None),
            |request| match request {
                IoRequest::Random { bytes: 2 } => IoAnswer::Random(Ok((vec![0xfb, 0xff]).into())),
                other => panic!("the fixture asks for {other:?}"),
            },
        );
    assert_eq!(seen, vec![IoRequest::Random { bytes: 2 }]);
    assert_eq!(payload_of(&response, "OK"), json!({ "value": "-_8" }));
}

#[test]
fn a_bad_count_fails_the_effect_before_any_ask() {
    for bad in [json!(0), json!(1025), json!(1.5), json!("many"), json!(-3)] {
        let (response, seen) = run(
            json!(["random-bytes", bad.clone(), routes()]),
            go(None),
            |request| panic!("a bad count must not ask the host, asked {request:?}"),
        );
        assert!(seen.is_empty(), "{bad}");
        let error = payload_of(&response, "BAD");
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .starts_with("random-bytes: the byte count must be an integer from 1 to 1024"),
            "{bad}: {error}"
        );
    }
}

#[test]
fn control_the_bounds_one_and_1024_are_asked() {
    for good in [1u32, 1024] {
        let (_, seen) = run(json!(["random-bytes", good, routes()]), go(None), |_| {
            IoAnswer::Random(Ok((vec![0; good as usize]).into()))
        });
        assert_eq!(seen, vec![IoRequest::Random { bytes: good }]);
    }
}

#[test]
fn a_host_failure_fires_the_failure_route() {
    let (response, _) = run(json!(["random-bytes", 8, routes()]), go(None), |_| {
        IoAnswer::Random(Err("entropy source down".to_string()))
    });
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({ "error": "entropy source down" })
    );
}

#[test]
fn a_keyed_random_bytes_echoes_its_key_in_success_and_failure_and_an_unkeyed_one_adds_none() {
    let keyed = |count: Json| json!(["random-bytes", count, {"key": "@payload.principal", "emit": {"success": "OK", "failure": "BAD"}}]);
    let (ok, _) = run(keyed(json!(2)), go(None), |_| IoAnswer::Random(Ok((vec![0xfb, 0xff]).into())));
    assert_eq!(payload_of(&ok, "OK"), json!({ "value": "-_8", "key": "evil" }));
    let (down, _) = run(keyed(json!(2)), go(None), |_| IoAnswer::Random(Err("entropy source down".to_string())));
    assert_eq!(payload_of(&down, "BAD"), json!({ "error": "entropy source down", "key": "evil" }));
    let (bad_count, _) = run(keyed(json!(0)), go(None), |request| panic!("asked {request:?}"));
    assert_eq!(payload_of(&bad_count, "BAD")["key"], "evil");
    let (plain, _) = run(json!(["random-bytes", 2, routes()]), go(None), |_| IoAnswer::Random(Ok((vec![0xfb, 0xff]).into())));
    assert_eq!(payload_of(&plain, "OK"), json!({ "value": "-_8" }));
    let (empty, _) = run(json!(["random-bytes", 2, {"key": "", "emit": {"success": "OK", "failure": "BAD"}}]), go(None), |request| panic!("asked {request:?}"));
    assert!(emitted(&empty).iter().all(|(event, _)| *event != "OK"), "an empty key never draws");
}

#[test]
fn explicit_app_status_uses_the_app_slot_with_or_without_a_viewer() {
    for user in [None, signed_in()] {
        let (response, seen) = run(
            json!(["credential-status", {"credential": "KEY", "scope": "app"}, routes()]),
            go(user),
            |request| {
                assert_eq!(
                    request,
                    &IoRequest::CredentialStatus {
                        name: "KEY".into(),
                        principal: None
                    }
                );
                IoAnswer::CredentialStatus(Ok(CredentialStanding::of(true)))
            },
        );
        assert_eq!(
            seen,
            vec![IoRequest::CredentialStatus {
                name: "KEY".into(),
                principal: None
            }]
        );
        assert_eq!(payload_of(&response, "OK"), json!({"connected": true, "label": ""}));
    }
}

#[test]
fn status_hands_the_program_the_account_label_and_nothing_else_about_the_credential() {
    let (response, _) = run(
        json!(["credential-status", {"credential": "KEY", "scope": "app"}, routes()]),
        go(None),
        |_| IoAnswer::CredentialStatus(Ok(CredentialStanding { connected: true, label: Some("ada@example.com".into()) })),
    );
    assert_eq!(payload_of(&response, "OK"), json!({"connected": true, "label": "ada@example.com"}));
    let (unlabeled, _) = run(
        json!(["credential-status", {"credential": "KEY", "scope": "app"}, routes()]),
        go(None),
        |_| IoAnswer::CredentialStatus(Ok(CredentialStanding::of(true))),
    );
    assert_eq!(payload_of(&unlabeled, "OK"), json!({"connected": true, "label": ""}), "no label reads as empty text");
}

#[test]
fn explicit_user_status_keeps_dispatch_authority_and_missing_viewer_refusal() {
    let effect = json!(["credential-status", {"credential": "KEY", "scope": "user"}, routes()]);
    let (response, seen) = run(effect.clone(), go(signed_in()), |request| {
        assert_eq!(
            request,
            &IoRequest::CredentialStatus {
                name: "KEY".into(),
                principal: Some("u1".into())
            }
        );
        IoAnswer::CredentialStatus(Ok(CredentialStanding::of(false)))
    });
    assert_eq!(seen.len(), 1);
    assert_eq!(payload_of(&response, "OK"), json!({"connected": false, "label": ""}));

    let (response, seen) = run(effect, go(None), |request| {
        panic!("anonymous user status asked {request:?}")
    });
    assert!(seen.is_empty());
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({"error": "credential-status: nobody is signed in"})
    );
}

#[test]
fn invalid_status_selections_never_ask_for_a_credential() {
    for selection in [
        json!({"credential": "KEY"}),
        json!({"scope": "app"}),
        json!({"credential": "KEY", "scope": "tenant"}),
        json!({"credential": "KEY", "scope": null}),
        json!({"credential": 5, "scope": "app"}),
        json!({"credential": "KEY", "scope": "app", "principal": "other"}),
        json!({"credential": "KEY", "scope": "app", "extra": true}),
    ] {
        let (response, seen) = run(
            json!(["credential-status", selection, routes()]),
            go(signed_in()),
            |request| panic!("invalid status selection asked {request:?}"),
        );
        assert!(seen.is_empty());
        assert!(payload_of(&response, "BAD")["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()));
    }
}

#[test]
fn control_explicit_app_status_does_not_enable_app_forget() {
    let (response, seen) = run(
        json!(["credential-forget", {"credential": "KEY", "scope": "app"}, routes()]),
        go(signed_in()),
        |request| panic!("app forget must remain unsupported, asked {request:?}"),
    );
    assert!(seen.is_empty());
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({"error": "credential-forget: the credential must be a secret knob's name"})
    );
}

#[test]
fn credential_status_takes_the_principal_from_the_dispatch_user_not_the_program() {
    let effect = json!(["credential-status", "tok_name", routes()]);
    let (response, seen) = run(effect.clone(), go(signed_in()), |request| match request {
        IoRequest::CredentialStatus { name, principal }
            if name == "tok_name" && principal.as_deref() == Some("u1") =>
        {
            IoAnswer::CredentialStatus(Ok(CredentialStanding::of(true)))
        }
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(
        seen,
        vec![IoRequest::CredentialStatus {
            name: "tok_name".into(),
            principal: Some("u1".into())
        }]
    );
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": true, "label": "" }));

    let (response, _) = run(effect, go(signed_in()), |_| {
        IoAnswer::CredentialStatus(Ok(CredentialStanding::of(false)))
    });
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": false, "label": "" }));
}

#[test]
fn credential_effects_need_a_signed_in_viewer_and_ask_nothing() {
    for head in ["credential-status", "credential-forget"] {
        let (response, seen) = run(json!([head, "tok_name", routes()]), go(None), |request| {
            panic!("an anonymous dispatch must not ask the host, asked {request:?}")
        });
        assert!(seen.is_empty(), "{head}");
        assert_eq!(
            payload_of(&response, "BAD"),
            json!({ "error": format!("{head}: nobody is signed in") })
        );
    }
}

#[test]
fn credential_forget_drops_the_viewers_value_and_reports_not_connected() {
    let (response, seen) = run(
        json!(["credential-forget", "tok_name", routes()]),
        go(signed_in()),
        |request| match request {
            IoRequest::CredentialForget { .. } => IoAnswer::CredentialForget(Ok(())),
            other => panic!("the fixture asks for {other:?}"),
        },
    );
    assert_eq!(
        seen,
        vec![IoRequest::CredentialForget {
            name: "tok_name".into(),
            principal: Some("u1".into())
        }]
    );
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": false, "label": "" }));
}

#[test]
fn a_non_text_credential_fails_the_effect() {
    let (response, seen) = run(
        json!(["credential-status", 7, routes()]),
        go(signed_in()),
        |request| panic!("a non-text credential must not ask the host, asked {request:?}"),
    );
    assert!(seen.is_empty());
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({ "error": "credential-status: the credential must be a secret knob's name" })
    );
}

#[test]
fn the_wire_round_trips_every_new_ask_and_answer() {
    use orbital_core_runtime::embed::wire::{IoAnswerWire, IoRequestWire};
    let requests = [
        IoRequest::Random { bytes: 16 },
        IoRequest::CredentialStatus {
            name: "n".into(),
            principal: Some("u".into()),
        },
        IoRequest::CredentialForget {
            name: "n".into(),
            principal: None,
        },
        IoRequest::CredentialConnect {
            grant: Box::new(
                orbital_core_runtime::credential_admin::CredentialGrant::from_request(&serde_json::from_value(connect_request()).unwrap(), None)
                    .unwrap(),
            ),
        },
        IoRequest::Secret {
            name: "n".into(),
            principal: Some("u".into()),
        },
        IoRequest::SecretSet {
            name: "n".into(),
            value: "v".into(),
            ttl_ms: 5,
            principal: Some("u".into()),
        },
    ];
    for request in requests {
        let wire = serde_json::to_string(&IoRequestWire::from(&request)).unwrap();
        let back: IoRequest = serde_json::from_str::<IoRequestWire>(&wire).unwrap().into();
        assert_eq!(back, request, "{wire}");
    }
    let refused = IoAnswer::CredentialConnect(Err(orbital_core_runtime::credential_admin::ConnectFailure::new(
        orbital_core_runtime::credential_admin::ConnectFailureKind::InvalidGrant,
        "the code was already used",
    )));
    let wire = serde_json::to_value(IoAnswerWire::from(&refused)).unwrap();
    assert_eq!(wire, json!({ "kind": "credentialConnect", "result": { "error": { "kind": "invalid_grant", "message": "the code was already used" } } }));
    assert_eq!(serde_json::from_value::<IoAnswerWire>(wire).map(IoAnswer::from).unwrap(), refused);
    let wire =
        serde_json::to_value(IoAnswerWire::from(&IoAnswer::Random(Ok((vec![1, 2, 3]).into())))).unwrap();
    assert_eq!(
        wire,
        json!({ "kind": "random", "result": { "ok": "AQID" } })
    );
    let back: IoAnswer = serde_json::from_value::<IoAnswerWire>(wire).unwrap().into();
    assert_eq!(back, IoAnswer::Random(Ok((vec![1, 2, 3]).into())));
    let bad: IoAnswerWire =
        serde_json::from_value(json!({ "kind": "random", "result": { "ok": "!!" } })).unwrap();
    assert!(matches!(IoAnswer::from(bad), IoAnswer::Random(Err(_))));
}

#[test]
fn a_random_answer_and_a_secret_set_value_are_redacted_in_debug() {
    let random = format!("{:?}", IoAnswer::Random(Ok((vec![9, 9, 9]).into())));
    assert!(
        !random.contains('9') && random.contains("redacted"),
        "{random}"
    );
    let set = format!(
        "{:?}",
        IoRequest::SecretSet {
            name: "n".into(),
            value: "hunter2".into(),
            ttl_ms: 1,
            principal: None
        }
    );
    assert!(!set.contains("hunter2"), "{set}");
}

fn connect_request() -> Json {
    json!({
        "credential": "tok_name", "tokenEndpoint": "https://auth.example/token", "grant": "authorization_code",
        "code": "single-use-code-7", "redirectUri": "https://app.example/cb", "verifier": "pkce-verifier-9",
        "clientId": "client_name", "clientSecret": "secret_name", "clientAuth": "basic", "scopes": ["openid"]
    })
}

#[test]
fn credential_connect_on_a_bare_host_answers_unavailable_and_the_ask_hides_code_and_verifier() {
    let (response, seen) = run(
        json!(["credential-connect", connect_request(), routes()]),
        go(None),
        |request| orbital_core_runtime::resumable::answer_ports(None, request),
    );
    assert_eq!(seen.len(), 1);
    assert!(matches!(&seen[0], IoRequest::CredentialConnect { grant } if grant.name == "tok_name" && grant.principal.is_none()));
    assert_eq!(
        payload_of(&response, "BAD"),
        json!({ "error": "this host holds no credential store that can connect", "kind": "unavailable" })
    );
    let shown = format!("{seen:?}");
    assert!(shown.contains("tok_name") && shown.contains("client_name"), "control: credential names show: {shown}");
    assert!(!shown.contains("single-use-code-7") && !shown.contains("pkce-verifier-9"), "leaked: {shown}");
}

#[test]
fn credential_connect_for_the_viewer_needs_a_signed_in_viewer_and_a_closed_request() {
    let mut user_scoped = connect_request();
    user_scoped["scope"] = json!("user");
    let (response, seen) = run(json!(["credential-connect", user_scoped.clone(), routes()]), go(None), |request| {
        panic!("an anonymous user-scoped connect must not ask the host, asked {request:?}")
    });
    assert!(seen.is_empty());
    assert_eq!(payload_of(&response, "BAD")["kind"], json!("unsigned"));
    let (_, seen) = run(json!(["credential-connect", user_scoped, routes()]), go(signed_in()), |request| match request {
        IoRequest::CredentialConnect { grant } if grant.principal.as_deref() == Some("u1") => {
            IoAnswer::CredentialConnect(Ok(CredentialStanding { connected: true, label: Some("me@x".into()) }))
        }
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(seen.len(), 1);
    let mut extra = connect_request();
    extra["principal"] = json!("other");
    let (response, seen) = run(json!(["credential-connect", extra, routes()]), go(signed_in()), |request| {
        panic!("an unknown key must not ask the host, asked {request:?}")
    });
    assert!(seen.is_empty());
    assert_eq!(payload_of(&response, "BAD")["kind"], json!("refused"));
}

mod credential_admin_effects {
    const RSA_SIGNING_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDBmj4cs/1X1KKS\nd7w4cQLXwY2/zQSivWFyZTFem1GZVnlg223x1KbrF+XtLGmIlWw4U6Ily/uiz645\nZFKwDJ4IRAGs89pnrAB8gR46c0aLRNYKfOpsu0ox0QZre8hgBQF5oUTdGXFQW4Sv\ndGVkM4zMtWCcb+nWWeM1+G86ghU57hjMj/5cnYEwv+waqAOnC7aMGQBNfol9lk4r\n48QKWa5TCibIbo3Hcs5UkqNZYsX+rJrUBtOk1f1/1fhXTAvWsMFudkcYH+M5hmif\nI+O20LVfOV7C2N4DhuWuk/sHHNyJVtwBnZjo3uAdEH2z1lK09JJSFwxkza6QYSRI\ndLC56SAlAgMBAAECggEALGnQhvAplc0TcItkVARk9Q8TkCUdw6GzabVaMoxqF0W1\nuakNG1W/5x3H4vV1zl4NAgz5nTci5pJDMypXJ8j1Ngs34S/d8juMrwNF5n3MeKWV\nfrlyY/hBSJVdpimy6YgscvUyoZA1JQyhr0iB/A8xZULUw36PA9wCGj8AcUknSm70\npQZHB+58X0JG+M+XlU6xW+Tb0V8r6CFGWSxhdOYaXwIBhdBJKvh1KiIHkCDPqX5i\nPsP09EsX7oDsS/+S/INVgqX/q89+ehPdzY19hXgWRD9sbH2h75ImIh/Axy52tJl/\nKCFptgFIf7WBuzJ93iMOVriCR+vncNSj1Gdm0pBq3wKBgQDx7kUsc23zRs8ca+30\n8OpMc81q/qcWBGzs+Fj941hdMRzYzUpk1+Re36Dj5PH1HPc6/h1c/Ip7c8dN1zds\nnJjbhCTpOMuLxhXy81jtWwVr9O4arc0rKH5ZjrDAmTeGtoPMhFjxKdWuljrB/1VA\ndZVhp2S4DPzPJcYu9RUs0JkkvwKBgQDM3H0Z6qiJwUSXVka9kY8rv0ntjr76UUWI\n+E7aHvENElt/YYFeaAG+Suib+bEJ6yqfJlrfF2Gut5PrC14jc9heO2CKkHa5R/8M\nzNALNZ3YSYwAaGlRLrAlRhc7Xe2uPww+Nev4X3O+NUO5MKgLzxgBzWjl+ND6K9Ck\nrsOon43AGwKBgQCsz5MPvmASf0jft2ShKxlhhPJ9c+sr+ftIhdkjeaqb3sIJRxLV\n17Tj4/+L6fS/LnKW6/9TdX0B0+DQhGOtrGovm0O2aABVFiiFgzfJS9QimjEeU3mT\nU3h1RkyRNqCIFIUuqHbTxRtCe0L7YgSmZ5dNywFRys0mtofK8PuwKEzMBwKBgQDJ\nP20SEajBsv/r35Cc16qbIk7AtTM9eqthk/T6Z90Lp1YFO8c389qKktIRr+fQQ5pa\nP/aTJEgMuEDybPdswnfykkgTtzk6aNTQQT+7U0+d0xpaqDeVzdSyuTlHU82TGFKx\npo0KMNLmLzM5cOgUubvuwVhkWtsIeBR+Q0TomN1KswKBgGtczq6Jm7Ic4+WZGzh7\nNVqwdthUdR5EdBjxi5I9SVIS1Rb6pH4Y2cUOBGCz55STj4heV3d0IRS0QCw3TtFi\n0coDp2D6a2eEZDeqdywBQVg8NYpKJI74XyXu+Ogkn0T3vQZtN+BH1fHrTfbSdTa/\nAM812eZIeiLOTXTbKy88Y2Xy\n-----END PRIVATE KEY-----\n";
    const RSA_SIGNATURE_OF_SIGN_ME: &str = "ZNheFwSQUoc3hijIwW3RCdNxljDrGANFey5HEtyMvYys8RMSrC3Unkq7ya6CpBJEltkF6/tpaBoglMlH9TSxZolEThukPFL8+vtR7EEch6I4oceaDqjOE6lxhKJfbU3v8elFyaMaxByOxklNcoKnqzNLBdH/waaJhkyLHg9gQ0beE5L1aKe90US3pjdAO3RCPp3kWUHeQSEm697MS5DJjlW/m1G8292iJaYf53B9hPPP9XcC9EVvRRf57ZwJJ3/RnVexo4UFEPkyqvmaPzL6h324ArZdV2veCtloWOG+fhOWM2LzDKRJwUS/7u8mdcmX8Z/ETOkbXQ946nfuXHOXLQ==";

    use super::*;
    use orbital_core_runtime::credential_admin::{RotateCounts, StoreListing, StoredName};

    fn listing() -> StoreListing {
        StoreListing { enabled: true, entries: vec![
            StoredName { service: "apikeys".into(), name: "SALT".into(), last4: Some("1111".into()) },
            StoredName { service: "apikeys".into(), name: "SHORT".into(), last4: None },
        ] }
    }

    fn installed_services() -> Json {
        orbital_core_base::language::registry::baked_service_catalog()
            .iter()
            .map(|service| {
                let credentials: Vec<Json> = service
                    .credentials
                    .iter()
                    .map(|c| json!({ "envVar": c.env_var, "required": c.required, "description": c.description }))
                    .collect();
                let mut row = json!({ "name": service.name, "credentials": credentials });
                if let Some(backend) = &service.backend {
                    row["backend"] = json!(backend);
                }
                row
            })
            .collect()
    }

    fn store(request: &IoRequest) -> IoAnswer {
        match request {
            IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(Some(listing()))),
            IoRequest::CredentialStoreWrite { .. } => IoAnswer::CredentialStoreWrite(Ok(())),
            IoRequest::CredentialStoreRemove { name } => IoAnswer::CredentialStoreRemove(Ok(name == "SALT")),
            IoRequest::CredentialStoreRotate => IoAnswer::CredentialStoreRotate(Ok(RotateCounts { rotated: 2, already_current: 1, unreadable: 0 })),
            IoRequest::CredentialModeRead => IoAnswer::CredentialModeRead(Ok(Some("real".into()))),
            IoRequest::CredentialModeWrite { mode } => IoAnswer::CredentialModeWrite(Ok(mode.clone())),
            IoRequest::Secret { name, .. } => IoAnswer::Secret(Ok((name == "SALT").then(|| "held".into()))),
            other => panic!("the fixture asks for {other:?}"),
        }
    }

    fn answer(effect: Json, user: Option<Json>) -> (Json, Vec<IoRequest>) {
        let (response, seen) = run(effect, go(user), store);
        (payload_of(&response, "OK"), seen)
    }

    #[test]
    fn each_credential_operator_makes_its_one_raw_store_ask_and_answers_the_raw_reply() {
        let (listed, seen) = answer(json!(["credential-list", {}, routes()]), signed_in());
        assert_eq!(
            listed,
            json!({ "enabled": true, "entries": [{ "service": "apikeys", "name": "SALT", "last4": "1111" }, { "service": "apikeys", "name": "SHORT" }], "env": [], "declared": [], "services": installed_services() }),
            "control: a row the store reports no tail for answers without a last4 key"
        );
        assert_eq!(seen, vec![IoRequest::CredentialStoreList]);

        let (saved, seen) = answer(json!(["credential-set", {"service": "apikeys", "envVar": "SALT", "value": "salt-value-1111"}, routes()]), signed_in());
        assert_eq!(saved, json!({ "saved": true }));
        assert!(matches!(seen.last(), Some(IoRequest::CredentialStoreWrite { service, name, value }) if service == "apikeys" && name == "SALT" && value.expose() == "salt-value-1111"));
        assert!(!format!("{seen:?}").contains("salt-value"), "the stored value never shows in a Debug of the asks");

        let (removed, _) = answer(json!(["credential-remove", {"envVar": "SALT"}, routes()]), signed_in());
        assert_eq!(removed, json!({ "removed": true }));
        let (absent, _) = answer(json!(["credential-remove", {"envVar": "OTHER"}, routes()]), signed_in());
        assert_eq!(absent, json!({ "removed": false }), "control: a name the store lacks removes nothing");

        let (rotated, _) = answer(json!(["credential-rotate", routes()]), signed_in());
        assert_eq!(rotated, json!({ "rotated": 2.0, "alreadyCurrent": 1.0, "unreadable": 0.0 }));

        let (read, seen) = answer(json!(["credential-mode", {}, routes()]), signed_in());
        assert_eq!(read, json!({ "mode": "real" }));
        assert_eq!(seen, vec![IoRequest::CredentialModeRead]);
        let (switched, _) = answer(json!(["credential-mode", {"mode": "mock"}, routes()]), signed_in());
        assert_eq!(switched, json!({ "mode": "mock" }));

        let (held, _) = answer(json!(["credential-test", {"envVar": "SALT"}, routes()]), signed_in());
        assert_eq!(held, json!({ "held": true }));
        let (missing, _) = answer(json!(["credential-test", {"envVar": "OTHER"}, routes()]), signed_in());
        assert_eq!(missing, json!({ "held": false }));
    }

    fn declaring() -> OrbitalSchema {
        let read = |knob: &str| json!(["hmac/sha256", format!("@config.{knob}"), "x", "hex"]);
        serde_json::from_value(json!({
            "name": "App",
            "osGrants": { "credentials": ["admin"] },
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "config": {
                        "long": { "type": "secret", "default": "ENV_LONG", "description": "l" },
                        "short": { "type": "secret", "default": "ENV_SHORT", "description": "s" },
                        "unset": { "type": "secret", "default": "ENV_UNSET", "description": "u" }
                    },
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [["credential-list", {}, routes()]] },
                            { "from": "idle", "to": "idle", "event": "READ", "effects": [
                                ["let", [["a", read("long")], ["b", read("short")], ["c", read("unset")]], ["emit", "OK", { "a": "@a" }]]
                            ] },
                            { "from": "idle", "to": "done", "event": "OK" },
                            { "from": "idle", "to": "failed", "event": "BAD" }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    const FULL_VALUE: &str = "abcdefgh6789";

    fn listed_with(answer_tails: impl Fn(&IoRequest) -> IoAnswer) -> (Json, Vec<IoRequest>) {
        let server = EmbeddedServer::new(declaring()).unwrap();
        let (response, seen) = run_on(server, go(signed_in()), |request| match request {
            IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(Some(listing()))),
            IoRequest::CredentialEnvTails { .. } => answer_tails(request),
            other => panic!("the fixture asks for {other:?}"),
        });
        (payload_of(&response, "OK"), seen)
    }

    fn host_env(request: &IoRequest) -> IoAnswer {
        use orbital_core_runtime::credentials::Credentials;
        use orbital_core_runtime::resumable::{answer_ports, PortHost};
        let creds = Credentials::from_env(
            [("ENV_LONG", FULL_VALUE), ("ENV_SHORT", "abcde"), ("OTHER", "zzzzzzzzzzzz")]
                .into_iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        );
        let on_event = |_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {};
        answer_ports(Some(&PortHost { secrets: &creds, on_event: &on_event, os: None }), request)
    }

    #[test]
    fn credential_list_adds_the_declared_env_names_the_hosts_environment_holds_with_their_last4() {
        let (listed, seen) = listed_with(host_env);
        assert_eq!(listed["env"], json!([{ "name": "ENV_LONG", "last4": "6789" }, { "name": "ENV_SHORT" }]));
        assert_eq!(
            seen,
            vec![
                IoRequest::CredentialStoreList,
                IoRequest::CredentialEnvTails { names: vec!["ENV_LONG".into(), "ENV_SHORT".into(), "ENV_UNSET".into()] },
            ],
            "the ask names the declared credentials and nothing else"
        );
    }

    #[test]
    fn the_env_ask_carries_and_journals_no_value() {
        let (listed, seen) = listed_with(host_env);
        let answer = host_env(&seen[1]);
        let wire = serde_json::to_string(&orbital_core_runtime::embed::wire::IoAnswerWire::from(&answer)).unwrap();
        for text in [format!("{seen:?}"), format!("{answer:?}"), wire, listed.to_string()] {
            assert!(!text.contains(FULL_VALUE), "{text}");
            assert!(!text.contains("abcde\""), "{text}");
        }
    }

    #[test]
    fn a_kernel_with_no_host_answers_an_empty_env_and_an_unsupported_host_does_the_same() {
        use orbital_core_runtime::credential_admin::EnvTails;
        let (bare, _) = listed_with(|request| orbital_core_runtime::resumable::answer_ports(None, request));
        assert_eq!(bare["env"], json!([]));
        assert_eq!(bare["enabled"], true, "control: the rest of the reply is untouched");
        let (unsupported, _) = listed_with(|_| IoAnswer::CredentialEnvTails(Ok(EnvTails::Unsupported)));
        assert_eq!(unsupported["env"], json!([]));
    }

    #[test]
    fn a_program_declaring_no_credential_asks_no_env_tails() {
        let (_, seen) = answer(json!(["credential-list", {}, routes()]), signed_in());
        assert_eq!(seen, vec![IoRequest::CredentialStoreList]);
    }

    #[test]
    fn the_wire_round_trips_the_env_tails_ask_and_answers() {
        use orbital_core_runtime::credential_admin::{EnvTail, EnvTails};
        use orbital_core_runtime::embed::wire::{IoAnswerWire, IoRequestWire};
        let request = IoRequest::CredentialEnvTails { names: vec!["A".into(), "B".into()] };
        let wire = serde_json::to_value(IoRequestWire::from(&request)).unwrap();
        assert_eq!(wire, json!({ "kind": "credentialEnvTails", "names": ["A", "B"] }));
        assert_eq!(IoRequest::from(serde_json::from_value::<IoRequestWire>(wire).unwrap()), request);
        for answer in [
            IoAnswer::CredentialEnvTails(Ok(EnvTails::Held { tails: vec![EnvTail { name: "A".into(), last4: Some("6789".into()) }, EnvTail { name: "B".into(), last4: None }] })),
            IoAnswer::CredentialEnvTails(Ok(EnvTails::Unsupported)),
            IoAnswer::CredentialEnvTails(Err("env unreadable".into())),
        ] {
            let wire = serde_json::to_value(IoAnswerWire::from(&answer)).unwrap();
            let back: IoAnswer = serde_json::from_value::<IoAnswerWire>(wire.clone()).unwrap().into();
            assert_eq!(back, answer, "{wire}");
        }
        let unsupported = serde_json::to_value(IoAnswerWire::from(&IoAnswer::CredentialEnvTails(Ok(EnvTails::Unsupported)))).unwrap();
        assert_eq!(unsupported, json!({ "kind": "credentialEnvTails", "result": { "ok": { "kind": "unsupported" } } }));
    }

    #[test]
    fn a_raw_credential_operator_needs_the_declared_admin_grant() {
        let ungranted = |grants: Json| {
            let server = EmbeddedServer::new(schema_with(json!(["credential-list", {}, routes()]), grants)).unwrap();
            run_on(server, go(signed_in()), store)
        };
        for grants in [json!({}), json!({ "stdio": ["out"] })] {
            let (response, seen) = ungranted(grants);
            assert_eq!(
                payload_of(&response, "BAD"),
                json!({ "error": "credential-list: the program declares no `credentials: [admin]` grant" })
            );
            assert!(seen.is_empty(), "an ungranted operator asks the store nothing: {seen:?}");
        }
        let (response, seen) = ungranted(json!({ "credentials": ["admin"] }));
        assert_eq!(payload_of(&response, "OK")["enabled"], true, "control: the declared grant lets it through");
        assert_eq!(seen, vec![IoRequest::CredentialStoreList]);
    }

    #[test]
    fn the_operators_apply_no_role_gate_and_a_request_outside_the_shape_is_a_host_failure() {
        let (anonymous, _) = answer(json!(["credential-list", {}, routes()]), None);
        assert_eq!(anonymous["enabled"], true, "the role gate is std-credential-admin's, not the operator's");
        let (response, seen) = run(json!(["credential-set", {"service": "apikeys"}, routes()]), go(signed_in()), store);
        assert_eq!(payload_of(&response, "BAD"), json!({ "error": "credential-set needs envVar" }));
        assert!(seen.is_empty(), "a request without its fields asks the store nothing: {seen:?}");
        let (response, _) = run(
            json!(["credential-set", {"service": "apikeys", "envVar": "SALT", "value": "v"}, routes()]),
            go(signed_in()),
            |request| match request {
                IoRequest::CredentialStoreWrite { .. } => IoAnswer::CredentialStoreWrite(Err("No credential store is installed on this host".into())),
                other => panic!("the fixture asks for {other:?}"),
            },
        );
        assert_eq!(payload_of(&response, "BAD"), json!({ "error": "No credential store is installed on this host" }));
    }

    // EXEC-CRYPTO-1: the kernel resolves a credential operand; the program never holds it.
    #[test]
    fn a_credential_operand_is_resolved_by_the_kernel_and_never_reaches_program_data() {
        let schema_for = |call: Json| -> OrbitalSchema {
            serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Keys",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "config": { "salt": { "type": "secret", "default": "SALT_NAME", "description": "salt" } },
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [
                                ["let", [["h", call.clone()]],
                                    ["emit", "OK", { "hash": "@h", "salt": "@config.salt" }]]
                            ] },
                            { "from": "idle", "to": "done", "event": "OK" }
                        ]
                    } }]
            }]
            }))
            .unwrap()
        };
        for (call, name, value, expected) in [
            (json!(["hash/scrypt", "sk_live_", "@config.salt"]), "SALT_NAME", "00112233445566778899", "010bbcf1a211930bca10afa35c740b1c70dcb058711e165c9918f1137c97b940"),
            (json!(["hmac/sha256", "@config.salt", "what do ya want for nothing?", "hex"]), "SALT_NAME", "Jefe", "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
            (json!(["hmac/sha256", "@config.salt", "20150830", "hex", "AWS4"]), "SALT_NAME", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "0138c7a6cbd60aa727b2f653a522567439dfb9f3e72b21f9b25941a42f04a7cd"),
            (json!(["rsa/sign", "@config.salt", "sign me"]), "SALT_NAME", RSA_SIGNING_KEY, RSA_SIGNATURE_OF_SIGN_ME),
        ] {
        let schema = schema_for(call.clone());
        let go = |salt: Option<&'static str>| {
            let server = EmbeddedServer::new(schema.clone()).unwrap();
            let mut seen = Vec::new();
            let mut progress = server.start("Keys", go(None)).unwrap();
            let response = loop {
                match progress {
                    ServerProgress::Done(response) => break response,
                    ServerProgress::Await { flight, requests } => {
                        let replies = requests
                            .into_iter()
                            .map(|pending| IoReply {
                                id: pending.id,
                                answer: match &pending.request {
                                    IoRequest::ReadRow { .. } => IoAnswer::Row(Err("no row".to_string())),
                                    IoRequest::Secret { name, principal } => {
                                        seen.push((name.clone(), principal.clone()));
                                        IoAnswer::Secret(Ok(salt.map(Into::into)))
                                    }
                                    other => panic!("the fixture asks for {other:?}"),
                                },
                            })
                            .collect();
                        progress = server.resume(flight, replies).unwrap();
                    }
                    ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
                }
            };
            (response, seen)
        };
        let (held, seen) = go(Some(value));
        assert_eq!(seen, vec![(name.to_string(), None)], "{call}");
        let hashed = payload_of(&held, "OK");
        assert_eq!(hashed["hash"], expected, "{call}");
        assert_eq!(hashed["salt"], name, "the program sees the credential's name, never its value");
        assert!(!hashed.to_string().contains(value));
        let (absent, _) = go(None);
        assert_eq!(payload_of(&absent, "OK")["hash"], Json::Null, "{call}: no value held: the result is null");
        }
    }

    #[test]
    fn the_wire_round_trips_every_credential_store_ask_and_answer_and_hides_the_value() {
        use orbital_core_runtime::credential_admin::{RotateCounts, StoreListing, StoredName};
        use orbital_core_runtime::embed::wire::{IoAnswerWire, IoRequestWire};
        let requests = [
            IoRequest::CredentialStoreList,
            IoRequest::CredentialStoreRead { name: "n".into() },
            IoRequest::CredentialStoreWrite { service: "s".into(), name: "n".into(), value: "hunter2".into() },
            IoRequest::CredentialStoreRemove { name: "n".into() },
            IoRequest::CredentialStoreRotate,
            IoRequest::CredentialModeRead,
            IoRequest::CredentialModeWrite { mode: "mock".into() },
        ];
        for request in requests {
            let wire = serde_json::to_string(&IoRequestWire::from(&request)).unwrap();
            let back: IoRequest = serde_json::from_str::<IoRequestWire>(&wire).unwrap().into();
            assert_eq!(back, request, "{wire}");
        }
        assert!(!format!("{:?}", IoRequest::CredentialStoreWrite { service: "s".into(), name: "n".into(), value: "hunter2".into() }).contains("hunter2"));
        assert!(!format!("{:?}", IoAnswer::CredentialStoreRead(Ok(Some("hunter2".into())))).contains("hunter2"));
        let answers = [
            IoAnswer::CredentialStoreList(Ok(Some(StoreListing { enabled: true, entries: vec![StoredName { service: "s".into(), name: "n".into(), last4: Some("6789".into()) }] }))),
            IoAnswer::CredentialStoreList(Ok(None)),
            IoAnswer::CredentialStoreRead(Ok(Some("v".into()))),
            IoAnswer::CredentialStoreRead(Ok(None)),
            IoAnswer::CredentialStoreWrite(Ok(())),
            IoAnswer::CredentialStoreRemove(Ok(true)),
            IoAnswer::CredentialStoreRotate(Ok(RotateCounts { rotated: 1, already_current: 2, unreadable: 3 })),
            IoAnswer::CredentialModeRead(Ok(Some("real".into()))),
            IoAnswer::CredentialModeWrite(Err("refused".into())),
        ];
        for answer in answers {
            let wire = serde_json::to_value(IoAnswerWire::from(&answer)).unwrap();
            let back: IoAnswer = serde_json::from_value::<IoAnswerWire>(wire.clone()).unwrap().into();
            assert_eq!(back, answer, "{wire}");
        }
        let rotate = serde_json::to_value(IoAnswerWire::from(&IoAnswer::CredentialStoreRotate(Ok(RotateCounts { rotated: 1, already_current: 2, unreadable: 3 })))).unwrap();
        assert_eq!(rotate, json!({ "kind": "credentialStoreRotate", "result": { "ok": { "rotated": 1, "alreadyCurrent": 2, "unreadable": 3 } } }));
    }
}

mod os_effects {
    use super::*;
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};

    fn data(text: &str) -> IoAnswer {
        IoAnswer::Os(Ok(Value::object(orbital_core_base::value::ValueMap::from_iter([("data".into(), Value::bytes(text.as_bytes().to_vec()))])).into()))
    }

    #[test]
    fn an_os_effect_asks_the_host_once_and_emits_the_answer_as_the_success_payload() {
        let (response, seen) = run(
            json!(["os/read-file", "/data/a.txt", routes()]),
            go(None),
            |request| match request {
                IoRequest::Os { op: OsOp::ReadFile, .. } => data("hello"),
                other => panic!("the fixture asks for {other:?}"),
            },
        );
        assert_eq!(seen.len(), 1);
        let IoRequest::Os { op, args, key, site, .. } = &seen[0] else { panic!("an os ask") };
        assert_eq!(*op, OsOp::ReadFile);
        assert_eq!(serde_json::to_value(args).unwrap(), json!(["/data/a.txt"]));
        assert!(key.is_none() && site.is_none());
        assert_eq!(payload_of(&response, "OK"), json!({ "data": "aGVsbG8=" }));
    }

    #[test]
    fn a_host_refusal_routes_the_failure_with_error_and_kind() {
        let (response, _) = run(
            json!(["os/read-file", "/etc/shadow", routes()]),
            go(None),
            |_| IoAnswer::Os(Err(OsError::new(OsErrorKind::Denied, "no fs grant covers /etc/shadow"))),
        );
        assert_eq!(
            payload_of(&response, "BAD"),
            json!({ "error": "no fs grant covers /etc/shadow", "kind": "denied" })
        );
        assert!(emitted(&response).iter().all(|(event, _)| *event != "OK"));
    }

    #[test]
    fn an_os_argument_binding_is_resolved_before_the_ask() {
        let (_, seen) = run(
            json!(["os/read-file", "@payload.principal", routes()]),
            go(None),
            |_| data("x"),
        );
        let IoRequest::Os { args, .. } = &seen[0] else { panic!("an os ask") };
        assert_eq!(serde_json::to_value(args).unwrap(), json!(["evil"]));
    }

    fn fetch_server(effect: Json, egress: Json) -> EmbeddedServer {
        let mut schema = serde_json::to_value(schema_with(effect, json!({}))).unwrap();
        schema["egress"] = egress;
        EmbeddedServer::new(serde_json::from_value(schema).unwrap()).unwrap()
    }

    fn fetch(url: &str) -> Json {
        json!(["os/fetch", "GET", url, { "headers": [{ "name": "accept", "value": "application/json" }] }, routes()])
    }

    #[test]
    fn a_fetch_of_a_declared_origin_asks_the_host_for_the_method_url_and_request_and_routes_its_answer() {
        let answer = json!({ "status": 404, "headers": [{ "name": "content-type", "value": "text/plain" }], "body": "bm9wZQ==" });
        let (response, seen) = run_on(fetch_server(fetch("https://api.example.test/items"), json!(["https://api.example.test"])), go(None), |_| {
            IoAnswer::Os(Ok(serde_json::from_value(answer.clone()).unwrap()))
        });
        let [IoRequest::Os { op, args, .. }] = &seen[..] else { panic!("one os ask, saw {seen:?}") };
        assert_eq!(*op, OsOp::Fetch);
        assert_eq!(
            serde_json::to_value(args).unwrap(),
            json!(["GET", "https://api.example.test/items", { "headers": [{ "name": "accept", "value": "application/json" }] }])
        );
        assert_eq!(payload_of(&response, "OK"), json!({ "status": 404, "headers": [{ "name": "content-type", "value": "text/plain" }], "body": "bm9wZQ==" }), "a non-2xx status is a success answer");
    }

    #[test]
    fn a_fetch_of_an_origin_the_program_does_not_declare_is_denied_without_asking_the_host() {
        for egress in [json!([]), json!(["https://other.example.test"]), json!(["http://api.example.test"])] {
            let (response, seen) = run_on(fetch_server(fetch("https://api.example.test/items"), egress.clone()), go(None), |request| {
                panic!("an undeclared origin must not reach the host, asked {request:?}")
            });
            assert!(seen.is_empty(), "{egress}");
            assert_eq!(
                payload_of(&response, "BAD"),
                json!({ "error": "os/fetch: https://api.example.test/items is not an origin the program declares in egress", "kind": "denied" }),
                "{egress}"
            );
        }
    }

    #[test]
    fn an_effect_with_no_executor_fails_and_routes_its_declared_failure_naming_the_operator() {
        let (response, seen) = run(
            json!(["http", "GET", "https://example.com", "/", {}, routes()]),
            go(None),
            |request| panic!("a retired operator must not reach the host, asked {request:?}"),
        );
        assert!(seen.is_empty());
        let error = payload_of(&response, "BAD")["error"].as_str().unwrap().to_string();
        assert!(error.contains("no executor for effect `http`"), "{error}");
        assert!(emitted(&response).iter().all(|(event, _)| *event != "OK"));
    }

    #[test]
    fn an_effect_with_no_executor_and_no_route_still_fails_visibly_in_its_results() {
        let (response, _) = run(json!(["http", "GET", "https://example.com", "/", {}]), go(None), |_| data("x"));
        let rejection = response.rejections.iter().flatten().next().expect("an effect-failed rejection");
        assert_eq!(rejection.code, orbital_core_runtime::effects::response::TransitionRejectionCode::EffectFailed);
        let error = rejection.error.as_deref().unwrap_or_default();
        assert!(error.contains("no executor for effect `http`"), "{error}");
    }

    #[test]
    fn control_a_declared_no_op_log_still_passes_and_routes_nothing() {
        let (response, _) = run(json!(["log", "info", "hello"]), go(None), |_| data("x"));
        assert!(emitted(&response).is_empty());
        assert!(response.rejections.iter().flatten().next().is_none());
    }

    fn streamed_fetch(url: &str) -> Json {
        json!(["os/fetch", "GET", url, { "stream": true }, routes()])
    }

    #[test]
    fn a_streamed_fetch_of_a_declared_origin_reaches_the_host_raw_and_its_handle_answer_is_routed() {
        let head = json!({ "status": 200, "headers": [{ "name": "content-type", "value": "text/event-stream" }], "body": "", "handle": 3 });
        let (response, seen) = run_on(fetch_server(streamed_fetch("https://api.example.test/events"), json!(["https://api.example.test"])), go(None), |_| {
            IoAnswer::Os(Ok(serde_json::from_value(head.clone()).unwrap()))
        });
        let [IoRequest::Os { op, args, .. }] = &seen[..] else { panic!("one os ask, saw {seen:?}") };
        assert_eq!(*op, OsOp::Fetch);
        assert_eq!(serde_json::to_value(args).unwrap(), json!(["GET", "https://api.example.test/events", { "stream": true }]));
        assert_eq!(payload_of(&response, "OK"), json!({ "status": 200, "headers": [{ "name": "content-type", "value": "text/event-stream" }], "body": "", "handle": 3 }));
    }

    #[test]
    fn a_streamed_fetch_of_an_origin_the_program_does_not_declare_is_denied_without_asking_the_host() {
        let (response, seen) = run_on(fetch_server(streamed_fetch("https://api.example.test/events"), json!(["https://other.example.test"])), go(None), |request| {
            panic!("an undeclared origin must not reach the host, asked {request:?}")
        });
        assert!(seen.is_empty());
        assert_eq!(payload_of(&response, "BAD")["kind"], json!("denied"));
    }

    #[test]
    fn an_os_recv_on_a_handle_the_kernel_holds_no_tls_socket_for_is_the_hosts_to_answer() {
        let (response, seen) = run(json!(["os/recv", 3, 4096, routes()]), go(None), |_| {
            IoAnswer::Os(Ok(serde_json::from_value(json!({ "data": "YWJj", "eof": false })).unwrap()))
        });
        let [IoRequest::Os { op, args, .. }] = &seen[..] else { panic!("one os ask, saw {seen:?}") };
        assert_eq!(*op, OsOp::Recv);
        assert_eq!(serde_json::to_value(args).unwrap(), json!([3.0, 4096.0]));
        assert_eq!(payload_of(&response, "OK"), json!({ "data": "YWJj", "eof": false }));
    }

    #[cfg(target_os = "macos")]
    const MATH: &str = "/usr/lib/libSystem.B.dylib";
    #[cfg(not(target_os = "macos"))]
    const MATH: &str = "libm.so.6";

    fn ffi_server() -> EmbeddedServer {
        let grants = json!({ "ffi": [{ "library": MATH, "symbol": "cos", "signature": "(f64) -> f64" }] });
        EmbeddedServer::new(schema_with(json!(["ffi/call", MATH, "cos", "@payload.args", routes()]), grants)).unwrap()
    }

    fn ffi_go(args: Json) -> EventRequest {
        EventRequest {
            event: "GO".to_string(),
            target_trait: Some("Host".to_string()),
            payload: Some(serde_json::from_value(json!({ "args": args })).unwrap()),
            ..Default::default()
        }
    }

    #[test]
    fn an_ffi_call_with_the_wrong_arity_is_invalid_without_asking_the_host() {
        let (response, seen) = run_on(ffi_server(), ffi_go(json!([])), |request| panic!("a malformed call must not reach the host, asked {request:?}"));
        assert!(seen.is_empty());
        assert_eq!(payload_of(&response, "BAD")["kind"], json!("invalid"));
    }

    #[test]
    fn an_ffi_call_to_a_symbol_the_program_does_not_grant_is_denied_without_asking_the_host() {
        let grants = json!({ "ffi": [{ "library": MATH, "symbol": "sin", "signature": "(f64) -> f64" }] });
        let server = EmbeddedServer::new(schema_with(json!(["ffi/call", MATH, "cos", "@payload.args", routes()]), grants)).unwrap();
        let (response, seen) = run_on(server, ffi_go(json!([0.0])), |request| panic!("an ungranted call must not reach the host, asked {request:?}"));
        assert!(seen.is_empty());
        assert_eq!(payload_of(&response, "BAD")["kind"], json!("denied"));
    }

    #[test]
    fn an_ffi_call_asks_the_host_with_the_granted_signature_and_plain_values() {
        let (response, seen) = run_on(ffi_server(), ffi_go(json!([0.0])), |_| {
            IoAnswer::Os(Ok(serde_json::from_value(json!({ "value": 1.0, "buffers": [] })).unwrap()))
        });
        let [IoRequest::Os { op, args, .. }] = &seen[..] else { panic!("one os ask, saw {seen:?}") };
        assert_eq!(*op, OsOp::FfiCall);
        assert_eq!(serde_json::to_value(args).unwrap(), json!([MATH, "cos", "(f64) -> f64", [0.0]]));
        assert_eq!(payload_of(&response, "OK"), json!({ "value": 1.0, "buffers": [] }));
    }

    #[cfg(feature = "ffi")]
    #[test]
    fn an_ffi_call_runs_the_granted_c_function_through_the_native_host() {
        use orbital_core_runtime::os_host::{answer_os, OsHandles, OsHost};
        let server = ffi_server();
        let grants = server.kernel().os_grants();
        let handles = OsHandles::default();
        let on_event = |_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {};
        let (response, _) = run_on(server, ffi_go(json!([0.0])), |request| {
            answer_os(Some(&OsHost { grants: &grants, handles: &handles, on_event: &on_event, secrets: None }), request)
        });
        assert_eq!(payload_of(&response, "OK")["value"], json!(1.0));
    }

    #[test]
    fn a_fetch_url_that_is_not_text_is_invalid_without_asking_the_host() {
        let (response, seen) = run_on(
            fetch_server(json!(["os/fetch", "GET", 7, routes()]), json!(["https://api.example.test"])),
            go(None),
            |request| panic!("a malformed fetch must not reach the host, asked {request:?}"),
        );
        assert!(seen.is_empty());
        assert_eq!(payload_of(&response, "BAD")["kind"], json!("invalid"));
    }

    #[test]
    fn a_stream_with_no_key_fails_before_any_ask() {
        let (response, seen) = run(
            json!(["os/watch-env", "MODE", routes()]),
            go(None),
            |request| panic!("a keyless stream must not ask the host, asked {request:?}"),
        );
        assert!(seen.is_empty());
        assert_eq!(
            payload_of(&response, "BAD"),
            json!({ "error": "os/watch-env: a stream needs a key", "kind": "invalid" })
        );
    }

    #[test]
    fn plain_stdin_reads_once_without_a_stream_site_and_routes_data_and_eof() {
        for payload in [json!({ "data": "zLE=", "eof": false }), json!({ "data": "", "eof": true })] {
            let (response, seen) = run(
                json!(["os/stdin-read", 2, routes()]),
                go(None),
                |_| IoAnswer::Os(Ok(serde_json::from_value(payload.clone()).unwrap())),
            );
            assert_eq!(seen.len(), 1);
            let IoRequest::Os { op, args, key, site, .. } = &seen[0] else { panic!("an os ask") };
            assert_eq!(*op, OsOp::StdinRead);
            assert_eq!(args, &vec![orbital_core_base::value::Value::Number(2.0)]);
            assert!(key.is_none() && site.is_none(), "plain stdin is one bounded read");
            assert_eq!(payload_of(&response, "OK"), payload);
            assert!(emitted(&response).iter().all(|(event, _)| *event != "BAD"));
        }
    }

    #[test]
    fn a_keyed_stream_asks_with_its_key_and_call_site_and_a_single_answer_does_not() {
        let (response, seen) = run(
            json!(["os/stdin-read", { "key": "in", "emit": { "success": "OK", "failure": "BAD", "onMessage": "BAD" } }]),
            go(None),
            |_| IoAnswer::Os(Ok(serde_json::from_value(json!({ "eof": true })).unwrap())),
        );
        let IoRequest::Os { op, key, site, .. } = &seen[0] else { panic!("an os ask") };
        assert_eq!(*op, OsOp::StdinRead);
        assert_eq!(key.as_deref().map(program_call_key), Some("in"));
        let site = site.as_ref().expect("a stream carries its call site");
        assert_eq!(site.trait_name, "Host");
        assert_eq!(site.emit.on_message.as_deref(), Some("BAD"));
        assert_eq!(payload_of(&response, "OK"), json!({ "eof": true, "key": "in" }));
    }

    #[test]
    fn the_wire_round_trips_an_os_ask_and_its_answers() {
        use orbital_core_runtime::embed::wire::{IoAnswerWire, IoRequestWire};
        let request = IoRequest::Os {
            op: OsOp::TcpConnect,
            args: vec![Value::string("db.internal"), Value::Number(5432.0)],
            key: Some("k".into()),
            site: None, principal: None, orbital: None
        };
        let wire = serde_json::to_value(IoRequestWire::from(&request)).unwrap();
        assert_eq!(
            wire,
            json!({ "kind": "os", "op": "os/tcp-connect", "args": ["db.internal", 5432.0], "key": "k" })
        );
        let back: IoRequest = serde_json::from_value::<IoRequestWire>(wire).unwrap().into();
        assert_eq!(back, request);
        for answer in [
            IoAnswer::Os(Ok(serde_json::from_value(json!({ "sock": 7 })).unwrap())),
            IoAnswer::Os(Err(OsError::new(OsErrorKind::NotFound, "no route"))),
        ] {
            let wire = serde_json::to_value(IoAnswerWire::from(&answer)).unwrap();
            let back: IoAnswer = serde_json::from_value::<IoAnswerWire>(wire).unwrap().into();
            assert_eq!(back, answer);
        }
        let failure = serde_json::to_value(IoAnswerWire::from(&IoAnswer::Os(Err(OsError::new(
            OsErrorKind::NotFound,
            "m",
        )))))
        .unwrap();
        assert_eq!(failure, json!({ "kind": "os", "result": { "error": { "kind": "not-found", "message": "m" } } }));
    }

    #[test]
    fn an_unknown_op_or_error_kind_on_the_wire_is_refused() {
        use orbital_core_runtime::embed::wire::{IoAnswerWire, IoRequestWire};
        assert!(serde_json::from_value::<IoRequestWire>(json!({ "kind": "os", "op": "os/fork", "args": [] })).is_err());
        assert!(serde_json::from_value::<IoRequestWire>(json!({ "kind": "os", "op": "os/path-join", "args": [] })).is_err());
        assert!(serde_json::from_value::<IoAnswerWire>(
            json!({ "kind": "os", "result": { "error": { "kind": "broken", "message": "m" } } })
        )
        .is_err());
    }

    #[test]
    fn an_os_answer_is_redacted_in_debug_but_its_failure_is_not() {
        let ok = format!("{:?}", data("hunter2"));
        assert!(!ok.contains("hunter2") && ok.contains("redacted"), "{ok}");
        let failure = format!("{:?}", IoAnswer::Os(Err(OsError::new(OsErrorKind::Io, "disk full"))));
        assert!(failure.contains("disk full"), "{failure}");
    }
}

mod lowering {
    use orbital_core_base::ir::{OirEffect, OirExpr};
    use orbital_core_base::schema::SExpression;
    use serde_json::{json, Value as Json};

    fn lower(form: Json) -> Result<Box<OirEffect>, String> {
        let form: SExpression = serde_json::from_value(form).unwrap();
        match orbital_core_eval::lower::lower_sexpr(&form) {
            Ok(OirExpr::Effect { effect }) => Ok(effect),
            Ok(other) => Err(format!("not an effect: {other:?}")),
            Err(e) => Err(e.to_string()),
        }
    }

    #[test]
    fn each_form_lowers_to_its_own_effect_with_the_declared_routes() {
        let routes = json!({ "emit": { "success": "OK", "failure": "BAD" } });
        let OirEffect::RandomBytes { emit, .. } =
            *lower(json!(["random-bytes", 16, routes.clone()])).unwrap()
        else {
            panic!("a random-bytes effect")
        };
        assert_eq!(
            emit.as_ref().and_then(|e| e.failure.clone()),
            Some("BAD".to_string())
        );
        let status = lower(json!(["credential-status", "@config.tok", routes.clone()])).unwrap();
        assert!(matches!(*status, OirEffect::CredentialStatus { .. }));
        let forget = lower(json!(["credential-forget", "@config.tok"])).unwrap();
        assert!(matches!(
            *forget,
            OirEffect::CredentialForget { emit: None, .. }
        ));
        assert_eq!(forget.kind(), "credential-forget");
        let check = lower(json!(["access-check", { "entity": "Note", "action": "read", "row": {}, "identity": "@payload.who" }, routes])).unwrap();
        assert!(matches!(&*check, OirEffect::AccessCheck { emit: Some(_), .. }));
        assert_eq!(check.kind(), "access-check");
        assert!(!OirExpr::effect(*check).collect_bindings().is_empty(), "the request's bindings are collected");
    }

    #[test]
    fn the_wrong_arity_and_a_non_object_option_do_not_lower() {
        for form in [
            json!(["random-bytes"]),
            json!(["random-bytes", 1, {}, {}]),
            json!(["credential-status"]),
            json!(["credential-forget", "a", "not-an-object"]),
            json!(["access-check"]),
            json!(["access-check", {}, "not-an-object"]),
        ] {
            assert!(lower(form.clone()).is_err(), "{form}");
        }
    }

    #[test]
    fn the_argument_binding_is_collected_so_the_checks_see_the_secret_knob_read() {
        let effect = lower(json!(["credential-status", "@config.tok"])).unwrap();
        assert!(!OirExpr::effect(*effect).collect_bindings().is_empty());
    }

    #[test]
    fn one_os_effect_per_family_lowers_to_the_generic_os_effect() {
        use orbital_core_base::os::OsOp;
        let routes = json!({ "key": "k", "emit": { "success": "OK", "failure": "BAD" } });
        let read = lower(json!(["os/read-file", "/data/a", routes.clone()])).unwrap();
        assert_eq!(read.kind(), "os/read-file");
        let OirEffect::Os { op, args, key, emit } = *read else { panic!("an os effect") };
        assert_eq!((op, args.len()), (OsOp::ReadFile, 1));
        assert!(key.is_some());
        assert_eq!(emit.and_then(|e| e.failure), Some("BAD".to_string()));

        let spawn = lower(json!(["os/spawn", "git", ["status"], { "cwd": "/repo", "stdin": "null" }, routes.clone()]))
            .unwrap();
        let OirEffect::Os { op, args, emit, .. } = *spawn else { panic!("an os effect") };
        assert_eq!((op, args.len()), (OsOp::Spawn, 3));
        assert!(emit.is_some());

        let connect = lower(json!(["os/tcp-connect", "db.internal", 5432])).unwrap();
        let OirEffect::Os { op, args, key, emit } = *connect else { panic!("an os effect") };
        assert_eq!((op, args.len()), (OsOp::TcpConnect, 2));
        assert!(key.is_none() && emit.is_none());

        let stdin = lower(json!(["os/stdin-read", { "key": "in", "emit": { "onMessage": "DATA" } }])).unwrap();
        let OirEffect::Os { op, args, key, emit } = *stdin else { panic!("an os effect") };
        assert_eq!((op, args.len()), (OsOp::StdinRead, 0));
        assert!(key.is_some());
        assert_eq!(emit.and_then(|e| e.on_message), Some("DATA".to_string()));
    }

    #[test]
    fn an_os_effect_with_the_wrong_argument_count_or_a_non_object_envelope_does_not_lower() {
        for form in [
            json!(["os/read-file"]),
            json!(["os/read-file", "a", "utf8", "extra"]),
            json!(["os/cwd", "unexpected"]),
            json!(["os/tcp-connect", "host"]),
            json!(["os/read-file", "a", "b", "c", {}]),
        ] {
            assert!(lower(form.clone()).is_err(), "{form}");
        }
    }

    #[test]
    fn the_os_argument_bindings_are_collected_and_the_pure_path_ops_are_not_effects() {
        let effect = lower(json!(["os/read-file", "@payload.path"])).unwrap();
        assert!(!OirExpr::effect(*effect).collect_bindings().is_empty());
        let pure = lower(json!(["os/path-join", "a", "b"]));
        assert!(pure.is_err(), "{pure:?}");
    }

    #[test]
    fn every_os_effect_runs_on_the_server_kv_on_either_side_and_a_pure_path_op_anywhere() {
        use orbital_core_base::runs_on::{runs_on, RunsOn};
        use orbital_core_base::os::OsOp;
        for op in OsOp::ALL {
            let site = if op.is_kv() || op.runs_on_any_host() { RunsOn::Any } else { RunsOn::Server };
            assert_eq!(runs_on(op.name()), site, "{}: a kv store, os/fetch and the os/recv and os/close that read and release a streamed fetch handle (the table's runsOn is any: a browser host answers them, a native host answers sockets) are reachable from both sides, every other os/* effect runs on the server", op.name());
        }
        assert_eq!(runs_on("os/path-join"), RunsOn::Any);
        assert_eq!(runs_on("os/fork"), RunsOn::Any);
    }

    #[test]
    fn each_os_effect_names_its_success_payload_and_one_failure_shape() {
        use orbital_core_base::emit_contract::{failure_payload, message_payload, success_payload, CallVariant, ProducedPayload};
        use orbital_core_base::os::OsOp;
        for op in OsOp::ALL {
            assert!(
                matches!(failure_payload(op.name(), CallVariant::Plain, false), ProducedPayload::Shape(shape) if shape.len() == 2),
                "{}",
                op.name()
            );
        }
        assert!(matches!(
            success_payload("os/read-file", CallVariant::Plain),
            ProducedPayload::Shape(shape) if shape.iter().map(|f| f.name).collect::<Vec<_>>() == ["data", "key"]
        ));
        assert!(matches!(
            message_payload("os/stdin-read"),
            ProducedPayload::Shape(shape) if shape.iter().map(|f| f.name).collect::<Vec<_>>() == ["data", "key"]
        ));
        assert_eq!(message_payload("os/read-file"), ProducedPayload::Opaque);
        assert_eq!(success_payload("os/cpu-usage", CallVariant::Plain), ProducedPayload::Opaque);
        assert_eq!(success_payload("os/path-join", CallVariant::Plain), ProducedPayload::Opaque);
    }
}


mod os_deferred {
    use super::*;
    use orbital_core_base::os::OsOp;

    fn keyed_wait() -> Json {
        json!(["os/wait", 7, { "key": "k", "emit": { "success": "OK", "failure": "BAD" } }])
    }

    #[test]
    fn a_keyed_wait_answered_deferred_commits_the_step_and_fires_no_route() {
        let (response, seen) = run(keyed_wait(), go(None), |_| IoAnswer::ServiceDeferred);
        assert_eq!(seen.len(), 1);
        let IoRequest::Os { op, key, site, .. } = &seen[0] else { panic!("an os ask") };
        assert_eq!(*op, OsOp::Wait);
        assert_eq!(key.as_deref().map(program_call_key), Some("k"), "the host's key carries the program's");
        let site = site.as_ref().expect("a deferrable wait carries its call site");
        assert_eq!(site.trait_name, "Host");
        assert_eq!(site.emit.success.as_deref(), Some("OK"));
        assert_eq!(site.emit.failure.as_deref(), Some("BAD"));
        assert!(response.success);
        assert!(emitted(&response).iter().all(|(event, _)| *event != "OK" && *event != "BAD"), "{:?}", emitted(&response));
    }

    fn host_key_of(seen: &[IoRequest]) -> String {
        let IoRequest::Os { key: Some(key), .. } = &seen[0] else { panic!("a keyed os ask") };
        key.clone()
    }

    /// EXEC-QUEUE-10: the host keeps one pending completion per key, so the key it is handed names who asked: the same
    /// program key from two sessions (two viewers' std-http-client reads, both `key: "read"`) never collides.
    #[test]
    fn the_same_program_key_reaches_the_host_under_a_distinct_key_per_session() {
        let (_, program) = run(keyed_wait(), go(None), |_| IoAnswer::ServiceDeferred);
        let (_, viewer) = run(keyed_wait(), go(signed_in()), |_| IoAnswer::ServiceDeferred);
        let (program, viewer) = (host_key_of(&program), host_key_of(&viewer));
        assert_ne!(program, viewer);
        assert_ne!(program, "k", "the bare program key would collide across callers");
        assert_eq!((program_call_key(&program), program_call_key(&viewer)), ("k", "k"));
    }

    #[test]
    fn a_completion_under_the_host_key_fires_with_the_program_key() {
        let (_, seen) = run(keyed_wait(), go(None), |_| IoAnswer::ServiceDeferred);
        let IoRequest::Os { key: Some(key), site: Some(site), .. } = &seen[0] else { panic!("a keyed, routed os ask") };
        let (event, payload) = completion_event(site, key, Ok(serde_json::from_value(json!({ "code": 0 })).unwrap())).expect("a routed outcome");
        assert_eq!(event, "OK");
        assert_eq!(payload.get_path("key").and_then(|k| k.as_str().map(str::to_string)).as_deref(), Some("k"));
    }

    #[test]
    fn control_the_same_keyed_wait_answered_with_a_result_fires_the_success_route_with_its_key() {
        let (response, _) = run(keyed_wait(), go(None), |_| {
            IoAnswer::Os(Ok(serde_json::from_value(json!({ "code": 0 })).unwrap()))
        });
        assert_eq!(payload_of(&response, "OK"), json!({ "code": 0, "key": "k" }));
    }

    #[test]
    fn an_unkeyed_wait_answered_deferred_is_a_refused_resume_not_a_silent_success() {
        let server = EmbeddedServer::new(schema(json!(["os/wait", 7, routes()]))).unwrap();
        let mut progress = server.start("Main", go(None)).unwrap();
        loop {
            let ServerProgress::Await { flight, requests } = progress else { panic!("the wait never asked") };
            let asked_os = requests.iter().any(|pending| matches!(pending.request, IoRequest::Os { .. }));
            if asked_os {
                assert!(requests.iter().all(|pending| !matches!(&pending.request, IoRequest::Os { key: Some(_), .. })));
            }
            let replies = requests
                .into_iter()
                .map(|pending| IoReply {
                    id: pending.id,
                    answer: match pending.request {
                        IoRequest::Os { .. } => IoAnswer::ServiceDeferred,
                        _ => IoAnswer::Row(Err("no row".to_string())),
                    },
                })
                .collect();
            let resumed = server.resume(flight, replies);
            if asked_os {
                assert!(resumed.is_err(), "an unkeyed ask has nothing to complete into");
                return;
            }
            progress = resumed.unwrap();
        }
    }
}

#[cfg(unix)]
mod os_completion {
    use super::*;
    use orbital_core_base::effect_types::RuntimeEnvironment;
    use orbital_core_base::os::OsOp;
    use orbital_core_base::os_grants::OsGrants;
    use orbital_core_runtime::config::KernelConfig;
    use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
    use orbital_core_runtime::kernel::RuntimeKernel;
    use orbital_core_runtime::os_host::completion::{CompletionAnswerer, CompletionDone, KernelCompletionSink};
    use orbital_core_runtime::os_host::{answer_os, OsHandles, OsHost};
    use orbital_core_runtime::resumable::{BlockingHost, CallSite, DispatchError, DispatchOutcomes};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    fn supervisor(child_script: &str) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "code", "type": "number" },
                    { "name": "key", "type": "string" }
                ] },
                "traits": [{ "name": "Sup", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true }, { "name": "spawning" },
                            { "name": "waiting" }, { "name": "recorded" }, { "name": "failed" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "spawning", "event": "GO", "effects": [
                                ["os/spawn", "/bin/sh", ["-c", child_script], { "stdin": "null" },
                                    { "emit": { "success": "SPAWNED", "failure": "SPAWN_BAD" } }]
                            ] },
                            { "from": "spawning", "to": "waiting", "event": "SPAWNED", "effects": [
                                ["os/wait", "@payload.pid", { "key": "child",
                                    "emit": { "success": "CHILD_EXITED", "failure": "CHILD_WAIT_FAILED" } }]
                            ] },
                            { "from": "waiting", "to": "recorded", "event": "CHILD_EXITED", "effects": [
                                ["set", "@entity.code", "@payload.code"],
                                ["set", "@entity.key", "@payload.key"]
                            ] },
                            { "from": "waiting", "to": "failed", "event": "CHILD_WAIT_FAILED" }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    struct Wired {
        kernel: Arc<RuntimeKernel>,
        sink: Arc<KernelCompletionSink>,
        handles: Arc<OsHandles>,
        done: mpsc::Receiver<(String, Result<usize, String>)>,
    }

    fn wired(child_script: &str) -> Wired {
        wired_with(supervisor(child_script))
    }

    fn wired_with(schema: OrbitalSchema) -> Wired {
        wired_granted(schema, OsGrants { exec: vec!["/bin/sh".to_string()], ..OsGrants::default() })
    }

    fn wired_granted(schema: OrbitalSchema, grants: OsGrants) -> Wired {
        let store: Arc<dyn EntitySource> = Arc::new(InMemoryEntityCache::new());
        let kernel = Arc::new(RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), KernelConfig::default()));
        kernel.register_schema(schema).unwrap();
        let handles = Arc::new(OsHandles::default());
        let grants = Arc::new(grants);
        let answer: Arc<CompletionAnswerer> = {
            let (handles, grants) = (handles.clone(), grants.clone());
            Arc::new(move |request: &IoRequest| match request {
                IoRequest::Os { .. } => {
                    let on_event = |_: Option<&CallSite>, _: Value| {};
                    answer_os(Some(&OsHost { grants: &grants, handles: &handles, on_event: &on_event, secrets: None }), request)
                }
                other => BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(other),
            })
        };
        let (tx, done) = mpsc::channel();
        let tx = Mutex::new(tx);
        let on_done: Arc<CompletionDone> = Arc::new(
            move |_site: &CallSite, event: &str, _: &orbital_core_base::value::Value, _: Option<&str>, result: Result<DispatchOutcomes, DispatchError>| {
                let summary = result.map(|outcomes| outcomes.len()).map_err(|error| error.to_string());
                let _ = tx.lock().unwrap().send((event.to_string(), summary));
            },
        );
        let sink = Arc::new(KernelCompletionSink::new(kernel.clone(), answer, on_done));
        handles.completions.set_sink(sink.clone());
        Wired { kernel, sink, handles, done }
    }

    fn getenv_reply(name: &str, key: Option<&str>) -> (String, serde_json::Value) {
        let options = match key {
            Some(key) => json!({ "key": key, "emit": { "success": "ENV_OK", "failure": "ENV_BAD" } }),
            None => json!({ "emit": { "success": "ENV_OK", "failure": "ENV_BAD" } }),
        };
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Sup", "linkedEntity": "Job", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "ok" }, { "name": "bad" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "GO", "effects": [["os/getenv", name, options]] },
                        { "from": "idle", "to": "ok", "event": "ENV_OK", "effects": [["set", "@entity.got", "@payload"]] },
                        { "from": "idle", "to": "bad", "event": "ENV_BAD", "effects": [["set", "@entity.got", "@payload"]] }
                    ]
                } }]
            }]
        }))
        .unwrap();
        let store: Arc<dyn EntitySource> = Arc::new(InMemoryEntityCache::new());
        let kernel = Arc::new(RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), KernelConfig::default()));
        kernel.register_schema(schema).unwrap();
        let handles = Arc::new(OsHandles::default());
        let grants = Arc::new(OsGrants { env: vec!["HOME".to_string()], ..OsGrants::default() });
        let answer: Arc<CompletionAnswerer> = Arc::new(move |request: &IoRequest| match request {
            IoRequest::Os { .. } => {
                let on_event = |_: Option<&CallSite>, _: Value| {};
                answer_os(Some(&OsHost { grants: &grants, handles: &handles, on_event: &on_event, secrets: None }), request)
            }
            other => BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(other),
        });
        let on_done: Arc<CompletionDone> = Arc::new(|_: &CallSite, _: &str, _: &orbital_core_base::value::Value, _: Option<&str>, _: Result<DispatchOutcomes, DispatchError>| {});
        let sink = Arc::new(KernelCompletionSink::new(kernel.clone(), answer, on_done));
        let progress = kernel.start_dispatch("Sup", None, "GO", Value::Null, None, None).unwrap();
        sink.drive(progress).unwrap();
        let held = kernel.snapshot_trait("Sup", None);
        (held.state, serde_json::to_value(&held.entity).unwrap()["got"].clone())
    }

    #[test]
    fn an_os_answer_given_in_place_carries_its_key_on_success_and_failure() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(getenv_reply("HOME", Some("k1")), ("ok".to_string(), json!({ "value": home, "key": "k1" })));
        let (state, got) = getenv_reply("PATH", Some("k2"));
        assert_eq!((state.as_str(), &got["key"], &got["kind"]), ("bad", &json!("k2"), &json!("denied")));
    }

    #[test]
    fn control_an_unkeyed_os_answer_carries_no_key() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(getenv_reply("HOME", None), ("ok".to_string(), json!({ "value": home })));
        let (_, got) = getenv_reply("PATH", None);
        assert_eq!(got.get("key"), None);
    }

    fn go_sup(wired: &Wired) {
        let progress = wired.kernel.start_dispatch("Sup", None, "GO", Value::Null, None, None).unwrap();
        wired.sink.drive(progress).unwrap();
    }

    fn reader() -> OrbitalSchema {
        let routes = |ok: &str| json!({ "success": ok, "failure": "READ_BAD" });
        serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "out", "type": "string" },
                    { "name": "err", "type": "string" }
                ] },
                "traits": [{ "name": "Sup", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "spawning" }, { "name": "reading" }],
                        "transitions": [
                            { "from": "idle", "to": "spawning", "event": "GO", "effects": [
                                ["os/spawn", "/bin/sh", ["-c", "printf ERR >&2; sleep 1; printf OUT"],
                                    { "stdin": "null", "stdout": "pipe", "stderr": "pipe" },
                                    { "emit": { "success": "SPAWNED", "failure": "SPAWN_BAD" } }]
                            ] },
                            { "from": "spawning", "to": "reading", "event": "SPAWNED", "effects": [
                                ["os/read", "@payload.stdout", 4096, { "key": "out", "emit": routes("OUT_DONE") }],
                                ["os/read", "@payload.stderr", 4096, { "key": "err", "emit": routes("ERR_DONE") }]
                            ] },
                            { "from": "reading", "to": "reading", "event": "OUT_DONE", "effects": [
                                ["set", "@entity.out", "@payload.data"]
                            ] },
                            { "from": "reading", "to": "reading", "event": "ERR_DONE", "effects": [
                                ["set", "@entity.err", "@payload.data"]
                            ] }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn two_keyed_reads_on_two_pipes_are_outstanding_together_and_both_complete() {
        let wired = wired_with(reader());
        let started = std::time::Instant::now();
        go_sup(&wired);
        assert!(started.elapsed() < Duration::from_millis(700), "the dispatch blocked on a pipe read: {:?}", started.elapsed());
        assert_eq!(wired.kernel.snapshot_trait("Sup", None).state, "reading");
        let (first, _) = wired.done.recv_timeout(Duration::from_secs(10)).expect("the stderr read completes while stdout is still empty");
        assert_eq!(first, "ERR_DONE");
        let (second, _) = wired.done.recv_timeout(Duration::from_secs(10)).expect("the stdout read completes later");
        assert_eq!(second, "OUT_DONE");
        let held = wired.kernel.snapshot_trait("Sup", None);
        assert!(held.entity.get_path("err").is_some_and(|v| !v.is_null()), "{:?}", held.entity);
        assert!(held.entity.get_path("out").is_some_and(|v| !v.is_null()), "{:?}", held.entity);
    }

    #[test]
    fn control_an_unkeyed_read_still_answers_in_place() {
        let handles = OsHandles::default();
        let grants = OsGrants { exec: vec!["/bin/sh".to_string()], ..OsGrants::default() };
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let host = OsHost { grants: &grants, handles: &handles, on_event: &on_event, secrets: None };
        let spawn = IoRequest::Os {
            op: OsOp::Spawn,
            args: vec![
                Value::string("/bin/sh"),
                serde_json::from_value(json!(["-c", "printf hi"])).unwrap(),
                serde_json::from_value(json!({ "stdin": "null", "stdout": "pipe", "stderr": "null" })).unwrap(),
            ],
            key: None,
            site: None, principal: None, orbital: None
        };
        let IoAnswer::Os(Ok(spawned)) = answer_os(Some(&host), &spawn) else { panic!("spawn") };
        let fd = spawned.expose().get_path("stdout").cloned().expect("a stdout fd");
        let read = IoRequest::Os { op: OsOp::Read, args: vec![fd, Value::Number(16.0)], key: None, site: None, principal: None, orbital: None };
        assert!(matches!(answer_os(Some(&host), &read), IoAnswer::Os(Ok(_))));
    }

    fn file_reader(path: &str) -> OrbitalSchema {
        let routes = |ok: &str| json!({ "success": ok, "failure": "BAD" });
        let keyed = |key: &str, ok: &str| json!({ "key": key, "emit": routes(ok) });
        serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "data", "type": "string" },
                    { "name": "eof", "type": "boolean" },
                    { "name": "key", "type": "string" }
                ] },
                "traits": [{ "name": "Sup", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true }, { "name": "opening" }, { "name": "reading" },
                            { "name": "read" }, { "name": "slurping" }, { "name": "slurped" }, { "name": "bad" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "opening", "event": "GO", "effects": [
                                ["os/open", path, "r", { "emit": routes("OPENED") }]
                            ] },
                            { "from": "opening", "to": "reading", "event": "OPENED", "effects": [
                                ["os/read", "@payload.fd", 64, keyed("f1", "READ_DONE")]
                            ] },
                            { "from": "reading", "to": "read", "event": "READ_DONE", "effects": [
                                ["set", "@entity.data", "@payload.data"],
                                ["set", "@entity.eof", "@payload.eof"],
                                ["set", "@entity.key", "@payload.key"]
                            ] },
                            { "from": "idle", "to": "slurping", "event": "SLURP", "effects": [
                                ["os/read-file", path, keyed("f2", "SLURPED")]
                            ] },
                            { "from": "idle", "to": "slurping", "event": "SLURP_NOW", "effects": [
                                ["os/read-file", path, { "emit": routes("SLURPED") }]
                            ] },
                            { "from": "slurping", "to": "slurped", "event": "SLURPED", "effects": [
                                ["set", "@entity.data", "@payload.data"],
                                ["set", "@entity.key", "@payload.key"]
                            ] },
                            { "from": "opening", "to": "bad", "event": "BAD" },
                            { "from": "reading", "to": "bad", "event": "BAD" },
                            { "from": "slurping", "to": "bad", "event": "BAD" }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    fn wired_file(dir: &std::path::Path) -> (Wired, String) {
        let dir = dir.canonicalize().unwrap();
        let path = dir.join("deferred.txt");
        std::fs::write(&path, b"hello").unwrap();
        let path = path.to_str().unwrap().to_string();
        let grants = OsGrants {
            fs: vec![orbital_core_base::os_grants::FsGrant {
                glob: format!("{}/**", dir.to_str().unwrap()),
                modes: vec![orbital_core_base::os_grants::FsMode::Read],
            }],
            ..OsGrants::default()
        };
        (wired_granted(file_reader(&path), grants), path)
    }

    #[test]
    fn a_keyed_routed_read_of_a_regular_file_is_delivered_as_its_own_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let (wired, _) = wired_file(dir.path());
        go_sup(&wired);
        assert_eq!(wired.kernel.snapshot_trait("Sup", None).state, "reading", "the GO dispatch does not take the read in place");
        let (event, summary) = wired.done.recv_timeout(Duration::from_secs(10)).expect("one completion dispatch");
        assert_eq!(event, "READ_DONE");
        assert!(summary.is_ok(), "{summary:?}");
        let held = wired.kernel.snapshot_trait("Sup", None);
        assert_eq!(held.state, "read");
        assert_eq!(held.entity.get_path("data"), Some(&Value::bytes(b"hello".to_vec())));
        assert_eq!(held.entity.get_path("eof"), Some(&Value::Bool(false)));
        assert_eq!(held.entity.get_path("key"), Some(&Value::string("f1")));
        assert!(wired.done.recv_timeout(Duration::from_millis(300)).is_err(), "exactly one completion");
    }

    #[test]
    fn control_an_unkeyed_read_file_is_answered_inline() {
        let dir = tempfile::tempdir().unwrap();
        let (wired, _) = wired_file(dir.path());
        let progress = wired.kernel.start_dispatch("Sup", None, "SLURP_NOW", Value::Null, None, None).unwrap();
        wired.sink.drive(progress).unwrap();
        let held = wired.kernel.snapshot_trait("Sup", None);
        assert_eq!(held.state, "slurped", "answered within the SLURP_NOW dispatch");
        assert_eq!(held.entity.get_path("data"), Some(&Value::bytes(b"hello".to_vec())));
        assert!(wired.done.recv_timeout(Duration::from_millis(300)).is_err(), "no completion dispatch without a key");
    }

    #[test]
    fn a_keyed_routed_read_file_is_delivered_as_its_own_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let (wired, _) = wired_file(dir.path());
        let progress = wired.kernel.start_dispatch("Sup", None, "SLURP", Value::Null, None, None).unwrap();
        wired.sink.drive(progress).unwrap();
        assert_eq!(wired.kernel.snapshot_trait("Sup", None).state, "slurping", "the SLURP dispatch does not take the read in place");
        let (event, summary) = wired.done.recv_timeout(Duration::from_secs(10)).expect("one completion dispatch");
        assert_eq!(event, "SLURPED");
        assert!(summary.is_ok(), "{summary:?}");
        let held = wired.kernel.snapshot_trait("Sup", None);
        assert_eq!(held.state, "slurped");
        assert_eq!(held.entity.get_path("data"), Some(&Value::bytes(b"hello".to_vec())));
        assert_eq!(held.entity.get_path("key"), Some(&Value::string("f2")));
        assert!(wired.done.recv_timeout(Duration::from_millis(300)).is_err(), "exactly one completion");
    }

    #[test]
    fn a_deferred_wait_dispatches_the_child_exit_through_the_kernel_sink_into_a_second_transition() {
        let wired = wired("exit 3");
        go_sup(&wired);
        let (event, summary) = wired.done.recv_timeout(Duration::from_secs(10)).expect("one completion dispatch");
        assert_eq!(event, "CHILD_EXITED");
        assert!(summary.is_ok(), "{summary:?}");
        let held = wired.kernel.snapshot_trait("Sup", None);
        assert_eq!(held.state, "recorded");
        assert_eq!(held.entity.get_path("code"), Some(&Value::Number(3.0)));
        assert_eq!(held.entity.get_path("key"), Some(&Value::string("child")));
        assert!(wired.done.recv_timeout(Duration::from_millis(300)).is_err(), "exactly one completion");
    }

    #[test]
    fn cancelling_the_key_before_the_child_ends_dispatches_no_completion_and_the_trait_keeps_waiting() {
        let wired = wired("sleep 30");
        go_sup(&wired);
        assert_eq!(wired.kernel.snapshot_trait("Sup", None).state, "waiting");
        assert!(wired
            .handles
            .cancel_completion(&orbital_core_runtime::resumable::host_call_key(
                None, "Sup", "", "child"
            )));
        wired.handles.close_all();
        assert!(wired.done.recv_timeout(Duration::from_millis(500)).is_err(), "a cancelled key completed");
        assert_eq!(wired.kernel.snapshot_trait("Sup", None).state, "waiting");
    }

    const CHAIN_LIMIT: usize = 2100;

    fn stat_chain(path: &str, key: Option<&str>, limit: usize) -> OrbitalSchema {
        let mut options = json!({ "emit": { "success": "STEP", "failure": "BAD" } });
        if let Some(key) = key {
            options["key"] = json!(key);
        }
        let ask = json!(["os/stat", path, options]);
        serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "n", "type": "number", "default": 0 }
                ] },
                "traits": [{ "name": "Sup", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "looping" }, { "name": "done" }, { "name": "bad" }],
                        "transitions": [
                            { "from": "idle", "to": "looping", "event": "GO", "effects": [["set", "@entity.n", 0], ask.clone()] },
                            { "from": "looping", "to": "looping", "event": "STEP", "guard": ["<", "@entity.n", limit],
                              "effects": [["set", "@entity.n", ["+", "@entity.n", 1]], ask] },
                            { "from": "looping", "to": "done", "event": "STEP", "effects": [["set", "@entity.got", "@payload"]] },
                            { "from": "looping", "to": "bad", "event": "BAD", "effects": [["set", "@entity.got", "@payload"]] }
                        ]
                    } }]
            }]
        }))
        .unwrap()
    }

    fn all_modes() -> Vec<orbital_core_base::os_grants::FsMode> {
        use orbital_core_base::os_grants::FsMode::*;
        vec![Read, Write, Create, Remove, Meta]
    }

    fn chain_wired(dir: &std::path::Path, key: Option<&str>, limit: usize) -> Wired {
        let dir = dir.canonicalize().unwrap();
        let file = dir.join("probe.txt");
        std::fs::write(&file, b"x").unwrap();
        let grants = OsGrants {
            fs: vec![orbital_core_base::os_grants::FsGrant { glob: format!("{}/**", dir.to_str().unwrap()), modes: all_modes() }],
            ..OsGrants::default()
        };
        wired_granted(stat_chain(file.to_str().unwrap(), key, limit), grants)
    }

    fn number_of(wired: &Wired, field: &str) -> f64 {
        match wired.kernel.snapshot_trait("Sup", None).entity.get_path(field) {
            Some(read) if read.is_numeric() => read.as_number().expect("an exact number"),
            other => panic!("{field}: {other:?}"),
        }
    }

    #[test]
    fn a_keyed_read_only_fs_chain_past_the_step_cap_finishes_across_dispatches() {
        let dir = tempfile::tempdir().unwrap();
        let wired = chain_wired(dir.path(), Some("k"), CHAIN_LIMIT);
        go_sup(&wired);
        let mut completions = 0usize;
        while wired.kernel.snapshot_trait("Sup", None).state != "done" {
            let (event, summary) = wired.done.recv_timeout(Duration::from_secs(20)).expect("the chain stalled");
            assert!(summary.is_ok(), "{event}: {summary:?}");
            completions += 1;
        }
        assert_eq!(number_of(&wired, "n"), CHAIN_LIMIT as f64);
        assert!(completions > 2000, "each ask completes as a dispatch of its own: {completions}");
        assert!(wired.handles.completions.live_keys().is_empty());
    }

    #[test]
    fn control_the_same_chain_answered_inline_is_still_cut_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let wired = chain_wired(dir.path(), None, CHAIN_LIMIT);
        let progress = wired.kernel.start_dispatch("Sup", None, "GO", Value::Null, None, None).unwrap();
        let outcomes = wired.sink.drive(progress).unwrap();
        assert!(outcomes.iter().any(|(_, result)| result.cascade_cut.is_some()), "an inline chain must hit CROSS_TRAIT_CASCADE_CAP");
        assert_ne!(wired.kernel.snapshot_trait("Sup", None).state, "done");
        assert!(number_of(&wired, "n") < CHAIN_LIMIT as f64);
    }

    #[test]
    fn control_a_grant_refusal_carries_the_same_error_deferred_as_inline() {
        let dir = tempfile::tempdir().unwrap();
        let refused = |key: Option<&str>| {
            let grants = OsGrants::default();
            let wired = wired_granted(stat_chain(dir.path().join("denied").to_str().unwrap(), key, 0), grants);
            go_sup(&wired);
            if key.is_some() {
                let (event, summary) = wired.done.recv_timeout(Duration::from_secs(10)).expect("deferred refusal");
                assert_eq!((event.as_str(), summary.is_ok()), ("BAD", true));
            }
            let held = wired.kernel.snapshot_trait("Sup", None);
            assert_eq!(held.state, "bad");
            let mut got = serde_json::to_value(&held.entity).unwrap()["got"].clone();
            got.as_object_mut().unwrap().remove("key");
            got
        };
        let inline = refused(None);
        assert_eq!(inline["kind"], "denied");
        assert_eq!(refused(Some("k")), inline);
    }

    #[test]
    fn a_rename_issued_from_the_write_success_route_runs_after_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let (tmp, out) = (dir.join("a.tmp"), dir.join("a.txt"));
        let ask = |op: &str, args: Vec<Json>, key: &str, ok: &str| {
            let mut call = vec![json!(op)];
            call.extend(args);
            call.push(json!({ "key": key, "emit": { "success": ok, "failure": "BAD" } }));
            Json::Array(call)
        };
        let (tmp_s, out_s) = (tmp.to_str().unwrap(), out.to_str().unwrap());
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Sup", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "writing" }, { "name": "renaming" }, { "name": "done" }, { "name": "bad" }],
                        "transitions": [
                            { "from": "idle", "to": "writing", "event": "GO", "effects": [ask("os/write-file", vec![json!(tmp_s), json!("ZGF0YQ==")], "w", "WROTE")] },
                            { "from": "writing", "to": "renaming", "event": "WROTE", "effects": [ask("os/rename", vec![json!(tmp_s), json!(out_s)], "r", "RENAMED")] },
                            { "from": "renaming", "to": "done", "event": "RENAMED" },
                            { "from": "writing", "to": "bad", "event": "BAD" },
                            { "from": "renaming", "to": "bad", "event": "BAD" }
                        ]
                    } }]
            }]
        }))
        .unwrap();
        let grants = OsGrants {
            fs: vec![orbital_core_base::os_grants::FsGrant { glob: format!("{}/**", dir.to_str().unwrap()), modes: all_modes() }],
            ..OsGrants::default()
        };
        let wired = wired_granted(schema, grants);
        go_sup(&wired);
        for expected in ["WROTE", "RENAMED"] {
            let (event, summary) = wired.done.recv_timeout(Duration::from_secs(10)).expect("chained completion");
            assert_eq!(event, expected);
            assert!(summary.is_ok(), "{event}: {summary:?}");
        }
        assert_eq!(wired.kernel.snapshot_trait("Sup", None).state, "done");
        assert_eq!(std::fs::read(&out).unwrap(), b"data");
        assert!(!tmp.exists());
    }
}

#[cfg(unix)]
mod os_twin {
    use super::*;
    use orbital_core_base::os::OsOp;
    use orbital_core_base::os_grants::OsGrants;
    use orbital_core_runtime::os_host::{answer_os, OsHandles, OsHost};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use orbital_core_base::os::OsError;
    use orbital_core_runtime::os_host::completion::{completion_event, OsCompletionSink};
    use orbital_core_runtime::resumable::{CallRoutes, CallSite};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc as Shared, Mutex as StdMutex};

    struct Collect(StdMutex<Sender<(String, Json)>>);

    impl OsCompletionSink for Collect {
        fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>) {
            if let Some((event, payload)) = completion_event(site, key, outcome) {
                let fired = json!({ "event": event, "payload": serde_json::to_value(&payload).unwrap() });
                let _ = self.0.lock().unwrap().send((key.to_string(), fired));
            }
        }
    }

    fn routed_site() -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "Sup".to_string(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: CallRoutes {
                success: Some("DONE".to_string()),
                failure: Some("FAILED".to_string()),
                ..CallRoutes::default()
            },
        }
    }

    fn completion_of(rx: &Receiver<(String, Json)>, got: &mut HashMap<String, Json>, key: &str) -> Json {
        loop {
            if let Some(fired) = got.remove(key) {
                return fired;
            }
            let (arrived, fired) = rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("no completion for key {key}"));
            got.insert(arrived, fired);
        }
    }

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/os-asks.json")
    }

    fn text(vars: &HashMap<String, Json>, s: &str) -> String {
        let mut out = s.to_string();
        for (name, value) in vars {
            let shown = match value {
                Json::String(v) => v.clone(),
                Json::Number(n) => n.as_f64().map_or(n.to_string(), |f| format!("{}", f as i64)),
                other => other.to_string(),
            };
            out = out.replace(&format!("${{{name}}}"), &shown);
        }
        out
    }

    fn substitute(vars: &HashMap<String, Json>, value: &Json) -> Json {
        match value {
            Json::String(s) => Json::String(text(vars, s)),
            Json::Array(items) => Json::Array(items.iter().map(|i| substitute(vars, i)).collect()),
            Json::Object(map) => {
                if map.len() == 1 {
                    if let Some(Json::String(name)) = map.get("$var") {
                        return vars.get(name).cloned().unwrap_or(Json::Null);
                    }
                }
                Json::Object(map.iter().map(|(k, v)| (k.clone(), substitute(vars, v))).collect())
            }
            other => other.clone(),
        }
    }

    fn matches(actual: &Json, expected: &Json) -> bool {
        match expected {
            Json::Object(map) if map.get("$number") == Some(&Json::Bool(true)) => actual.is_number(),
            Json::Object(map) => actual.as_object().is_some_and(|a| {
                a.len() == map.len() && map.iter().all(|(k, v)| a.get(k).is_some_and(|x| matches(x, v)))
            }),
            Json::Array(items) => actual.as_array().is_some_and(|a| {
                a.len() == items.len() && items.iter().zip(a).all(|(e, x)| matches(x, e))
            }),
            Json::Number(n) => actual.as_f64().is_some() && actual.as_f64() == n.as_f64(),
            other => actual == other,
        }
    }

    #[test]
    fn the_embed_exports_name_the_deferring_ops_and_shape_a_completion_event() {
        use orbital_core_runtime::embed::{os_completion_event_json, os_deferring_ops_json};
        let names: Vec<String> = serde_json::from_str(&os_deferring_ops_json().unwrap()).unwrap();
        let finite_fs = [
            "os/read-file", "os/write-file", "os/append-file", "os/open", "os/close", "os/read", "os/write", "os/seek",
            "os/truncate", "os/fsync", "os/copy", "os/rename", "os/remove", "os/link", "os/symlink", "os/readlink",
            "os/mkdir", "os/rmdir", "os/list-dir", "os/glob", "os/mktemp", "os/realpath", "os/chdir", "os/stat",
            "os/lstat", "os/access", "os/exists", "os/chmod", "os/chown", "os/utimes",
        ];
        let rest = ["os/wait", "os/stdin-read", "os/send", "os/recv", "os/recvfrom", "os/irq-wait", "ffi/call", "gpu/device-open", "gpu/submit"];
        assert_eq!(names, finite_fs.iter().chain(rest.iter()).copied().collect::<Vec<_>>());
        assert!(!names.iter().any(|n| n == "os/cwd"), "control: os/cwd answers inline");
        let site = json!({ "trait": "Sup", "followOn": false, "emit": { "success": "DONE", "failure": "FAILED" } }).to_string();
        let ok = os_completion_event_json(&site, "k", &json!({ "ok": { "code": 3 } }).to_string()).unwrap();
        assert_eq!(serde_json::from_str::<Json>(&ok).unwrap(), json!({ "event": "DONE", "payload": { "code": 3, "key": "k" } }));
        let failed = os_completion_event_json(&site, "k", &json!({ "error": { "kind": "closed", "message": "gone" } }).to_string()).unwrap();
        assert_eq!(serde_json::from_str::<Json>(&failed).unwrap(), json!({ "event": "FAILED", "payload": { "error": "gone", "kind": "closed", "key": "k" } }));
        let unrouted = json!({ "trait": "Sup", "followOn": false, "emit": {} }).to_string();
        assert_eq!(os_completion_event_json(&unrouted, "k", &json!({ "ok": {} }).to_string()).unwrap(), "null");
    }

    #[test]
    fn os_twin_fixture_replay() {
        replay(false);
    }

    #[test]
    fn os_twin_fixture_replay_through_the_native_host() {
        replay(true);
    }

    struct NoSecrets;

    impl orbital_core_runtime::secret_source::SecretSource for NoSecrets {
        fn secret(&self, _: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    fn native_host() -> orbital_core_runtime::native_host::NativeHost {
        orbital_core_runtime::native_host::NativeHost::new(
            Shared::new(orbital_core_runtime::entity_source::InMemoryEntityCache::new()),
            Shared::new(NoSecrets),
            OsGrants::default(),
            Shared::new(OsHandles::default()),
            Shared::new(|_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {}),
        )
    }

    #[test]
    fn a_service_the_native_host_has_no_provider_for_is_refused_as_not_configured() {
        for (service, method) in [("hosting", "deploy"), ("registry", "publish"), ("no-such-service", "search")] {
            let request = IoRequest::CallService {
                service: service.to_string(),
                method: method.to_string(),
                args: Value::Null,
                key: None,
                caller: None,
                site: None,
            };
            let IoAnswer::Service(Err(message)) = native_host().answer(&request) else {
                panic!("{service}.{method}: not a refused service answer")
            };
            assert_eq!(message, format!("Integration not configured: {service}.{method}"));
        }
    }

    fn replay(through_native_host: bool) {
        let path = fixture_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            eprintln!("os_twin_fixture_replay: {} is absent (standalone checkout); skipped", path.display());
            return;
        };
        let fixture: Json = serde_json::from_str(&raw).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().canonicalize().unwrap();
        let anchors_dir = tempfile::tempdir().unwrap();
        let anchors_root = anchors_dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(anchors_root.join("start")).unwrap();
        std::fs::create_dir_all(anchors_root.join("home/.cfg")).unwrap();
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = echo.local_addr().unwrap().port();
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in echo.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut buf = [0u8; 256];
                    while let Ok(n) = stream.read(&mut buf) {
                        if n == 0 || stream.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });

        let mut vars: HashMap<String, Json> = HashMap::new();
        vars.insert("tmp".into(), json!(real.to_string_lossy()));
        vars.insert("anchors".into(), json!(anchors_root.to_string_lossy()));
        vars.insert("port".into(), json!(port));
        vars.insert("closedPort".into(), json!(closed));

        let declared: OsGrants = serde_json::from_value(substitute(&vars, &fixture["grants"])).unwrap();
        let anchors = orbital_core_runtime::os_host::OsAnchors {
            home: Some(anchors_root.join("home")),
            start: Some(anchors_root.join("start")),
            env: orbital_core_runtime::os_host::EnvSnapshot::from_pairs(std::env::var("PATH").map(|p| ("PATH".to_string(), p))),
            ..orbital_core_runtime::os_host::OsAnchors::default()
        };
        let grants = declared.resolve_anchors(&anchors).0;
        let handles = Shared::new(OsHandles { anchors, ..OsHandles::default() });
        let (told, rx) = channel();
        handles.completions.set_sink(Shared::new(Collect(StdMutex::new(told))));
        let mut arrived: HashMap<String, Json> = HashMap::new();
        let on_event = |_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {};
        let host = OsHost { grants: &grants, handles: &handles, on_event: &on_event, secrets: None };
        let native = orbital_core_runtime::native_host::NativeHost::new(
            Shared::new(orbital_core_runtime::entity_source::InMemoryEntityCache::new()),
            Shared::new(NoSecrets),
            grants.clone(),
            handles.clone(),
            Shared::new(|_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {}),
        );

        let steps = fixture["steps"].as_array().unwrap();
        assert!(steps.len() > 70);
        for step in steps {
            let name = step["name"].as_str().unwrap();
            if let Some(key) = step.get("cancel").and_then(Json::as_str) {
                assert_eq!(handles.cancel_completion(key), step["cancelled"].as_bool().unwrap(), "{name}");
                continue;
            }
            let op = OsOp::from_name(step["op"].as_str().unwrap()).unwrap_or_else(|| panic!("{name}: unknown op"));
            let args: Vec<Value> = step["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| serde_json::from_value(substitute(&vars, a)).unwrap())
                .collect();
            let key = step.get("key").and_then(Json::as_str).map(str::to_string);
            let site = key.as_ref().map(|_| routed_site());
            let request = IoRequest::Os { op, args, key: key.clone(), site, principal: None, orbital: None };
            let answered = if through_native_host { native.answer(&request) } else { answer_os(Some(&host), &request) };
            if let Some(deferred) = step.get("deferred") {
                assert!(matches!(answered, IoAnswer::ServiceDeferred), "{name}: {answered:?}");
                if deferred != "pending" {
                    let fired = completion_of(&rx, &mut arrived, key.as_deref().unwrap());
                    let want = substitute(&vars, deferred);
                    assert!(matches(&fired, &want), "{name}: {fired} vs {want}");
                }
                continue;
            }
            let IoAnswer::Os(answer) = answered else {
                panic!("{name}: not an os answer")
            };
            let expect = substitute(&vars, &step["expect"]);
            match answer {
                Ok(value) => {
                    let actual = serde_json::to_value(value.expose()).unwrap();
                    let want = expect.get("ok").unwrap_or_else(|| panic!("{name}: answered ok {actual}, expected {expect}"));
                    assert!(matches(&actual, want), "{name}: {actual} vs {want}");
                    if let Some(binds) = step.get("bind").and_then(Json::as_object) {
                        for (var, field) in binds {
                            vars.insert(var.clone(), actual[field.as_str().unwrap()].clone());
                        }
                    }
                }
                Err(error) => {
                    let want = expect.get("error").unwrap_or_else(|| panic!("{name}: answered error {error:?}, expected {expect}"));
                    assert_eq!(error.kind.as_str(), want["kind"].as_str().unwrap(), "{name}: {}", error.message);
                    if let Some(message) = want.get("message").and_then(Json::as_str) {
                        assert_eq!(error.message, message, "{name}");
                    }
                    if let Some(prefix) = want.get("messagePrefix").and_then(Json::as_str) {
                        assert!(error.message.starts_with(prefix), "{name}: {}", error.message);
                    }
                }
            }
        }
        handles.close_all();
    }
}

fn draw_effect() -> Json {
    json!(["emit", "OK", { "draw": ["prob/uniform", 0, 1] }])
}

fn draw_with(answer: Vec<u8>) -> (Json, Vec<IoRequest>) {
    let (response, seen) = run(draw_effect(), go(None), |request| match request {
        IoRequest::Random { bytes: 4 } => IoAnswer::Random(Ok((answer.clone()).into())),
        other => panic!("the fixture asks for {other:?}"),
    });
    (payload_of(&response, "OK"), seen)
}

#[test]
fn an_unseeded_prob_draw_asks_one_journaled_random_and_two_dispatches_with_different_answers_differ() {
    let (first, seen) = draw_with(vec![1, 2, 3, 4]);
    assert_eq!(seen, vec![IoRequest::Random { bytes: 4 }]);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (second, _) = draw_with(vec![9, 8, 7, 6]);
    assert_ne!(first, second, "different Random answers draw different values, whatever the clock says");
}

#[test]
fn replaying_a_dispatch_from_its_journal_draws_the_same_value_whatever_the_clock_says() {
    let (first, _) = draw_with(vec![1, 2, 3, 4]);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (replay, _) = draw_with(vec![1, 2, 3, 4]);
    assert_eq!(first, replay);
}

#[test]
fn a_seeded_prob_draw_makes_no_ask() {
    let (response, seen) = run(
        json!(["emit", "OK", { "draw": ["do", ["prob/seed", 42], ["prob/uniform", 0, 1]] }]),
        go(None),
        |request| panic!("a seeded stream must not ask the host, asked {request:?}"),
    );
    assert!(seen.is_empty());
    assert!(payload_of(&response, "OK")["draw"].is_number());
}

mod kv_asks {
    use orbital_core_runtime::kv_host::{json_text, KvStores};
    use orbital_core_runtime::os_host::{answer_os, KvMode, OsGrants, OsHandles, OsHost};
    use orbital_core_runtime::resumable::{CallSite, IoAnswer, IoRequest};
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
    use orbital_core_base::value::Value;
    use serde_json::{json, Value as Json};
    use std::collections::BTreeMap;

    fn grants(store: &str, modes: &[KvMode]) -> OsGrants {
        OsGrants { kv: BTreeMap::from([(store.to_string(), modes.to_vec())]), ..OsGrants::default() }
    }

    fn ask(handles: &OsHandles, grants: &OsGrants, op: &str, args: Vec<Value>) -> Result<Value, OsError> {
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let host = OsHost { grants, handles, on_event: &on_event, secrets: None };
        let op = OsOp::from_name(op).unwrap_or_else(|| panic!("{op} is not an op"));
        match answer_os(Some(&host), &IoRequest::Os { op, args, key: None, site: None, principal: None, orbital: None }) {
            IoAnswer::Os(answer) => answer.map(orbital_core_base::secret::Secret::into_inner),
            other => panic!("{other:?}"),
        }
    }

    fn v(json: Json) -> Value {
        serde_json::from_value(json).expect("value")
    }

    fn trace_slice() -> Json {
        json!([
            { "op": "kv/get", "args": ["rows", "order/1"], "expect": { "found": false } },
            { "op": "kv/put", "args": ["rows", "order/1", { "id": "1", "total": 12 }], "expect": { "storedKey": "order/1" } },
            { "op": "kv/put", "args": ["rows", "order/2", [1, null, "x"]], "expect": { "storedKey": "order/2" } },
            { "op": "kv/put", "args": ["rows", "order-line/1", false], "expect": { "storedKey": "order-line/1" } },
            { "op": "kv/scan", "args": ["rows", "order/"], "expect": { "entries": [
                { "key": "order/1", "value": { "id": "1", "total": 12 } },
                { "key": "order/2", "value": [1, null, "x"] }
            ] } },
            { "op": "kv/scan", "args": ["rows", "order/", 1], "expect": { "entries": [
                { "key": "order/1", "value": { "id": "1", "total": 12 } }
            ] } },
            { "op": "kv/delete", "args": ["rows", "order/2"], "expect": { "existed": true } },
            { "op": "kv/delete", "args": ["rows", "order/2"], "expect": { "existed": false } },
            { "op": "kv/batch", "args": ["rows", [
                { "kind": "put", "key": "order/4", "value": 4 },
                { "kind": "frobnicate", "key": "order/5" }
            ]], "expectError": "invalid" },
            { "op": "kv/get", "args": ["rows", "order/4"], "expect": { "found": false } },
            { "op": "kv/put", "args": ["", "k", 1], "expectError": "invalid" },
            { "op": "kv/put", "args": ["rows", 7, 1], "expectError": "invalid" },
            { "op": "kv/scan", "args": ["rows", "order/", -1], "expectError": "invalid" }
        ])
    }

    fn replay(kv: KvStores, steps: &Json) {
        let handles = OsHandles { kv, ..OsHandles::default() };
        let grants = grants("rows", &[KvMode::Read, KvMode::Write]);
        for (i, step) in steps.as_array().unwrap().iter().enumerate() {
            let op = step["op"].as_str().unwrap();
            let args: Vec<Value> = step["args"].as_array().unwrap().iter().cloned().map(v).collect();
            match (ask(&handles, &grants, op, args), step.get("expectError").and_then(Json::as_str)) {
                (Ok(value), None) => assert_eq!(json_text(&value), json_text(&v(step["expect"].clone())), "step {i} {op}"),
                (Err(error), Some(kind)) => assert_eq!(error.kind.as_str(), kind, "step {i} {op}: {}", error.message),
                (other, want) => panic!("step {i} {op}: answered {other:?}, expected {want:?} / {}", step["expect"]),
            }
        }
    }

    #[test]
    fn a_kv_trace_answers_byte_identical_on_memory_and_directory_stores() {
        replay(KvStores::memory("trace-app", "en"), &trace_slice());
        let dir = tempfile::tempdir().expect("tempdir");
        replay(KvStores::directory(dir.path(), "trace-app", "en"), &trace_slice());
        let file = dir.path().join("kv/trace-app/en/rows.json");
        assert_eq!(std::fs::read_to_string(file).unwrap(), r#"{"order-line/1":false,"order/1":{"id":"1","total":12}}"#);
    }

    #[test]
    fn the_store_file_is_one_sorted_json_object_portable_to_a_fresh_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let grants = grants("rows", &[KvMode::Read, KvMode::Write]);
        let handles = OsHandles { kv: KvStores::directory(dir.path(), "shop", "en"), ..OsHandles::default() };
        let ops = v(json!([{ "kind": "put", "key": "b\"/\n", "value": [1] }, { "kind": "put", "key": "a", "value": { "z": 1 } }]));
        assert_eq!(json_text(&ask(&handles, &grants, "kv/batch", vec![v(json!("rows")), ops]).unwrap()), r#"{"applied":2}"#);
        let at = dir.path().join("kv/shop/en");
        assert_eq!(std::fs::read_to_string(at.join("rows.json")).unwrap(), r#"{"a":{"z":1},"b\"/\n":[1]}"#);
        let names: Vec<String> = std::fs::read_dir(&at).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, ["rows.json"], "no temp file is left");
        let again = OsHandles { kv: KvStores::directory(dir.path(), "shop", "en"), ..OsHandles::default() };
        let got = ask(&again, &grants, "kv/get", vec![v(json!("rows")), v(json!("b\"/\n"))]).unwrap();
        assert_eq!(json_text(&got), r#"{"found":true,"value":[1]}"#);
        let other_locale = OsHandles { kv: KvStores::directory(dir.path(), "shop", "ar"), ..OsHandles::default() };
        let miss = ask(&other_locale, &grants, "kv/get", vec![v(json!("rows")), v(json!("a"))]).unwrap();
        assert_eq!(json_text(&miss), r#"{"found":false}"#, "control: another locale is another namespace");
        let empty_key = ask(&handles, &grants, "kv/put", vec![v(json!("rows")), v(json!("")), v(json!(0.5))]).unwrap();
        assert_eq!(json_text(&empty_key), r#"{"storedKey":""}"#, "edge: the empty key is a key");
        assert_eq!(std::fs::read_to_string(at.join("rows.json")).unwrap(), r#"{"":0.5,"a":{"z":1},"b\"/\n":[1]}"#);
    }

    #[cfg(unix)]
    #[test]
    fn a_batch_whose_commit_fails_leaves_the_previous_store_whole() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let grants = grants("rows", &[KvMode::Read, KvMode::Write]);
        let handles = OsHandles { kv: KvStores::directory(dir.path(), "shop", "en"), ..OsHandles::default() };
        let rows = || v(json!("rows"));
        ask(&handles, &grants, "kv/put", vec![rows(), v(json!("a")), v(json!(1))]).unwrap();
        let at = dir.path().join("kv/shop/en");
        let before = std::fs::read(at.join("rows.json")).unwrap();
        let batch = || v(json!([{ "kind": "put", "key": "b", "value": 2 }, { "kind": "delete", "key": "a" }]));
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o555)).unwrap();
        let refused = ask(&handles, &grants, "kv/batch", vec![rows(), batch()]);
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        if std::fs::metadata(&at).is_ok() && refused.is_ok() {
            panic!("the directory ignored its permissions; this host cannot run the failure case");
        }
        assert_eq!(refused.expect_err("read-only").kind, OsErrorKind::Io);
        assert_eq!(std::fs::read(at.join("rows.json")).unwrap(), before, "nothing of the failed batch is applied");
        assert_eq!(std::fs::read_dir(&at).unwrap().count(), 1, "no temp file is left");
        let invalid = v(json!([{ "kind": "put", "key": "b", "value": 2 }, { "kind": "frobnicate", "key": "a" }]));
        assert_eq!(ask(&handles, &grants, "kv/batch", vec![rows(), invalid]).expect_err("bad op").kind, OsErrorKind::Invalid);
        assert_eq!(std::fs::read(at.join("rows.json")).unwrap(), before, "an invalid batch applies nothing either");
        assert_eq!(json_text(&ask(&handles, &grants, "kv/batch", vec![rows(), batch()]).unwrap()), r#"{"applied":2}"#, "control: the same batch commits when the directory is writable");
        assert_eq!(std::fs::read_to_string(at.join("rows.json")).unwrap(), r#"{"b":2}"#);
    }

    #[test]
    fn the_kv_grant_is_checked_per_store_and_mode() {
        let handles = OsHandles { kv: KvStores::memory("shop", "en"), ..OsHandles::default() };
        let read_only = grants("rows", &[KvMode::Read]);
        let put = |g: &OsGrants, store: &str| ask(&handles, g, "kv/put", vec![v(json!(store)), v(json!("k")), v(json!(1))]);
        assert_eq!(put(&read_only, "rows").expect_err("no write").kind, OsErrorKind::Denied);
        assert!(ask(&handles, &read_only, "kv/scan", vec![v(json!("rows")), v(json!(""))]).is_ok(), "control: read is granted");
        let globbed = grants("rows*", &[KvMode::Read, KvMode::Write]);
        assert!(put(&globbed, "rows-log").is_ok(), "a store glob covers a matching name");
        assert_eq!(put(&globbed, "other").expect_err("outside").kind, OsErrorKind::Denied, "control: not the glob");
        assert_eq!(put(&OsGrants::default(), "rows").expect_err("closed").kind, OsErrorKind::Denied, "kv is closed by default");
        assert_eq!(put(&globbed, "../x").expect_err("bad name").kind, OsErrorKind::Invalid, "edge: a bad name is invalid before the grant");
        let unplaced = OsHandles::default();
        let none = ask(&unplaced, &globbed, "kv/get", vec![v(json!("rows")), v(json!("k"))]);
        assert_eq!(none.expect_err("no namespace").kind, OsErrorKind::Unsupported, "edge: a host with no app/locale");
    }
}

mod access_check_effects {
    use super::*;

    fn owned() -> Json {
        json!(["=", ["object/get", "@entity", "ownerId"], "@user.id"])
    }

    fn policed(effect: Json) -> EmbeddedServer {
        let mut program = serde_json::to_value(schema(effect)).unwrap();
        program["orbitals"][0]["entity"] = json!({
            "name": "Note", "persistence": "runtime", "read_policy": owned(), "create_policy": owned(),
            "fields": [{ "name": "id", "type": "string" }, { "name": "ownerId", "type": "string" }]
        });
        program["orbitals"][0]["traits"][0]["linkedEntity"] = json!("Note");
        program["orbitals"][0]["auxiliaryEntities"] = json!([{
            "name": "Open", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }]
        }]);
        EmbeddedServer::new(serde_json::from_value(program).unwrap()).unwrap()
    }

    fn check(entity: &str, action: &str, owner: &str, viewer: Json) -> Json {
        json!(["access-check", { "entity": entity, "action": action, "row": { "ownerId": owner }, "identity": viewer }, routes()])
    }

    fn verdict(effect: Json, user: Option<Json>) -> (EventResponse, Vec<IoRequest>) {
        run_on(policed(effect), go(user), |request| panic!("an access check asks the host nothing, asked {request:?}"))
    }

    #[test]
    fn create_is_allowed_for_the_owner_and_denied_to_anyone_else() {
        let (allowed, seen) = verdict(check("Note", "create", "u1", json!({ "id": "u1" })), None);
        assert!(seen.is_empty());
        assert_eq!(payload_of(&allowed, "OK"), json!({ "allowed": true }));
        let (denied, _) = verdict(check("Note", "create", "u2", json!({ "id": "u1" })), None);
        assert_eq!(payload_of(&denied, "OK"), json!({ "allowed": false }), "control: a denial is a verdict, not a failure");
        assert!(emitted(&denied).iter().all(|(event, _)| *event != "BAD"));
    }

    #[test]
    fn read_applies_the_owner_filter_to_the_named_viewer_not_the_dispatch_user() {
        let (own, _) = verdict(check("Note", "read", "u2", json!({ "id": "u2" })), signed_in());
        assert_eq!(payload_of(&own, "OK"), json!({ "allowed": true }));
        let (other, _) = verdict(check("Note", "read", "u1", json!({ "id": "u2" })), signed_in());
        assert_eq!(payload_of(&other, "OK"), json!({ "allowed": false }), "control: the dispatch's u1 never stands in for the viewer");
        let (anonymous, _) = verdict(check("Note", "read", "u1", Json::Null), signed_in());
        assert_eq!(payload_of(&anonymous, "OK"), json!({ "allowed": false }), "edge: a null viewer is nobody");
    }

    #[test]
    fn an_entity_with_no_policy_allows_every_viewer() {
        let (open, _) = verdict(check("Open", "create", "u9", Json::Null), None);
        assert_eq!(payload_of(&open, "OK"), json!({ "allowed": true }));
    }

    #[test]
    fn an_unknown_entity_is_refused_through_the_failure_route() {
        let (refused, seen) = verdict(check("Ghost", "create", "u1", json!({ "id": "u1" })), None);
        assert!(seen.is_empty());
        let error = payload_of(&refused, "BAD");
        assert_eq!(error["error"], json!("access-check: `Ghost` is not an entity of this program"));
        assert!(emitted(&refused).iter().all(|(event, _)| *event != "OK"));
        let (known, _) = verdict(check("Note", "create", "u1", json!({ "id": "u1" })), None);
        assert!(emitted(&known).iter().all(|(event, _)| *event != "BAD"), "control: a declared entity is answered");
    }

    #[test]
    fn an_unknown_action_or_a_missing_key_is_refused() {
        let (bad_action, _) = verdict(check("Note", "upsert", "u1", json!({ "id": "u1" })), None);
        assert_eq!(
            payload_of(&bad_action, "BAD")["error"],
            json!("access-check: action must be create, read, update or delete, got `upsert`")
        );
        let (no_viewer, _) =
            verdict(json!(["access-check", { "entity": "Note", "action": "read", "row": {} }, routes()]), None);
        assert_eq!(payload_of(&no_viewer, "BAD")["error"], json!("access-check: the request needs `identity` (null for nobody)"));
        let (renamed, _) = verdict(
            json!(["access-check", { "entity": "Note", "action": "read", "row": {}, "viewer": { "id": "u1" } }, routes()]),
            None,
        );
        assert_eq!(
            payload_of(&renamed, "BAD")["error"],
            json!("access-check: the request field `viewer` is refused: name the subject row as `identity`"),
            "no alias: the old name is refused"
        );
    }

    #[test]
    fn the_request_bindings_resolve_before_the_check() {
        let effect = json!(["access-check",
            { "entity": "Note", "action": "create", "row": { "ownerId": "@payload.principal" }, "identity": { "id": "@payload.principal" } },
            routes()]);
        let (response, _) = verdict(effect, None);
        assert_eq!(payload_of(&response, "OK"), json!({ "allowed": true }));
    }
}


// EXEC-OS-GRANTS-ORBITAL: an `os/` ask is answered against its calling orbital's own grants plus the app level.
mod os_grants_per_orbital {
    use super::*;
    use orbital_core_base::effect_types::RuntimeEnvironment;
    use orbital_core_runtime::config::KernelConfig;
    use orbital_core_runtime::credentials::Credentials;
    use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
    use orbital_core_runtime::kernel::RuntimeKernel;
    use orbital_core_runtime::native_host::NativeHost;
    use orbital_core_runtime::os_host::completion::{CompletionAnswerer, CompletionDone, KernelCompletionSink};
    use orbital_core_runtime::os_host::OsHandles;
    use orbital_core_runtime::resumable::{CallSite, DispatchError, DispatchOutcomes};
    use std::sync::Arc;

    fn orbital(name: &str, trait_name: &str, grants: Option<Json>) -> Json {
        let mut orbital = json!({
            "name": name,
            "entity": { "name": format!("{name}Row"), "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": trait_name, "linkedEntity": format!("{name}Row"), "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": "ok" }, { "name": "bad" }],
                "transitions": [
                    { "from": "idle", "to": "idle", "event": "GO", "effects": [["os/getenv", "HOME", { "emit": { "success": "ENV_OK", "failure": "ENV_BAD" } }]] },
                    { "from": "idle", "to": "ok", "event": "ENV_OK", "effects": [["set", "@entity.got", "@payload"]] },
                    { "from": "idle", "to": "bad", "event": "ENV_BAD", "effects": [["set", "@entity.got", "@payload"]] }
                ]
            } }]
        });
        if let Some(grants) = grants {
            orbital["osGrants"] = grants;
        }
        orbital
    }

    fn program(app: Option<Json>, a: Option<Json>, b: Option<Json>) -> OrbitalSchema {
        let mut program = json!({ "name": "App", "typeContractVersion": 1, "orbitals": [orbital("A", "TraitA", a), orbital("B", "TraitB", b)] });
        if let Some(app) = app {
            program["osGrants"] = app;
        }
        serde_json::from_value(program).unwrap()
    }

    fn state_of(schema: OrbitalSchema, trait_name: &str) -> String {
        let store: Arc<dyn EntitySource> = Arc::new(InMemoryEntityCache::new());
        let kernel = Arc::new(RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), KernelConfig::default()));
        kernel.register_schema(schema).unwrap();
        let host = NativeHost::new(
            store,
            Arc::new(Credentials::from_env(Default::default())),
            kernel.os_grant_table(),
            Arc::new(OsHandles::default()),
            Arc::new(|_: Option<&CallSite>, _: Value| {}),
        );
        let answer: Arc<CompletionAnswerer> = Arc::new(move |request: &IoRequest| host.answer(request));
        let on_done: Arc<CompletionDone> =
            Arc::new(|_: &CallSite, _: &str, _: &Value, _: Option<&str>, _: Result<DispatchOutcomes, DispatchError>| {});
        let sink = KernelCompletionSink::new(kernel.clone(), answer, on_done);
        let progress = kernel.start_dispatch(trait_name, None, "GO", Value::Null, None, None).unwrap();
        sink.drive(progress).unwrap();
        kernel.snapshot_trait(trait_name, None).state
    }

    fn home() -> Json {
        json!({ "env": ["HOME"] })
    }

    /// EXEC-GRANTS-2 through a running kernel.
    #[test]
    fn an_orbital_with_a_grant_is_answered_and_a_sibling_without_one_is_refused() {
        assert_eq!(state_of(program(None, Some(home()), None), "TraitA"), "ok");
        assert_eq!(state_of(program(None, Some(home()), None), "TraitB"), "bad");
    }

    #[test]
    fn control_an_app_level_grant_answers_every_orbital() {
        assert_eq!(state_of(program(Some(home()), None, None), "TraitA"), "ok");
        assert_eq!(state_of(program(Some(home()), None, None), "TraitB"), "ok");
    }

    #[test]
    fn control_no_grant_anywhere_refuses_every_orbital() {
        assert_eq!(state_of(program(None, None, None), "TraitA"), "bad");
        assert_eq!(state_of(program(None, None, None), "TraitB"), "bad");
    }
}

mod mem_effects {
    use super::*;
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
    use orbital_core_base::value::ValueMap;

    fn ops() -> Vec<(&'static str, Json)> {
        vec![
            ("NEW_BUMP", json!(["mem/arena-new", "bump", 64])),
            ("NEW_STACK", json!(["mem/arena-new", "stack", 64])),
            ("NEW_BIG", json!(["mem/arena-new", "bump", 4096])),
            ("ALLOC", json!(["mem/alloc", "@payload.arena", 16, 8])),
            ("BORROW_X", json!(["mem/borrow", "@payload.block", "exclusive"])),
            ("BORROW_S", json!(["mem/borrow", "@payload.block", "shared"])),
            ("WRITE", json!(["mem/write", "@payload.xborrow", 0, "@payload.data"])),
            ("RELEASE_X", json!(["mem/release", "@payload.xborrow"])),
            ("RELEASE_S", json!(["mem/release", "@payload.sborrow"])),
            ("FREE", json!(["mem/free", "@payload.block"])),
            ("DROP", json!(["mem/arena-drop", "@payload.arena"])),
        ]
    }

    fn server() -> EmbeddedServer {
        let mut transitions: Vec<Json> = ops()
            .into_iter()
            .map(|(event, mut effect)| {
                effect.as_array_mut().unwrap().push(routes());
                json!({ "from": "idle", "to": "idle", "event": event, "effects": [effect] })
            })
            .collect();
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "READ", "effects": [
            ["emit", "OK", { "data": ["mem/read", "@payload.sborrow", 0, 4], "length": ["mem/length", "@payload.sborrow"] }]
        ] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "PARK", "effects": [
            ["mem/arena-new", "bump", 8, routes()],
            ["os/read-file", "/data/a.txt", { "emit": { "success": "READ_OK", "failure": "BAD" } }]
        ] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "OK" }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "BAD" }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "READ_OK" }));
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "osGrants": { "mem": [{ "target": "arena:1024", "kinds": ["create", "free"] }] },
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": transitions
                    } }]
            }]
        }))
        .unwrap();
        EmbeddedServer::new(schema).unwrap()
    }

    #[derive(Debug)]
    enum Answer {
        Ok(Value),
        Failed(String),
    }

    struct Session {
        server: EmbeddedServer,
        held: ValueMap,
    }

    impl Session {
        fn new() -> Self {
            Self { server: server(), held: ValueMap::default() }
        }

        fn request(&self, event: &str) -> EventRequest {
            EventRequest {
                event: event.to_string(),
                target_trait: Some("Host".to_string()),
                payload: Some(Value::object(self.held.clone())),
                ..Default::default()
            }
        }

        fn answer(response: &EventResponse) -> Answer {
            let find = |event: &str| response.emitted_events.iter().find(|e| e.event == event);
            if let Some(ok) = find("OK") {
                return Answer::Ok(ok.payload.clone().expect("an answer carries a payload"));
            }
            let bad = find("BAD").unwrap_or_else(|| panic!("neither OK nor BAD: {:?}", response.emitted_events));
            let payload = bad.payload.clone().expect("a failure carries a payload");
            match payload.get("kind") {
                Some(Value::String(kind)) => Answer::Failed(kind.to_string()),
                other => panic!("a failure names its kind: {other:?}"),
            }
        }

        fn call(&mut self, event: &str, keep_borrow_as: Option<&str>) -> Answer {
            let (response, _) = drive(&self.server, self.request(event), |request| panic!("a mem op asked the host: {request:?}"));
            let answer = Self::answer(&response);
            if let Answer::Ok(Value::Object(fields)) = &answer {
                for (key, value) in fields.iter() {
                    let name = match (key.as_str() == "borrow", keep_borrow_as) {
                        (true, Some(name)) => name,
                        _ => key.as_str(),
                    };
                    self.held.insert(name.into(), value.clone());
                }
            }
            answer
        }

        fn ok(&mut self, event: &str, keep_borrow_as: Option<&str>) {
            match self.call(event, keep_borrow_as) {
                Answer::Ok(_) => {}
                other => panic!("{event} should succeed: {other:?}"),
            }
        }

        fn fails_with(&mut self, event: &str, kind: &str) {
            match self.call(event, None) {
                Answer::Failed(found) => assert_eq!(found, kind, "{event}"),
                other => panic!("{event} should fail with {kind}: {other:?}"),
            }
        }
    }

    fn setup() -> Session {
        let mut session = Session::new();
        session.ok("NEW_BUMP", None);
        session.ok("ALLOC", None);
        session.held.insert("data".into(), Value::bytes(b"abcd".to_vec()));
        session
    }

    #[test]
    fn alloc_exclusive_write_release_then_shared_read_round_trips() {
        let mut session = setup();
        session.ok("BORROW_X", Some("xborrow"));
        session.ok("WRITE", None);
        session.ok("RELEASE_X", None);
        session.ok("BORROW_S", Some("sborrow"));
        let (response, _) = drive(&session.server, session.request("READ"), |request| panic!("asked the host: {request:?}"));
        let Answer::Ok(read) = Session::answer(&response) else { panic!("read answered") };
        assert_eq!(read.get("data").and_then(Value::as_bytes), Some(&b"abcd"[..]));
        assert_eq!(read.get("length").and_then(Value::as_int), Some(16));
    }

    #[test]
    fn a_second_exclusive_borrow_is_borrowed_and_two_shared_borrows_succeed() {
        let mut session = setup();
        session.ok("BORROW_X", Some("xborrow"));
        session.fails_with("BORROW_X", "borrowed");
        session.fails_with("BORROW_S", "borrowed");
        session.ok("RELEASE_X", None);
        session.ok("BORROW_S", Some("sborrow"));
        session.ok("BORROW_S", Some("other"));
    }

    #[test]
    fn a_write_takes_base64_text_as_bytes_and_refuses_text_that_is_not_base64() {
        let mut session = setup();
        session.ok("BORROW_X", Some("xborrow"));
        session.held.insert("data".into(), Value::string("not base64"));
        session.fails_with("WRITE", "invalid");
        session.held.insert("data".into(), Value::string("d3h5eg=="));
        session.ok("WRITE", None);
        session.ok("RELEASE_X", None);
        session.ok("BORROW_S", Some("sborrow"));
        let (response, _) = drive(&session.server, session.request("READ"), |request| panic!("asked the host: {request:?}"));
        let Answer::Ok(read) = Session::answer(&response) else { panic!("read answered") };
        assert_eq!(read.get("data").and_then(Value::as_bytes), Some(&b"wxyz"[..]));
    }

    #[test]
    fn a_shared_borrow_cannot_write() {
        let mut session = setup();
        session.ok("BORROW_S", Some("xborrow"));
        session.fails_with("WRITE", "borrowed");
    }

    #[test]
    fn free_and_arena_drop_with_a_live_borrow_are_borrowed_and_free_after_release_closes_the_block() {
        let mut session = setup();
        session.ok("BORROW_X", Some("xborrow"));
        session.fails_with("FREE", "borrowed");
        session.fails_with("DROP", "borrowed");
        session.ok("RELEASE_X", None);
        session.ok("FREE", None);
        session.fails_with("FREE", "closed");
        session.fails_with("BORROW_X", "closed");
        session.ok("DROP", None);
        session.fails_with("ALLOC", "closed");
    }

    /// EXEC-METAL-1: the kernel answers `mem/*` against the orbital's grants; no host is asked.
    #[test]
    fn an_arena_beyond_the_grant_is_denied() {
        let mut session = Session::new();
        session.fails_with("NEW_BIG", "denied");
        session.ok("NEW_BUMP", None);
    }

    #[test]
    fn a_stack_arena_is_gone_in_the_next_dispatch_and_a_bump_arena_is_not() {
        let mut session = Session::new();
        session.ok("NEW_STACK", None);
        let stack = session.held.get("arena").cloned().unwrap();
        session.ok("NEW_BUMP", None);
        session.ok("ALLOC", None);
        let bump = session.held.get("arena").cloned().unwrap();
        session.held.insert("arena".into(), stack);
        session.fails_with("ALLOC", "closed");
        session.held.insert("arena".into(), bump);
        session.ok("ALLOC", None);
    }

    #[test]
    fn an_alloc_beyond_the_arenas_capacity_is_invalid() {
        let mut session = Session::new();
        session.ok("NEW_BUMP", None);
        for _ in 0..4 {
            session.ok("ALLOC", None);
        }
        session.fails_with("ALLOC", "invalid");
    }

    #[test]
    fn a_forged_handle_is_invalid() {
        let mut session = Session::new();
        session.held.insert("arena".into(), Value::Int(1));
        session.fails_with("ALLOC", "invalid");
    }

    #[test]
    fn a_step_that_parks_and_replays_allocates_once() {
        let mut session = Session::new();
        let (response, seen) = drive(&session.server, session.request("PARK"), |request| match request {
            IoRequest::Os { op: OsOp::ReadFile, .. } => IoAnswer::Os(Err(OsError::new(OsErrorKind::Denied, "no"))),
            other => panic!("the fixture asks for {other:?}"),
        });
        assert_eq!(seen.len(), 1);
        let oks: Vec<_> = response.emitted_events.iter().filter(|e| e.event == "OK").collect();
        assert_eq!(oks.len(), 1);
        let arena = oks[0].payload.as_ref().and_then(|p| p.get("arena")).and_then(|a| a.get("id")).and_then(Value::as_int);
        assert_eq!(arena, Some(1));
        let Answer::Ok(next) = session.call("NEW_BUMP", None) else { panic!("a second arena") };
        let id = next.get("arena").and_then(|a| a.get("id")).and_then(Value::as_int);
        assert_eq!(id, Some(2));
    }
}

mod kernel_effects {
    use super::*;
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
    use orbital_core_base::value::ValueMap;

    const FULL: fn() -> Json = || {
        json!({
            "mem": [
                { "target": "shared:1024", "kinds": ["create", "atomic", "free"] },
                { "target": "arena:1024", "kinds": ["create", "free"] }
            ],
            "kernel": [
                { "target": "queue", "kinds": ["reorder"] },
                { "target": "introspect", "kinds": ["read"] }
            ]
        })
    };

    fn host_ops() -> Vec<(&'static str, Json)> {
        vec![
            ("GO_FRONT", json!(["kernel/queue-front", "T2"])),
            ("GO_BACK", json!(["kernel/queue-back", "T1"])),
            ("NEW_SHARED", json!(["mem/arena-new", "shared", 64])),
            ("NEW_BIG_SHARED", json!(["mem/arena-new", "shared", 4096])),
            ("NEW_BUMP", json!(["mem/arena-new", "bump", 64])),
            ("ALLOC", json!(["mem/alloc", "@payload.arena", 16, 8])),
            ("ADD", json!(["mem/atomic-add", "@payload.block", 0, 5])),
            ("LOAD", json!(["mem/atomic-load", "@payload.block", 0])),
            ("STORE", json!(["mem/atomic-store", "@payload.block", 0, 3])),
            ("CAS", json!(["mem/atomic-cas", "@payload.block", 0, 10, 7])),
            ("FREEZE", json!(["mem/freeze", "@payload.block"])),
            ("BORROW_X", json!(["mem/borrow", "@payload.block", "exclusive"])),
            ("BORROW_S", json!(["mem/borrow", "@payload.block", "shared"])),
            ("RELEASE_X", json!(["mem/release", "@payload.xborrow"])),
            ("WRITE", json!(["mem/write", "@payload.xborrow", 0, "@payload.data"])),
            ("OPEN", json!(["program/open", "@payload.program", { "grants": "@payload.grants", "arenas": "@payload.arenas" }])),
            ("OPEN_BARE", json!(["program/open", "@payload.program", {}])),
        ]
    }

    fn server(grants: Json) -> EmbeddedServer {
        let mut transitions: Vec<Json> = host_ops()
            .into_iter()
            .map(|(event, mut effect)| {
                effect.as_array_mut().unwrap().push(routes());
                let mut effects = vec![effect];
                if event.starts_with("GO_") {
                    effects.insert(0, json!(["emit", "E2"]));
                    effects.insert(0, json!(["emit", "E1"]));
                }
                json!({ "from": "idle", "to": "idle", "event": event, "effects": effects })
            })
            .collect();
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "GO_PLAIN", "effects": [["emit", "E1"], ["emit", "E2"]] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "GO_OWN", "effects": [
            ["emit", "E1"], ["emit", "OWN"], ["kernel/queue-front", "NOPE", routes()]
        ] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "OWN", "effects": [["emit", "OWNED"]] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "FAIL_WRITE", "effects": [
            ["mem/write", "@payload.xborrow", 0, "@payload.data", routes()],
            ["emit", "BADEMIT", { "n": "not a number" }]
        ] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "FAIL_FREEZE", "effects": [
            ["mem/freeze", "@payload.block", routes()],
            ["emit", "BADEMIT", { "n": "not a number" }]
        ] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "WRITE_ADD", "effects": [
            ["mem/write", "@payload.xborrow", 0, "@payload.data", { "emit": { "success": "WROTE", "failure": "BAD" } }],
            ["mem/atomic-add", "@payload.block", 0, 1, routes()]
        ] }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "READ", "effects": [
            ["emit", "OK", { "data": ["mem/read", "@payload.xborrow", 0, 4] }]
        ] }));
        for event in ["WROTE", "BADEMIT", "OWNED"] {
            transitions.push(json!({ "from": "idle", "to": "idle", "event": event }));
        }
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "PARK", "effects": [
            ["mem/atomic-add", "@payload.block", 0, 1, routes()],
            ["os/read-file", "/data/a.txt", { "emit": { "success": "READ_OK", "failure": "BAD" } }]
        ] }));
        let spawn = |options: Json| {
            json!({ "from": "idle", "to": "idle", "event": "SPAWN", "effects": [[
                "program/dispatch", "@payload.address", "@payload.data",
                { "guest": "@payload.guest" }, options
            ]] })
        };
        transitions.push(spawn(json!({ "key": "k1", "emit": { "success": "DONE", "failure": "FAILED" } })));
        transitions.push({
            let mut unkeyed = spawn(json!({ "emit": { "success": "DONE", "failure": "FAILED" } }));
            unkeyed["event"] = json!("SPAWN_UNKEYED");
            unkeyed
        });
        for event in ["OK", "BAD", "READ_OK", "DONE", "FAILED"] {
            transitions.push(json!({ "from": "idle", "to": "idle", "event": event }));
        }
        let listener = |name: &str, heard: &str, trigger: &str, effects: Json| {
            json!({ "name": name, "linkedEntity": "Job",
                "emits": [{ "event": "R1" }, { "event": "R2" }, { "event": "SEEN" }],
                "listens": [{ "event": heard, "triggers": trigger, "source": { "kind": "trait", "trait": "Host" } }],
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": trigger, "effects": effects }] } })
        };
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "osGrants": grants,
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    { "name": "Host", "linkedEntity": "Job", "emits": [{ "event": "E1" }, { "event": "E2" }, { "event": "OWN" }, { "event": "OWNED" },
                            { "event": "BADEMIT", "payloadSchema": [{ "name": "n", "type": "number" }] }],
                        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": transitions } },
                    listener("L1", "E1", "T1", json!([
                        ["emit", "R1"],
                        ["emit", "SEEN", { "work": ["kernel/worklist"], "frame": ["kernel/frame"], "stamp": ["kernel/stamp"] }]
                    ])),
                    listener("L2", "E2", "T2", json!([["emit", "R2"]]))
                ]
            }]
        }))
        .unwrap();
        EmbeddedServer::new(schema).unwrap()
    }

    fn request(event: &str, held: &ValueMap) -> EventRequest {
        EventRequest {
            event: event.to_string(),
            target_trait: Some("Host".to_string()),
            payload: Some(Value::object(held.clone())),
            ..Default::default()
        }
    }

    fn order(grants: Json, event: &str) -> Vec<String> {
        let server = server(grants);
        let (response, _) = drive(&server, request(event, &ValueMap::default()), |request| panic!("asked the host: {request:?}"));
        response.emitted_events.iter().map(|emitted| emitted.event.clone()).collect()
    }

    fn at(events: &[String], name: &str) -> usize {
        events.iter().position(|event| event == name).unwrap_or_else(|| panic!("{name} missing from {events:?}"))
    }

    #[derive(Debug)]
    enum Answer {
        Ok(Value),
        Failed(String),
    }

    struct Session {
        server: EmbeddedServer,
        held: ValueMap,
    }

    impl Session {
        fn new(grants: Json) -> Self {
            Self { server: server(grants), held: ValueMap::default() }
        }

        fn answer(response: &EventResponse) -> Answer {
            if let Some(ok) = response.emitted_events.iter().find(|e| e.event == "OK") {
                return Answer::Ok(ok.payload.clone().expect("an answer carries a payload"));
            }
            let bad = response.emitted_events.iter().find(|e| e.event == "BAD").expect("neither OK nor BAD");
            match bad.payload.as_ref().and_then(|payload| payload.get("kind")) {
                Some(Value::String(kind)) => Answer::Failed(kind.to_string()),
                other => panic!("a failure names its kind: {other:?}"),
            }
        }

        fn call(&mut self, event: &str) -> Answer {
            let (response, _) = drive(&self.server, request(event, &self.held), |request| panic!("asked the host: {request:?}"));
            let answer = Self::answer(&response);
            if let Answer::Ok(Value::Object(fields)) = &answer {
                for (key, value) in fields.iter() {
                    if key.as_str() == "borrow" {
                        self.held.insert("xborrow".into(), value.clone());
                    } else {
                        self.held.insert(key.clone(), value.clone());
                    }
                }
            }
            answer
        }

        fn ok(&mut self, event: &str) -> Value {
            match self.call(event) {
                Answer::Ok(value) => value,
                other => panic!("{event} should succeed: {other:?}"),
            }
        }

        fn fails_with(&mut self, event: &str, kind: &str) {
            match self.call(event) {
                Answer::Failed(found) => assert_eq!(found, kind, "{event}"),
                other => panic!("{event} should fail with {kind}: {other:?}"),
            }
        }

        fn shared_block() -> Self {
            let mut session = Self::new(FULL());
            session.ok("NEW_SHARED");
            session.ok("ALLOC");
            session
        }
    }

    fn value_of(answer: &Value) -> i64 {
        answer.get("value").and_then(Value::as_int).expect("the answer carries an int value")
    }

    #[test]
    fn control_without_a_directive_the_listeners_run_in_emit_order() {
        let events = order(FULL(), "GO_PLAIN");
        assert!(at(&events, "R1") < at(&events, "R2"), "{events:?}");
    }

    #[test]
    fn queue_front_moves_a_pending_listener_delivery_ahead() {
        let events = order(FULL(), "GO_FRONT");
        assert!(at(&events, "R2") < at(&events, "R1"), "{events:?}");
    }

    #[test]
    fn queue_back_moves_a_pending_listener_delivery_behind() {
        let events = order(FULL(), "GO_BACK");
        assert!(at(&events, "R2") < at(&events, "R1"), "{events:?}");
    }

    #[test]
    fn queue_front_without_the_reorder_grant_is_denied_and_changes_nothing() {
        let grants = json!({ "kernel": [{ "target": "introspect", "kinds": ["read"] }] });
        let mut session = Session::new(grants.clone());
        session.fails_with("GO_FRONT", "denied");
        let events = order(grants, "GO_FRONT");
        assert!(at(&events, "R1") < at(&events, "R2"), "{events:?}");
    }

    #[test]
    fn introspection_answers_the_worklist_frame_and_stamp_of_the_step() {
        let first = server(FULL());
        let (response, _) = drive(&first, request("GO_PLAIN", &ValueMap::default()), |request| panic!("asked the host: {request:?}"));
        let seen = response.emitted_events.iter().find(|e| e.event == "SEEN").expect("the listener reported");
        let payload = seen.payload.as_ref().expect("a report carries a payload");
        let work = payload.get("work").expect("work");
        let Value::Array(items) = work else { panic!("work is a list: {work:?}") };
        assert_eq!(items.len(), 1);
        let pending = &items[0];
        assert_eq!(pending.get("event"), Some(&Value::string("T2")));
        assert_eq!(pending.get("trait"), Some(&Value::string("L2")));
        assert_eq!(pending.get("orbital"), Some(&Value::string("Main")));
        let frame = payload.get("frame").expect("frame");
        assert_eq!(frame.get("trait"), Some(&Value::string("L1")));
        assert_eq!(frame.get("state"), Some(&Value::string("idle")));
        assert_eq!(frame.get("event"), Some(&Value::string("T1")));
        let stamp = payload.get("stamp").expect("stamp");
        assert!(stamp.get("now").and_then(Value::as_int).is_some());
        assert_eq!(
            stamp.get("step").and_then(Value::as_int),
            Some(0),
            "L1's delivery is the dispatch's first cascade delivery: Host handles none of its own emits and the seed step is not counted"
        );
        let journal = stamp.get("journal").and_then(Value::as_int).expect("journal position");
        assert!(journal >= 0);
        let (again, _) = drive(&server(FULL()), request("GO_PLAIN", &ValueMap::default()), |request| panic!("asked the host: {request:?}"));
        let again = again.emitted_events.iter().find(|e| e.event == "SEEN").and_then(|e| e.payload.as_ref()).and_then(|p| p.get("stamp")).and_then(|s| s.get("journal")).and_then(Value::as_int);
        assert_eq!(again, Some(journal), "the same dispatch on a fresh kernel reaches the same journal position");
        assert!(stamp.get("dispatch").and_then(Value::as_int).is_some());
    }

    mod guest {
        use super::*;
        use orbital_core_runtime::entity_source::InMemoryEntityCache;
        use orbital_core_runtime::native_host::NativeHost;
        use orbital_core_runtime::os_host::completion::OsCompletionSink;
        use orbital_core_runtime::os_host::{OsGrantTable, OsHandles};
        use orbital_core_runtime::resumable::{substrate_completion_event, CallSite};
        use orbital_core_runtime::secret_source::SecretSource;
        use std::sync::{mpsc, Arc, Mutex};
        use std::thread::ThreadId;
        use std::time::Duration;

        type Settle = (CallSite, String, Result<Value, Value>, ThreadId);

        struct Settled(Mutex<mpsc::Sender<Settle>>);

        impl OsCompletionSink for Settled {
            fn complete(&self, _: &CallSite, _: &str, _: Result<Value, OsError>) {}

            fn settle(&self, site: &CallSite, key: &str, outcome: Result<Value, Value>) {
                self.0.lock().unwrap().send((site.clone(), key.to_string(), outcome, std::thread::current().id())).unwrap();
            }
        }

        struct NoSecrets;

        impl SecretSource for NoSecrets {
            fn secret(&self, _: &str) -> Result<Option<String>, String> {
                Ok(None)
            }
        }

        fn native() -> (NativeHost, mpsc::Receiver<Settle>) {
            let handles = Arc::new(OsHandles::default());
            let (tx, rx) = mpsc::channel();
            handles.completions.set_sink(Arc::new(Settled(Mutex::new(tx))));
            let host = NativeHost::new(
                Arc::new(InMemoryEntityCache::new()),
                Arc::new(NoSecrets),
                OsGrantTable::default(),
                handles,
                Arc::new(|_: Option<&CallSite>, _: Value| {}),
            );
            (host, rx)
        }

        fn parent_grants() -> Json {
            json!({
                "mem": [{ "target": "shared:1024", "kinds": ["create", "atomic", "free"] }],
                "kernel": [{ "target": "guest", "kinds": ["spawn"] }]
            })
        }

        fn guest_program() -> Json {
            json!({
                "name": "Guest", "typeContractVersion": 1, "orbitals": [{
                    "name": "G",
                    "entity": { "name": "Cell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                    "traits": [{ "name": "Work", "linkedEntity": "Cell",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "BUMP", "effects": [["mem/atomic-add", "@payload.arena.blocks.0", 0, 5, routes()]] },
                                { "from": "idle", "to": "idle", "event": "BORROW", "effects": [["mem/borrow", "@payload.arena.blocks.0", "shared", routes()]] },
                                { "from": "idle", "to": "idle", "event": "OK" },
                                { "from": "idle", "to": "idle", "event": "BAD" }
                            ]
                        } }]
                }]
            })
        }

        fn value(json: Json) -> Value {
            serde_json::from_value(json).unwrap()
        }

        /// A session holding a shared arena with one block, and the guest opened over it with `grants` and that arena.
        fn opened(parent: Json, grants: Json) -> Session {
            let mut session = Session::new(parent);
            session.ok("NEW_SHARED");
            session.ok("ALLOC");
            open(&mut session, grants);
            session
        }

        fn open(session: &mut Session, grants: Json) {
            let arena = session.held.get("arena").cloned().expect("the session holds an arena");
            session.held.insert("program".into(), value(guest_program()));
            session.held.insert("grants".into(), value(grants));
            session.held.insert("arenas".into(), Value::array(vec![arena]));
            let opened = session.ok("OPEN");
            let handle = opened.get("result").and_then(|result| result.get("guest")).cloned().expect("program/open answers the guest handle");
            session.held.insert("guest".into(), handle);
        }

        /// `event` held for the open guest at `address` with `data`.
        fn spawn_at(session: &Session, event: &str, address: &str, data: ValueMap) -> EventRequest {
            let mut held = session.held.clone();
            held.insert("address".into(), Value::string(address.to_string()));
            held.insert("data".into(), Value::object(data));
            request(event, &held)
        }

        fn arena_data(session: &Session) -> ValueMap {
            let mut data = ValueMap::default();
            data.insert("arena".into(), session.held.get("arena").cloned().expect("the session holds an arena"));
            data
        }

        fn atomic_grants() -> Json {
            json!({ "mem": [{ "target": "shared:64", "kinds": ["atomic"] }] })
        }

        fn emitted_payload(response: &EventResponse, event: &str) -> Value {
            response.emitted_events.iter().find(|e| e.event == event).and_then(|e| e.payload.clone()).unwrap_or_else(|| panic!("{event} was not emitted: {:?}", response.emitted_events))
        }

        fn open_refusal(parent: Json, grants: Json) -> String {
            let mut session = Session::new(parent);
            session.ok("NEW_SHARED");
            session.ok("ALLOC");
            let arena = session.held.get("arena").cloned().expect("an arena");
            session.held.insert("program".into(), value(guest_program()));
            session.held.insert("grants".into(), value(grants));
            session.held.insert("arenas".into(), Value::array(vec![arena]));
            let (response, seen) = drive(&session.server, request("OPEN", &session.held), |request| panic!("program/open asks no host: {request:?}"));
            assert!(seen.is_empty());
            let bad = emitted_payload(&response, "BAD");
            format!("{} {}", bad.get("kind").and_then(Value::as_str).unwrap_or_default(), bad.get("error").and_then(Value::as_str).unwrap_or_default())
        }

        /// EXEC-CONC-0a primitive 3: a keyed guest dispatch runs on the guest's own lane and answers as a completion.
        #[test]
        fn a_keyed_dispatch_runs_the_guest_on_its_own_thread_and_its_atomic_add_on_a_shared_block_is_seen_by_the_parent() {
            let mut session = opened(parent_grants(), atomic_grants());
            let (host, settled) = native();
            let (response, seen) = drive(&session.server, spawn_at(&session, "SPAWN", "G.Work.BUMP", arena_data(&session)), |request| match request {
                IoRequest::Guest { .. } => host.answer(request),
                other => panic!("the fixture asks for {other:?}"),
            });
            assert!(response.success);
            assert!(matches!(seen.as_slice(), [IoRequest::Guest { key: Some(_), .. }]), "{seen:?}");
            let pending = session.server.kernel().pending_asks();
            assert!(
                matches!(pending.as_slice(), [ask] if ask.operator == "program/dispatch" && ask.key == "k1"),
                "the running guest is a pending ask of its key: {pending:?}"
            );
            let (site, key, outcome, thread) = settled.recv_timeout(Duration::from_secs(10)).expect("the guest settles later");
            assert_ne!(thread, std::thread::current().id(), "the guest ran on a thread of its own");
            let (event, payload) = substrate_completion_event(&site, &key, outcome).expect("the call site routes the outcome");
            assert_eq!(event, "DONE");
            assert_eq!(payload.get("key"), Some(&Value::string("k1")));
            assert!(payload.get("result").and_then(|result| result.get("outcomes")).is_some(), "{payload:?}");
            let mut completion = ValueMap::default();
            completion.insert("key".into(), Value::string("k1"));
            let (done, _) = drive(&session.server, request("DONE", &completion), |request| panic!("asked the host: {request:?}"));
            assert!(done.success, "the completion is an ordinary event of the parent");
            assert!(session.server.kernel().pending_asks().is_empty(), "the completion ends the guest's ask");
            assert_eq!(value_of(&session.ok("LOAD")), 5, "the guest's add is the parent's add");
        }

        #[test]
        fn an_unkeyed_dispatch_into_a_granted_guest_runs_in_place_through_the_host() {
            let mut session = opened(parent_grants(), atomic_grants());
            let (host, settled) = native();
            let (response, seen) = drive(&session.server, spawn_at(&session, "SPAWN_UNKEYED", "G.Work.BUMP", arena_data(&session)), |request| match request {
                IoRequest::Guest { .. } => host.answer(request),
                other => panic!("the fixture asks for {other:?}"),
            });
            assert!(matches!(seen.as_slice(), [IoRequest::Guest { key: None, .. }]), "{seen:?}");
            let done = emitted_payload(&response, "DONE");
            assert!(done.get("result").and_then(|result| result.get("outcomes")).is_some(), "answered in place: {done:?}");
            assert!(settled.try_recv().is_err(), "control: nothing settles later");
            assert_eq!(value_of(&session.ok("LOAD")), 5, "the add happened before the dispatch answered");
        }

        /// The events the guest emitted at `address`, run to completion on its own thread.
        fn guest_emitted(session: &Session, address: &str) -> Vec<Value> {
            let (host, settled) = native();
            drive(&session.server, spawn_at(session, "SPAWN", address, arena_data(session)), |request| match request {
                IoRequest::Guest { .. } => host.answer(request),
                other => panic!("the fixture asks for {other:?}"),
            });
            let (_, _, outcome, _) = settled.recv_timeout(Duration::from_secs(10)).expect("the guest settles");
            let payload = outcome.expect("the guest dispatch ran");
            match payload.get("result").and_then(|result| result.get("emitted")) {
                Some(Value::Array(emitted)) => emitted.iter().cloned().collect(),
                other => panic!("the guest reports what it emitted: {other:?}"),
            }
        }

        #[test]
        fn a_guest_borrows_a_frozen_shared_block_but_not_a_live_one() {
            let live = opened(parent_grants(), atomic_grants());
            let emitted = guest_emitted(&live, "G.Work.BORROW");
            assert_eq!(emitted[0].get("event"), Some(&Value::string("BAD")), "{emitted:?}");
            assert_eq!(emitted[0].get("payload").and_then(|p| p.get("kind")), Some(&Value::string("invalid")));
            let mut frozen = Session::new(parent_grants());
            frozen.ok("NEW_SHARED");
            frozen.ok("ALLOC");
            frozen.ok("FREEZE");
            open(&mut frozen, atomic_grants());
            let emitted = guest_emitted(&frozen, "G.Work.BORROW");
            assert_eq!(emitted[0].get("event"), Some(&Value::string("OK")), "{emitted:?}");
        }

        #[test]
        fn once_a_guest_holds_the_arena_the_owner_reaches_a_live_block_only_through_atomics() {
            let mut session = opened(parent_grants(), atomic_grants());
            session.fails_with("BORROW_X", "invalid");
            session.fails_with("BORROW_S", "invalid");
            assert_eq!(value_of(&session.ok("ADD")), 5, "control: atomics still reach it");
            session.ok("FREEZE");
            session.ok("BORROW_S");
            assert_eq!(session.ok("READ").get("data").and_then(Value::as_bytes).expect("bytes").to_vec(), vec![5, 0, 0, 0], "a frozen block stays readable and keeps the cell the atomic add left");
        }

        #[test]
        fn without_the_kernel_guest_grant_grants_or_arenas_at_open_and_a_keyed_dispatch_are_denied() {
            let grants = json!({ "mem": [{ "target": "shared:1024", "kinds": ["create", "atomic", "free"] }] });
            assert!(open_refusal(grants.clone(), atomic_grants()).starts_with("denied"), "a guest given grants needs the spawn grant");
            let mut bare = Session::new(grants);
            bare.held.insert("program".into(), value(guest_program()));
            let opened = bare.ok("OPEN_BARE");
            bare.held.insert("guest".into(), opened.get("result").and_then(|r| r.get("guest")).cloned().expect("a handle"));
            let (response, _) = drive(&bare.server, spawn_at(&bare, "SPAWN", "G.Work.OK", ValueMap::default()), |request| panic!("a refusal asks no host: {request:?}"));
            assert_eq!(emitted_payload(&response, "FAILED").get("kind"), Some(&Value::string("denied")), "a keyed dispatch gives the guest its own lane");
            let (in_place, _) = drive(&bare.server, spawn_at(&bare, "SPAWN_UNKEYED", "G.Work.OK", ValueMap::default()), |request| panic!("a guest without grants runs in this kernel: {request:?}"));
            assert!(in_place.emitted_events.iter().any(|e| e.event == "DONE"), "control: an unkeyed dispatch into a bare guest needs no grant");
        }

        #[test]
        fn a_guest_given_grants_beyond_the_openers_fails_naming_them() {
            let beyond = json!({ "mem": [{ "target": "shared:64", "kinds": ["atomic"] }], "sys": ["cpu"], "env": ["HOME"] });
            let refusal = open_refusal(parent_grants(), beyond);
            assert!(refusal.starts_with("denied"), "{refusal}");
            assert!(refusal.contains("sys [cpu]") && refusal.contains("env \"HOME\""), "{refusal}");
            assert!(!refusal.contains("mem \""), "the covered grant is not named: {refusal}");
        }

        #[test]
        fn only_an_arena_handed_over_at_open_crosses_and_grants_are_not_given_per_dispatch() {
            let session = opened(parent_grants(), atomic_grants());
            let mut block = ValueMap::default();
            block.insert("block".into(), session.held.get("block").cloned().expect("the session holds a block"));
            let (response, seen) = drive(&session.server, spawn_at(&session, "SPAWN", "G.Work.OK", block), |request| panic!("a refusal asks no host: {request:?}"));
            assert!(seen.is_empty());
            assert_eq!(emitted_payload(&response, "FAILED").get("kind"), Some(&Value::string("invalid")), "a block is not an arena");
            let mut other = Session::new(parent_grants());
            other.ok("NEW_SHARED");
            other.ok("ALLOC");
            open(&mut other, atomic_grants());
            other.ok("NEW_SHARED");
            let (response, _) = drive(&other.server, spawn_at(&other, "SPAWN", "G.Work.OK", arena_data(&other)), |request| panic!("a refusal asks no host: {request:?}"));
            assert_eq!(emitted_payload(&response, "FAILED").get("kind"), Some(&Value::string("invalid")), "an arena not handed over at open does not cross");
        }
    }

    #[test]
    fn atomic_add_answers_the_new_value_and_cas_swaps_only_on_a_match() {
        let mut session = Session::shared_block();
        assert_eq!(value_of(&session.ok("ADD")), 5);
        assert_eq!(value_of(&session.ok("ADD")), 10);
        assert_eq!(value_of(&session.ok("LOAD")), 10);
        let swapped = session.ok("CAS");
        assert_eq!((value_of(&swapped), swapped.get("swapped")), (10, Some(&Value::Bool(true))));
        let missed = session.ok("CAS");
        assert_eq!((value_of(&missed), missed.get("swapped")), (7, Some(&Value::Bool(false))));
        assert_eq!(value_of(&session.ok("LOAD")), 7);
    }

    #[test]
    fn a_step_that_parks_and_replays_adds_once() {
        let mut session = Session::shared_block();
        let (response, seen) = drive(&session.server, request("PARK", &session.held), |request| match request {
            IoRequest::Os { op: OsOp::ReadFile, .. } => IoAnswer::Os(Err(OsError::new(OsErrorKind::Denied, "no"))),
            other => panic!("the fixture asks for {other:?}"),
        });
        assert_eq!(seen.len(), 1);
        let Answer::Ok(added) = Session::answer(&response) else { panic!("the add answered") };
        assert_eq!(value_of(&added), 1);
        assert_eq!(value_of(&session.ok("LOAD")), 1);
    }

    #[test]
    fn an_atomic_op_on_a_local_block_is_invalid_and_without_the_atomic_grant_is_denied() {
        let mut local = Session::new(FULL());
        local.ok("NEW_BUMP");
        local.ok("ALLOC");
        local.fails_with("ADD", "invalid");
        let mut denied = Session::new(json!({ "mem": [{ "target": "shared:1024", "kinds": ["create"] }] }));
        denied.ok("NEW_SHARED");
        denied.ok("ALLOC");
        denied.fails_with("ADD", "denied");
    }

    #[test]
    fn freeze_then_write_fails_and_the_frozen_block_stays_readable() {
        let mut session = Session::new(FULL());
        session.ok("NEW_BUMP");
        session.ok("ALLOC");
        session.held.insert("data".into(), Value::bytes(b"abcd".to_vec()));
        session.ok("BORROW_X");
        session.ok("WRITE");
        session.ok("RELEASE_X");
        session.ok("FREEZE");
        session.fails_with("BORROW_X", "borrowed");
        session.ok("BORROW_S");
    }

    #[test]
    fn freezing_a_shared_block_refuses_atomic_writes_but_not_loads() {
        let mut session = Session::shared_block();
        session.ok("STORE");
        session.ok("FREEZE");
        session.fails_with("STORE", "borrowed");
        assert_eq!(value_of(&session.ok("LOAD")), 3);
    }

    #[test]
    fn a_shared_arena_needs_the_shared_grant() {
        let mut arena_only = Session::new(json!({ "mem": [{ "target": "arena:1024", "kinds": ["create"] }] }));
        arena_only.fails_with("NEW_SHARED", "denied");
        let mut session = Session::new(FULL());
        session.fails_with("NEW_BIG_SHARED", "denied");
        session.ok("NEW_SHARED");
    }

    fn failing(session: &Session, event: &str) {
        match session.server.start("Main", request(event, &session.held)) {
            Ok(ServerProgress::Done(response)) => assert!(!response.success, "{event} should be rejected"),
            Ok(_) => panic!("{event} should not park"),
            Err(_) => {}
        }
    }

    fn read_back(session: &mut Session) -> Vec<u8> {
        let read = session.ok("READ");
        read.get("data").and_then(Value::as_bytes).expect("bytes").to_vec()
    }

    #[test]
    fn a_failed_step_leaves_no_shared_freeze_behind_and_a_committed_one_is_visible() {
        let mut session = Session::shared_block();
        failing(&session, "FAIL_FREEZE");
        session.ok("STORE");
        session.ok("FREEZE");
        session.fails_with("STORE", "borrowed");
    }

    #[test]
    fn the_owner_initialises_a_committed_shared_block_before_any_guest_holds_it() {
        let mut session = Session::shared_block();
        session.ok("BORROW_X");
        session.held.insert("data".into(), Value::bytes(b"abcd".to_vec()));
        session.ok("WRITE");
        session.ok("RELEASE_X");
        session.ok("BORROW_S");
        let read = session.ok("READ");
        assert_eq!(read.get("data").and_then(Value::as_bytes).expect("bytes").to_vec(), b"abcd".to_vec());
        let written = i64::from(u32::from_le_bytes(*b"abcd"));
        assert_eq!(value_of(&session.ok("ADD")), written + 5, "control: atomics reach the written cell too");
    }

    #[test]
    fn a_frozen_shared_block_is_read_through_a_shared_borrow_and_takes_no_exclusive_one() {
        let mut session = Session::shared_block();
        session.ok("FREEZE");
        session.fails_with("BORROW_X", "borrowed");
        session.ok("BORROW_S");
        let read = session.ok("READ");
        assert_eq!(read.get("data").and_then(Value::as_bytes).expect("bytes").to_vec(), vec![0; 4]);
    }

    #[test]
    fn a_seed_step_reorder_naming_nothing_keeps_own_emits_ahead_of_listeners() {
        let events = order(FULL(), "GO_OWN");
        assert!(at(&events, "OWNED") < at(&events, "R1"), "{events:?}");
    }
}

mod gpu_effects {
    use super::*;
    use orbital_core_base::gpu::GpuAnswerer;
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
    use orbital_core_base::value::ValueMap;
    use orbital_core_runtime::os_host::{answer_os, OsHandles, OsHost};
    use std::sync::{Arc, Mutex};

    fn ops() -> Vec<(&'static str, Json)> {
        vec![
            ("OPEN", json!(["gpu/device-open", "@payload.desc"])),
            ("CLOSE", json!(["gpu/device-close", "@payload.device"])),
            ("BUF", json!(["gpu/buffer-create", "@payload.device", "@payload.desc"])),
            ("TEX", json!(["gpu/texture-create", "@payload.device", "@payload.desc"])),
            ("SHADER", json!(["gpu/shader-create", "@payload.device", "@payload.wgsl"])),
            ("PIPE", json!(["gpu/pipeline-create", "@payload.device", "@payload.desc"])),
            ("BIND", json!(["gpu/bind-group-create", "@payload.pipeline", "@payload.group", "@payload.entries"])),
            ("WRITE", json!(["gpu/submit", "@payload.device", [{ "write": { "buffer": "@payload.buffer", "offset": "@payload.offset", "data": "@payload.data" } }]])),
            ("SUBMIT", json!(["gpu/submit", "@payload.device", "@payload.commands"])),
            ("READ", json!(["gpu/submit", "@payload.device", [{ "read": { "buffer": "@payload.buffer", "offset": "@payload.offset", "length": "@payload.length" } }]])),
            ("RELEASE", json!(["gpu/release", "@payload.handle"])),
        ]
    }

    fn server_with(grants: Json, keyed_open: bool) -> EmbeddedServer {
        server_with_extra(grants, keyed_open, Vec::new())
    }

    fn server_with_extra(grants: Json, keyed_open: bool, extra: Vec<Json>) -> EmbeddedServer {
        let mut transitions: Vec<Json> = ops()
            .into_iter()
            .map(|(event, mut effect)| {
                effect.as_array_mut().unwrap().push(routes());
                json!({ "from": "idle", "to": "idle", "event": event, "effects": [effect] })
            })
            .collect();
        if keyed_open {
            transitions.push(json!({ "from": "idle", "to": "idle", "event": "OPEN_KEYED", "effects": [
                ["gpu/device-open", "@payload.desc", { "key": "k", "emit": { "success": "OK", "failure": "BAD" } }]
            ] }));
        }
        transitions.extend(extra);
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "OK" }));
        transitions.push(json!({ "from": "idle", "to": "idle", "event": "BAD" }));
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "osGrants": grants,
            "typeContractVersion": 1, "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": transitions } }]
            }]
        }))
        .unwrap();
        EmbeddedServer::new(schema).unwrap()
    }

    fn grants(max: u64, kinds: &[&str]) -> Json {
        json!({ "gpu": [{ "target": format!("device:{max}"), "kinds": kinds }] })
    }

    fn j(value: Json) -> Value {
        serde_json::from_value(value).unwrap()
    }

    fn record(fields: Vec<(&str, Value)>) -> Value {
        Value::object(fields.into_iter().map(|(key, value)| (key.into(), value)).collect::<ValueMap>())
    }

    /// The host: answers plain ids, one counter for every kind, and records what it was asked.
    #[derive(Default)]
    struct Fake {
        next: i64,
        asked: Vec<(OsOp, Vec<Value>)>,
    }

    impl Fake {
        fn answer(&mut self, request: &IoRequest) -> IoAnswer {
            let IoRequest::Os { op, args, .. } = request else { panic!("a gpu op asked {request:?}") };
            self.asked.push((*op, args.clone()));
            self.next += 1;
            let id = Value::Int(self.next);
            let fields = match op {
                OsOp::GpuDeviceOpen => vec![("device", id), ("features", j(json!([]))), ("limits", j(json!({})))],
                OsOp::GpuBufferCreate => vec![("buffer", id)],
                OsOp::GpuTextureCreate => vec![("texture", id)],
                OsOp::GpuShaderCreate => vec![("shader", id)],
                OsOp::GpuPipelineCreate => vec![("pipeline", id)],
                OsOp::GpuBindGroupCreate => vec![("bindGroup", id)],
                OsOp::GpuSubmit => {
                    let reads = args[1].as_array().map_or(0, |commands| commands.iter().filter(|c| c.get("read").is_some()).count());
                    vec![("reads", Value::array(vec![Value::bytes(vec![1, 2, 3]); reads]))]
                }
                _ => vec![],
            };
            IoAnswer::Os(Ok(record(fields).into()))
        }
    }

    #[derive(Debug)]
    enum Answer {
        Ok(Value),
        Failed(String),
    }

    struct Session {
        server: EmbeddedServer,
        held: ValueMap,
        fake: Fake,
    }

    impl Session {
        fn new(max: u64, kinds: &[&str]) -> Self {
            Self::on(server_with(grants(max, kinds), false))
        }

        fn on(server: EmbeddedServer) -> Self {
            Self { server, held: ValueMap::default(), fake: Fake::default() }
        }

        fn put(&mut self, key: &str, value: Value) {
            self.held.insert(key.into(), value);
        }

        fn held(&self, key: &str) -> Value {
            self.held.get(key).cloned().unwrap_or_else(|| panic!("nothing held as {key}"))
        }

        fn call(&mut self, event: &str) -> Answer {
            let request = EventRequest {
                event: event.to_string(),
                target_trait: Some("Host".to_string()),
                payload: Some(Value::object(self.held.clone())),
                ..Default::default()
            };
            let fake = &mut self.fake;
            let (response, _) = drive(&self.server, request, |request| fake.answer(request));
            let find = |name: &str| response.emitted_events.iter().find(|e| e.event == name);
            if let Some(ok) = find("OK") {
                let payload = ok.payload.clone().expect("an answer carries a payload");
                if let Value::Object(fields) = &payload {
                    for (key, value) in fields.iter() {
                        self.held.insert(key.clone(), value.clone());
                    }
                }
                return Answer::Ok(payload);
            }
            let bad = find("BAD").unwrap_or_else(|| panic!("neither OK nor BAD: {:?}", response.emitted_events));
            match bad.payload.as_ref().and_then(|p| p.get("kind")) {
                Some(Value::String(kind)) => Answer::Failed(kind.to_string()),
                other => panic!("a failure names its kind: {other:?}"),
            }
        }

        fn ok(&mut self, event: &str) {
            match self.call(event) {
                Answer::Ok(_) => {}
                other => panic!("{event} should succeed: {other:?}"),
            }
        }

        fn fails_with(&mut self, event: &str, kind: &str) {
            let before = self.fake.asked.len();
            match self.call(event) {
                Answer::Failed(found) => assert_eq!(found, kind, "{event}"),
                other => panic!("{event} should fail with {kind}: {other:?}"),
            }
            assert_eq!(self.fake.asked.len(), before, "{event} must not reach the host");
        }

        fn open(&mut self) {
            self.put("desc", j(json!({})));
            self.ok("OPEN");
        }

        fn buffer(&mut self, size: i64) -> Answer {
            self.put("desc", j(json!({ "size": size, "usage": ["storage"] })));
            self.call("BUF")
        }

        fn texture(&mut self, width: i64, height: i64, mips: i64) -> Answer {
            self.put("desc", j(json!({ "width": width, "height": height, "format": "rgba8unorm", "usage": ["render"], "mips": mips })));
            self.call("TEX")
        }

        /// A compute pipeline, a bind group over the held buffer, and a render pipeline, all on the held device.
        fn pipelines(&mut self) {
            self.put("wgsl", Value::string("fn main() {}"));
            self.ok("SHADER");
            let shader = self.held("shader");
            self.put("desc", record(vec![("kind", Value::string("compute")), ("shader", shader.clone()), ("entry", Value::string("main"))]));
            self.ok("PIPE");
            let compute = self.held("pipeline");
            self.put("compute", compute.clone());
            self.put("pipeline", compute);
            self.put("group", Value::Int(0));
            self.put("entries", Value::array(vec![record(vec![("binding", Value::Int(0)), ("buffer", self.held("buffer"))])]));
            self.ok("BIND");
            self.put("desc", record(vec![("kind", Value::string("render")), ("shader", shader), ("entry", Value::string("vs"))]));
            self.ok("PIPE");
            let render = self.held("pipeline");
            self.put("render", render);
        }

        fn command(&mut self, command: Value) -> Answer {
            self.put("commands", Value::array(vec![command]));
            self.call("SUBMIT")
        }

        fn compute_command(&self) -> Value {
            let body = record(vec![
                ("pipeline", self.held("compute")),
                ("bindGroups", Value::array(vec![self.held("bindGroup")])),
                ("dispatch", j(json!([1, 1, 1]))),
            ]);
            record(vec![("compute", body)])
        }
    }

    fn kind_of(answer: Answer) -> String {
        match answer {
            Answer::Failed(kind) => kind,
            Answer::Ok(payload) => panic!("expected a failure, got {payload:?}"),
        }
    }

    /// EXEC-GPU-4: a step that makes two gpu asks parks on the first and re-runs on its journal, so the first answer is
    /// replayed, never asked again, and the handle it mints and the bytes it books are counted once.
    #[test]
    fn a_step_replayed_on_its_journal_asks_each_gpu_op_once_and_books_its_bytes_once() {
        let two = json!({ "from": "idle", "to": "idle", "event": "TWO", "effects": [
            ["gpu/buffer-create", "@payload.device", "@payload.desc", { "emit": { "success": "FIRST", "failure": "BAD" } }],
            ["gpu/buffer-create", "@payload.device", "@payload.desc", { "emit": { "success": "SECOND", "failure": "BAD" } }]
        ] });
        let routes = vec![
            json!({ "from": "idle", "to": "idle", "event": "FIRST" }),
            json!({ "from": "idle", "to": "idle", "event": "SECOND" }),
        ];
        let run = || {
            let mut session = Session::on(server_with_extra(grants(32, &["open"]), false, [vec![two.clone()], routes.clone()].concat()));
            session.open();
            session.put("desc", j(json!({ "size": 16, "usage": ["storage"] })));
            let request = EventRequest {
                event: "TWO".to_string(),
                target_trait: Some("Host".to_string()),
                payload: Some(Value::object(session.held.clone())),
                ..Default::default()
            };
            let fake = &mut session.fake;
            let (response, _) = drive(&session.server, request, |request| fake.answer(request));
            let minted: Vec<Value> = ["FIRST", "SECOND"]
                .iter()
                .map(|name| {
                    let event = response.emitted_events.iter().find(|e| e.event == *name).unwrap_or_else(|| panic!("{name}: {:?}", response.emitted_events));
                    event.payload.as_ref().and_then(|p| p.get("buffer")).cloned().expect("a minted buffer")
                })
                .collect();
            let creates = session.fake.asked.iter().filter(|(op, _)| *op == OsOp::GpuBufferCreate).count();
            (session, minted, creates)
        };
        let (mut session, minted, creates) = run();
        assert_eq!(creates, 2, "the replayed first ask is answered from the journal, not asked again");
        assert_ne!(minted[0], minted[1], "each ask mints its own handle");
        assert!(minted.iter().all(|b| b.is_sealed_as("GpuBuffer")), "{minted:?}");
        assert_eq!(kind_of(session.buffer(1)), "invalid", "the two 16-byte buffers fill the 32-byte grant: booked once each");
        let (_, again, _) = run();
        assert_eq!(serde_json::to_string(&minted).unwrap(), serde_json::to_string(&again).unwrap(), "the same answers mint byte-identical handles");
    }

    #[test]
    fn a_device_open_without_a_gpu_grant_is_denied_without_asking_the_host() {
        let mut session = Session::on(server_with(json!({}), false));
        session.put("desc", j(json!({})));
        session.fails_with("OPEN", "denied");
        let mut control = Session::new(64, &["open"]);
        control.open();
        assert_eq!(control.fake.asked.len(), 1);
    }

    #[test]
    fn a_grant_without_open_denies_the_open_and_one_with_it_mints_a_device_carrying_the_max_bytes() {
        let mut session = Session::new(64, &["compute"]);
        session.put("desc", j(json!({})));
        session.fails_with("OPEN", "denied");
        let mut granted = Session::new(1024, &["open"]);
        granted.open();
        let device = granted.held("device");
        assert!(device.is_sealed_as("GpuDevice"));
        assert_eq!(device.get("maxBytes").and_then(Value::as_int), Some(1024));
        assert_eq!(device.get("id").and_then(Value::as_int), Some(1));
        assert!(granted.held("features").is_array(), "the rest of the answer is kept");
    }

    #[test]
    fn a_host_with_no_answerer_answers_unsupported_and_one_with_an_answerer_is_asked() {
        struct Recording(Mutex<Vec<(OsOp, Vec<Value>)>>);
        impl GpuAnswerer for Recording {
            fn answer(&self, op: OsOp, args: &[Value]) -> Result<Value, OsError> {
                self.0.lock().unwrap().push((op, args.to_vec()));
                Ok(record(vec![("device", Value::Int(9)), ("features", j(json!([]))), ("limits", j(json!({})))]))
            }
            fn close_all(&self) {}
        }
        let on_event = |_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {};
        let attempt = |handles: &OsHandles| {
            let server = server_with(grants(64, &["open"]), false);
            let os_grants = server.kernel().os_grants();
            let mut session = Session::on(server);
            session.put("desc", j(json!({})));
            let request = EventRequest {
                event: "OPEN".to_string(),
                target_trait: Some("Host".to_string()),
                payload: Some(Value::object(session.held.clone())),
                ..Default::default()
            };
            let (response, _) = drive(&session.server, request, |request| {
                answer_os(Some(&OsHost { grants: &os_grants, handles, on_event: &on_event, secrets: None }), request)
            });
            response
        };
        let bare = OsHandles::default();
        assert_eq!(payload_of(&attempt(&bare), "BAD")["kind"], json!("unsupported"));
        let installed = OsHandles::default();
        let recording = Arc::new(Recording(Mutex::new(Vec::new())));
        installed.install_gpu(recording.clone());
        let response = attempt(&installed);
        assert!(emitted(&response).iter().any(|(event, _)| *event == "OK"), "{:?}", emitted(&response));
        let asked = recording.0.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, OsOp::GpuDeviceOpen);
    }

    #[test]
    fn handles_reach_the_host_as_plain_ids_and_come_back_as_sealed_values() {
        let mut session = Session::new(1 << 20, &["open", "compute"]);
        session.open();
        assert!(matches!(session.buffer(16), Answer::Ok(_)));
        let buffer = session.held("buffer");
        assert!(buffer.is_sealed_as("GpuBuffer"));
        assert_eq!(
            (buffer.get("id").and_then(Value::as_int), buffer.get("device").and_then(Value::as_int), buffer.get("size").and_then(Value::as_int)),
            (Some(2), Some(1), Some(16))
        );
        let (op, args) = &session.fake.asked[1];
        assert_eq!(*op, OsOp::GpuBufferCreate);
        assert_eq!(args[0].as_int(), Some(1), "the device went as its id");
        assert!(args.iter().all(|arg| !matches!(arg, Value::Sealed(_))));
        session.pipelines();
        let asked = &session.fake.asked;
        let pipe = &asked.iter().find(|(op, _)| *op == OsOp::GpuPipelineCreate).unwrap().1;
        assert_eq!(pipe[1].get("shader").and_then(Value::as_int), Some(3), "a nested shader went as its id");
        let bind = &asked.iter().find(|(op, _)| *op == OsOp::GpuBindGroupCreate).unwrap().1;
        assert_eq!(bind[0].as_int(), Some(4));
        let entries = bind[2].as_array().unwrap();
        assert_eq!(entries[0].get("buffer").and_then(Value::as_int), Some(2));
        let compute = session.compute_command();
        assert!(matches!(session.command(compute), Answer::Ok(_)));
        let (op, args) = session.fake.asked.last().unwrap();
        assert_eq!(*op, OsOp::GpuSubmit);
        let command = args[1].as_array().unwrap()[0].get("compute").unwrap().clone();
        assert_eq!(command.get("pipeline").and_then(Value::as_int), Some(4));
        assert_eq!(command.get("bindGroups").and_then(Value::as_array).unwrap()[0].as_int(), Some(5));
    }

    #[test]
    fn a_forged_number_where_a_handle_is_expected_is_invalid_and_a_real_handle_is_not() {
        let mut session = Session::new(64, &["open"]);
        session.open();
        let device = session.held("device");
        session.put("device", Value::Int(1));
        session.put("desc", j(json!({ "size": 4, "usage": [] })));
        session.fails_with("BUF", "invalid");
        session.put("device", device.clone());
        session.ok("BUF");
        session.put("handle", Value::Int(2));
        session.fails_with("RELEASE", "invalid");
        session.put("handle", session.held("buffer"));
        session.ok("RELEASE");
        session.put("handle", device);
        session.fails_with("RELEASE", "invalid");
    }

    #[test]
    fn a_handle_of_the_wrong_kind_is_invalid() {
        let mut session = Session::new(64, &["open"]);
        session.open();
        session.ok_buffer(4);
        session.put("device", session.held("buffer"));
        session.fails_with("BUF", "invalid");
    }

    impl Session {
        fn ok_buffer(&mut self, size: i64) {
            assert!(matches!(self.buffer(size), Answer::Ok(_)));
        }
    }

    #[test]
    fn use_after_release_answers_closed_without_asking_the_host() {
        let mut session = Session::new(64, &["open"]);
        session.open();
        session.ok_buffer(8);
        session.put("handle", session.held("buffer"));
        session.put("offset", Value::Int(0));
        session.put("data", Value::bytes(vec![1, 2]));
        session.ok("WRITE");
        session.ok("RELEASE");
        assert_eq!(session.fake.asked.last().map(|(op, _)| *op), Some(OsOp::GpuRelease));
        session.fails_with("RELEASE", "closed");
        session.fails_with("WRITE", "closed");
    }

    #[test]
    fn closing_a_device_closes_its_handles() {
        let mut session = Session::new(64, &["open"]);
        session.open();
        session.ok_buffer(8);
        session.put("handle", session.held("buffer"));
        session.ok("CLOSE");
        session.fails_with("RELEASE", "closed");
        session.fails_with("BUF", "closed");
    }

    #[test]
    fn a_buffer_beyond_the_granted_max_bytes_is_invalid_without_asking_the_host_and_release_frees_the_bytes() {
        let mut session = Session::new(64, &["open"]);
        session.open();
        session.ok_buffer(64);
        let first = session.held("buffer");
        assert_eq!(kind_of(session.buffer(1)), "invalid");
        assert_eq!(session.fake.asked.len(), 2, "the refused buffer never reached the host");
        session.put("handle", first);
        session.ok("RELEASE");
        session.ok_buffer(64);
    }

    #[test]
    fn a_texture_is_counted_over_its_mip_levels() {
        let total = 4 * 4 * 4 + 2 * 2 * 4 + 4;
        let mut exact = Session::new(total, &["open"]);
        exact.open();
        assert!(matches!(exact.texture(4, 4, 3), Answer::Ok(_)));
        assert_eq!(kind_of(exact.buffer(1)), "invalid", "the mips used every granted byte");
        let mut short = Session::new(total - 1, &["open"]);
        short.open();
        assert_eq!(kind_of(short.texture(4, 4, 3)), "invalid");
        assert_eq!(short.fake.asked.len(), 1);
        let mut one_level = Session::new(4 * 4 * 4 + 1, &["open"]);
        one_level.open();
        assert!(matches!(one_level.texture(4, 4, 1), Answer::Ok(_)));
        one_level.ok_buffer(1);
    }

    #[test]
    fn a_malformed_command_list_is_invalid_and_a_well_formed_one_is_asked() {
        let mut session = Session::new(1 << 20, &["open", "compute"]);
        session.open();
        session.ok_buffer(16);
        session.pipelines();
        let compute = session.compute_command();
        let before = session.fake.asked.len();
        assert_eq!(kind_of(session.command(record(vec![]))), "invalid", "zero of the five");
        let both = record(vec![
            ("compute", compute.get("compute").unwrap().clone()),
            ("clear", record(vec![("buffer", session.held("buffer"))])),
        ]);
        assert_eq!(kind_of(session.command(both)), "invalid", "two of the five");
        assert_eq!(kind_of(session.command(record(vec![("compute", Value::Int(1))]))), "invalid");
        let plain = record(vec![("compute", record(vec![
            ("pipeline", Value::Int(4)), ("bindGroups", j(json!([]))), ("dispatch", j(json!([1, 1, 1]))),
        ]))]);
        assert_eq!(kind_of(session.command(plain)), "invalid", "a forged pipeline number");
        assert_eq!(session.fake.asked.len(), before);
        assert!(matches!(session.command(compute), Answer::Ok(_)));
        assert_eq!(session.fake.asked.len(), before + 1);
    }

    #[test]
    fn a_render_command_under_a_compute_only_grant_is_denied() {
        let mut session = Session::new(1 << 20, &["open", "compute"]);
        session.open();
        session.ok_buffer(16);
        session.pipelines();
        assert!(matches!(session.texture(2, 2, 1), Answer::Ok(_)));
        let render = record(vec![("render", record(vec![
            ("pipeline", session.held("render")),
            ("bindGroups", j(json!([]))),
            ("target", session.held("texture")),
            ("vertices", Value::Int(3)),
        ]))]);
        let before = session.fake.asked.len();
        assert_eq!(kind_of(session.command(render.clone())), "denied");
        assert_eq!(session.fake.asked.len(), before);
        let compute = session.compute_command();
        assert!(matches!(session.command(compute), Answer::Ok(_)));

        let mut rendering = Session::new(1 << 20, &["open", "render"]);
        rendering.open();
        rendering.ok_buffer(16);
        rendering.pipelines();
        assert!(matches!(rendering.texture(2, 2, 1), Answer::Ok(_)));
        let render = record(vec![("render", record(vec![
            ("pipeline", rendering.held("render")),
            ("bindGroups", j(json!([]))),
            ("target", rendering.held("texture")),
            ("vertices", Value::Int(3)),
        ]))]);
        assert!(matches!(rendering.command(render), Answer::Ok(_)));
    }

    #[test]
    fn a_readback_needs_the_readback_kind_and_stays_inside_the_buffer() {
        let mut denied = Session::new(64, &["open"]);
        denied.open();
        denied.ok_buffer(8);
        denied.put("offset", Value::Int(0));
        denied.put("length", Value::Int(8));
        denied.fails_with("READ", "denied");

        let mut granted = Session::new(64, &["open", "readback"]);
        granted.open();
        granted.ok_buffer(8);
        granted.put("offset", Value::Int(0));
        granted.put("length", Value::Int(8));
        granted.ok("READ");
        assert_eq!(granted.held("reads").as_array().map(|r| r[0].as_bytes()), Some(Some(&[1u8, 2, 3][..])));
        let (op, args) = granted.fake.asked.last().unwrap();
        assert_eq!(*op, OsOp::GpuSubmit);
        assert_eq!(args[1].as_array().unwrap()[0].get("read").and_then(|r| r.get("buffer")).and_then(Value::as_int), Some(2), "the host gets the plain buffer id");
        granted.put("length", Value::Int(9));
        granted.fails_with("READ", "invalid");
        granted.put("offset", Value::Int(4));
        granted.put("length", Value::Int(5));
        granted.fails_with("READ", "invalid");
    }

    #[test]
    fn a_read_without_offset_or_length_reads_the_whole_buffer_and_the_kernel_says_so() {
        let mut session = Session::new(64, &["open", "readback"]);
        session.open();
        session.ok_buffer(16);
        let read = record(vec![("read", record(vec![("buffer", session.held("buffer"))]))]);
        assert!(matches!(session.command(read), Answer::Ok(_)));
        let (_, args) = session.fake.asked.last().unwrap();
        let sent = args[1].as_array().unwrap()[0].get("read").cloned().unwrap();
        assert_eq!(sent.get("offset").and_then(Value::as_int), Some(0), "the host picks no default");
        assert_eq!(sent.get("length").and_then(Value::as_int), Some(16));
        let past = record(vec![("read", record(vec![("buffer", session.held("buffer")), ("offset", Value::Int(17))]))]);
        assert_eq!(kind_of(session.command(past)), "invalid", "an offset past the end has no rest to read");
    }

    #[test]
    fn a_write_names_exactly_one_target_and_a_texture_write_takes_no_offset() {
        let mut session = Session::new(1 << 20, &["open"]);
        session.open();
        session.ok_buffer(8);
        assert!(matches!(session.texture(4, 4, 1), Answer::Ok(_)));
        let data = Value::bytes(vec![0; 4]);
        let write = |fields: Vec<(&str, Value)>| record(vec![("write", record(fields))]);
        let both = write(vec![("buffer", session.held("buffer")), ("texture", session.held("texture")), ("data", data.clone())]);
        assert_eq!(kind_of(session.command(both)), "invalid");
        assert_eq!(kind_of(session.command(write(vec![("data", data.clone())]))), "invalid", "a write with no target");
        let offset = write(vec![("texture", session.held("texture")), ("offset", Value::Int(0)), ("data", data.clone())]);
        assert_eq!(kind_of(session.command(offset)), "invalid");
        let swapped = write(vec![("buffer", session.held("texture")), ("data", data.clone())]);
        assert_eq!(kind_of(session.command(swapped)), "invalid", "a texture handed as the buffer");
        let texture = write(vec![("texture", session.held("texture")), ("data", data.clone())]);
        assert!(matches!(session.command(texture), Answer::Ok(_)), "control: a texture write");
        let buffer = write(vec![("buffer", session.held("buffer")), ("data", data)]);
        assert!(matches!(session.command(buffer), Answer::Ok(_)), "control: a buffer write at the default offset");
        let (_, args) = session.fake.asked.last().unwrap();
        assert_eq!(args[1].as_array().unwrap()[0].get("write").and_then(|w| w.get("offset")).and_then(Value::as_int), Some(0));
    }

    #[test]
    fn one_submit_carries_writes_a_dispatch_and_reads_in_order_and_needs_readback_only_for_reads() {
        let mut session = Session::new(1 << 20, &["open", "compute"]);
        session.open();
        session.ok_buffer(8);
        session.pipelines();
        let write = record(vec![("write", record(vec![("buffer", session.held("buffer")), ("data", Value::bytes(vec![1; 8]))]))]);
        let read = record(vec![("read", record(vec![("buffer", session.held("buffer"))]))]);
        session.put("commands", Value::array(vec![write.clone(), session.compute_command()]));
        assert!(matches!(session.call("SUBMIT"), Answer::Ok(_)), "control: no read, no readback grant needed");
        session.put("commands", Value::array(vec![write.clone(), session.compute_command(), read.clone()]));
        assert_eq!(kind_of(session.call("SUBMIT")), "denied", "a read needs the readback kind");

        let mut granted = Session::new(1 << 20, &["open", "compute", "readback"]);
        granted.open();
        granted.ok_buffer(8);
        granted.pipelines();
        let write = record(vec![("write", record(vec![("buffer", granted.held("buffer")), ("data", Value::bytes(vec![1; 8]))]))]);
        let read = record(vec![("read", record(vec![("buffer", granted.held("buffer"))]))]);
        granted.put("commands", Value::array(vec![write, granted.compute_command(), read.clone(), read]));
        let Answer::Ok(payload) = granted.call("SUBMIT") else { panic!("the job runs") };
        assert_eq!(payload.get("reads").and_then(Value::as_array).map(Vec::len), Some(2), "one bytes per read, in order");
        let (_, args) = granted.fake.asked.last().unwrap();
        let kinds: Vec<String> = args[1].as_array().unwrap().iter().map(|c| c.as_object().unwrap().keys().next().unwrap().to_string()).collect();
        assert_eq!(kinds, ["write", "compute", "read", "read"], "the host gets the list in the order written");
    }

    #[test]
    fn a_write_takes_bytes_or_their_base64_text_and_refuses_anything_else() {
        let mut session = Session::new(64, &["open"]);
        session.open();
        session.ok_buffer(8);
        session.put("offset", Value::Int(0));
        session.put("data", Value::string("abc"));
        session.fails_with("WRITE", "invalid");
        session.put("data", Value::Int(7));
        session.fails_with("WRITE", "invalid");
        session.put("data", Value::bytes(vec![7]));
        session.ok("WRITE");
        let data_of = |args: &Vec<Value>| args[1].as_array().unwrap()[0].get("write").and_then(|w| w.get("data")).cloned().unwrap();
        let (op, args) = session.fake.asked.last().unwrap();
        assert_eq!(*op, OsOp::GpuSubmit);
        assert_eq!(data_of(args).as_bytes(), Some(&[7u8][..]));
        session.put("data", Value::bytes(vec![7; 9]));
        session.fails_with("WRITE", "invalid");
        let big = Value::bytes(vec![5u8; 8]);
        let sent = big.as_bytes().map(<[u8]>::as_ptr);
        session.put("data", big);
        session.ok("WRITE");
        let (_, args) = session.fake.asked.last().unwrap();
        assert_eq!(data_of(args).as_bytes().map(<[u8]>::as_ptr), sent, "a bytes payload reaches the host as the same buffer, never a copy");
        session.put("data", Value::string("CAk="));
        session.ok("WRITE");
        let (_, args) = session.fake.asked.last().unwrap();
        assert_eq!(data_of(args).as_bytes(), Some(&[8u8, 9][..]), "base64 text is bytes on the wire (EXEC-EVAL-11) and reaches the host as bytes");
    }

    #[test]
    fn a_deferred_device_open_completes_with_a_sealed_device_the_kernel_then_accepts() {
        let server = server_with(grants(256, &["open"]), true);
        let mut session = Session::on(server);
        session.put("desc", j(json!({})));
        let request = EventRequest {
            event: "OPEN_KEYED".to_string(),
            target_trait: Some("Host".to_string()),
            payload: Some(Value::object(session.held.clone())),
            ..Default::default()
        };
        let (response, seen) = drive(&session.server, request, |_| IoAnswer::ServiceDeferred);
        assert!(emitted(&response).iter().all(|(event, _)| *event != "OK" && *event != "BAD"));
        let IoRequest::Os { key: Some(key), .. } = &seen[0] else { panic!("a keyed os ask") };
        let outcome = Ok(record(vec![("device", Value::Int(5)), ("features", j(json!([]))), ("limits", j(json!({})))]));
        let completed = session.server.kernel().gpu_complete(key, outcome).expect("the open completes");
        let device = completed.get("device").cloned().unwrap();
        assert!(device.is_sealed_as("GpuDevice"));
        assert_eq!(device.get("maxBytes").and_then(Value::as_int), Some(256));
        assert_eq!(device.get("id").and_then(Value::as_int), Some(5));
        session.put("device", device);
        session.fake.next = 5;
        session.ok_buffer(8);

        let other = session.server.kernel().gpu_complete("someone-else", Ok(record(vec![("device", Value::Int(6))])));
        assert_eq!(other.unwrap().get("device").and_then(Value::as_int), Some(6), "control: an unknown key is returned as it stands");
    }

    fn wire_of(json: String) -> Json {
        serde_json::from_str(&json).expect("wire json")
    }

    fn device_answer() -> Value {
        record(vec![("device", Value::Int(1)), ("features", j(json!([]))), ("limits", j(json!({})))])
    }

    #[test]
    fn a_server_holding_gpu_asks_hands_the_host_a_ticket_and_takes_the_held_answer_back() {
        let server = server_with(grants(1 << 20, &["open"]), false);
        let request = json!({ "event": "OPEN", "targetTrait": "Host", "payload": { "desc": {} } }).to_string();
        let control = wire_of(server.start_json("Main", &request).unwrap());
        assert_eq!(control["requests"][0]["request"]["kind"], "os", "control: without hold_gpu the ask crosses whole: {control}");

        let server = server_with(grants(1 << 20, &["open"]), false);
        server.hold_gpu();
        let started = wire_of(server.start_json("Main", &request).unwrap());
        let pending = &started["requests"][0];
        assert_eq!(pending["request"]["kind"], "gpu", "{started}");
        assert!(pending["request"].get("args").is_none(), "the host never sees the arguments: {started}");
        let ticket = pending["request"]["ticket"].as_u64().unwrap();
        let ask = server.take_gpu_ask(ticket).expect("held");
        assert_eq!(ask.op, OsOp::GpuDeviceOpen);
        assert!(server.take_gpu_ask(ticket).is_none(), "a held ask is taken once");
        let held = server.hold_gpu_answer(Ok(device_answer()));
        let replies = json!([{ "id": pending["id"], "answer": { "kind": "gpuHeld", "ticket": held } }]).to_string();
        let done = wire_of(server.resume_json(started["flight"].as_u64().unwrap(), &replies).unwrap());
        let ok = done["response"]["emittedEvents"].as_array().unwrap().iter().find(|e| e["event"] == "OK").cloned();
        assert!(ok.is_some_and(|ok| ok["payload"]["device"]["id"] == 1), "the held answer reached the kernel, which minted the device: {done}");
    }

    #[test]
    fn a_held_deferred_completion_starts_as_a_request_naming_its_held_payload_once() {
        let server = server_with_extra(grants(1 << 20, &["open"]), false, vec![
            json!({ "from": "idle", "to": "idle", "event": "OPEN_HELD", "effects": [
                ["gpu/device-open", "@payload.desc", { "key": "h", "emit": { "success": "GOT", "failure": "BAD" } }]
            ] }),
            json!({ "from": "idle", "to": "idle", "event": "GOT", "effects": [["emit", "ECHO", { "device": "@payload.device" }]] }),
        ]);
        server.hold_gpu();
        let request = json!({ "event": "OPEN_HELD", "targetTrait": "Host", "payload": { "desc": {} } }).to_string();
        let started = wire_of(server.start_json("Main", &request).unwrap());
        let pending = &started["requests"][0];
        let ask = server.take_gpu_ask(pending["request"]["ticket"].as_u64().unwrap()).expect("held");
        assert!(ask.key.is_some() && ask.site.is_some(), "a keyed ask under a route defers");
        let replies = json!([{ "id": pending["id"], "answer": { "kind": "serviceDeferred" } }]).to_string();
        wire_of(server.resume_json(started["flight"].as_u64().unwrap(), &replies).unwrap());
        let completion = server.gpu_completion(&ask, Ok(device_answer())).expect("the success route");
        assert_eq!((completion.event.as_str(), completion.trait_name.as_str()), ("GOT", "Host"));
        let held = json!({ "event": completion.event, "targetTrait": completion.trait_name, "heldPayload": completion.held_payload }).to_string();
        let done = wire_of(server.start_json("Main", &held).unwrap());
        let echo = done["response"]["emittedEvents"].as_array().unwrap().iter().find(|e| e["event"] == "ECHO").cloned().expect("ECHO");
        assert_eq!(echo["payload"]["device"]["maxBytes"], 1 << 20, "the kernel minted the device from the held payload: {done}");
        assert!(server.start_json("Main", &held).unwrap_err().contains("no completion payload is held"), "a held payload is taken once");
    }

    #[test]
    fn a_deferred_open_that_failed_mints_nothing() {
        let server = server_with(grants(256, &["open"]), true);
        let mut session = Session::on(server);
        session.put("desc", j(json!({})));
        let request = EventRequest {
            event: "OPEN_KEYED".to_string(),
            target_trait: Some("Host".to_string()),
            payload: Some(Value::object(session.held.clone())),
            ..Default::default()
        };
        let (_, seen) = drive(&session.server, request, |_| IoAnswer::ServiceDeferred);
        let IoRequest::Os { key: Some(key), .. } = &seen[0] else { panic!("a keyed os ask") };
        let failed = session.server.kernel().gpu_complete(key, Err(OsError::new(OsErrorKind::Io, "no adapter")));
        assert_eq!(failed.unwrap_err().kind, OsErrorKind::Io);
    }
}
