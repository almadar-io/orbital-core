use super::*;

#[test]
fn program_host_effects_are_substrate_operators_through_the_installed_table() {
    assert!(is_substrate_operator("behavior/resolve"));
    assert!(is_substrate_operator("program/eval"));
    assert!(is_substrate_operator("program/validate"));
    assert!(
        is_substrate_operator("llm/generate"),
        "a prefixed substrate op"
    );
    assert!(
        !is_substrate_operator("behavior/apply"),
        "control: a pure behavior operator"
    );
    assert!(!is_substrate_operator("persist"), "control: a core effect");
}

#[test]
fn a_substrate_operator_is_exactly_one_the_table_hosts_on_the_substrate_or_the_program() {
    use crate::language::registry::operators;
    let mut checked_substrate = 0;
    for (name, operator) in operators() {
        let hosted = matches!(operator.host.as_deref(), Some("program" | "substrate"));
        assert_eq!(is_substrate_operator(name), hosted, "{name}");
        checked_substrate += usize::from(operator.host.as_deref() == Some("substrate"));
    }
    assert!(
        checked_substrate > 0,
        "the table hosts operators on the substrate"
    );
    for name in [
        "llm/generate",
    ] {
        let host = crate::language::registry::get_operator(name).and_then(|o| o.host.as_deref());
        assert_eq!(host, Some("substrate"), "{name}");
        assert!(is_substrate_operator(name), "{name}");
    }
    for name in ["os/read-file", "kv/get", "persist", "str/concat", "no/such"] {
        assert!(!is_substrate_operator(name), "control: {name}");
    }
}

#[test]
fn test_effect_kind() {
    assert_eq!(OirEffect::emit("test").kind(), "emit");
    assert_eq!(OirEffect::fetch("Task").kind(), "fetch");
    assert_eq!(
        OirEffect::render_ui("main", crate::ir::types::OirPattern::new("entity-table")).kind(),
        "render-ui"
    );
}

#[test]
fn test_effect_classification() {
    let fetch = OirEffect::fetch("Task");
    let class = fetch.classify();
    assert_eq!(class.client_action, ClientAction::Ignore);
    assert_eq!(class.server_action, ServerAction::Execute);

    let render = OirEffect::render_ui("main", crate::ir::types::OirPattern::new("entity-table"));
    let class = render.classify();
    assert_eq!(class.client_action, ClientAction::Execute);
    assert_eq!(class.server_action, ServerAction::ReturnToClient);
}

#[test]
fn test_is_client_server_effect() {
    let fetch = OirEffect::fetch("Task");
    assert!(!fetch.is_client_effect());
    assert!(fetch.is_server_effect());

    let navigate = OirEffect::navigate(OirExpr::string("/home"));
    assert!(navigate.is_client_effect());
    assert!(!navigate.is_server_effect());

    let emit = OirEffect::emit("test");
    assert!(emit.is_client_effect());
    assert!(emit.is_server_effect());
}

#[test]
fn emit_config_roundtrip_success_failure() {
    let cfg = OirEmitConfig {
        success: Some("LOADED".into()),
        failure: Some("LOAD_FAILED".into()),
        on_message: None,
        cancelled: None,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    // camelCase + skips None fields
    assert_eq!(json, r#"{"success":"LOADED","failure":"LOAD_FAILED"}"#);

    let round: OirEmitConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(round, cfg);
}

#[test]
fn emit_config_roundtrip_watch_shape() {
    let cfg = OirEmitConfig {
        success: None,
        failure: Some("SUBSCRIBE_FAILED".into()),
        on_message: Some("PUSH_RECEIVED".into()),
        cancelled: None,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(json.contains(r#""failure":"SUBSCRIBE_FAILED""#));
    assert!(json.contains(r#""onMessage":"PUSH_RECEIVED""#));
    let round: OirEmitConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(round, cfg);
}

#[test]
fn emit_config_default_is_empty() {
    let cfg = OirEmitConfig::default();
    assert!(cfg.is_empty());
    assert_eq!(serde_json::to_string(&cfg).unwrap(), "{}");
}

#[test]
fn emit_config_event_names_iter_is_stable() {
    let cfg = OirEmitConfig {
        success: Some("A".into()),
        failure: Some("B".into()),
        on_message: Some("C".into()),
        cancelled: Some("D".into()),
    };
    let names: Vec<&str> = cfg.event_names().collect();
    assert_eq!(names, vec!["A", "B", "C", "D"]);
}

#[test]
fn fetch_with_emit_serializes_field() {
    let cfg = OirEmitConfig {
        success: Some("PATIENT_LOADED".into()),
        failure: Some("PATIENT_LOAD_FAILED".into()),
        ..Default::default()
    };
    let eff = OirEffect::Fetch {
        entity_type: "Patient".into(),
        id: None,
        filter: None,
        order_by: None,
        limit: None,
        offset: None,
        include: None,
        nearest: None,
        emit: Some(cfg.clone()),
    };
    let json = serde_json::to_value(&eff).unwrap();
    assert_eq!(
        json["emit"],
        serde_json::json!({
            "success": "PATIENT_LOADED",
            "failure": "PATIENT_LOAD_FAILED"
        })
    );

    // Round-trip
    let round: OirEffect = serde_json::from_value(json).unwrap();
    if let OirEffect::Fetch { emit, .. } = round {
        assert_eq!(emit, Some(cfg));
    } else {
        panic!("expected Fetch");
    }
}

#[test]
fn fetch_nearest_serializes_only_when_set() {
    let plain = serde_json::to_value(OirEffect::fetch("Doc")).unwrap();
    assert!(plain.get("nearest").is_none());
    let mut eff = OirEffect::fetch("Doc");
    if let OirEffect::Fetch { nearest, .. } = &mut eff {
        *nearest = Some(OirExpr::number(3.0));
    }
    let json = serde_json::to_value(&eff).unwrap();
    assert!(json.get("nearest").is_some());
    let round: OirEffect = serde_json::from_value(json).unwrap();
    assert_eq!(round, eff);
}

#[test]
fn fetch_without_emit_skips_field() {
    let eff = OirEffect::fetch("Patient");
    let json = serde_json::to_value(&eff).unwrap();
    assert!(
        json.get("emit").is_none(),
        "emit field should be skipped when None, got: {}",
        json
    );
}

#[test]
fn legacy_fetch_json_without_emit_deserializes() {
    // Schemas authored before this field existed must still load.
    let json = serde_json::json!({
        "kind": "fetch",
        "entity_type": "Patient",
    });
    let eff: OirEffect = serde_json::from_value(json).unwrap();
    if let OirEffect::Fetch { emit, .. } = eff {
        assert!(emit.is_none());
    } else {
        panic!("expected Fetch");
    }
}

#[test]
fn emit_config_cancelled_roundtrips_and_counts_as_set() {
    let cfg = OirEmitConfig {
        cancelled: Some("CLASSIFY_CANCELLED".into()),
        ..Default::default()
    };
    assert!(!cfg.is_empty());
    let json = serde_json::to_string(&cfg).unwrap();
    assert_eq!(json, r#"{"cancelled":"CLASSIFY_CANCELLED"}"#);
    let round: OirEmitConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(round, cfg);
}

#[test]
fn keyed_call_service_roundtrips_and_exposes_key_binding() {
    let eff = OirEffect::CallService {
        service: "llm".into(),
        method: "classifyMany".into(),
        args: OirExpr::object(crate::ir::expr::OirFields::default()),
        key: Some(OirExpr::string("batch-1")),
        emit: Some(OirEmitConfig {
            cancelled: Some("CANCELLED".into()),
            ..Default::default()
        }),
    };
    let json = serde_json::to_value(&eff).unwrap();
    assert!(json.get("key").is_some());
    assert_eq!(json["emit"]["cancelled"], "CANCELLED");
    let round: OirEffect = serde_json::from_value(json).unwrap();
    assert_eq!(round, eff);
    assert_eq!(eff.child_exprs().len(), 2);
}

#[test]
fn unkeyed_call_service_skips_key_field() {
    let eff = OirEffect::call_service(
        "llm",
        "generate",
        OirExpr::object(crate::ir::expr::OirFields::default()),
    );
    let json = serde_json::to_value(&eff).unwrap();
    assert!(json.get("key").is_none(), "{json}");
    assert_eq!(eff.child_exprs().len(), 1);
}

#[test]
fn cancel_call_is_a_server_effect_with_a_key_child() {
    let eff = OirEffect::cancel_call(OirExpr::string("batch-1"));
    assert_eq!(eff.kind(), "cancel-call");
    assert!(eff.is_server_effect());
    assert!(!eff.is_client_effect());
    assert_eq!(eff.child_exprs().len(), 1);
    let round: OirEffect = serde_json::from_value(serde_json::to_value(&eff).unwrap()).unwrap();
    assert_eq!(round, eff);
}
