use super::*;
use crate::entity_source::InMemoryEntityCache;

fn kernel_with(schema: OrbitalSchema) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn schema_with_theme(theme_key: &str) -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "theme": theme_key,
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "PanelView",
                "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                }
            }]
        }]
    }))
    .unwrap()
}

/// §157/C7 `theme-declaration`: a declared `-dark` theme key must resolve
/// as Dark when `set_theme_mode` was NEVER called — the kernel's default
/// (unset) toggle must not silently force Light over the key's own
/// suffix. `ClientKernel::set_theme_mode` is the passthrough this proves
/// exists; here it is exercised directly against `RuntimeKernel`.
#[test]
fn theme_key_suffix_selects_mode() {
    let kernel = kernel_with(schema_with_theme("clay-dark"));
    let resolved = kernel.resolved_theme();
    assert_eq!(
        resolved.key, "clay-dark",
        "an unset toggle must honor the declared key's own -dark suffix, not force Light"
    );
    assert_eq!(resolved.mode, ThemeMode::Dark);
}

/// An explicit `set_theme_mode` call still overrides the declared key's
/// suffix, exactly as documented on `RuntimeKernel::set_theme_mode`.
#[test]
fn explicit_theme_mode_overrides_key_suffix() {
    let kernel = kernel_with(schema_with_theme("clay-dark"));
    kernel.set_theme_mode(ThemeMode::Light);
    let resolved = kernel.resolved_theme();
    assert_eq!(resolved.key, "clay-light");
    assert_eq!(resolved.mode, ThemeMode::Light);
}
