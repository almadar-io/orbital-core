//! Where a `secret`-typed config knob may flow (`docs/Almadar_Embedded_Kernel.md`
//! H.5): only into an `http` auth credential slot, and every credential slot
//! must be one.
//!
//! A whole-program walk over the program's JSON, like
//! [`crate::messages::render_only_violations`]. It needs the `@config.<knob>`
//! reads before inline substitutes them with the knob's value, so a caller
//! runs it on the parsed program and on every behavior file `uses` loads
//! (an atom's knobs are typed in the atom).

use serde_json::{Map, Value};

pub const CREDENTIAL_NOT_SECRET_CODE: &str = "ORB_HTTP_AUTH_CREDENTIAL_NOT_SECRET";
pub const SECRET_OUTSIDE_AUTH_CODE: &str = "ORB_SECRET_OUTSIDE_AUTH";

/// One breach of the secret-flow rules, located by program path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretFlowViolation {
    /// An `http` auth credential slot that is not a `secret`-typed knob.
    CredentialNotSecret { path: String, slot: String },
    /// A `secret`-typed knob read anywhere but an auth credential slot.
    OutsideAuth { path: String },
}

impl SecretFlowViolation {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CredentialNotSecret { .. } => CREDENTIAL_NOT_SECRET_CODE,
            Self::OutsideAuth { .. } => SECRET_OUTSIDE_AUTH_CODE,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::CredentialNotSecret { path, .. } | Self::OutsideAuth { path } => path,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::CredentialNotSecret { slot, .. } if slot.starts_with("credential-") => {
                format!("`{slot}` takes a `secret`-typed config knob (@config.<knob>)")
            }
            Self::CredentialNotSecret { slot, .. } => format!(
                "`http` auth.{slot} must be a `secret`-typed config knob (@config.<knob>)"
            ),
            Self::OutsideAuth { .. } => {
                "a `secret`-typed knob is read outside an `http` auth credential slot".to_string()
            }
        }
    }

    pub fn suggestion(&self) -> &'static str {
        match self {
            Self::CredentialNotSecret { .. } => {
                "Declare `<knob>: secret = \"CREDENTIAL_NAME\"` in config and pass @config.<knob>"
            }
            Self::OutsideAuth { .. } => {
                "Secrets are credential names the host resolves; use them only in `http` auth"
            }
        }
    }
}

/// The auth keys that carry a credential name, per scheme.
pub fn credential_slots(scheme: &str) -> &'static [&'static str] {
    match scheme {
        "bearer" | "api-key" | "hmac-sha256" => &["credential"],
        "basic" => &["user", "credential"],
        "oauth2-client" | "token-exchange" => &["clientId", "clientSecret"],
        _ => &[],
    }
}

/// The credential name a slot value carries: a bare `@config.<knob>` string, or a `{ credential, scope? }` object.
fn slot_credential(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) => Some(s),
        Value::Object(o) if o.keys().all(|k| k == "credential" || k == "scope") => {
            o.get("credential").and_then(Value::as_str)
        }
        _ => None,
    }
}

/// A `{ credential, .. }` object: a credential slot wherever a token-exchange body or a slot position allows one.
fn is_slot_object(value: &Value) -> bool {
    matches!(value, Value::Object(o) if o.contains_key("credential"))
}

/// The fields of a token-exchange `body: { json | form: { field: ... } }`.
fn token_body_fields(auth: &Map<String, Value>) -> impl Iterator<Item = (&String, &Value)> {
    auth.get("body")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|body| body.values())
        .filter_map(Value::as_object)
        .flatten()
}

/// `knob` of a whole-string `@config.<knob>` read (no deeper path).
pub fn config_knob(value: &str) -> Option<&str> {
    let knob = value.strip_prefix("@config.")?;
    (!knob.is_empty() && !knob.contains('.')).then_some(knob)
}

/// Every secret-flow breach in `program`, in walk order.
pub fn secret_flow_violations(program: &Value) -> Vec<SecretFlowViolation> {
    let mut out = Vec::new();
    let Some(orbitals) = program.get("orbitals").and_then(Value::as_array) else {
        return out;
    };
    for (oi, orbital) in orbitals.iter().enumerate() {
        for (ti, tr) in orbital.get("traits").and_then(Value::as_array).into_iter().flatten().enumerate() {
            let secrets = secret_knobs(tr);
            let base = format!("orbitals[{oi}].traits[{ti}]");
            let rows = [
                ("stateMachine.transitions", tr.get("stateMachine").and_then(|m| m.get("transitions"))),
                ("ticks", tr.get("ticks")),
                ("listens", tr.get("listens")),
            ];
            for (field, rows) in rows {
                for (ri, row) in rows.and_then(Value::as_array).into_iter().flatten().enumerate() {
                    let at = format!("{base}.{field}[{ri}]");
                    if let Some(guard) = row.get("guard") {
                        check_expr(guard, &secrets, &format!("{at}.guard"), &mut out);
                    }
                    for (ei, effect) in row.get("effects").and_then(Value::as_array).into_iter().flatten().enumerate() {
                        check_expr(effect, &secrets, &format!("{at}.effects[{ei}]"), &mut out);
                    }
                }
            }
        }
    }
    out
}

fn secret_knobs(trait_def: &Value) -> Vec<&str> {
    trait_def
        .get("config")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, field)| field.get("type").and_then(Value::as_str) == Some("secret"))
        .map(|(name, _)| name.as_str())
        .collect()
}

fn is_http(items: &[Value]) -> bool {
    items.first().and_then(Value::as_str) == Some("http")
}

/// `credential-status` / `credential-forget`: their first argument is a credential slot.
fn credential_effect(items: &[Value]) -> Option<&str> {
    items
        .first()
        .and_then(Value::as_str)
        .filter(|head| matches!(*head, "credential-status" | "credential-forget"))
}

/// The trailing options object of a keyed call (`emit` and/or `key`), as opposed
/// to a request object.
fn has_trailing_options(items: &[Value]) -> bool {
    items.len() > 4
        && matches!(items.last(), Some(Value::Object(o)) if o.contains_key("emit") || o.contains_key("key"))
}

fn http_request(items: &[Value]) -> Option<&Map<String, Value>> {
    match items.get(4) {
        Some(Value::Object(request)) if items.len() == 6 || !has_trailing_options(items) => Some(request),
        _ => None,
    }
}

fn check_expr(expr: &Value, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    each_http_call(expr, &mut |items| check_credential_slots(items, secrets, path, out));
    each_credential_effect(expr, &mut |head, items| {
        let is_secret_knob = items
            .get(1)
            .and_then(Value::as_str)
            .and_then(config_knob)
            .is_some_and(|knob| secrets.contains(&knob));
        if !is_secret_knob {
            out.push(SecretFlowViolation::CredentialNotSecret {
                path: path.to_string(),
                slot: head.to_string(),
            });
        }
    });
    if !secrets.is_empty() {
        scan_reads(expr, secrets, path, out);
    }
}

fn each_http_call<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'a [Value])) {
    match expr {
        Value::Array(items) => {
            if is_http(items) {
                visit(items);
            }
            for item in items {
                each_http_call(item, visit);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                each_http_call(value, visit);
            }
        }
        _ => {}
    }
}

fn each_credential_effect<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'a str, &'a [Value])) {
    match expr {
        Value::Array(items) => {
            if let Some(head) = credential_effect(items) {
                visit(head, items);
            }
            for item in items {
                each_credential_effect(item, visit);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                each_credential_effect(value, visit);
            }
        }
        _ => {}
    }
}

fn check_credential_slots(items: &[Value], secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    let Some(Value::Object(auth)) = http_request(items).and_then(|r| r.get("auth")) else {
        return;
    };
    let Some(scheme) = auth.get("scheme").and_then(Value::as_str) else {
        return;
    };
    let mut check = |slot: String, value: &Value| {
        let is_secret_knob = slot_credential(value)
            .and_then(config_knob)
            .is_some_and(|knob| secrets.contains(&knob));
        if !is_secret_knob {
            out.push(SecretFlowViolation::CredentialNotSecret {
                path: path.to_string(),
                slot,
            });
        }
    };
    for slot in credential_slots(scheme) {
        if let Some(value) = auth.get(*slot) {
            check((*slot).to_string(), value);
        }
    }
    if scheme == "token-exchange" {
        for (name, value) in token_body_fields(auth) {
            if is_slot_object(value) {
                check(format!("body.{name}"), value);
            }
        }
    }
}

fn is_secret_read(s: &str, secrets: &[&str]) -> bool {
    s.strip_prefix("@config.").is_some_and(|rest| {
        let knob = rest.split('.').next().unwrap_or(rest);
        secrets.contains(&knob)
    })
}

fn scan_reads(expr: &Value, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    match expr {
        Value::String(s) => {
            if is_secret_read(s, secrets) {
                out.push(SecretFlowViolation::OutsideAuth { path: path.to_string() });
            }
        }
        Value::Array(items) => {
            let http = is_http(items);
            let credential = credential_effect(items).is_some();
            for (index, item) in items.iter().enumerate() {
                match (http, item) {
                    (true, Value::Object(request)) => scan_request_reads(request, secrets, path, out),
                    _ if credential && index == 1 => {}
                    _ => scan_reads(item, secrets, path, out),
                }
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                scan_reads(value, secrets, path, out);
            }
        }
        _ => {}
    }
}

fn scan_request_reads(request: &Map<String, Value>, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    for (key, value) in request {
        match (key.as_str(), value) {
            ("auth", Value::Object(auth)) => {
                let slots = credential_slots(auth.get("scheme").and_then(Value::as_str).unwrap_or(""));
                let scheme = auth.get("scheme").and_then(Value::as_str);
                for (slot, slot_value) in auth {
                    if slots.contains(&slot.as_str()) {
                        continue;
                    }
                    match (scheme, slot.as_str(), slot_value) {
                        (Some("token-exchange"), "body", Value::Object(body)) => {
                            for fields in body.values() {
                                for (_, field) in fields.as_object().into_iter().flatten() {
                                    if !is_slot_object(field) {
                                        scan_reads(field, secrets, path, out);
                                    }
                                }
                            }
                        }
                        _ => scan_reads(slot_value, secrets, path, out),
                    }
                }
            }
            _ => scan_reads(value, secrets, path, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn program(config: Value, effects: Value) -> Value {
        json!({ "orbitals": [{ "traits": [{
            "config": config,
            "stateMachine": { "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": effects }] }
        }] }] })
    }

    fn config() -> Value {
        json!({ "apiKey": { "type": "secret", "default": "K" }, "plain": { "type": "string", "default": "x" } })
    }

    fn call(request: Value) -> Value {
        json!([["http", "GET", "https://a.example", "/s", request, { "emit": { "success": "OK" } }]])
    }

    fn codes(program: &Value) -> Vec<&'static str> {
        secret_flow_violations(program).iter().map(SecretFlowViolation::code).collect()
    }

    #[test]
    fn a_non_secret_credential_slot_is_reported_per_scheme_slot() {
        for auth in [
            json!({ "scheme": "bearer", "credential": "@config.plain" }),
            json!({ "scheme": "bearer", "credential": "LITERAL" }),
            json!({ "scheme": "bearer", "credential": "@config.missing" }),
            json!({ "scheme": "basic", "user": "alice", "credential": "@config.apiKey" }),
            json!({ "scheme": "oauth2-client", "tokenUrl": "https://a.example", "clientId": "@config.apiKey", "clientSecret": "@payload.s" }),
        ] {
            let found = codes(&program(config(), call(json!({ "auth": auth.clone() }))));
            assert_eq!(found, vec![CREDENTIAL_NOT_SECRET_CODE], "{auth}");
        }
    }

    #[test]
    fn control_secret_knobs_in_every_credential_slot_are_clean() {
        for auth in [
            json!({ "scheme": "bearer", "credential": "@config.apiKey" }),
            json!({ "scheme": "basic", "user": "@config.apiKey", "credential": "@config.apiKey" }),
            json!({ "scheme": "api-key", "credential": "@config.apiKey", "name": "k", "in": "header" }),
            json!({ "scheme": "hmac-sha256", "credential": "@config.apiKey", "name": "S", "signs": "@payload.m", "encoding": "hex" }),
        ] {
            assert!(codes(&program(config(), call(json!({ "auth": auth.clone() })))).is_empty(), "{auth}");
        }
    }

    fn exchange(body: Value) -> Value {
        call(json!({ "auth": {
            "scheme": "token-exchange", "tokenUrl": "https://a.example", "body": body,
            "clientAuth": "post", "clientId": "@config.apiKey", "clientSecret": { "credential": "@config.apiKey", "scope": "user" }
        } }))
    }

    #[test]
    fn a_token_exchange_body_accepts_credential_slots_of_secret_knobs() {
        for body in [
            json!({ "form": { "refresh_token": { "credential": "@config.apiKey", "scope": "user" }, "scope": "read" } }),
            json!({ "json": { "username": { "credential": "@config.apiKey" }, "device": "web", "n": 7 } }),
            json!({ "form": {} }),
        ] {
            assert!(codes(&program(config(), exchange(body.clone()))).is_empty(), "{body}");
        }
    }

    #[test]
    fn a_token_exchange_body_slot_that_is_not_a_secret_knob_is_reported_with_its_field() {
        for slot in [
            json!({ "credential": "@config.plain" }),
            json!({ "credential": "LITERAL", "scope": "user" }),
            json!({ "credential": "@config.missing" }),
        ] {
            let p = program(config(), exchange(json!({ "form": { "token": slot.clone() } })));
            let found = secret_flow_violations(&p);
            assert_eq!(found.len(), 1, "{slot}");
            assert_eq!(found[0].code(), CREDENTIAL_NOT_SECRET_CODE);
            assert!(matches!(&found[0], SecretFlowViolation::CredentialNotSecret { slot, .. } if slot == "body.token"));
        }
    }

    #[test]
    fn a_bare_secret_read_in_a_token_exchange_body_is_still_outside_auth() {
        let p = program(config(), exchange(json!({ "form": { "token": "@config.apiKey" } })));
        assert_eq!(codes(&p), vec![SECRET_OUTSIDE_AUTH_CODE]);
        let nested = program(config(), exchange(json!({ "json": { "a": { "b": "@config.apiKey" } } })));
        assert_eq!(codes(&nested), vec![SECRET_OUTSIDE_AUTH_CODE]);
    }

    #[test]
    fn slot_objects_are_legal_only_in_slot_positions_and_the_token_exchange_body() {
        let headers = call(json!({ "headers": { "X": { "credential": "@config.apiKey" } } }));
        assert_eq!(codes(&program(config(), headers)), vec![SECRET_OUTSIDE_AUTH_CODE]);
        let json_body = call(json!({ "json": { "k": { "credential": "@config.apiKey" } } }));
        assert_eq!(codes(&program(config(), json_body)), vec![SECRET_OUTSIDE_AUTH_CODE]);
        let other_scheme = call(json!({ "auth": { "scheme": "bearer", "credential": "@config.apiKey", "body": { "form": { "k": { "credential": "@config.apiKey" } } } } }));
        assert_eq!(codes(&program(config(), other_scheme)), vec![SECRET_OUTSIDE_AUTH_CODE]);
        let set = json!([["set", "@entity.t", { "credential": "@config.apiKey" }]]);
        assert_eq!(codes(&program(config(), set)), vec![SECRET_OUTSIDE_AUTH_CODE]);
    }

    #[test]
    fn a_scoped_slot_object_is_accepted_in_a_fixed_slot_and_a_malformed_one_is_reported() {
        let scoped = call(json!({ "auth": { "scheme": "bearer", "credential": { "credential": "@config.apiKey", "scope": "user" } } }));
        assert!(codes(&program(config(), scoped)).is_empty());
        for bad in [
            json!({ "credential": "@config.apiKey", "extra": 1 }),
            json!({ "scope": "user" }),
            json!({ "credential": "@config.plain", "scope": "user" }),
        ] {
            let p = program(config(), call(json!({ "auth": { "scheme": "bearer", "credential": bad.clone() } })));
            assert_eq!(codes(&p), vec![CREDENTIAL_NOT_SECRET_CODE], "{bad}");
        }
    }

    #[test]
    fn a_secret_read_outside_auth_is_reported() {
        let set = json!([["set", "@entity.token", "@config.apiKey"]]);
        assert_eq!(codes(&program(config(), set)), vec![SECRET_OUTSIDE_AUTH_CODE]);

        let header = call(json!({ "headers": { "X": "@config.apiKey" } }));
        assert_eq!(codes(&program(config(), header)), vec![SECRET_OUTSIDE_AUTH_CODE]);

        let non_slot = call(json!({ "auth": { "scheme": "api-key", "credential": "@config.apiKey", "name": "@config.apiKey", "in": "header" } }));
        assert_eq!(codes(&program(config(), non_slot)), vec![SECRET_OUTSIDE_AUTH_CODE]);
    }

    #[test]
    fn a_secret_read_in_a_guard_and_a_tick_is_reported() {
        let mut p = program(config(), json!([]));
        p["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["guard"] = json!(["=", "@config.apiKey", "x"]);
        p["orbitals"][0]["traits"][0]["ticks"] = json!([{ "effects": [["set", "@entity.t", "@config.apiKey"]] }]);
        assert_eq!(codes(&p), vec![SECRET_OUTSIDE_AUTH_CODE, SECRET_OUTSIDE_AUTH_CODE]);
    }

    #[test]
    fn control_reading_a_plain_knob_is_clean() {
        let set = json!([["set", "@entity.token", "@config.plain"]]);
        assert!(codes(&program(config(), set)).is_empty());
    }

    #[test]
    fn a_secret_named_in_another_knobs_config_declaration_stays_legal() {
        let cfg = json!({
            "apiKey": { "type": "secret", "default": "K" },
            "alias": { "type": "secret", "default": "@config.apiKey" }
        });
        assert!(codes(&program(cfg, json!([]))).is_empty());
    }

    #[test]
    fn the_violation_path_names_the_effect() {
        let found = secret_flow_violations(&program(config(), json!([["set", "@entity.t", "@config.apiKey"]])));
        assert_eq!(found[0].path(), "orbitals[0].traits[0].stateMachine.transitions[0].effects[0]");
        assert!(!found[0].message().is_empty() && !found[0].suggestion().is_empty());
    }

    #[test]
    fn the_credential_effects_take_a_secret_knob_and_nothing_else() {
        for head in ["credential-status", "credential-forget"] {
            let ok = json!([[head, "@config.apiKey", { "emit": { "success": "OK" } }]]);
            assert!(codes(&program(config(), ok)).is_empty(), "{head}");
            for bad in ["@config.plain", "LITERAL", "@payload.name"] {
                let found = codes(&program(config(), json!([[head, bad]])));
                assert_eq!(found, vec![CREDENTIAL_NOT_SECRET_CODE], "{head} {bad}");
            }
        }
    }

    #[test]
    fn a_secret_read_in_a_credential_effects_options_is_still_outside_auth() {
        let found = codes(&program(config(), json!([["credential-status", "@config.apiKey", { "x": "@config.apiKey" }]])));
        assert_eq!(found, vec![SECRET_OUTSIDE_AUTH_CODE]);
    }

    #[test]
    fn a_program_without_orbitals_has_no_violations() {
        assert!(secret_flow_violations(&json!({})).is_empty());
    }
}
