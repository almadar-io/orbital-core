use super::*;

/// Build a minimal `TraitDefinition` with a `config` map of `key → default
/// string` from JSON — the kernel only reads `config[k].default` for the
/// forward chain, so an empty state machine suffices.
fn trait_with_config(name: &str, cfg: &[(&str, &str)]) -> TraitDefinition {
    let config: serde_json::Map<String, serde_json::Value> = cfg
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_string(),
                serde_json::json!({ "type": "string", "default": v }),
            )
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "name": name,
        "stateMachine": { "states": [], "transitions": [] },
        "config": config,
    }))
    .expect("trait fixture")
}

fn traits(defs: Vec<TraitDefinition>) -> HashMap<String, TraitDefinition> {
    defs.into_iter().map(|t| (t.name.clone(), t)).collect()
}

/// A peer's `@config.<key>` forward chains to the host trait's knob default.
#[test]
fn wrapper_forward_chains_to_host_default() {
    let button = trait_with_config("Button1", &[("label", "@config.buttonLabel")]);
    let host = trait_with_config("ExportMenu", &[("buttonLabel", "Export")]);
    let all = traits(vec![button.clone(), host]);
    let cfg = config_defaults_value(&button, &all);
    let o = cfg.as_object().unwrap();
    // The render-ui reads `@config.buttonLabel` (the collapsed forward token),
    // so the host knob must be exposed under its own name…
    assert_eq!(
        o.get("buttonLabel"),
        Some(&Value::String("Export".to_string()))
    );
    // …and the forwarding field is collapsed too.
    assert_eq!(o.get("label"), Some(&Value::String("Export".to_string())));
}

/// A host call-site override of the knob flows through to the child — the
/// override is folded into the host's `default` by the inline phase, so the
/// chain resolves to it, not the atom's own default.
#[test]
fn wrapper_forward_picks_up_host_override() {
    let button = trait_with_config("Button1", &[("label", "@config.buttonLabel")]);
    let host = trait_with_config("ExportMenu", &[("buttonLabel", "Download CSV")]);
    let all = traits(vec![button.clone(), host]);
    let cfg = config_defaults_value(&button, &all);
    assert_eq!(
        cfg.as_object().unwrap().get("buttonLabel"),
        Some(&Value::String("Download CSV".to_string())),
    );
}

/// No host declares the knob → the forward is left live for the binding
/// validator to flag (no fabrication).
#[test]
fn wrapper_forward_unresolved_when_no_host_knob() {
    let button = trait_with_config("Button1", &[("label", "@config.buttonLabel")]);
    let all = traits(vec![button.clone()]);
    let cfg = config_defaults_value(&button, &all);
    assert_eq!(
        cfg.as_object().unwrap().get("label"),
        Some(&Value::String("@config.buttonLabel".to_string())),
    );
}

/// A self-referential forward (`X = @config.X`) terminates instead of looping.
#[test]
fn self_forward_terminates() {
    let t = trait_with_config("Loop", &[("x", "@config.x")]);
    let all = traits(vec![t.clone()]);
    let cfg = config_defaults_value(&t, &all);
    assert_eq!(
        cfg.as_object().unwrap().get("x"),
        Some(&Value::String("@config.x".to_string())),
    );
}
