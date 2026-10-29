use super::*;

fn log_of(src: &str) -> Option<(LogLevel, String)> {
    match parse_effect(&serde_json::from_str::<SExpression>(src).unwrap())? {
        Effect::Log { level, message, .. } => Some((level, message)),
        _ => None,
    }
}

fn custom_of(src: &str) -> (Vec<Value>, bool) {
    match parse_effect(&serde_json::from_str::<SExpression>(src).unwrap()) {
        Some(Effect::Custom { args, emit, .. }) => (args, emit.is_some()),
        other => panic!("not a custom effect: {other:?}"),
    }
}

#[test]
fn options_written_beside_emit_reach_the_op_as_its_last_argument() {
    let (args, routed) = custom_of(r#"["behavior/source", {"behavior": "x"}, {"file": true, "emit": {"success": "A", "failure": "B"}}]"#);
    assert!(routed);
    assert_eq!(args.len(), 2, "{args:?}");
    assert_eq!(args[1].as_object().and_then(|o| o.get("file")), Some(&Value::Bool(true)));
    assert!(args[1].as_object().is_some_and(|o| !o.contains_key("emit")), "{args:?}");
}

#[test]
fn control_an_emit_only_record_leaves_the_arguments_as_written() {
    let (args, routed) = custom_of(r#"["behavior/source", {"behavior": "x"}, {"key": "k", "emit": {"success": "A", "failure": "B"}}]"#);
    assert!(routed);
    assert_eq!(args.len(), 1, "{args:?}");
    let (args, _) = custom_of(r#"["program/step", "p", [], {"report": "raw"}, {"emit": {"success": "A", "failure": "B"}}]"#);
    assert_eq!(args.len(), 3, "a separate options record stays where it was: {args:?}");
}

#[test]
fn a_trailing_level_sets_the_log_level() {
    assert_eq!(log_of(r#"["log", "store retry", "warn"]"#), Some((LogLevel::Warn, "store retry".into())));
    assert_eq!(log_of(r#"["log", "miss", "debug"]"#), Some((LogLevel::Debug, "miss".into())));
    assert_eq!(log_of(r#"["log", "down", "error"]"#), Some((LogLevel::Error, "down".into())));
}

#[test]
fn control_a_log_without_a_level_is_info() {
    assert_eq!(log_of(r#"["log", "seeded"]"#), Some((LogLevel::Info, "seeded".into())));
}

#[test]
fn control_a_level_word_alone_is_the_message() {
    assert_eq!(log_of(r#"["log", "info"]"#), Some((LogLevel::Info, "info".into())));
}

#[test]
fn an_unknown_log_level_is_not_a_log_effect() {
    assert_eq!(log_of(r#"["log", "msg", "loud"]"#), None);
}

fn failed_with(src: &str) -> EffectResult {
    EffectResult::failed(parse_effect(&serde_json::from_str::<SExpression>(src).unwrap()).unwrap(), "no row")
}

// A failure the program routed to its declared `emit.failure` is handled, whatever the effect's kind (G-ORBITAL-CORE-035).
#[test]
fn a_failure_routed_to_a_declared_event_is_handled_for_every_effect_kind() {
    for src in [
        r#"["fetch", "Contact", {"id": "x", "emit": {"success": "LOADED", "failure": "LOAD_FAILED"}}]"#,
        r#"["persist", "create", "Contact", {"name": "a"}, {"emit": {"failure": "LOAD_FAILED"}}]"#,
    ] {
        assert_eq!(failed_with(src).routed_failure().as_deref(), Some("LOAD_FAILED"), "{src}");
    }
}

#[test]
fn control_a_failure_with_no_declared_failure_route_is_not_handled() {
    for src in [
        r#"["fetch", "Contact", {"id": "x", "emit": {"success": "LOADED"}}]"#,
        r#"["fetch", "Contact", {"id": "x"}]"#,
        r#"["persist", "create", "Contact", {"name": "a"}]"#,
    ] {
        assert_eq!(failed_with(src).routed_failure(), None, "{src}");
    }
}

#[test]
fn edge_a_success_is_never_a_routed_failure() {
    let fetch = parse_effect(&serde_json::from_str::<SExpression>(r#"["fetch", "Contact", {"emit": {"failure": "LOAD_FAILED"}}]"#).unwrap()).unwrap();
    assert_eq!(EffectResult::executed(fetch).routed_failure(), None);
}

#[test]
fn test_parse_emit_effect() {
    let expr: SExpression =
        serde_json::from_str(r#"["emit", "USER_REGISTERED", {"userId": "123"}]"#).unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Emit { event, payload } => {
            assert_eq!(event, "USER_REGISTERED");
            assert!(payload.as_object().is_some());
        }
        _ => panic!("Expected Emit effect"),
    }
}

#[test]
fn client_kind_matches_kinds() {
    // Every client-effect variant reports a kind that is listed in CLIENT_EFFECT_KINDS...
    let client_variants = [
        Effect::RenderUi {
            slot: "main".into(),
            content: Value::Null,
        },
        Effect::Navigate {
            path: "/".into(),
            params: None,
            options: None,
        },
        Effect::NavigateBack,
        Effect::Emit {
            event: "E".into(),
            payload: Value::Null,
        },
    ];
    let reported: Vec<&str> = client_variants
        .iter()
        .map(|e| e.client_kind().expect("client variant has a kind"))
        .collect();
    for kind in &reported {
        assert!(
            CLIENT_EFFECT_KINDS.contains(kind),
            "{kind} missing from CLIENT_EFFECT_KINDS"
        );
    }
    // ...and CLIENT_EFFECT_KINDS lists exactly those, nothing stale.
    assert_eq!(reported, CLIENT_EFFECT_KINDS);

    // A server effect is not client-targeted.
    assert_eq!(
        Effect::Fetch {
            entity_type: "User".into(),
            id: None,
            filter: None,
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            nearest: None,
            emit: None,
        }
        .client_kind(),
        None
    );
}

#[test]
fn parse_call_service_reads_the_key_option() {
    let expr: SExpression = serde_json::from_str(
        r#"["call-service", "test-svc", "run", {"items": []}, {"key": "batch-1", "emit": {"cancelled": "C"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::CallService { key, .. } => assert_eq!(key.as_ref().and_then(Value::as_str), Some("batch-1")),
        other => panic!("expected CallService, got {other:?}"),
    }
}

#[test]
fn parse_call_service_without_options_has_no_key() {
    let expr: SExpression =
        serde_json::from_str(r#"["call-service", "test-svc", "run", {}]"#).unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::CallService { key, .. } => assert_eq!(key, None),
        other => panic!("expected CallService, got {other:?}"),
    }
}

#[test]
fn parse_host_effects_read_their_argument_and_emit() {
    let random: SExpression =
        serde_json::from_str(r#"["random-bytes", 32, {"emit": {"success": "S", "failure": "F"}}]"#).unwrap();
    match parse_effect(&random).unwrap() {
        Effect::RandomBytes { bytes, emit, .. } => {
            assert_eq!(bytes, Value::Number(32.0));
            assert_eq!(emit.and_then(|e| e.success).as_deref(), Some("S"));
        }
        other => panic!("expected RandomBytes, got {other:?}"),
    }
    for (head, status) in [("credential-status", true), ("credential-forget", false)] {
        let expr: SExpression = serde_json::from_str(&format!(r#"["{head}", "tok", {{"emit": {{"success": "S"}}}}]"#)).unwrap();
        let parsed = parse_effect(&expr).unwrap();
        assert_eq!(parsed.operator(), head);
        assert_eq!(matches!(parsed, Effect::CredentialStatus { .. }), status);
        assert!(is_known_effect_name(head));
    }
    assert!(is_known_effect_name("random-bytes"));
    for op in crate::credential_admin::CredentialAdminOp::ALL {
        let request = if op.takes_request() { r#"{"service": "s"}, "# } else { "" };
        let expr: SExpression =
            serde_json::from_str(&format!(r#"["{}", {request}{{"emit": {{"success": "S", "failure": "F"}}}}]"#, op.name())).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CredentialAdmin { op: parsed, request, emit } => {
                assert_eq!(parsed, op);
                assert_eq!(request.as_object().is_some_and(|fields| fields.contains_key("service")), op.takes_request());
                assert_eq!(emit.and_then(|e| e.failure).as_deref(), Some("F"));
            }
            other => panic!("expected CredentialAdmin, got {other:?}"),
        }
        assert!(is_known_effect_name(op.name()));
        let bare: SExpression = serde_json::from_str(&format!(r#"["{}"]"#, op.name())).unwrap();
        assert_eq!(parse_effect(&bare).is_none(), op.takes_request(), "{}: only an effect that takes a request needs one", op.name());
    }
    assert!(!is_known_effect_name("credential-purge"));
    let check: SExpression = serde_json::from_str(
        r#"["access-check", {"entity": "Note", "action": "read", "row": {}, "viewer": null}, {"emit": {"success": "S", "failure": "F"}}]"#,
    )
    .unwrap();
    match parse_effect(&check).unwrap() {
        Effect::AccessCheck { request, emit } => {
            assert_eq!(request.get_path("entity"), Some(&Value::string("Note")));
            assert_eq!(emit.and_then(|e| e.failure).as_deref(), Some("F"));
        }
        other => panic!("expected AccessCheck, got {other:?}"),
    }
    assert!(is_known_effect_name("access-check"));
    assert!(parse_effect(&serde_json::from_str(r#"["access-check"]"#).unwrap()).is_none(), "control: the request is required");
}
