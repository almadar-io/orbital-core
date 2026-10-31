// A config value that is a `@user.*` binding reads the session's identity one hop, a missing field becoming '' (twin of `buildConfigBinding`, `almadar-runtime/src/traits/config-defaults.ts`; ui `BrowserPlayground.api-gateway`). EXEC-IDENTITY-1: `@user` is the session's bound identity, the anonymous viewer when none is.

use crate::support::identity;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::effects::response::EventRequest;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use serde_json::json;

fn server() -> EmbeddedServer {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "gw",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Gw",
            "entity": { "name": "Route", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Header", "linkedEntity": "Route",
                "config": {
                    "viewerRole": { "type": "string", "default": "@user.role" },
                    "tier": { "type": "string", "default": "@user.tier" },
                    "label": { "type": "string", "default": "Routes" } },
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", "main", { "type": "typography", "content": "@config.label", "role": "@config.viewerRole", "tier": "@config.tier" }]] }] } }]
        }]
    }))
    .unwrap();
    let server = EmbeddedServer::new(schema).unwrap();
    identity::enable(server.kernel(), &["role", "tier"]);
    server
}

fn props(server: &EmbeddedServer, client: &str) -> serde_json::Value {
    let request = EventRequest {
        event: "INIT".to_string(),
        target_trait: Some("Header".to_string()),
        client_id: Some(client.to_string()),
        ..Default::default()
    };
    let ServerProgress::Done(response) = server.start("Gw", request).unwrap() else {
        panic!("the render asks nothing of the host");
    };
    let effects = serde_json::to_value(response.client_effects()).unwrap();
    effects[0][2].clone()
}

#[test]
fn a_config_value_naming_a_user_field_reads_the_session_identity() {
    let server = server();
    identity::sign_in_server(&server, "c1", "u1", json!({ "role": "admin", "tier": "3" }));
    let p = props(&server, "c1");
    assert_eq!(p["role"], json!("admin"));
    assert_eq!(p["tier"], json!("3"));
}

#[test]
fn a_missing_user_field_is_the_empty_string_never_the_sigil() {
    let server = server();
    identity::sign_in_server(&server, "c1", "u1", json!({}));
    let p = props(&server, "c1");
    assert_eq!(p["role"], json!(""));
    assert_eq!(p["tier"], json!(""));
}

#[test]
fn an_unbound_session_reads_the_anonymous_viewer_and_another_session_keeps_its_own_identity() {
    let server = server();
    identity::sign_in_server(&server, "c1", "u1", json!({ "role": "admin" }));
    assert_eq!(props(&server, "c2")["role"], json!("anonymous"), "a session that never signed in");
    assert_eq!(props(&server, "c1")["role"], json!("admin"), "control: the signed-in session is untouched");
}

#[test]
fn control_a_plain_config_default_is_untouched() {
    let server = server();
    identity::sign_in_server(&server, "c1", "u1", json!({ "role": "admin" }));
    assert_eq!(props(&server, "c1")["content"], json!("Routes"));
}
