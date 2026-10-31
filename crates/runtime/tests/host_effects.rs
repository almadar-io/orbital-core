// `random-bytes`, `credential-status` and `credential-forget` through the kernel (EXEC-HTTP-2): one journaled
// host ask each, the principal comes from the dispatch's @user and never the program, and a failure fires
// the declared route with `{ error }` and no ask.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::credential_admin::CredentialStanding;
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
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

fn go(user: Option<Json>) -> EventRequest {
    EventRequest {
        event: "GO".to_string(),
        target_trait: Some("Host".to_string()),
        payload: Some(serde_json::from_value(json!({ "principal": "evil" })).unwrap()),
        user: user.map(|u| serde_json::from_value(u).unwrap()),
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
        IoAnswer::Os(Ok(serde_json::from_value(json!({ "data": text })).unwrap()))
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
        assert_eq!(payload_of(&response, "OK"), json!({ "data": "hello" }));
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
        assert_eq!(payload_of(&response, "OK"), json!({ "status": 404.0, "headers": [{ "name": "content-type", "value": "text/plain" }], "body": "bm9wZQ==" }), "a non-2xx status is a success answer");
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
        assert_eq!(payload_of(&response, "OK"), json!({ "status": 200.0, "headers": [{ "name": "content-type", "value": "text/event-stream" }], "body": "", "handle": 3.0 }));
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
        assert_eq!(key.as_deref(), Some("in"));
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
            site: None, principal: None,
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
        let check = lower(json!(["access-check", { "entity": "Note", "action": "read", "row": {}, "viewer": "@payload.who" }, routes])).unwrap();
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
        assert_eq!((*op, key.as_deref()), (OsOp::Wait, Some("k")));
        let site = site.as_ref().expect("a deferrable wait carries its call site");
        assert_eq!(site.trait_name, "Host");
        assert_eq!(site.emit.success.as_deref(), Some("OK"));
        assert_eq!(site.emit.failure.as_deref(), Some("BAD"));
        assert!(response.success);
        assert!(emitted(&response).iter().all(|(event, _)| *event != "OK" && *event != "BAD"), "{:?}", emitted(&response));
    }

    #[test]
    fn control_the_same_keyed_wait_answered_with_a_result_fires_the_success_route_with_its_key() {
        let (response, _) = run(keyed_wait(), go(None), |_| {
            IoAnswer::Os(Ok(serde_json::from_value(json!({ "code": 0 })).unwrap()))
        });
        assert_eq!(payload_of(&response, "OK"), json!({ "code": 0.0, "key": "k" }));
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
            site: None, principal: None,
        };
        let IoAnswer::Os(Ok(spawned)) = answer_os(Some(&host), &spawn) else { panic!("spawn") };
        let fd = spawned.expose().get_path("stdout").cloned().expect("a stdout fd");
        let read = IoRequest::Os { op: OsOp::Read, args: vec![fd, Value::Number(16.0)], key: None, site: None, principal: None };
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
        assert_eq!(held.entity.get_path("data"), Some(&Value::string("aGVsbG8=")));
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
        assert_eq!(held.entity.get_path("data"), Some(&Value::string("aGVsbG8=")));
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
        assert_eq!(held.entity.get_path("data"), Some(&Value::string("aGVsbG8=")));
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
        assert!(wired.handles.cancel_completion("child"));
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
            Some(Value::Number(n)) => *n,
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
        let rest = ["os/wait", "os/stdin-read", "os/send", "os/recv", "os/recvfrom", "os/irq-wait"];
        assert_eq!(names, finite_fs.iter().chain(rest.iter()).copied().collect::<Vec<_>>());
        assert!(!names.iter().any(|n| n == "os/cwd"), "control: os/cwd answers inline");
        let site = json!({ "trait": "Sup", "followOn": false, "emit": { "success": "DONE", "failure": "FAILED" } }).to_string();
        let ok = os_completion_event_json(&site, "k", &json!({ "ok": { "code": 3 } }).to_string()).unwrap();
        assert_eq!(serde_json::from_str::<Json>(&ok).unwrap(), json!({ "event": "DONE", "payload": { "code": 3.0, "key": "k" } }));
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

        let grants: OsGrants = serde_json::from_value(substitute(&vars, &fixture["grants"])).unwrap();
        let handles = Shared::new(OsHandles {
            anchors: orbital_core_runtime::os_host::OsAnchors {
                home: Some(anchors_root.join("home")),
                start: Some(anchors_root.join("start")),
                ..orbital_core_runtime::os_host::OsAnchors::default()
            },
            ..OsHandles::default()
        });
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
            let request = IoRequest::Os { op, args, key: key.clone(), site, principal: None };
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
        match answer_os(Some(&host), &IoRequest::Os { op, args, key: None, site: None, principal: None }) {
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
        json!(["access-check", { "entity": entity, "action": action, "row": { "ownerId": owner }, "viewer": viewer }, routes()])
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
        assert_eq!(payload_of(&no_viewer, "BAD")["error"], json!("access-check: the request needs `viewer` (null for nobody)"));
    }

    #[test]
    fn the_request_bindings_resolve_before_the_check() {
        let effect = json!(["access-check",
            { "entity": "Note", "action": "create", "row": { "ownerId": "@payload.principal" }, "viewer": { "id": "@payload.principal" } },
            routes()]);
        let (response, _) = verdict(effect, None);
        assert_eq!(payload_of(&response, "OK"), json!({ "allowed": true }));
    }
}

