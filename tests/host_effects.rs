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

fn payload_of<'a>(response: &'a EventResponse, event: &str) -> Json {
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
    let (response, seen) = run(json!(["random-bytes", 2, routes()]), go(None), |request| match request {
        IoRequest::Random { bytes: 2 } => IoAnswer::Random(Ok(vec![0xfb, 0xff])),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(seen, vec![IoRequest::Random { bytes: 2 }]);
    assert_eq!(payload_of(&response, "OK"), json!({ "value": "-_8" }));
}

#[test]
fn a_bad_count_fails_the_effect_before_any_ask() {
    for bad in [json!(0), json!(1025), json!(1.5), json!("many"), json!(-3)] {
        let (response, seen) = run(json!(["random-bytes", bad.clone(), routes()]), go(None), |request| {
            panic!("a bad count must not ask the host, asked {request:?}")
        });
        assert!(seen.is_empty(), "{bad}");
        let error = payload_of(&response, "BAD");
        assert!(
            error["error"].as_str().unwrap().starts_with("random-bytes: the byte count must be an integer from 1 to 1024"),
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
    assert_eq!(payload_of(&response, "BAD"), json!({ "error": "entropy source down" }));
}

#[test]
fn credential_status_takes_the_principal_from_the_dispatch_user_not_the_program() {
    let effect = json!(["credential-status", "tok_name", routes()]);
    let (response, seen) = run(effect.clone(), go(signed_in()), |request| match request {
        IoRequest::CredentialStatus { name, principal } if name == "tok_name" && principal.as_deref() == Some("u1") => {
            IoAnswer::CredentialStatus(Ok(true))
        }
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(
        seen,
        vec![IoRequest::CredentialStatus { name: "tok_name".into(), principal: Some("u1".into()) }]
    );
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": true }));

    let (response, _) = run(effect, go(signed_in()), |_| IoAnswer::CredentialStatus(Ok(false)));
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
    let (response, seen) = run(json!(["credential-forget", "tok_name", routes()]), go(signed_in()), |request| match request {
        IoRequest::CredentialForget { .. } => IoAnswer::CredentialForget(Ok(())),
        other => panic!("the fixture asks for {other:?}"),
    });
    assert_eq!(
        seen,
        vec![IoRequest::CredentialForget { name: "tok_name".into(), principal: Some("u1".into()) }]
    );
    assert_eq!(payload_of(&response, "OK"), json!({ "connected": false }));
}

#[test]
fn a_non_text_credential_fails_the_effect() {
    let (response, seen) = run(json!(["credential-status", 7, routes()]), go(signed_in()), |request| {
        panic!("a non-text credential must not ask the host, asked {request:?}")
    });
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
        IoRequest::CredentialStatus { name: "n".into(), principal: Some("u".into()) },
        IoRequest::CredentialForget { name: "n".into(), principal: None },
        IoRequest::Secret { name: "n".into(), principal: Some("u".into()) },
        IoRequest::SecretSet { name: "n".into(), value: "v".into(), ttl_ms: 5, principal: Some("u".into()) },
    ];
    for request in requests {
        let wire = serde_json::to_string(&IoRequestWire::from(&request)).unwrap();
        let back: IoRequest = serde_json::from_str::<IoRequestWire>(&wire).unwrap().into();
        assert_eq!(back, request, "{wire}");
    }
    let wire = serde_json::to_value(IoAnswerWire::from(&IoAnswer::Random(Ok(vec![1, 2, 3])))).unwrap();
    assert_eq!(wire, json!({ "kind": "random", "result": { "ok": "AQID" } }));
    let back: IoAnswer = serde_json::from_value::<IoAnswerWire>(wire).unwrap().into();
    assert_eq!(back, IoAnswer::Random(Ok(vec![1, 2, 3])));
    let bad: IoAnswerWire = serde_json::from_value(json!({ "kind": "random", "result": { "ok": "!!" } })).unwrap();
    assert!(matches!(IoAnswer::from(bad), IoAnswer::Random(Err(_))));
}

#[test]
fn a_random_answer_and_a_secret_set_value_are_redacted_in_debug() {
    let random = format!("{:?}", IoAnswer::Random(Ok(vec![9, 9, 9])));
    assert!(!random.contains('9') && random.contains("redacted"), "{random}");
    let set = format!(
        "{:?}",
        IoRequest::SecretSet { name: "n".into(), value: "hunter2".into(), ttl_ms: 1, principal: None }
    );
    assert!(!set.contains("hunter2"), "{set}");
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
        let OirEffect::RandomBytes { emit, .. } = *lower(json!(["random-bytes", 16, routes.clone()])).unwrap() else {
            panic!("a random-bytes effect")
        };
        assert_eq!(emit.as_ref().and_then(|e| e.failure.clone()), Some("BAD".to_string()));
        let status = lower(json!(["credential-status", "@config.tok", routes.clone()])).unwrap();
        assert!(matches!(*status, OirEffect::CredentialStatus { .. }));
        let forget = lower(json!(["credential-forget", "@config.tok"])).unwrap();
        assert!(matches!(*forget, OirEffect::CredentialForget { emit: None, .. }));
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
}
