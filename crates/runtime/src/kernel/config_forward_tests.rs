use orbital_core_base::json_value::from_json_value;
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
    from_json_value(&(serde_json::json!({
        "name": name,
        "stateMachine": { "states": [], "transitions": [] },
        "config": config,
    })))
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
        Some(&Value::string("Export"))
    );
    // …and the forwarding field is collapsed too.
    assert_eq!(o.get("label"), Some(&Value::string("Export")));
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
        Some(&Value::string("Download CSV")),
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
        Some(&Value::string("@config.buttonLabel")),
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
        Some(&Value::string("@config.x")),
    );
}

fn kernel_with(defs: Vec<TraitDefinition>) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(crate::entity_source::InMemoryEntityCache::new()),
        KernelConfig::default(),
    );
    for def in defs {
        kernel.register_trait(Arc::new(def)).expect("registers");
    }
    kernel
}

fn shared_map(v: &Value) -> &orbital_core_base::value::Shared<ValueMap> {
    match v {
        Value::Object(m) => m,
        other => panic!("config is an object, got {other:?}"),
    }
}

/// A trait's config is built once per registered program, not on every step.
#[test]
fn a_traits_config_read_twice_is_one_built_table() {
    let t = trait_with_config("Theme", &[("preset", "naqsh"), ("style", "")]);
    let kernel = kernel_with(vec![t.clone()]);
    let first = kernel.trait_config_defaults(&t);
    let second = kernel.trait_config_defaults(&t);
    assert_eq!(first.as_object().unwrap().get("preset"), Some(&Value::string("naqsh")));
    assert!(orbital_core_base::value::Shared::ptr_eq(shared_map(&first), shared_map(&second)));
}

/// Registering another trait drops the built tables: a forward to the new trait's knob now resolves.
#[test]
fn a_new_registration_rebuilds_a_forward_it_answers() {
    let button = trait_with_config("Button1", &[("label", "@config.buttonLabel")]);
    let kernel = kernel_with(vec![button.clone()]);
    assert_eq!(
        kernel.trait_config_defaults(&button).as_object().unwrap().get("label"),
        Some(&Value::string("@config.buttonLabel")),
    );
    kernel
        .register_trait(Arc::new(trait_with_config("ExportMenu", &[("buttonLabel", "Export")])))
        .expect("registers");
    assert_eq!(
        kernel.trait_config_defaults(&button).as_object().unwrap().get("label"),
        Some(&Value::string("Export")),
    );
}

/// Control: the cached table never carries one viewer's `@user.` reads to the next.
#[test]
fn a_user_forward_binds_each_viewer_over_the_cached_table() {
    let t = trait_with_config("Greeting", &[("who", "@user.name")]);
    let kernel = kernel_with(vec![t.clone()]);
    let ada = bind_user_forwards(kernel.trait_config_defaults(&t), &from_json_value::<Value>(&serde_json::json!({ "name": "Ada" })).unwrap());
    let bo = bind_user_forwards(kernel.trait_config_defaults(&t), &from_json_value::<Value>(&serde_json::json!({ "name": "Bo" })).unwrap());
    assert_eq!(ada.as_object().unwrap().get("who"), Some(&Value::string("Ada")));
    assert_eq!(bo.as_object().unwrap().get("who"), Some(&Value::string("Bo")));
    assert_eq!(
        kernel.trait_config_defaults(&t).as_object().unwrap().get("who"),
        Some(&Value::string("@user.name")),
    );
}
