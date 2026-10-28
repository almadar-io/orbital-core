// A config value that is a `@user.*` binding reads the viewer one hop, a missing field becoming '' (twin of `buildConfigBinding`, `almadar-runtime/src/traits/config-defaults.ts`; ui `BrowserPlayground.api-gateway`).

use orbital_core_runtime::embed::EmbeddedKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;

fn kernel() -> EmbeddedKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "gw",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Gw",
            "entity": { "name": "Route", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Header", "linkedEntity": "Route", "dispatch": "client",
                "config": {
                    "viewerRole": { "type": "string", "default": "@user.role" },
                    "tier": { "type": "number", "default": "@user.profile.tier" },
                    "label": { "type": "string", "default": "Routes" } },
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", "main", { "type": "typography", "content": "@config.label", "role": "@config.viewerRole", "tier": "@config.tier" }]] }] } }]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c1").unwrap()
}

fn props(k: &EmbeddedKernel) -> serde_json::Value {
    let wire = k.dispatch("Header", None, "INIT", Value::Null).unwrap();
    let effects = serde_json::to_value(wire.response.client_effects()).unwrap();
    effects[0][2].clone()
}

fn viewer(v: serde_json::Value) -> Value {
    serde_json::from_value(v).unwrap()
}

#[test]
fn a_config_value_naming_a_user_field_reads_the_viewer() {
    let k = kernel();
    k.set_current_user(viewer(
        json!({ "id": "u1", "role": "admin", "profile": { "tier": 3 } }),
    ));
    let p = props(&k);
    assert_eq!(p["role"], json!("admin"));
    assert_eq!(p["tier"].as_f64(), Some(3.0));
}

#[test]
fn a_missing_user_field_is_the_empty_string_never_the_sigil() {
    let k = kernel();
    k.set_current_user(viewer(json!({ "id": "u1" })));
    let p = props(&k);
    assert_eq!(p["role"], json!(""));
    assert_eq!(p["tier"], json!(""));
}

#[test]
fn control_a_plain_config_default_is_untouched() {
    let k = kernel();
    k.set_current_user(viewer(json!({ "id": "u1", "role": "admin" })));
    assert_eq!(props(&k)["content"], json!("Routes"));
}
