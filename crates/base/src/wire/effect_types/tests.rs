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
fn the_oauth_effects_parse_with_their_arity_and_reject_the_rest() {
    let parse = |src: &str| parse_effect(&serde_json::from_str::<SExpression>(src).unwrap());
    assert!(matches!(
        parse(r#"["oauth-authorize", {"tag":"google"}, {"redirectUri":"u"}, {"emit":{"success":"OK"}}]"#),
        Some(Effect::OAuthAuthorize { emit: Some(_), .. })
    ));
    assert!(matches!(parse(r#"["oauth-authorize", {}, {}]"#), Some(Effect::OAuthAuthorize { emit: None, .. })));
    assert!(matches!(parse(r#"["oauth-connection-status", "google"]"#), Some(Effect::OAuthConnectionStatus { .. })));
    assert!(matches!(
        parse(r#"["oauth-connection-forget", "github", {"emit":{"success":"OK"}}]"#),
        Some(Effect::OAuthConnectionForget { emit: Some(_), .. })
    ));
    for src in [r#"["oauth-authorize", {}]"#, r#"["oauth-authorize", {}, {}, {}, {}]"#, r#"["oauth-connection-status"]"#, r#"["oauth-connection-forget", "a", {}, {}]"#] {
        assert!(parse(src).is_none(), "{src}");
    }
    for head in ["oauth-authorize", "oauth-connection-status", "oauth-connection-forget"] {
        assert!(is_known_effect_name(head), "{head}");
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
            assert_eq!(request.get_path("entity"), Some(&Value::String("Note".into())));
            assert_eq!(emit.and_then(|e| e.failure).as_deref(), Some("F"));
        }
        other => panic!("expected AccessCheck, got {other:?}"),
    }
    assert!(is_known_effect_name("access-check"));
    assert!(parse_effect(&serde_json::from_str(r#"["access-check"]"#).unwrap()).is_none(), "control: the request is required");
}

#[test]
fn parse_redis_reads_the_command_the_connection_options_the_key_and_the_routes() {
    let expr: SExpression = serde_json::from_str(
        r#"["redis", "redis://cache.internal", ["GET", "k"], {"auth": "PW", "db": 2}, {"key": "c1", "emit": {"success": "S", "failure": "F"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Redis { origin, command, options, key, emit } => {
            assert_eq!(origin, Value::String("redis://cache.internal".to_string()));
            assert_eq!(command, Value::Array(vec![Value::String("GET".to_string()), Value::String("k".to_string())]));
            let options = options.as_object().cloned().unwrap();
            assert_eq!(options.get("auth"), Some(&Value::String("PW".to_string())));
            assert_eq!(options.get("db"), Some(&Value::Number(2.0)));
            assert!(options.get("key").is_none() && options.get("emit").is_none());
            assert_eq!(key, Some(Value::String("c1".to_string())));
            let emit = emit.unwrap();
            assert_eq!((emit.success.as_deref(), emit.failure.as_deref()), (Some("S"), Some("F")));
        }
        other => panic!("expected Redis, got {other:?}"),
    }
    assert!(is_known_effect_name("redis"));
}

#[test]
fn parse_redis_accepts_one_trailing_object_that_mixes_options_and_routes_and_none_at_all() {
    let mixed: SExpression =
        serde_json::from_str(r#"["redis", "redis://h", ["PING"], {"db": 1, "emit": {"success": "S"}}]"#).unwrap();
    match parse_effect(&mixed).unwrap() {
        Effect::Redis { options, emit, .. } => {
            assert_eq!(options.as_object().and_then(|o| o.get("db")), Some(&Value::Number(1.0)));
            assert_eq!(emit.and_then(|e| e.success).as_deref(), Some("S"));
        }
        other => panic!("expected Redis, got {other:?}"),
    }
    let bare: SExpression = serde_json::from_str(r#"["redis", "redis://h", ["PING"]]"#).unwrap();
    assert!(matches!(parse_effect(&bare), Some(Effect::Redis { key: None, emit: None, .. })));
    assert_eq!(parse_effect(&bare).unwrap().operator(), "redis");
}

#[test]
fn parse_postgres_reads_sql_params_connection_options_key_and_routes() {
    let expr: SExpression = serde_json::from_str(
        r#"["postgres", "postgres://db.internal:5432", "SELECT $1::int", [1], {"auth": "PW", "user": "app", "database": "main"}, {"key": "q1", "emit": {"success": "S", "failure": "F"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Postgres { origin, sql, params, options, key, emit } => {
            assert_eq!(origin, Value::String("postgres://db.internal:5432".to_string()));
            assert_eq!(sql, Value::String("SELECT $1::int".to_string()));
            assert_eq!(params, Value::Array(vec![Value::Number(1.0)]));
            let options = options.as_object().cloned().unwrap();
            assert_eq!(options.get("database"), Some(&Value::String("main".to_string())));
            assert!(options.get("key").is_none() && options.get("emit").is_none());
            assert_eq!(key, Some(Value::String("q1".to_string())));
            assert_eq!(emit.and_then(|e| e.failure).as_deref(), Some("F"));
        }
        other => panic!("expected Postgres, got {other:?}"),
    }
    assert!(is_known_effect_name("postgres"));
}

#[test]
fn control_parse_postgres_refuses_the_wrong_arity_and_a_non_object_trailer() {
    for src in [r#"["postgres", "postgres://h", "SELECT 1"]"#, r#"["postgres", "postgres://h", "SELECT 1", [], {}, {}, {}]"#, r#"["postgres", "postgres://h", "SELECT 1", [], "x"]"#] {
        let expr: SExpression = serde_json::from_str(src).unwrap();
        assert!(parse_effect(&expr).is_none(), "{src}");
    }
}

#[test]
fn control_parse_redis_refuses_the_wrong_arity_and_a_non_object_trailer() {
    for src in [r#"["redis", "redis://h"]"#, r#"["redis", "redis://h", ["PING"], {}, {}, {}]"#, r#"["redis", "redis://h", ["PING"], "x"]"#] {
        let expr: SExpression = serde_json::from_str(src).unwrap();
        assert!(parse_effect(&expr).is_none(), "{src}");
    }
}

#[test]
fn parse_host_effects_refuse_the_wrong_arity_and_never_fall_to_custom() {
    for src in [r#"["random-bytes"]"#, r#"["random-bytes", 1, {}, {}]"#, r#"["credential-status"]"#, r#"["credential-forget", "a", {}, {}]"#] {
        let expr: SExpression = serde_json::from_str(src).unwrap();
        assert!(parse_effect(&expr).is_none(), "{src}");
    }
}

#[test]
fn parse_os_effects_read_args_key_and_emit_and_every_op_is_known() {
    let expr: SExpression = serde_json::from_str(
        r#"["os/spawn", "git", ["status"], {"cwd": "/r"}, {"key": "p", "emit": {"success": "S", "onMessage": "M"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Os { op, args, key, emit } => {
            assert_eq!(op, OsOp::Spawn);
            assert_eq!(args.len(), 3);
            assert_eq!(key, Some(Value::String("p".to_string())));
            let emit = emit.unwrap();
            assert_eq!((emit.success.as_deref(), emit.on_message.as_deref()), (Some("S"), Some("M")));
        }
        other => panic!("expected Os, got {other:?}"),
    }
    let plain: SExpression = serde_json::from_str(r#"["os/cwd"]"#).unwrap();
    assert!(matches!(parse_effect(&plain), Some(Effect::Os { op: OsOp::Cwd, key: None, emit: None, .. })));
    for op in OsOp::ALL {
        assert!(is_known_effect_name(op.name()));
    }
    assert_eq!(parse_effect(&plain).unwrap().operator(), "os/cwd");
}

#[test]
fn parse_os_effects_refuse_the_wrong_arity_and_the_unknown_ops_still_fall_to_custom() {
    for src in [r#"["os/cwd", "x"]"#, r#"["os/read-file"]"#, r#"["os/read-file", "a", "b", "c"]"#, r#"["os/tcp-connect", "h"]"#] {
        let expr: SExpression = serde_json::from_str(src).unwrap();
        assert!(parse_effect(&expr).is_none(), "{src}");
    }
    let unknown: SExpression = serde_json::from_str(r#"["os/watch-http", "/x"]"#).unwrap();
    assert!(matches!(parse_effect(&unknown), Some(Effect::Custom { .. })));
}

#[test]
fn parse_cancel_call() {
    let expr: SExpression = serde_json::from_str(r#"["cancel-call", "batch-1"]"#).unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::CancelCall { key } => assert_eq!(key, Value::String("batch-1".into())),
        other => panic!("expected CancelCall, got {other:?}"),
    }
    assert!(is_known_effect_name("cancel-call"));
    assert_eq!(
        Effect::CancelCall { key: "k".into() }.operator(),
        "cancel-call"
    );
}

#[test]
fn parse_cancel_call_takes_a_list_of_keys_and_refuses_other_shapes() {
    let list: SExpression = serde_json::from_str(r#"["cancel-call", "@entity.keys"]"#).unwrap();
    assert!(parse_effect(&list).is_some(), "a binding stays one expression until resolved");
    let literal: SExpression = serde_json::from_str(r#"["cancel-call", ["list", "a", "b"]]"#).unwrap();
    assert!(matches!(
        parse_effect(&literal),
        Some(Effect::CancelCall { key: Value::Array(items) }) if items.len() == 3
    ));
    for refused in [r#"["cancel-call", 3]"#, r#"["cancel-call", {"a": 1}]"#, r#"["cancel-call", null]"#] {
        let expr: SExpression = serde_json::from_str(refused).unwrap();
        assert!(parse_effect(&expr).is_none(), "{refused}");
    }
}

#[test]
fn parse_cancel_call_without_a_key_is_not_an_effect() {
    let expr: SExpression = serde_json::from_str(r#"["cancel-call"]"#).unwrap();
    assert!(parse_effect(&expr).is_none());
}

#[test]
fn parse_browser_prompt_and_embed_carry_their_key_and_routes() {
    let prompt: SExpression = serde_json::from_str(
        r#"["browser/prompt", "@payload.text", "{}", {"key": "batch-1", "emit": {"success": "OK", "failure": "BAD", "cancelled": "STOP"}}]"#,
    )
    .unwrap();
    match parse_effect(&prompt).unwrap() {
        Effect::BrowserPrompt { text, schema, key, emit } => {
            assert_eq!(text, Value::String("@payload.text".into()));
            assert_eq!(schema, Value::String("{}".into()));
            assert_eq!(key, Some(Value::String("batch-1".into())));
            assert_eq!(emit.unwrap().cancelled.as_deref(), Some("STOP"));
        }
        other => panic!("expected BrowserPrompt, got {other:?}"),
    }
    let embed: SExpression = serde_json::from_str(r#"["browser/embed", ["a", "b"]]"#).unwrap();
    match parse_effect(&embed).unwrap() {
        Effect::BrowserEmbed { key, emit, .. } => assert!(key.is_none() && emit.is_none()),
        other => panic!("expected BrowserEmbed, got {other:?}"),
    }
    assert_eq!(parse_effect(&embed).unwrap().operator(), "browser/embed");
}

#[test]
fn parse_browser_tab_effects_carry_their_request_key_and_routes() {
    let query: SExpression = serde_json::from_str(
        r#"["browser/tab-query", {"tab": "?tab", "selector": ".r"}, {"key": "q-1", "emit": {"success": "OK", "failure": "BAD", "cancelled": "STOP"}}]"#,
    )
    .unwrap();
    match parse_effect(&query).unwrap() {
        Effect::BrowserTab { op, request, key, emit } => {
            assert_eq!(op, crate::tab::TabOp::Query);
            assert_eq!(request.as_object().and_then(|o| o.get("selector")), Some(&Value::String(".r".into())));
            assert_eq!(key, Some(Value::String("q-1".into())));
            assert_eq!(emit.unwrap().cancelled.as_deref(), Some("STOP"));
        }
        other => panic!("expected BrowserTab, got {other:?}"),
    }
    for op in crate::tab::TabOp::ALL {
        let src = format!(r#"["{}", {{}}]"#, op.name());
        let expr: SExpression = serde_json::from_str(&src).unwrap();
        assert_eq!(parse_effect(&expr).unwrap().operator(), op.name());
    }
}

#[test]
fn control_browser_tab_effects_refuse_the_wrong_arity() {
    for src in [r#"["browser/tab-list"]"#, r#"["browser/tab-open", {}, {}, {}]"#, r#"["browser/tab-nope", {}]"#] {
        let expr: SExpression = serde_json::from_str(src).unwrap();
        assert!(!matches!(parse_effect(&expr), Some(Effect::BrowserTab { .. })), "{src}");
    }
}

#[test]
fn parse_browser_prompt_and_embed_refuse_the_wrong_arity() {
    for src in [r#"["browser/prompt", "t"]"#, r#"["browser/prompt", "t", "s", {}, {}]"#, r#"["browser/embed"]"#, r#"["browser/embed", [], {}, {}]"#] {
        let expr: SExpression = serde_json::from_str(src).unwrap();
        assert!(parse_effect(&expr).is_none(), "{src}");
    }
}

#[test]
fn test_parse_persist_create() {
    let expr: SExpression =
        serde_json::from_str(r#"["persist", "create", "User", {"name": "John"}]"#).unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Persist {
            operation,
            entity_type,
            ..
        } => {
            assert_eq!(operation, PersistOperation::Create);
            assert_eq!(entity_type, "User");
        }
        _ => panic!("Expected Persist effect"),
    }
}

#[test]
fn test_parse_persist_update() {
    let expr: SExpression =
        serde_json::from_str(r#"["persist", "update", "User", "user-123", {"name": "Jane"}]"#)
            .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Persist {
            operation,
            entity_type,
            id,
            ..
        } => {
            assert_eq!(operation, PersistOperation::Update);
            assert_eq!(entity_type, "User");
            assert_eq!(id, Some("user-123".to_string()));
        }
        _ => panic!("Expected Persist effect"),
    }
}

#[test]
fn a_trailing_where_is_the_persist_guard_never_positional() {
    let parse = |src: &str| parse_effect(&serde_json::from_str::<SExpression>(src).unwrap()).unwrap();
    let guard = |e: &Effect| match e {
        Effect::Persist { guard, id, data, .. } => (guard.clone(), id.clone(), data.clone()),
        _ => panic!("Expected Persist effect"),
    };
    let lambda: Value = serde_json::from_str(r#"["fn", "row", ["=", "@row.used", false]]"#).unwrap();
    let (g, id, _) = guard(&parse(r#"["persist", "delete", "T", "@payload.id", {"where": ["fn", "row", ["=", "@row.used", false]]}]"#));
    assert_eq!((g, id.as_deref()), (Some(lambda.clone()), Some("@payload.id")));
    let (g, id, data) = guard(&parse(r#"["persist", "update", "T", "@payload.data", {"where": ["fn", "row", ["=", "@row.used", false]], "emit": {"success": "OK"}}]"#));
    assert_eq!((g, id), (Some(lambda), None));
    assert_eq!(data, Value::String("@payload.data".to_string()));
    let (g, _, _) = guard(&parse(r#"["persist", "delete", "T", "@payload.id"]"#));
    assert_eq!(g, None);
}

/// C-PERSIST-UPDATE-TRAILING-EMIT-MISPARSED: `(persist update Entity ?data
/// {emit:{...}})` — no separate id, data carries its own id, emit is a
/// TRAILING OPTIONS object — must not be read as (id, data).
#[test]
fn test_parse_persist_update_with_trailing_emit_options_is_not_misread_as_id_data() {
    let expr: SExpression = serde_json::from_str(
        r#"["persist", "update", "Ticket", "@payload.data", {"emit": {"success": "TICKET_UPDATED"}}]"#,
    )
    .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Persist {
            operation,
            entity_type,
            id,
            data,
            emit,
            guard: _,
        } => {
            assert_eq!(operation, PersistOperation::Update);
            assert_eq!(entity_type, "Ticket");
            assert_eq!(
                id, None,
                "no separate id was declared — must not be the emit config"
            );
            assert_eq!(
                data,
                Value::String("@payload.data".to_string()),
                "the real data token, not the trailing {{emit}} object"
            );
            assert_eq!(
                emit.and_then(|e| e.success),
                Some("TICKET_UPDATED".to_string()),
                "the trailing {{emit}} object is captured, not just stripped"
            );
        }
        _ => panic!("Expected Persist effect"),
    }
}

/// A trailing object WITHOUT an `emit` key is real positional data, not
/// options — the explicit-id shape (`[.., id, data]`) must keep working.
#[test]
fn test_parse_persist_update_explicit_id_with_non_emit_trailing_object_still_works() {
    let expr: SExpression =
        serde_json::from_str(r#"["persist", "update", "User", "user-123", {"name": "Jane"}]"#)
            .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Persist { id, data, .. } => {
            assert_eq!(id, Some("user-123".to_string()));
            let name = data
                .as_object()
                .and_then(|o| o.get("name"))
                .and_then(|v| v.as_str());
            assert_eq!(name, Some("Jane"));
        }
        _ => panic!("Expected Persist effect"),
    }
}

/// `(persist delete Entity id {emit:{...}})` — delete's own arm always
/// reads its id positionally regardless of trailing options; confirm the
/// stripping helper doesn't disturb it.
#[test]
fn test_parse_persist_delete_with_trailing_emit_options() {
    let expr: SExpression = serde_json::from_str(
        r#"["persist", "delete", "Ticket", "@payload.id", {"emit": {"success": "TICKET_DELETED"}}]"#,
    )
    .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Persist { operation, id, .. } => {
            assert_eq!(operation, PersistOperation::Delete);
            assert_eq!(id, Some("@payload.id".to_string()));
        }
        _ => panic!("Expected Persist effect"),
    }
}

#[test]
fn test_parse_render_ui() {
    let expr: SExpression = serde_json::from_str(
        r#"["render-ui", "main", {"type": "page-header", "title": "Users"}]"#,
    )
    .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::RenderUi { slot, content } => {
            assert_eq!(slot, "main");
            assert!(content.as_object().is_some());
        }
        _ => panic!("Expected RenderUi effect"),
    }
}

#[test]
fn test_parse_navigate() {
    let expr: SExpression = serde_json::from_str(r#"["navigate", "/users"]"#).unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Navigate {
            path,
            params,
            options,
        } => {
            assert_eq!(path, "/users");
            assert!(params.is_none());
            assert!(options.is_none());
        }
        _ => panic!("Expected Navigate effect"),
    }
}

#[test]
fn test_parse_set() {
    let expr: SExpression =
        serde_json::from_str(r#"["set", "@entity.status", "active"]"#).unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Set { path, value } => {
            assert_eq!(path, "@entity.status");
            assert_eq!(value, Value::String("active".to_string()));
        }
        _ => panic!("Expected Set effect"),
    }
}

#[test]
fn test_parse_custom_effect() {
    let expr: SExpression = serde_json::from_str(r#"["custom-effect", "arg1", 42]"#).unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Custom { name, args, .. } => {
            assert_eq!(name, "custom-effect");
            assert_eq!(args.len(), 2);
        }
        _ => panic!("Expected Custom effect"),
    }
}

#[test]
fn test_runtime_environment() {
    assert!(RuntimeEnvironment::Server.is_server());
    assert!(!RuntimeEnvironment::Server.is_client());
    assert!(RuntimeEnvironment::Client.is_client());
    assert!(!RuntimeEnvironment::Client.is_server());
}

#[test]
fn test_effect_result() {
    let effect = Effect::Emit {
        event: "TEST".to_string(),
        payload: Value::Null,
    };

    let executed = EffectResult::executed(effect.clone());
    assert!(executed.is_executed());

    let noop = EffectResult::noop(effect.clone(), "client-only");
    assert!(noop.is_noop());

    let delegated = EffectResult::delegated(effect.clone(), RuntimeEnvironment::Server);
    assert!(delegated.is_delegated());

    let failed = EffectResult::failed(effect, "error message");
    assert!(failed.is_failed());
}

#[test]
fn parse_fetch_reads_total_none_and_only_none() {
    let total_of = |config: &str| {
        let expr: SExpression =
            serde_json::from_str(&format!(r#"["fetch", "User", {config}]"#)).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Fetch { total, .. } => total,
            other => panic!("not a fetch: {other:?}"),
        }
    };
    assert_eq!(
        total_of(r#"{"total": "none"}"#),
        Some(crate::row_set::RowTotalMode::None)
    );
    assert_eq!(total_of(r#"{"total": "exact"}"#), None);
    assert_eq!(total_of(r#"{"limit": 5}"#), None);
    let expr: SExpression = serde_json::from_str(r#"["fetch", "User"]"#).unwrap();
    let Effect::Fetch { total, .. } = parse_effect(&expr).unwrap() else {
        panic!("not a fetch")
    };
    assert_eq!(total, None);
}

#[test]
fn parse_fetch_reads_the_nearest_option() {
    let nearest_of = |config: &str| {
        let expr: SExpression =
            serde_json::from_str(&format!(r#"["fetch", "Doc", {config}]"#)).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Fetch { nearest, .. } => nearest,
            other => panic!("not a fetch: {other:?}"),
        }
    };
    let got = nearest_of(r#"{"nearest": {"field": "embedding", "vector": "@payload.v", "k": 3}}"#)
        .expect("nearest kept");
    let members = got.as_object().expect("an object");
    assert_eq!(members.get("field").and_then(Value::as_str), Some("embedding"));
    assert!(members.get("k").is_some());
    assert!(nearest_of(r#"{"limit": 5}"#).is_none());
    assert!(nearest_of(r#"{"nearest": null}"#).is_none());
}

#[test]
fn test_parse_fetch_all() {
    // ["fetch", "User"] - fetch all users (from Orbital_Pattern.orb INIT transition)
    let expr: SExpression = serde_json::from_str(r#"["fetch", "User"]"#).unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Fetch {
            entity_type,
            id,
            filter,
            limit,
            offset,
            ..
        } => {
            assert_eq!(entity_type, "User");
            assert!(id.is_none());
            assert!(filter.is_none());
            assert!(limit.is_none());
            assert!(offset.is_none());
        }
        _ => panic!("Expected Fetch effect"),
    }
}

/// A `call-service` options object carries its completion events beside `key:` — JS
/// `EffectExecutor.ts` reads `emit` from `args[3]` (drift D20).
#[test]
fn a_call_service_options_object_carries_its_completion_events() {
    let expr: SExpression = serde_json::from_str(
        r#"["call-service", "test-svc", "run", {"texts": ["a"]}, {"key": "k1", "emit": {"success": "EMBEDDED", "failure": "EMBED_FAILED"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::CallService { key, emit, .. } => {
            assert_eq!(key.as_ref().and_then(Value::as_str), Some("k1"));
            let emit = emit.expect("emit parsed");
            assert_eq!(emit.success.as_deref(), Some("EMBEDDED"));
            assert_eq!(emit.failure.as_deref(), Some("EMBED_FAILED"));
        }
        other => panic!("expected CallService, got {other:?}"),
    }
}

#[test]
fn control_a_call_service_without_options_declares_no_completion() {
    let expr: SExpression =
        serde_json::from_str(r#"["call-service", "test-svc", "run", {}]"#).unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::CallService { emit, .. } => assert!(emit.is_none()),
        other => panic!("expected CallService, got {other:?}"),
    }
}

/// A substrate/program op's trailing `{ emit }` object is its completion config, not a
/// positional argument — JS `splitSubstrateEmit` (drift D20).
#[test]
fn a_substrate_ops_trailing_emit_object_is_split_from_its_arguments() {
    let expr: SExpression = serde_json::from_str(
        r#"["program/eval", {"name": "App", "orbitals": []}, {"final": true}, {"emit": {"success": "EVALUATED", "failure": "EVAL_FAILED"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Custom { name, args, key, emit } => {
            assert!(key.is_none());
            assert_eq!(name, "program/eval");
            assert_eq!(args.len(), 2, "the emit object is not an argument");
            assert_eq!(
                emit.expect("emit parsed").success.as_deref(),
                Some("EVALUATED")
            );
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn a_substrate_ops_options_key_is_kept_beside_its_emit() {
    let expr: SExpression = serde_json::from_str(
        r#"["program/dispatch", "Notes.ADD", {"title": "x"}, {"key": "@payload.k", "emit": {"success": "DONE"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Custom { args, key, emit, .. } => {
            assert_eq!(args.len(), 2, "the options object is not an argument");
            assert_eq!(key, Some(Value::String("@payload.k".to_string())));
            assert_eq!(emit.expect("emit parsed").success.as_deref(), Some("DONE"));
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn edge_a_key_without_emit_stays_in_its_argument() {
    let expr: SExpression = serde_json::from_str(r#"["program/eval", {"key": "k"}]"#).unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Custom { args, key, emit, .. } => {
            assert_eq!(args.len(), 1);
            assert!(key.is_none() && emit.is_none());
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn edge_a_trailing_object_without_emit_stays_an_argument() {
    let expr: SExpression = serde_json::from_str(
        r#"["program/eval", {"name": "App", "orbitals": []}, {"final": true}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Custom { args, emit, .. } => {
            assert_eq!(args.len(), 2);
            assert!(emit.is_none());
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn test_parse_fetch_emit() {
    // The fetch-completion `emit` (RS-G16): a data-loading trait declares the
    // events fired when the fetch resolves, advancing it loading → loaded.
    let expr: SExpression = serde_json::from_str(
        r#"["fetch", "InventoryItem", {"limit": 10, "emit": {"success": "BrowseItemLoaded", "failure": "BrowseItemLoadFailed"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::Fetch { limit, emit, .. } => {
            assert_eq!(limit, Some(Value::Number(10.0)));
            let emit = emit.expect("emit parsed");
            assert_eq!(emit.success.as_deref(), Some("BrowseItemLoaded"));
            assert_eq!(emit.failure.as_deref(), Some("BrowseItemLoadFailed"));
        }
        _ => panic!("Expected Fetch effect"),
    }

    // No `emit` key → None (the common case stays unchanged).
    let bare: SExpression = serde_json::from_str(r#"["fetch", "User"]"#).unwrap();
    match parse_effect(&bare).unwrap() {
        Effect::Fetch { emit, .. } => assert!(emit.is_none()),
        _ => panic!("Expected Fetch effect"),
    }
}

#[test]
fn test_parse_fetch_by_id() {
    // ["fetch", "User", {"id": "user-123"}] - fetch single user (from Orbital_Pattern.orb EDIT transition)
    let expr: SExpression =
        serde_json::from_str(r#"["fetch", "User", {"id": "user-123"}]"#).unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Fetch {
            entity_type,
            id,
            filter,
            ..
        } => {
            assert_eq!(entity_type, "User");
            assert_eq!(id, Some(Value::String("user-123".to_string())));
            assert!(filter.is_none());
        }
        _ => panic!("Expected Fetch effect"),
    }
}

#[test]
fn test_parse_fetch_with_filter() {
    // ["fetch", "User", {"filter": ["=", "@entity.status", "active"]}]
    let expr: SExpression = serde_json::from_str(
        r#"["fetch", "User", {"filter": ["=", "@entity.status", "active"]}]"#,
    )
    .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Fetch {
            entity_type,
            id,
            filter,
            ..
        } => {
            assert_eq!(entity_type, "User");
            assert!(id.is_none());
            assert!(filter.is_some());
            // Verify filter is an array
            let filter_value = filter.unwrap();
            assert!(filter_value.as_array().is_some());
        }
        _ => panic!("Expected Fetch effect"),
    }
}

#[test]
fn test_parse_fetch_with_pagination() {
    let expr: SExpression =
        serde_json::from_str(r#"["fetch", "Connection", {"limit": 10, "offset": 20}]"#)
            .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Fetch {
            entity_type,
            limit,
            offset,
            ..
        } => {
            assert_eq!(entity_type, "Connection");
            assert_eq!(limit, Some(Value::Number(10.0)));
            assert_eq!(offset, Some(Value::Number(20.0)));
        }
        _ => panic!("Expected Fetch effect"),
    }
}

#[test]
fn parse_persist_batch_keeps_the_ops_expression_and_emit() {
    let expr: SExpression = serde_json::from_str(
        r#"["persist", "batch", [["create", "User", {"name": "A"}]], {"emit": {"success": "SAVED", "failure": "FAILED"}}]"#,
    )
    .unwrap();
    match parse_effect(&expr).unwrap() {
        Effect::PersistBatch { ops, emit } => {
            assert!(matches!(ops, Value::Array(ref a) if a.len() == 1));
            let emit = emit.unwrap();
            assert_eq!(emit.success.as_deref(), Some("SAVED"));
            assert_eq!(emit.failure.as_deref(), Some("FAILED"));
        }
        other => panic!("expected PersistBatch, got {other:?}"),
    }
    assert_eq!(parse_effect(&expr).unwrap().operator(), "persist");
}

#[test]
fn test_parse_fetch_with_include() {
    // ["fetch", "Inspection", {"include": ["company", "inspector"]}]
    let expr: SExpression = serde_json::from_str(
        r#"["fetch", "Inspection", {"include": ["company", "inspector"]}]"#,
    )
    .unwrap();

    let effect = parse_effect(&expr).unwrap();
    match effect {
        Effect::Fetch {
            entity_type,
            include,
            ..
        } => {
            assert_eq!(entity_type, "Inspection");
            assert!(include.is_some());
            let include_vec = include.unwrap();
            assert_eq!(include_vec.len(), 2);
            assert!(include_vec.contains(&"company".to_string()));
            assert!(include_vec.contains(&"inspector".to_string()));
        }
        _ => panic!("Expected Fetch effect"),
    }
}
