use orbital_core::secrets::{
    secret_flow_violations, CREDENTIAL_NOT_SECRET_CODE, SECRET_OUTSIDE_AUTH_CODE,
};
use serde_json::{json, Value};

fn program(effect: Value) -> Value {
    json!({"orbitals": [{"traits": [{
        "config": {
            "key": {"type": "secret", "default": "KEY"},
            "plain": {"type": "string", "default": "KEY"}
        },
        "stateMachine": {"transitions": [{"effects": [effect]}]}
    }]}]})
}

fn codes(effect: Value) -> Vec<&'static str> {
    secret_flow_violations(&program(effect))
        .iter()
        .map(|violation| violation.code())
        .collect()
}

#[test]
fn status_accepts_explicit_scopes_only_for_a_direct_secret_knob() {
    for scope in ["app", "user"] {
        assert!(
            codes(json!(["credential-status", {"credential": "@config.key", "scope": scope}]))
                .is_empty()
        );
        for credential in ["@config.plain", "KEY", "@payload.credential"] {
            assert!(codes(
                json!(["credential-status", {"credential": credential, "scope": scope}])
            )
            .contains(&CREDENTIAL_NOT_SECRET_CODE));
        }
    }
}

#[test]
fn malformed_scoped_status_is_structurally_refused() {
    for selection in [
        json!({"credential": "@config.key"}),
        json!({"scope": "app"}),
        json!({"credential": "@config.key", "scope": "tenant"}),
        json!({"credential": "@config.key", "scope": null}),
        json!({"credential": "@config.key", "scope": "app", "principal": "other"}),
        json!({"credential": "@config.key", "scope": "app", "extra": true}),
    ] {
        assert!(codes(json!(["credential-status", selection]))
            .contains(&"ORB_CREDENTIAL_STATUS_SLOT_INVALID"));
    }
}

#[test]
fn scoped_status_does_not_hide_secret_reads_outside_the_credential_leaf() {
    for selection in [
        json!({"credential": "@config.key", "scope": "@config.key"}),
        json!({"credential": "@config.key", "scope": "app", "principal": "@config.key"}),
    ] {
        assert!(codes(json!(["credential-status", selection])).contains(&SECRET_OUTSIDE_AUTH_CODE));
    }
    assert!(codes(json!(["credential-status", {"credential": "@config.key", "scope": "app"}, {"emit": {"success": "@config.key"}}])).contains(&SECRET_OUTSIDE_AUTH_CODE));
}

#[test]
fn controls_bare_user_effects_are_unchanged_and_forget_stays_bare_only() {
    for head in ["credential-status", "credential-forget"] {
        assert!(codes(json!([head, "@config.key"])).is_empty());
        assert!(codes(json!([head, "@config.plain"])).contains(&CREDENTIAL_NOT_SECRET_CODE));
    }
    assert!(
        codes(json!(["credential-forget", {"credential": "@config.key", "scope": "app"}]))
            .contains(&CREDENTIAL_NOT_SECRET_CODE)
    );
}
