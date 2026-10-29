use super::*;

#[test]
fn a_contract_literal_union_reads_in_the_lolo_spelling_and_fits_a_string() {
    let union = lolo_spelling("'beginner' | 'intermediate' | 'advanced'");
    assert_eq!(union, r#""beginner" | "intermediate" | "advanced""#);
    assert!(crate::type_compat::set_value_type_compatible(
        "string", &union
    ));
    assert_eq!(
        lolo_spelling("('solo' | 'teams')[]"),
        r#"["solo" | "teams"]"#
    );
    assert_eq!(
        lolo_spelling("string | number"),
        "string | number",
        "control: a non-literal union is untouched"
    );
    assert_eq!(
        lolo_spelling("'it''s'"),
        "'it''s'",
        "control: not a plain literal, left as written"
    );
    assert!(
        !crate::type_compat::set_value_type_compatible("number", &union),
        "control: a string union does not fit a number"
    );
}

#[test]
fn a_contract_list_type_reads_in_the_lolo_spelling() {
    let shape: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        r#"{"steps":"AgentActivity[]","grid":"number[][]","name":"string","tags":{"type":"string[]"}}"#,
    )
    .unwrap();
    let types: Vec<(String, String)> = response_shape_fields(&shape)
        .into_iter()
        .map(|f| (f.name, f.field_type))
        .collect();
    assert_eq!(
        types,
        [
            ("grid".to_string(), "[[number]]".to_string()),
            ("name".to_string(), "string".to_string()),
            ("steps".to_string(), "[AgentActivity]".to_string()),
            ("tags".to_string(), "[string]".to_string()),
        ]
    );
    assert!(crate::type_compat::set_value_type_compatible(
        "[AgentActivity]",
        &types[2].1
    ));
    assert!(!crate::type_compat::set_value_type_compatible(
        "[string]",
        &types[2].1
    ));
}

fn shaped_registry() -> ServiceRegistryData {
    parse_service_registry_data(
        r#"{"services": {"files": {"actions": [
                {"name": "upload", "params": [],
                 "responseShape": {"url": "string", "meta": {"type": "object"}, "loose": 3}},
                {"name": "ping", "params": [], "responseShape": {}},
                {"name": "bare", "params": []}]}}}"#,
    )
    .unwrap()
}

#[test]
fn a_registry_action_exposes_its_response_shape_sorted_and_typed() {
    let registry = shaped_registry();
    let shape = registry.response_shape("files", "upload").expect("shape");
    let found: Vec<(&str, &str)> = shape
        .iter()
        .map(|f| (f.name.as_str(), f.field_type.as_str()))
        .collect();
    assert_eq!(
        found,
        vec![("loose", "any"), ("meta", "object"), ("url", "string")]
    );
}

#[test]
fn an_action_without_a_usable_response_shape_has_none() {
    let registry = shaped_registry();
    assert!(
        registry.response_shape("files", "ping").is_none(),
        "empty shape"
    );
    assert!(
        registry.response_shape("files", "bare").is_none(),
        "no shape"
    );
    assert!(
        registry.response_shape("files", "missing").is_none(),
        "unknown action"
    );
    assert!(
        registry.response_shape("nope", "upload").is_none(),
        "unknown service"
    );
}

// Mobile shells (android/ios) are NO-OP-DISPATCH: they interpret
// server-returned clientEffects at runtime and never compile S-expr
// operators, so no canonical category may target them. If this ever
// changes, the exemption must be revisited (that shell would then need its
// own operator-completeness gate, like Python's).
#[test]
fn no_canonical_category_targets_a_mobile_shell() {
    let doc: serde_json::Value = serde_json::from_str(operators_json()).unwrap();
    let forbidden = ["android", "ios", "kotlin", "swift", "mobile"];
    let mut offenders = Vec::new();
    for (cat, meta) in doc["categories"].as_object().unwrap() {
        if let Some(targets) = meta["target"].as_array() {
            for t in targets.iter().filter_map(|t| t.as_str()) {
                if forbidden.contains(&t) {
                    offenders.push(format!("{cat} -> {t}"));
                }
            }
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "Mobile shells are NO-OP-DISPATCH and must not be an operator target, \
             but these categories target one:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn operators_parse_and_lookup() {
    // Smoke test: a few well-known operators must exist.
    assert!(is_known_operator("+"));
    assert!(is_known_operator("if"));
    assert!(is_known_operator("set"));
    assert!(is_known_operator("render-ui"));
    assert!(is_known_operator("behavior/compose"));
    assert!(!is_known_operator("definitely-not-a-real-op-xyz"));
}

#[test]
fn categories_parse() {
    let cats = categories();
    assert!(cats.contains_key("arithmetic"));
    assert!(cats.contains_key("std-composition"));
    let std_comp = cats.get("std-composition").unwrap();
    assert!(std_comp.target.iter().any(|t| t == "rust"));
}

#[test]
fn patterns_parse_and_lookup() {
    assert!(is_known_pattern("table-view"));
    assert!(is_known_pattern("button"));
    assert!(!is_known_pattern("definitely-not-a-pattern-xyz"));
    let table = get_pattern("table-view").unwrap();
    assert_eq!(table.category, "display");
    assert!(table.props_schema.contains_key("entity"));
}

#[test]
fn program_host_effects_are_the_installed_tables_host_program_operators() {
    let from_table: std::collections::BTreeSet<&str> = operators()
        .iter()
        .filter(|(_, op)| op.host.as_deref() == Some("program"))
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(from_table.contains("behavior/resolve"), "{from_table:?}");
    assert!(from_table.contains("program/validate"), "{from_table:?}");
    assert_eq!(
        program_host_effects().collect::<std::collections::BTreeSet<_>>(),
        from_table
    );
    for op in &from_table {
        assert!(is_program_host_effect(op), "{op}");
    }
    for op in [
        "persist",
        "call-service",
        "behavior/apply",
        "llm/generate",
        "no/such",
    ] {
        assert!(!is_program_host_effect(op), "control: {op}");
    }
}

#[test]
fn os_host_effects_are_the_installed_tables_host_os_operators() {
    let from_os: std::collections::BTreeSet<&str> = os_host_effects().collect();
    for op in ["os/read-file", "os/spawn", "os/wait", "os/kill"] {
        assert!(from_os.contains(op), "{op} in {from_os:?}");
    }
    for op in ["os/path-join", "persist", "no/such"] {
        assert!(!from_os.contains(op), "control: {op}");
    }
}

#[test]
fn emit_capable_operators_are_the_ones_whose_parameters_take_an_emit_options_object() {
    for op in [
        "fetch", "persist", "call-service", "random-bytes", "credential-status",
        "credential-forget", "credential-set", "access-check", "os/read-file", "kv/get",
    ] {
        assert!(takes_emit_options(op), "{op} takes emit options");
    }
    for op in ["emit", "agent/invoke", "+", "no/such"] {
        assert!(!takes_emit_options(op), "control: {op} takes none");
    }
}

#[test]
fn every_os_and_kv_operator_declares_a_grant_and_no_other_operator_does() {
    for (name, op) in operators() {
        let granted = matches!(op.host.as_deref(), Some("os" | "kv"));
        assert_eq!(op.grant.is_some(), granted, "{name}");
    }
    assert_eq!(get_operator("os/read").and_then(|op| op.grant.as_deref()), Some(&[][..]), "a handle call needs nothing");
}

#[test]
fn the_host_effects_are_the_tables_closed_calls_with_their_option_keys_request_and_range() {
    let mut names: Vec<&str> = closed_call_effects().collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "access-check", "credential-forget", "credential-list", "credential-mode", "credential-remove",
            "credential-rotate", "credential-set", "credential-status", "credential-test", "random-bytes"
        ]
    );
    let bytes = get_operator("random-bytes").expect("random-bytes");
    let range = bytes.closed_call.as_ref().and_then(|c| c.int_range).expect("range");
    assert_eq!((range.arg, range.min, range.max), (0, 1, 1024));
    assert_eq!(bytes.option_keys(), Some((vec!["emit", "key"], vec!["failure", "success"])));
    assert_eq!(get_operator("access-check").and_then(|op| op.option_keys()), Some((vec!["emit"], vec!["failure", "success"])), "control: only random-bytes is keyed");
    let list = get_operator("credential-list").expect("list");
    assert_eq!(list.request_fields(), Some((vec!["service"], vec![])), "service is optional");
    assert_eq!(get_operator("credential-set").and_then(|op| op.request_fields()), Some((vec!["envVar", "service", "value"], vec!["envVar", "service", "value"])));
    assert!(get_operator("credential-set").is_some_and(|op| op.closed_call.as_ref().is_some_and(|c| c.request)));
    assert!(!get_operator("access-check").is_some_and(|op| op.closed_call.as_ref().is_some_and(|c| c.request)), "control: access-check's request is checked by its own contract");
    assert_eq!(get_operator("fetch").map(|op| op.closed_call.is_none()), Some(true), "control: a data effect is not a host effect");
}

#[test]
fn tcp_default_ports_are_the_tables_per_scheme() {
    for (scheme, port) in [("redis", 6379), ("rediss", 6379), ("postgres", 5432), ("postgresql", 5432)] {
        assert_eq!(tcp_scheme_default_port(scheme), Some(port), "{scheme}");
    }
    for scheme in ["http", "https", "mysql", ""] {
        assert_eq!(tcp_scheme_default_port(scheme), None, "control: {scheme}");
    }
    assert_eq!(get_operator("redis").map(|op| op.default_ports.len()), Some(2));
    assert_eq!(get_operator("fetch").map(|op| op.default_ports.is_empty()), Some(true));
}

#[test]
fn sigil_context_rules_come_from_the_installed_core() {
    let guard = allowed_sigils_for_context("guard").expect("guard rules");
    assert!(guard.iter().any(|r| r == "payload"));
    let tick = allowed_sigils_for_context("tick").expect("tick rules");
    assert!(
        !tick.iter().any(|r| r == "payload"),
        "control: ticks fire on a clock and have no payload"
    );
    assert_eq!(allowed_sigils_for_context("nope"), None);
    assert_eq!(
        known_contexts(),
        ["guard", "effect", "tick"].into_iter().collect()
    );
}
