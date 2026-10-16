// `random-bytes`, `credential-status` and `credential-forget` through the kernel (EXEC-HTTP-2): one journaled
// host ask each, the principal comes from the dispatch's @user and never the program, and a failure fires
// the declared route with `{ error }` and no ask.

use orbital_core::embed::{EmbeddedServer, ServerProgress};
use orbital_core::runtime::resumable::{IoAnswer, IoReply, IoRequest};
use orbital_core::{EventRequest, EventResponse, OrbitalSchema, Value};
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
    mut host: impl FnMut(&IoRequest) -> IoAnswer,
) -> (EventResponse, Vec<IoRequest>) {
    let server = EmbeddedServer::new(schema(effect)).unwrap();
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
                IoAnswer::CredentialStatus(Ok(true))
            },
        );
        assert_eq!(
            seen,
            vec![IoRequest::CredentialStatus {
                name: "KEY".into(),
                principal: None
            }]
        );
        assert_eq!(payload_of(&response, "OK"), json!({"connected": true}));
    }
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
        IoAnswer::CredentialStatus(Ok(false))
    });
    assert_eq!(seen.len(), 1);
    assert_eq!(payload_of(&response, "OK"), json!({"connected": false}));

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
            IoAnswer::CredentialStatus(Ok(true))
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
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": true }));

    let (response, _) = run(effect, go(signed_in()), |_| {
        IoAnswer::CredentialStatus(Ok(false))
    });
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": false }));
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
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": false }));
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
    use orbital_core::embed::wire::{IoAnswerWire, IoRequestWire};
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

mod os_effects {
    use super::*;
    use orbital_core::runtime::os::{OsError, OsErrorKind, OsOp};

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
        assert_eq!(payload_of(&response, "OK"), json!({ "eof": true }));
    }

    #[test]
    fn the_wire_round_trips_an_os_ask_and_its_answers() {
        use orbital_core::embed::wire::{IoAnswerWire, IoRequestWire};
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
        use orbital_core::embed::wire::{IoAnswerWire, IoRequestWire};
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
    use orbital_core::ir::{OirEffect, OirExpr};
    use orbital_core::schema::SExpression;
    use serde_json::{json, Value as Json};

    fn lower(form: Json) -> Result<Box<OirEffect>, String> {
        let form: SExpression = serde_json::from_value(form).unwrap();
        match orbital_core::lower::lower_sexpr(&form) {
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
        use orbital_core::runtime::os::OsOp;
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
        use orbital_core::effects::{runs_on, RunsOn};
        use orbital_core::runtime::os::OsOp;
        for op in OsOp::ALL {
            assert_eq!(runs_on(op.name()), RunsOn::Server, "{}", op.name());
        }
        assert_eq!(runs_on("os/path-join"), RunsOn::Any);
        assert_eq!(runs_on("os/fork"), RunsOn::Any);
    }

    #[test]
    fn each_os_effect_names_its_success_payload_and_one_failure_shape() {
        use orbital_core::effects::{failure_payload, message_payload, success_payload, CallVariant, ProducedPayload};
        use orbital_core::runtime::os::OsOp;
        for op in OsOp::ALL {
            assert!(
                matches!(failure_payload(op.name(), false), ProducedPayload::Shape(shape) if shape.len() == 2),
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
    use orbital_core::oauth::store::MemoryOAuthStore;
    use orbital_core::runtime::http_service::HttpResponse;
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
    }

    use orbital_core::runtime::http_service::HttpRequest;

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
        use orbital_core::embed::wire::{IoAnswerWire, IoRequestWire};
        use orbital_core::oauth::ask::{OAuthAsk, OAuthProvider, OAuthReply};
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

