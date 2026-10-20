// `random-bytes`, `credential-status` and `credential-forget` through the kernel (EXEC-HTTP-2): one journaled
// host ask each, the principal comes from the dispatch's @user and never the program, and a failure fires
// the declared route with `{ error }` and no ask.

use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use orbital_core_runtime::credential_admin::CredentialStanding;
use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};
use {orbital_core_runtime::effects::response::{EventRequest, EventResponse}, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn schema(effect: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
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
                IoRequest::Random { bytes: 2 } => IoAnswer::Random(Ok(vec![0xfb, 0xff])),
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
            IoAnswer::Random(Ok(vec![0; good as usize]))
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
    let (ok, _) = run(keyed(json!(2)), go(None), |_| IoAnswer::Random(Ok(vec![0xfb, 0xff])));
    assert_eq!(payload_of(&ok, "OK"), json!({ "value": "-_8", "key": "evil" }));
    let (down, _) = run(keyed(json!(2)), go(None), |_| IoAnswer::Random(Err("entropy source down".to_string())));
    assert_eq!(payload_of(&down, "BAD"), json!({ "error": "entropy source down", "key": "evil" }));
    let (bad_count, _) = run(keyed(json!(0)), go(None), |request| panic!("asked {request:?}"));
    assert_eq!(payload_of(&bad_count, "BAD")["key"], "evil");
    let (plain, _) = run(json!(["random-bytes", 2, routes()]), go(None), |_| IoAnswer::Random(Ok(vec![0xfb, 0xff])));
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
    let wire =
        serde_json::to_value(IoAnswerWire::from(&IoAnswer::Random(Ok(vec![1, 2, 3])))).unwrap();
    assert_eq!(
        wire,
        json!({ "kind": "random", "result": { "ok": "AQID" } })
    );
    let back: IoAnswer = serde_json::from_value::<IoAnswerWire>(wire).unwrap().into();
    assert_eq!(back, IoAnswer::Random(Ok(vec![1, 2, 3])));
    let bad: IoAnswerWire =
        serde_json::from_value(json!({ "kind": "random", "result": { "ok": "!!" } })).unwrap();
    assert!(matches!(IoAnswer::from(bad), IoAnswer::Random(Err(_))));
}

#[test]
fn a_random_answer_and_a_secret_set_value_are_redacted_in_debug() {
    let random = format!("{:?}", IoAnswer::Random(Ok(vec![9, 9, 9])));
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

mod credential_admin_effects {
    use super::*;
    use orbital_core_base::language_data::{install_language_data, LanguageData};

    const REGISTRY: &str = r#"{ "services": {
        "apikeys": { "backend": "production", "credentials": [{ "envVar": "SALT", "required": true, "description": "salt" }] },
        "hosting": { "backend": "production", "credentials": [] }
    } }"#;

    fn install_registry() {
        let _ = install_language_data(LanguageData {
            core_version: "0.0.0".into(),
            root: std::path::PathBuf::new(),
            patterns: "{}".into(),
            component_mapping: "{}".into(),
            event_contracts: "{}".into(),
            services: REGISTRY.into(),
            binding_rules: "{}".into(),
            program_types: "{}".into(),
            themes: Vec::new(),
        });
    }

    fn admin() -> Option<Json> {
        Some(json!({ "id": "u1", "role": "admin" }))
    }

    fn secret_none(request: &IoRequest) -> Option<IoAnswer> {
        matches!(request, IoRequest::Secret { .. }).then(|| IoAnswer::Secret(Ok(None)))
    }

    #[test]
    fn credential_set_checks_the_role_the_store_and_the_declared_name_then_writes_through_one_store_ask() {
        install_registry();
        let effect = json!(["credential-set", {"service": "apikeys", "envVar": "SALT", "value": "salt-value-1111"}, routes()]);
        let (response, seen) = run(effect.clone(), go(admin()), |request| {
            secret_none(request).unwrap_or_else(|| match request {
                IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(Some(
                    orbital_core_runtime::credential_admin::StoreListing { enabled: true, entries: Vec::new() },
                ))),
                IoRequest::CredentialStoreWrite { .. } => IoAnswer::CredentialStoreWrite(Ok(())),
                other => panic!("the fixture asks for {other:?}"),
            })
        });
        assert_eq!(
            payload_of(&response, "OK"),
            json!({ "saved": true, "service": "apikeys", "envVar": "SALT", "last4": "1111" })
        );
        assert!(matches!(seen.last(), Some(IoRequest::CredentialStoreWrite { service, name, value }) if service == "apikeys" && name == "SALT" && value == "salt-value-1111"));
        assert!(!format!("{seen:?}").contains("salt-value"), "the stored value never shows in a Debug of the asks");

        let (denied, seen) = run(effect.clone(), go(signed_in()), |request| {
            secret_none(request).unwrap_or_else(|| panic!("a refused role asks nothing more, asked {request:?}"))
        });
        assert_eq!(
            payload_of(&denied, "BAD"),
            json!({ "error": "forbidden: \"set\" requires one of roles: admin, owner", "code": "AUTH_ERROR" })
        );
        assert_eq!(seen.len(), 1);
        let (anonymous, _) = run(effect, go(None), |request| {
            secret_none(request).unwrap_or_else(|| panic!("anonymous asks nothing more, asked {request:?}"))
        });
        assert_eq!(payload_of(&anonymous, "BAD")["code"], "AUTH_ERROR");
    }

    /// The app a console manages: its `github` orbital reads the `GITHUB_TOKEN` secret knob in an `http` auth slot.
    fn shown_program() -> Json {
        json!({ "name": "shop", "orbitals": [{ "name": "github", "traits": [{
            "name": "Repo", "config": { "token": { "type": "secret", "default": "GITHUB_TOKEN" } },
            "stateMachine": { "transitions": [{ "from": "idle", "to": "idle", "event": "SYNC", "effects": [
                ["http", "GET", "https://api.github.com", "/user", { "auth": { "scheme": "bearer", "credential": "@config.token" } }, { "emit": { "success": "OK" } }]
            ] }] }
        }] }] })
    }

    fn set_github_token(server: EmbeddedServer) -> (EventResponse, Vec<IoRequest>) {
        run_on(server, go(admin()), |request| {
            secret_none(request).unwrap_or_else(|| match request {
                IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(Some(
                    orbital_core_runtime::credential_admin::StoreListing { enabled: true, entries: Vec::new() },
                ))),
                IoRequest::CredentialStoreWrite { .. } => IoAnswer::CredentialStoreWrite(Ok(())),
                other => panic!("the fixture asks for {other:?}"),
            })
        })
    }

    fn console() -> EmbeddedServer {
        EmbeddedServer::new(schema(json!(["credential-set", {"service": "github", "envVar": "GITHUB_TOKEN", "value": "ghp-token-9876"}, routes()]))).unwrap()
    }

    // The integrations console manages the credentials of the app the host names (G-PLAYGROUND-RUNTIME-013).
    #[test]
    fn a_console_sets_a_credential_the_program_it_manages_declares() {
        install_registry();
        let server = console();
        server.set_managed_program(&shown_program());
        let (response, seen) = set_github_token(server);
        assert_eq!(payload_of(&response, "OK"), json!({ "saved": true, "service": "github", "envVar": "GITHUB_TOKEN", "last4": "9876" }));
        assert!(seen.iter().any(|r| matches!(r, IoRequest::CredentialStoreWrite { service, name, .. } if service == "github" && name == "GITHUB_TOKEN")));
    }

    #[test]
    fn control_a_console_managing_no_program_refuses_it_by_service() {
        install_registry();
        let (response, seen) = set_github_token(console());
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::CredentialStoreWrite { .. })));
        assert_eq!(payload_of(&response, "BAD")["error"], "Service \"github\" declares no credentials");
    }

    #[test]
    fn edge_naming_another_program_replaces_the_managed_one() {
        install_registry();
        let server = console();
        server.set_managed_program(&shown_program());
        server.set_managed_program(&json!({ "name": "blank", "orbitals": [] }));
        let (response, _) = set_github_token(server);
        assert_eq!(payload_of(&response, "BAD")["error"], "Service \"github\" declares no credentials");
    }

    #[test]
    fn an_undeclared_name_is_refused_before_any_write_is_asked() {
        install_registry();
        let (response, seen) = run(
            json!(["credential-set", {"service": "apikeys", "envVar": "OTHER", "value": "v"}, routes()]),
            go(admin()),
            |request| {
                secret_none(request).unwrap_or_else(|| match request {
                    IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(Some(
                        orbital_core_runtime::credential_admin::StoreListing { enabled: true, entries: Vec::new() },
                    ))),
                    other => panic!("the fixture asks for {other:?}"),
                })
            },
        );
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::CredentialStoreWrite { .. })));
        assert_eq!(
            payload_of(&response, "BAD"),
            json!({ "error": "\"OTHER\" is not a declared credential of \"apikeys\" (declared: SALT)", "code": null })
        );
    }

    #[test]
    fn credential_mode_is_validated_in_the_kernel_and_applied_through_one_flag_ask() {
        let (response, seen) = run(json!(["credential-mode", {"mode": "mock"}, routes()]), go(admin()), |request| {
            secret_none(request).unwrap_or_else(|| match request {
                IoRequest::CredentialModeWrite { mode } => IoAnswer::CredentialModeWrite(Ok(mode.clone())),
                other => panic!("the fixture asks for {other:?}"),
            })
        });
        assert_eq!(payload_of(&response, "OK"), json!({ "mode": "mock" }));
        assert!(matches!(seen.last(), Some(IoRequest::CredentialModeWrite { mode }) if mode == "mock"));
        let (bad, seen) = run(json!(["credential-mode", {"mode": "hybrid"}, routes()]), go(admin()), |request| {
            secret_none(request).unwrap_or_else(|| panic!("an invalid mode asks no flag, asked {request:?}"))
        });
        assert_eq!(payload_of(&bad, "BAD")["error"], "mode must be \"mock\" or \"real\"");
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::CredentialModeWrite { .. })));
    }

    #[test]
    fn the_catalog_includes_the_secret_knobs_the_registered_program_reads() {
        install_registry();
        let program: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "KeysOrbital",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "config": { "salt": { "type": "secret", "default": "PROGRAM_SALT", "description": "The program salt." } },
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [["credential-list", {"service": "KeysOrbital"}, routes()]] },
                            { "from": "idle", "to": "done", "event": "OK" },
                            { "from": "idle", "to": "failed", "event": "BAD" },
                            { "from": "idle", "to": "idle", "event": "PROBE", "effects": [["credential-status", {"credential": "@config.salt", "scope": "app"}, routes()]] }
                        ]
                    } }]
            }]
        }))
        .unwrap();
        let server = EmbeddedServer::new(program).unwrap();
        let mut progress = server.start("KeysOrbital", go(None)).unwrap();
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
                                IoRequest::Secret { .. } => IoAnswer::Secret(Ok(None)),
                                IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(None)),
                                IoRequest::CredentialModeRead => IoAnswer::CredentialModeRead(Ok(None)),
                                other => panic!("the fixture asks for {other:?}"),
                            },
                        })
                        .collect();
                    progress = server.resume(flight, replies).unwrap();
                }
            }
        };
        let listed = payload_of(&response, "OK");
        let entries = listed["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{listed}");
        assert_eq!(entries[0]["id"], "KeysOrbital/PROGRAM_SALT");
        assert_eq!(entries[0]["description"], "The program salt.");
        assert_eq!(entries[0]["source"], "none");
    }

    #[test]
    fn credential_rotate_and_remove_need_a_store_and_an_admin() {
        let (response, _) = run(json!(["credential-rotate", routes()]), go(admin()), |request| {
            secret_none(request).unwrap_or_else(|| match request {
                IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(None)),
                other => panic!("the fixture asks for {other:?}"),
            })
        });
        assert_eq!(payload_of(&response, "BAD")["error"], "No credential store is installed on this host");
        let (rotated, _) = run(json!(["credential-rotate", routes()]), go(admin()), |request| {
            secret_none(request).unwrap_or_else(|| match request {
                IoRequest::CredentialStoreList => IoAnswer::CredentialStoreList(Ok(Some(
                    orbital_core_runtime::credential_admin::StoreListing { enabled: true, entries: Vec::new() },
                ))),
                IoRequest::CredentialStoreRotate => IoAnswer::CredentialStoreRotate(Ok(orbital_core_runtime::credential_admin::RotateCounts {
                    rotated: 2,
                    already_current: 1,
                    unreadable: 0,
                })),
                other => panic!("the fixture asks for {other:?}"),
            })
        });
        assert_eq!(payload_of(&rotated, "OK"), json!({ "rotated": 2.0, "alreadyCurrent": 1.0, "unreadable": 0.0 }));
    }

    #[test]
    fn a_scrypt_salt_credential_is_resolved_by_the_kernel_and_never_reaches_program_data() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "Keys",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "config": { "salt": { "type": "secret", "default": "SALT_NAME", "description": "salt" } },
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [
                                ["let", [["h", ["hash/scrypt", "sk_live_", "@config.salt"]]],
                                    ["emit", "OK", { "hash": "@h", "salt": "@config.salt" }]]
                            ] },
                            { "from": "idle", "to": "done", "event": "OK" }
                        ]
                    } }]
            }]
        }))
        .unwrap();
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
                                        IoAnswer::Secret(Ok(salt.map(str::to_string)))
                                    }
                                    other => panic!("the fixture asks for {other:?}"),
                                },
                            })
                            .collect();
                        progress = server.resume(flight, replies).unwrap();
                    }
                }
            };
            (response, seen)
        };
        let (held, seen) = go(Some("00112233445566778899"));
        assert_eq!(seen, vec![("SALT_NAME".to_string(), None)]);
        let hashed = payload_of(&held, "OK");
        assert_eq!(hashed["hash"], "010bbcf1a211930bca10afa35c740b1c70dcb058711e165c9918f1137c97b940");
        assert_eq!(hashed["salt"], "SALT_NAME", "the program sees the credential's name, never its value");
        assert!(!hashed.to_string().contains("00112233445566778899"));
        let (absent, _) = go(None);
        assert_eq!(payload_of(&absent, "OK")["hash"], Json::Null, "no value held: the hash is null");
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
            IoAnswer::CredentialStoreList(Ok(Some(StoreListing { enabled: true, entries: vec![StoredName { service: "s".into(), name: "n".into() }] }))),
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
        let IoRequest::Os { op, args, key, site } = &seen[0] else { panic!("an os ask") };
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

    #[test]
    fn a_stream_with_no_key_fails_before_any_ask() {
        let (response, seen) = run(
            json!(["os/stdin-read", routes()]),
            go(None),
            |request| panic!("a keyless stream must not ask the host, asked {request:?}"),
        );
        assert!(seen.is_empty());
        assert_eq!(
            payload_of(&response, "BAD"),
            json!({ "error": "os/stdin-read: a stream needs a key", "kind": "invalid" })
        );
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
            args: vec![Value::String("db.internal".into()), Value::Number(5432.0)],
            key: Some("k".into()),
            site: None,
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
    }

    #[test]
    fn the_wrong_arity_and_a_non_object_option_do_not_lower() {
        for form in [
            json!(["random-bytes"]),
            json!(["random-bytes", 1, {}, {}]),
            json!(["credential-status"]),
            json!(["credential-forget", "a", "not-an-object"]),
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
    fn every_os_effect_runs_on_the_server_and_a_pure_path_op_does_not_run_anywhere_in_particular() {
        use orbital_core_base::runs_on::{runs_on, RunsOn};
        use orbital_core_base::os::OsOp;
        for op in OsOp::ALL {
            assert_eq!(runs_on(op.name()), RunsOn::Server, "{}", op.name());
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
            ProducedPayload::Shape(shape) if shape.iter().map(|f| f.name).collect::<Vec<_>>() == ["data"]
        ));
        assert!(matches!(
            message_payload("os/stdin-read"),
            ProducedPayload::Shape(shape) if shape.iter().map(|f| f.name).collect::<Vec<_>>() == ["data"]
        ));
        assert_eq!(message_payload("os/read-file"), ProducedPayload::Opaque);
        assert_eq!(success_payload("os/cpu-usage", CallVariant::Plain), ProducedPayload::Opaque);
        assert_eq!(success_payload("os/path-join", CallVariant::Plain), ProducedPayload::Opaque);
    }
}

mod oauth {
// `oauth-authorize`, `oauth-connection-status`, `oauth-connection-forget` and the `oauth-client` ops through the
// kernel: the driver owns every request, the host only persists, the principal is the dispatch's @user, and no
// token, verifier or vendor body reaches a payload, an event or a Debug.

    use super::*;
    use base64::Engine;
    use orbital_core_runtime::oauth::store::MemoryOAuthStore;
    use orbital_core_runtime::http_service::HttpResponse;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
    use std::sync::{Arc, Mutex};

    const ISSUER: &str = "https://idp.example.com";
    const DISCOVERY: &str = r#"{"issuer":"https://idp.example.com","authorization_endpoint":"https://idp.example.com/authorize","token_endpoint":"https://idp.example.com/token","revocation_endpoint":"https://idp.example.com/revoke","userinfo_endpoint":"https://idp.example.com/userinfo"}"#;
    const TOKEN_OK: &str = r#"{"access_token":"AT-SECRET","token_type":"Bearer","expires_in":3600,"refresh_token":"RT-SECRET","scope":"openid"}"#;

    fn client() -> Json {
        json!({"tag":"google","mode":"real","issuer":ISSUER,"clientId":"CID","clientSecret":"CSEC","clientAuth":"post"})
    }

    fn oauth_schema(effect: Json) -> OrbitalSchema {
        let mut value = serde_json::to_value(schema(effect)).unwrap();
        value["egress"] = json!([ISSUER]);
        serde_json::from_value(value).unwrap()
    }

    struct Host {
        store: MemoryOAuthStore,
        now: Arc<AtomicI64>,
        counter: AtomicU8,
        http: Mutex<Vec<HttpRequest>>,
        token: Mutex<(u16, String)>,
        /// What the person's consent answers: `None` is a host that asks no one, `Some(Err)` a host whose window failed.
        consent: Mutex<Option<Result<Option<String>, String>>>,
    }

    use orbital_core_runtime::http_service::HttpRequest;

    impl Host {
        fn new() -> Arc<Self> {
            let now = Arc::new(AtomicI64::new(1_000_000));
            let seen = now.clone();
            Arc::new(Self {
                store: MemoryOAuthStore::new(move || seen.load(Ordering::SeqCst)),
                now,
                counter: AtomicU8::new(1),
                http: Mutex::new(Vec::new()),
                token: Mutex::new((200, TOKEN_OK.to_string())),
                consent: Mutex::new(None),
            })
        }

        fn answer(&self, request: &IoRequest) -> IoAnswer {
            match request {
                IoRequest::Secret { name, .. } => IoAnswer::Secret(Ok(match name.as_str() {
                    "CID" => Some("client-id".to_string()),
                    "CSEC" => Some("client-secret".to_string()),
                    "GID" => Some("gh-id".to_string()),
                    "GSEC" => Some("gh-secret".to_string()),
                    _ => None,
                })),
                IoRequest::Random { bytes } => {
                    let fill = self.counter.fetch_add(1, Ordering::SeqCst);
                    IoAnswer::Random(Ok(vec![fill; *bytes as usize]))
                }
                IoRequest::OAuth { ask: ask @ orbital_core_base::oauth_ask::OAuthAsk::Consent { .. }, principal } => IoAnswer::OAuth(match self.consent.lock().unwrap().clone() {
                    Some(Ok(code)) => Ok(orbital_core_base::oauth_ask::OAuthReply::Code { code }),
                    Some(Err(message)) => Err(message),
                    None => self.store.answer(ask, principal.as_deref()),
                }),
                IoRequest::OAuth { ask, principal } => IoAnswer::OAuth(self.store.answer(ask, principal.as_deref())),
                IoRequest::Http { request, .. } => {
                    self.http.lock().unwrap().push(request.clone());
                    let json = vec![("content-type".to_string(), "application/json".to_string())];
                    let (status, body) = if request.url.ends_with("/.well-known/openid-configuration") {
                        (200, DISCOVERY.to_string())
                    } else {
                        self.token.lock().unwrap().clone()
                    };
                    IoAnswer::Http(Ok(HttpResponse { status, headers: json, body: body.into_bytes() }))
                }
                other => panic!("the fixture asks for {other:?}"),
            }
        }

        fn run(self: &Arc<Self>, effect: Json, user: Option<Json>) -> (EventResponse, Vec<IoRequest>) {
            let host = self.clone();
            let server = EmbeddedServer::new(oauth_schema(effect)).unwrap();
            let mut seen = Vec::new();
            let mut progress = server.start("Main", go(user)).unwrap();
            loop {
                match progress {
                    ServerProgress::Done(response) => return (response, seen),
                    ServerProgress::Await { flight, requests } => {
                        let replies = requests
                            .into_iter()
                            .map(|pending| {
                                if matches!(pending.request, IoRequest::ReadRow { .. }) {
                                    return IoReply { id: pending.id, answer: IoAnswer::Row(Err("no row".to_string())) };
                                }
                                seen.push(pending.request.clone());
                                IoReply { id: pending.id, answer: host.answer(&pending.request) }
                            })
                            .collect();
                        progress = server.resume(flight, replies).unwrap();
                    }
                }
            }
        }

        fn http_count(&self) -> usize {
            self.http.lock().unwrap().len()
        }
    }

    fn discovery() -> Json {
        json!(["http", "GET", ISSUER, "/.well-known/openid-configuration",
            {"auth": {"scheme":"oauth-client","op":"discovery","client":client()}}, routes()])
    }

    fn authorize(subject: &str) -> Json {
        json!(["oauth-authorize", client(), {"redirectUri":"https://app.example.com/cb","scope":["openid","email"],"subject":subject,
            "params":{"access_type":"offline"}}, routes()])
    }

    fn token_call(op: &str, provider: &str, state: &str) -> Json {
        json!(["http", "POST", "oauth-endpoint", "token",
            {"auth": {"scheme":"oauth-client","op":op,"client":client(),"provider":provider,"code":"C1","state":state}}, routes()])
    }

    fn user(id: &str) -> Option<Json> {
        Some(json!({"id": id, "role": "member"}))
    }

    fn started(host: &Arc<Host>, subject: &str) -> (String, String) {
        host.run(discovery(), user("u1"));
        let (response, _) = host.run(authorize(subject), user("u1"));
        let payload = payload_of(&response, "OK");
        (payload["authUrl"].as_str().unwrap().to_string(), payload["state"].as_str().unwrap().to_string())
    }

    fn failure(response: &EventResponse) -> (String, String) {
        let payload = payload_of(response, "BAD");
        (payload["reason"].as_str().unwrap().to_string(), payload["code"].as_str().unwrap().to_string())
    }

    #[test]
    fn the_pending_grant_and_the_connection_live_in_the_callers_credentials() {
        use orbital_core_runtime::http_service::SecretSource;
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let credentials = host.store.credentials();
        let grant = format!("oauth:grant:{state}");
        let held = credentials.secret_of(&grant, None).unwrap().expect("the grant is one app slot keyed by state");
        assert!(held.contains("\"subject\":\"u1\""), "the authorizing subject travels inside the record");
        assert_eq!(credentials.secret_of(&grant, Some("u1")), Ok(None));
        host.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(credentials.secret_of(&grant, None), Ok(None), "the grant is consumed");
        let held = credentials.secret_of("oauth:connection:google", Some("u1")).unwrap().expect("connection kept");
        assert!(held.contains("AT-SECRET"), "the tokens stay with the credential");
        assert_eq!(credentials.secret_of("oauth:connection:google", Some("u2")), Ok(None));
        assert_eq!(credentials.secret_of("oauth:connection:google", None), Ok(None));
    }

    #[test]
    fn authorize_then_connect_keeps_the_grant_with_the_host_and_shows_the_program_only_the_row() {
        let host = Host::new();
        let (auth_url, state) = started(&host, "u1");
        assert!(auth_url.starts_with("https://idp.example.com/authorize?"), "{auth_url}");
        assert!(auth_url.contains("access_type=offline") && auth_url.contains("code_challenge_method=S256"), "{auth_url}");
        let (response, seen) = host.run(token_call("connect", "google", &state), user("u1"));
        let row = payload_of(&response, "OK");
        assert_eq!(row["provider"], "google");
        assert_eq!(row["connected"], true);
        let sent = host.http.lock().unwrap().last().cloned().unwrap();
        assert_eq!(sent.url, "https://idp.example.com/token");
        let body = sent.text().to_string();
        assert!(body.contains("code=C1") && body.contains("client_secret=client-secret"), "{body}");
        let verifier = body.split('&').find_map(|pair| pair.strip_prefix("code_verifier=")).expect("verifier");
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert!(auth_url.contains(&format!("code_challenge={challenge}")), "the verifier answers the challenge");
        let everything = format!("{response:?} {seen:?} {row}");
        for secret in ["AT-SECRET", "RT-SECRET", verifier] {
            assert!(!everything.contains(secret), "{secret} leaked");
        }
        let (status, _) = host.run(json!(["oauth-connection-status", "google", routes()]), user("u1"));
        assert_eq!(payload_of(&status, "OK")["connected"], true);
        let (other, _) = host.run(json!(["oauth-connection-status", "google", routes()]), user("u2"));
        assert_eq!(payload_of(&other, "OK")["connected"], false);
    }

    #[test]
    fn a_host_that_asks_the_person_hands_the_consent_code_back_beside_the_url_and_state() {
        use orbital_core_base::oauth_ask::OAuthAsk;
        let host = Host::new();
        *host.consent.lock().unwrap() = Some(Ok(Some("CODE-FROM-BROWSER".to_string())));
        host.run(discovery(), user("u1"));
        let (response, seen) = host.run(authorize("u1"), user("u1"));
        let payload = payload_of(&response, "OK");
        assert_eq!(payload["code"], "CODE-FROM-BROWSER");
        let asked: Vec<&OAuthAsk> = seen
            .iter()
            .filter_map(|request| match request {
                IoRequest::OAuth { ask: ask @ OAuthAsk::Consent { .. }, .. } => Some(ask),
                _ => None,
            })
            .collect();
        assert_eq!(asked.len(), 1, "the person is asked once: {seen:?}");
        let OAuthAsk::Consent { auth_url, state, redirect_uri } = asked[0] else { unreachable!() };
        assert_eq!((auth_url.as_str(), state.as_str(), redirect_uri.as_str()), (payload["authUrl"].as_str().unwrap(), payload["state"].as_str().unwrap(), "https://app.example.com/cb"));
        let (connected, _) = host.run(token_call("connect", "google", &payload["state"].as_str().unwrap().to_string()), user("u1"));
        assert_eq!(payload_of(&connected, "OK")["connected"], true, "the code the consent returned completes the flow the usual way");
    }

    #[test]
    fn control_a_host_that_asks_no_one_declines_or_fails_the_window_and_a_mock_authorization_asks_nobody() {
        use orbital_core_base::oauth_ask::OAuthAsk;
        let consent_asks = |seen: &[IoRequest]| seen.iter().filter(|request| matches!(request, IoRequest::OAuth { ask: OAuthAsk::Consent { .. }, .. })).count();
        let none = Host::new();
        none.run(discovery(), user("u1"));
        let (response, seen) = none.run(authorize("u1"), user("u1"));
        assert_eq!(payload_of(&response, "OK")["code"], "", "a host that cannot ask answers no code");
        assert_eq!(consent_asks(&seen), 1);

        let declined = Host::new();
        *declined.consent.lock().unwrap() = Some(Ok(None));
        declined.run(discovery(), user("u1"));
        let (response, _) = declined.run(authorize("u1"), user("u1"));
        assert_eq!(payload_of(&response, "OK")["code"], "", "a declined consent is no code, never a stale one");

        let broken = Host::new();
        *broken.consent.lock().unwrap() = Some(Err("window closed".to_string()));
        broken.run(discovery(), user("u1"));
        let (response, _) = broken.run(authorize("u1"), user("u1"));
        assert_eq!(failure(&response).0, "store-unavailable");

        let mock = Host::new();
        *mock.consent.lock().unwrap() = Some(Ok(Some("MUST-NOT-APPEAR".to_string())));
        let (response, seen) = mock.run(
            json!(["oauth-authorize", mock_client(), {"redirectUri":"https://app.example.com/cb","scope":["openid","email"],"subject":"u1"}, routes()]),
            user("u1"),
        );
        assert_eq!(consent_asks(&seen), 0);
        assert_eq!(payload_of(&response, "OK")["code"], "");
    }

    #[test]
    fn a_state_is_taken_once_two_hosts_one_state_make_one_token_request() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let before = host.http_count();
        host.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(host.http_count(), before + 1);
        let (second, _) = host.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(failure(&second), ("state-unknown".to_string(), "AUTH_ERROR".to_string()));
        assert_eq!(host.http_count(), before + 1);
    }

    #[test]
    fn a_provider_mismatch_fails_after_the_take_and_before_any_http_and_a_retry_finds_nothing() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let before = host.http_count();
        let (response, _) = host.run(token_call("connect", "github", &state), user("u1"));
        assert_eq!(failure(&response).0, "provider-mismatch");
        assert_eq!(host.http_count(), before, "zero HTTP");
        let (retry, _) = host.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(failure(&retry).0, "state-unknown");
    }

    #[test]
    fn control_the_same_provider_connects() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let (response, _) = host.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(payload_of(&response, "OK")["connected"], true);
    }

    #[test]
    fn the_completing_subject_must_be_the_one_that_started_and_the_state_is_consumed() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let (response, _) = host.run(token_call("connect", "google", &state), user("u2"));
        assert_eq!(failure(&response).0, "subject-mismatch");
        let (retry, _) = host.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(failure(&retry).0, "state-unknown");
    }

    #[test]
    fn an_anonymous_connect_asks_no_store_and_sends_nothing() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let before = host.http_count();
        let (response, seen) = host.run(token_call("connect", "google", &state), None);
        assert_eq!(failure(&response).0, "not-signed-in");
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::OAuth { .. })), "{seen:?}");
        assert_eq!(host.http_count(), before);
        let (still, _) = host.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(payload_of(&still, "OK")["connected"], true, "the state was not consumed");
    }

    #[test]
    fn a_pending_grant_lives_exactly_ten_minutes_of_the_host_clock() {
        for (advance, expect_ok) in [(600_000, true), (600_001, false)] {
            let host = Host::new();
            let (_, state) = started(&host, "u1");
            host.now.fetch_add(advance, Ordering::SeqCst);
            let (response, _) = host.run(token_call("connect", "google", &state), user("u1"));
            if expect_ok {
                assert_eq!(payload_of(&response, "OK")["connected"], true);
            } else {
                assert_eq!(failure(&response).0, "state-expired");
            }
        }
    }

    #[test]
    fn status_and_forget_are_per_user_and_an_anonymous_viewer_is_denied() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        host.run(token_call("connect", "google", &state), user("u1"));
        let (anonymous, _) = host.run(json!(["oauth-connection-status", "google", routes()]), None);
        assert_eq!(failure(&anonymous).0, "not-signed-in");
        let (forget_other, _) = host.run(json!(["oauth-connection-forget", "google", routes()]), user("u2"));
        assert_eq!(payload_of(&forget_other, "OK")["connected"], false);
        let (still, _) = host.run(json!(["oauth-connection-status", "google", routes()]), user("u1"));
        assert_eq!(payload_of(&still, "OK")["connected"], true);
        host.run(json!(["oauth-connection-forget", "google", routes()]), user("u1"));
        let (gone, _) = host.run(json!(["oauth-connection-status", "google", routes()]), user("u1"));
        assert_eq!(payload_of(&gone, "OK")["connected"], false);
        let (unknown, _) = host.run(json!(["oauth-connection-status", "okta", routes()]), user("u1"));
        assert!(payload_of(&unknown, "BAD")["error"].as_str().unwrap().contains("provider must be google, github or auth0"));
    }

    #[test]
    fn a_store_the_host_does_not_have_fails_store_unavailable_before_the_caller_check() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let bare = Arc::new(Host {
            store: MemoryOAuthStore::new(|| 0).without_connections(),
            now: host.now.clone(),
            counter: AtomicU8::new(1),
            http: Mutex::new(Vec::new()),
            token: Mutex::new((200, TOKEN_OK.to_string())),
            consent: Mutex::new(None),
        });
        let (response, _) = bare.run(json!(["oauth-connection-status", "google", routes()]), None);
        assert_eq!(failure(&response).0, "store-unavailable");
        let (connect, _) = bare.run(token_call("connect", "google", &state), user("u1"));
        assert_eq!(failure(&connect).0, "store-unavailable");
    }

    #[test]
    fn a_rejecting_or_malformed_token_reply_fails_a_fixed_message_without_any_vendor_text() {
        for (status, body, reason) in [
            (400u16, r#"{"error":"invalid_grant","error_description":"LEAK-SECRET-TOKEN"}"#, "token-rejected"),
            (200, r#"{"access_token":"LEAK-SECRET-TOKEN""#, "response-invalid"),
        ] {
            let host = Host::new();
            let (_, state) = started(&host, "u1");
            *host.token.lock().unwrap() = (status, body.to_string());
            let (response, seen) = host.run(token_call("connect", "google", &state), user("u1"));
            assert_eq!(failure(&response).0, reason);
            let shown = format!("{response:?} {seen:?}");
            assert!(!shown.contains("LEAK-SECRET-TOKEN"), "{shown}");
            let error = payload_of(&response, "BAD")["error"].as_str().unwrap().to_string();
            assert!(!error.contains("invalid_grant"), "{error}");
            let (row, _) = host.run(json!(["oauth-connection-status", "google", routes()]), user("u1"));
            assert_eq!(payload_of(&row, "OK")["connected"], false, "nothing was saved");
        }
    }

    #[test]
    fn the_public_code_op_returns_the_legacy_token_fields() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let call = json!(["http", "POST", "oauth-endpoint", "token",
            {"auth": {"scheme":"oauth-client","op":"code","client":client(),"code":"C1","state":state,"subject":"u1"}}, routes()]);
        let (response, _) = host.run(call, user("u1"));
        let payload = payload_of(&response, "OK");
        assert_eq!(payload["accessToken"], "AT-SECRET");
        assert_eq!(payload["refreshToken"], "RT-SECRET");
        assert_eq!(payload["expiresIn"], 3600.0);
        assert_eq!(payload["tokenType"], "bearer");
        assert_eq!(payload["subject"], "u1");
    }

    #[test]
    fn discovery_is_cached_through_the_host_and_authorize_without_it_fails_metadata_invalid() {
        let host = Host::new();
        let (cold, _) = host.run(authorize("u1"), user("u1"));
        assert_eq!(failure(&cold).0, "metadata-invalid");
        host.run(discovery(), user("u1"));
        let first = host.http_count();
        host.run(discovery(), user("u1"));
        assert_eq!(host.http_count(), first, "the second discovery is a cache hit");
        let (warm, _) = host.run(authorize("u1"), user("u1"));
        assert!(payload_of(&warm, "OK")["authUrl"].as_str().unwrap().contains("state="));
    }

    #[test]
    fn a_missing_client_secret_fails_unconfigured_client_and_sends_nothing() {
        let host = Host::new();
        let before = host.http_count();
        let mut other = client();
        other["clientSecret"] = json!("MISSING");
        let call = json!(["http", "POST", "oauth-endpoint", "token",
            {"auth": {"scheme":"oauth-client","op":"refresh","client":other,"refreshToken":"R"}}, routes()]);
        let (response, _) = host.run(call, user("u1"));
        assert_eq!(failure(&response), ("unconfigured-client".to_string(), "NOT_CONFIGURED".to_string()));
        assert_eq!(host.http_count(), before);
    }

    #[test]
    fn authorize_with_an_unset_client_secret_fails_unconfigured_client_and_a_set_one_still_authorizes() {
        let host = Host::new();
        host.run(discovery(), user("u1"));
        let mut other = client();
        other["clientSecret"] = json!("MISSING");
        let (response, _) = host.run(json!(["oauth-authorize", other, {"redirectUri":"https://a/cb"}, routes()]), user("u1"));
        assert_eq!(failure(&response).0, "unconfigured-client");
        let (control, _) = host.run(authorize("u1"), user("u1"));
        assert!(payload_of(&control, "OK")["authUrl"].as_str().unwrap().contains("state="));
    }

    #[test]
    fn a_public_client_ignores_a_declared_secret_slot_and_sends_no_secret_while_a_secret_client_still_needs_it() {
        let public = || json!({"tag":"google","mode":"real","issuer":ISSUER,"clientId":"CID","clientSecret":"UNHELD","clientAuth":"none"});
        let host = Host::new();
        host.run(json!(["http", "GET", ISSUER, "/.well-known/openid-configuration", {"auth": {"scheme":"oauth-client","op":"discovery","client":public()}}, routes()]), user("u1"));
        let (response, seen) = host.run(json!(["oauth-authorize", public(), {"redirectUri":"https://app.example.com/cb","scope":["openid"],"subject":"u1"}, routes()]), user("u1"));
        let authorized = payload_of(&response, "OK");
        assert!(authorized["authUrl"].as_str().unwrap().contains("client_id=client-id"), "{authorized}");
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::Secret { name, .. } if name == "UNHELD")), "the unheld secret slot is never asked: {seen:?}");
        let state = authorized["state"].as_str().unwrap().to_string();
        let exchange = json!(["http", "POST", "oauth-endpoint", "token", {"auth": {"scheme":"oauth-client","op":"connect","client":public(),"provider":"google","code":"C1","state":state}}, routes()]);
        let (connected, seen) = host.run(exchange, user("u1"));
        assert_eq!(payload_of(&connected, "OK")["connected"], true);
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::Secret { name, .. } if name == "UNHELD")), "{seen:?}");
        let body = host.http.lock().unwrap().last().cloned().unwrap().text().to_string();
        assert!(body.contains("client_id=client-id") && !body.contains("client_secret"), "a public client sends only its id: {body}");
        // control: the same declaration as a secret client still fails unconfigured when the slot is unheld.
        let secret = json!({"tag":"google","mode":"real","issuer":ISSUER,"clientId":"CID","clientSecret":"UNHELD","clientAuth":"post"});
        let (refused, _) = host.run(json!(["oauth-authorize", secret, {"redirectUri":"https://app.example.com/cb"}, routes()]), user("u1"));
        assert_eq!(failure(&refused).0, "unconfigured-client");
    }

    fn mock_client() -> Json {
        json!({"tag":"google","mode":"mock","issuer":ISSUER,"clientId":"CID","clientAuth":"post"})
    }

    fn mock_call(op: &str, word: &str, method: &str, inputs: Json) -> Json {
        let mut auth = json!({"scheme":"oauth-client","op":op,"client":mock_client()});
        for (k, v) in inputs.as_object().unwrap() {
            auth[k] = v.clone();
        }
        json!(["http", method, "oauth-endpoint", word, {"auth": auth}, routes()])
    }

    fn mock_authorize() -> Json {
        json!(["oauth-authorize", mock_client(), {"redirectUri":"https://app.example.com/cb","scope":["openid","email"],"subject":"u1"}, routes()])
    }

    /// Authorize and complete a mock grant: (host, the `code` payload).
    fn mock_session() -> (Arc<Host>, Json) {
        let host = Host::new();
        let (response, seen) = host.run(mock_authorize(), user("u1"));
        let authorized = payload_of(&response, "OK");
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::Secret { .. } | IoRequest::Http { .. })), "mock asks no credential and sends nothing: {seen:?}");
        let state = authorized["state"].as_str().unwrap().to_string();
        let url = authorized["authUrl"].as_str().unwrap();
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?response_type=code&scope=openid+email&redirect_uri=https%3A%2F%2Fapp.example.com%2Fcb&state=")
                && url.ends_with("&client_id=mock-client-id"),
            "{url}"
        );
        assert_eq!(state.len(), 32);
        let code = mock_call("code", "token", "POST", json!({"code":"C","state":state,"subject":"u1"}));
        let (response, seen) = host.run(code, user("u1"));
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::Secret { .. } | IoRequest::Http { .. })), "{seen:?}");
        let tokens = payload_of(&response, "OK");
        (host, tokens)
    }

    fn userinfo(host: &Arc<Host>, access: &str) -> EventResponse {
        host.run(mock_call("userinfo", "userinfo", "GET", json!({"accessToken":access})), user("u1")).0
    }

    #[test]
    fn a_mock_grant_mints_tokens_with_the_provider_semantics_and_asks_no_credential() {
        let (host, tokens) = mock_session();
        assert_eq!(tokens["accessToken"].as_str().unwrap().len(), 64);
        assert_eq!(tokens["refreshToken"].as_str().unwrap().len(), 64);
        assert_eq!((tokens["expiresIn"].clone(), tokens["tokenType"].clone()), (json!(3600.0), json!("bearer")));
        assert_eq!(tokens["scope"], json!(["openid", "email"]));
        assert_eq!(tokens["subject"], "u1");
        let info = payload_of(&userinfo(&host, tokens["accessToken"].as_str().unwrap()), "OK");
        let sub = info["sub"].as_str().unwrap();
        assert_eq!(sub.len(), 16);
        assert_eq!(info["email"], format!("user-{}@google.example.com", &sub[..8]));
        assert_eq!(info["name"], "Mock User (google)");
        assert_eq!(info["picture"], format!("https://google.example.com/avatar/{}.png", &sub[..8]));
        let (replay, _) = host.run(mock_call("code", "token", "POST", json!({"code":"C","state":"nope","subject":"u1"})), user("u1"));
        assert_eq!(failure(&replay).0, "state-unknown");
    }

    #[test]
    fn mock_refresh_carries_the_session_over_and_revoke_removes_by_either_token() {
        let (host, tokens) = mock_session();
        let (access, refresh) = (tokens["accessToken"].as_str().unwrap(), tokens["refreshToken"].as_str().unwrap());
        let (refreshed, _) = host.run(mock_call("refresh", "token", "POST", json!({"refreshToken":refresh})), user("u1"));
        let renewed = payload_of(&refreshed, "OK");
        let new_access = renewed["accessToken"].as_str().unwrap();
        assert_ne!(new_access, access);
        assert_eq!(renewed["expiresIn"], 3600.0);
        assert_eq!(payload_of(&userinfo(&host, new_access), "OK")["name"], "Mock User (google)");
        assert_eq!(failure(&userinfo(&host, access)).0, "token-rejected", "the old access token is gone");
        let (unknown, _) = host.run(mock_call("refresh", "token", "POST", json!({"refreshToken":"nope"})), user("u1"));
        assert_eq!(failure(&unknown).0, "token-rejected");
        let revoke = |token: &str| payload_of(&host.run(mock_call("revoke", "revocation", "POST", json!({"token":token})), user("u1")).0, "OK")["revoked"].clone();
        assert_eq!(revoke("nope"), false);
        assert_eq!(revoke(refresh), true, "a refresh token revokes its access token");
        assert_eq!(failure(&userinfo(&host, new_access)).0, "token-rejected");
        assert_eq!(revoke(refresh), false);
    }

    #[test]
    fn a_mock_access_token_expires_by_the_host_clock_but_its_refresh_token_lives_on() {
        let (host, tokens) = mock_session();
        let (access, refresh) = (tokens["accessToken"].as_str().unwrap(), tokens["refreshToken"].as_str().unwrap());
        host.now.fetch_add(3_600_000, Ordering::SeqCst);
        assert_eq!(payload_of(&userinfo(&host, access), "OK")["name"], "Mock User (google)", "valid exactly at the boundary");
        host.now.fetch_add(1, Ordering::SeqCst);
        assert_eq!(failure(&userinfo(&host, access)).0, "token-rejected");
        let (refreshed, _) = host.run(mock_call("refresh", "token", "POST", json!({"refreshToken":refresh})), user("u1"));
        let renewed = payload_of(&refreshed, "OK");
        assert_eq!(
            failure(&userinfo(&host, renewed["accessToken"].as_str().unwrap())).0,
            "userinfo-mismatch",
            "the expired session was dropped, so the renewed token has none"
        );
    }

    #[test]
    fn a_mock_authorization_never_expires_and_the_subject_must_match() {
        let host = Host::new();
        let (response, _) = host.run(mock_authorize(), user("u1"));
        let state = payload_of(&response, "OK")["state"].as_str().unwrap().to_string();
        host.now.fetch_add(86_400_000, Ordering::SeqCst);
        let (other, _) = host.run(mock_call("code", "token", "POST", json!({"code":"C","state":state,"subject":"u2"})), user("u2"));
        assert_eq!(failure(&other).0, "subject-mismatch");
        let (again, _) = host.run(mock_call("code", "token", "POST", json!({"code":"C","state":state,"subject":"u1"})), user("u1"));
        assert_eq!(failure(&again).0, "state-unknown", "the mismatch consumed it");
        let (response, _) = host.run(mock_authorize(), user("u1"));
        let state = payload_of(&response, "OK")["state"].as_str().unwrap().to_string();
        host.now.fetch_add(86_400_000, Ordering::SeqCst);
        let (ok, _) = host.run(mock_call("code", "token", "POST", json!({"code":"C","state":state,"subject":"u1"})), user("u1"));
        assert_eq!(payload_of(&ok, "OK")["subject"], "u1");
    }

    #[test]
    fn a_mock_connect_saves_the_row_with_the_granted_scopes() {
        let host = Host::new();
        let (response, _) = host.run(mock_authorize(), user("u1"));
        let state = payload_of(&response, "OK")["state"].as_str().unwrap().to_string();
        let (connected, _) = host.run(mock_call("connect", "token", "POST", json!({"provider":"google","code":"C","state":state})), user("u1"));
        let row = payload_of(&connected, "OK");
        assert_eq!((row["connected"].clone(), row["scope"].clone()), (json!(true), json!(["openid", "email"])));
    }

    #[test]
    fn a_keyed_authorize_echoes_its_key_in_success_and_failure_and_an_unkeyed_one_says_null() {
        let host = Host::new();
        let keyed = |client: Json| json!(["oauth-authorize", client, {"redirectUri":"https://a/cb"}, {"key":"@payload.principal","emit":{"success":"OK","failure":"BAD"}}]);
        let (cold, _) = host.run(keyed(client()), user("u1"));
        let payload = payload_of(&cold, "BAD");
        assert_eq!((payload["reason"].clone(), payload["key"].clone()), (json!("metadata-invalid"), json!("evil")));
        host.run(discovery(), user("u1"));
        let (warm, _) = host.run(keyed(client()), user("u1"));
        assert_eq!(payload_of(&warm, "OK")["key"], "evil");
        let (plain, _) = host.run(json!(["oauth-authorize", client(), {"redirectUri":"https://a/cb"}, routes()]), user("u1"));
        assert!(payload_of(&plain, "OK")["key"].is_null());
        let (bad_option, _) = host.run(json!(["oauth-authorize", client(), {"nope":1}, {"key":"k","emit":{"failure":"BAD"}}]), user("u1"));
        assert_eq!(payload_of(&bad_option, "BAD")["key"], "k", "a configuration failure still echoes the key");
        let (empty, _) = host.run(json!(["oauth-authorize", client(), {"redirectUri":"u"}, {"key":"","emit":{"success":"OK","failure":"BAD"}}]), user("u1"));
        assert!(emitted(&empty).iter().all(|(event, _)| *event != "OK"), "an empty key never authorizes");
    }

    fn mock_for(tag: &str) -> Json {
        json!({"tag":tag,"mode":"mock","issuer":ISSUER,"clientId":"CID","clientAuth":"post"})
    }

    fn routed_code(clients: Json, state: &str) -> Json {
        json!(["http", "POST", "oauth-endpoint", "token",
            {"auth": {"scheme":"oauth-client","op":"code","clients":clients,"code":"C1","state":state,"subject":"u1"}}, routes()])
    }

    fn authorize_as(host: &Arc<Host>, client: Json) -> String {
        let (response, _) = host.run(json!(["oauth-authorize", client, {"redirectUri":"https://a/cb","scope":["x"],"subject":"u1"}, routes()]), user("u1"));
        payload_of(&response, "OK")["state"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_mock_token_answers_the_provider_that_issued_it_and_the_clients_map_routes_by_the_grants_provider() {
        let host = Host::new();
        let clients = json!({"google": mock_for("google"), "github": mock_for("github")});
        for (tag, domain) in [("github", "github.example.com"), ("google", "google.example.com")] {
            let state = authorize_as(&host, mock_for(tag));
            let (response, seen) = host.run(routed_code(clients.clone(), &state), user("u1"));
            let tokens = payload_of(&response, "OK");
            assert!(!seen.iter().any(|r| matches!(r, IoRequest::Http { .. })), "{seen:?}");
            let info = payload_of(&userinfo(&host, tokens["accessToken"].as_str().unwrap()), "OK");
            assert!(info["email"].as_str().unwrap().ends_with(&format!("@{domain}")), "{tag}: {info}");
            assert_eq!(info["name"], format!("Mock User ({tag})"));
        }
    }

    #[test]
    fn a_grant_whose_provider_is_not_declared_is_a_provider_mismatch_with_no_http_and_no_secret() {
        let host = Host::new();
        let state = authorize_as(&host, mock_for("auth0"));
        let before = host.http_count();
        let (response, seen) = host.run(routed_code(json!({"google": mock_for("google"), "github": mock_for("github")}), &state), user("u1"));
        assert_eq!(failure(&response).0, "provider-mismatch");
        assert_eq!(host.http_count(), before);
        assert!(!seen.iter().any(|r| matches!(r, IoRequest::Secret { .. })), "{seen:?}");
        let (retry, _) = host.run(routed_code(json!({"auth0": mock_for("auth0")}), &state), user("u1"));
        assert_eq!(failure(&retry).0, "state-unknown", "the take consumed it");
    }

    #[test]
    fn real_clients_ask_only_the_selected_clients_secrets_and_exchange_with_them() {
        let host = Host::new();
        host.run(discovery(), user("u1"));
        let github = json!({"tag":"github","mode":"real","endpoints":{"authorization":"https://idp.example.com/authorize","token":"https://idp.example.com/gh-token"},"clientId":"GID","clientSecret":"GSEC","clientAuth":"post"});
        let state = authorize_as(&host, github.clone());
        let clients = json!({"google": client(), "github": github});
        let (response, seen) = host.run(routed_code(clients, &state), user("u1"));
        assert_eq!(payload_of(&response, "OK")["accessToken"], "AT-SECRET");
        let asked: Vec<&str> = seen.iter().filter_map(|r| match r { IoRequest::Secret { name, .. } => Some(name.as_str()), _ => None }).collect();
        assert_eq!(asked, ["GID", "GSEC"], "only the selected client's secrets, after the take");
        let sent = host.http.lock().unwrap().last().cloned().unwrap();
        assert_eq!(sent.url, "https://idp.example.com/gh-token");
        let body = sent.text().to_string();
        assert!(body.contains("client_id=gh-id") && body.contains("client_secret=gh-secret"), "{body}");
    }

    #[test]
    fn control_a_single_client_op_code_still_works() {
        let host = Host::new();
        let (_, state) = started(&host, "u1");
        let call = json!(["http", "POST", "oauth-endpoint", "token",
            {"auth": {"scheme":"oauth-client","op":"code","client":client(),"code":"C1","state":state,"subject":"u1"}}, routes()]);
        assert_eq!(payload_of(&host.run(call, user("u1")).0, "OK")["subject"], "u1");
    }

    #[test]
    fn the_wire_round_trips_every_oauth_ask_and_reply() {
        use orbital_core_runtime::embed::wire::{IoAnswerWire, IoRequestWire};
        use orbital_core_base::oauth_ask::{OAuthAsk, OAuthProvider, OAuthReply};
        let asks = [
            OAuthAsk::PendingTake { state: "s".into() },
            OAuthAsk::ConnectionAvailable,
            OAuthAsk::ConnectionStatus { provider: OAuthProvider::Github },
            OAuthAsk::MetadataGet { issuer: ISSUER.into() },
            OAuthAsk::Clock,
        ];
        for ask in asks {
            let request = IoRequest::OAuth { ask, principal: Some("u".into()) };
            let wire = serde_json::to_string(&IoRequestWire::from(&request)).unwrap();
            assert!(wire.contains(r#""kind":"oauth""#), "{wire}");
            let back: IoRequest = serde_json::from_str::<IoRequestWire>(&wire).unwrap().into();
            assert_eq!(back, request);
        }
        let answer = IoAnswer::OAuth(Ok(OAuthReply::Millis { value: 7 }));
        let wire = serde_json::to_value(IoAnswerWire::from(&answer)).unwrap();
        assert_eq!(wire, json!({"kind":"oauth","result":{"ok":{"reply":"millis","value":7}}}));
        assert_eq!(IoAnswer::from(serde_json::from_value::<IoAnswerWire>(wire).unwrap()), answer);
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
            "orbitals": [{
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
        let store: Arc<dyn EntitySource> = Arc::new(InMemoryEntityCache::new());
        let kernel = Arc::new(RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), KernelConfig::default()));
        kernel.register_schema(schema).unwrap();
        let handles = Arc::new(OsHandles::default());
        let grants = Arc::new(OsGrants { exec: vec!["/bin/sh".to_string()], ..OsGrants::default() });
        let answer: Arc<CompletionAnswerer> = {
            let (handles, grants) = (handles.clone(), grants.clone());
            Arc::new(move |request: &IoRequest| match request {
                IoRequest::Os { .. } => {
                    let on_event = |_: Option<&CallSite>, _: Value| {};
                    answer_os(Some(&OsHost { grants: &grants, handles: &handles, on_event: &on_event }), request)
                }
                other => BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(other),
            })
        };
        let (tx, done) = mpsc::channel();
        let tx = Mutex::new(tx);
        let on_done: Arc<CompletionDone> = Arc::new(
            move |_site: &CallSite, event: &str, result: Result<DispatchOutcomes, DispatchError>| {
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
            "orbitals": [{
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
                answer_os(Some(&OsHost { grants: &grants, handles: &handles, on_event: &on_event }), request)
            }
            other => BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(other),
        });
        let on_done: Arc<CompletionDone> = Arc::new(|_: &CallSite, _: &str, _: Result<DispatchOutcomes, DispatchError>| {});
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
            "orbitals": [{
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
        let host = OsHost { grants: &grants, handles: &handles, on_event: &on_event };
        let spawn = IoRequest::Os {
            op: OsOp::Spawn,
            args: vec![
                Value::String("/bin/sh".into()),
                serde_json::from_value(json!(["-c", "printf hi"])).unwrap(),
                serde_json::from_value(json!({ "stdin": "null", "stdout": "pipe", "stderr": "null" })).unwrap(),
            ],
            key: None,
            site: None,
        };
        let IoAnswer::Os(Ok(spawned)) = answer_os(Some(&host), &spawn) else { panic!("spawn") };
        let fd = spawned.get_path("stdout").cloned().expect("a stdout fd");
        let read = IoRequest::Os { op: OsOp::Read, args: vec![fd, Value::Number(16.0)], key: None, site: None };
        assert!(matches!(answer_os(Some(&host), &read), IoAnswer::Os(Ok(_))));
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
        assert_eq!(held.entity.get_path("key"), Some(&Value::String("child".to_string())));
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
        assert_eq!(names, ["os/read", "os/wait", "os/recv", "os/recvfrom"]);
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

    struct NoNetwork;

    impl orbital_core_runtime::http_service::Transport for NoNetwork {
        fn send(
            &self,
            _: &orbital_core_runtime::http_service::HttpRequest,
        ) -> Result<orbital_core_runtime::http_service::HttpResponse, orbital_core_runtime::http_service::TransportError> {
            Err(orbital_core_runtime::http_service::TransportError::Failed("no network".to_string()))
        }
    }

    struct NoSecrets;

    impl orbital_core_runtime::http_service::SecretSource for NoSecrets {
        fn secret(&self, _: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    fn native_host() -> orbital_core_runtime::native_host::NativeHost {
        orbital_core_runtime::native_host::NativeHost::new(
            Shared::new(orbital_core_runtime::entity_source::InMemoryEntityCache::new()),
            Shared::new(NoNetwork),
            Shared::new(NoSecrets),
            Vec::new(),
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
            },
            ..OsHandles::default()
        });
        let (told, rx) = channel();
        handles.completions.set_sink(Shared::new(Collect(StdMutex::new(told))));
        let mut arrived: HashMap<String, Json> = HashMap::new();
        let on_event = |_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {};
        let host = OsHost { grants: &grants, handles: &handles, on_event: &on_event };
        let native = orbital_core_runtime::native_host::NativeHost::new(
            Shared::new(orbital_core_runtime::entity_source::InMemoryEntityCache::new()),
            Shared::new(NoNetwork),
            Shared::new(NoSecrets),
            Vec::new(),
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
            let request = IoRequest::Os { op, args, key: key.clone(), site };
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
                    let actual = serde_json::to_value(&value).unwrap();
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
        IoRequest::Random { bytes: 4 } => IoAnswer::Random(Ok(answer.clone())),
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
