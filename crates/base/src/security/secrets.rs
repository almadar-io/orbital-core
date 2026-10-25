//! Where a `secret`-typed config knob may flow (`docs/Almadar_Embedded_Kernel.md`
//! H.5): only into an `http` auth credential slot, and every credential slot
//! must be one.
//!
//! A whole-program walk over the program's JSON, like
//! [`crate::messages::render_only_violations`]. It needs the `@config.<knob>`
//! reads before inline substitutes them with the knob's value, so a caller
//! runs it on the parsed program and on every behavior file `uses` loads
//! (an atom's knobs are typed in the atom).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde_json::{Map, Value};

pub const CREDENTIAL_NOT_SECRET_CODE: &str = "ORB_HTTP_AUTH_CREDENTIAL_NOT_SECRET";
pub const SECRET_OUTSIDE_AUTH_CODE: &str = "ORB_SECRET_OUTSIDE_AUTH";
pub const CREDENTIAL_STATUS_SLOT_INVALID_CODE: &str = "ORB_CREDENTIAL_STATUS_SLOT_INVALID";

/// The slot name of a `redis` call's `auth` option in a [`SecretFlowViolation::CredentialNotSecret`].
pub const REDIS_AUTH_SLOT: &str = "redis.auth";

/// The slot name of a `postgres` call's `auth` option.
pub const POSTGRES_AUTH_SLOT: &str = "postgres.auth";

/// One breach of the secret-flow rules, located by program path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretFlowViolation {
    CredentialStatusSlotInvalid { path: String },
    /// An `http` auth credential slot that is not a `secret`-typed knob.
    CredentialNotSecret { path: String, slot: String },
    /// A `secret`-typed knob read anywhere but an auth credential slot.
    OutsideAuth { path: String },
}

impl SecretFlowViolation {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CredentialStatusSlotInvalid { .. } => CREDENTIAL_STATUS_SLOT_INVALID_CODE,
            Self::CredentialNotSecret { .. } => CREDENTIAL_NOT_SECRET_CODE,
            Self::OutsideAuth { .. } => SECRET_OUTSIDE_AUTH_CODE,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::CredentialNotSecret { path, .. }
            | Self::OutsideAuth { path }
            | Self::CredentialStatusSlotInvalid { path } => path,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::CredentialStatusSlotInvalid { .. } => {
                "credential-status: expected a secret knob or { credential, scope: app | user }".to_string()
            }
            Self::CredentialNotSecret { slot, .. } if slot.starts_with("credential-") => {
                format!("`{slot}` takes a `secret`-typed config knob (@config.<knob>)")
            }
            Self::CredentialNotSecret { slot, .. } if slot == REDIS_AUTH_SLOT => {
                "`redis` options.auth must be a `secret`-typed config knob (@config.<knob>)".to_string()
            }
            Self::CredentialNotSecret { slot, .. } if slot == POSTGRES_AUTH_SLOT => {
                "`postgres` options.auth must be a `secret`-typed config knob (@config.<knob>)".to_string()
            }
            Self::CredentialNotSecret { slot, .. } => format!(
                "`http` auth.{slot} must be a `secret`-typed config knob (@config.<knob>)"
            ),
            Self::OutsideAuth { .. } => {
                "a `secret`-typed knob is read outside an `http` auth, `redis` or `postgres` options.auth credential slot".to_string()
            }
        }
    }

    pub fn suggestion(&self) -> &'static str {
        match self {
            Self::CredentialStatusSlotInvalid { .. } => {
                "Use a secret knob directly, or { credential: @config.<knob>, scope: app | user }"
            }
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
        "jwt-bearer" => &["key"],
        "aws-sigv4" => &["accessKeyId", "secretAccessKey", "sessionToken"],
        "vapid" => &["privateKey"],
        _ => &[],
    }
}

/// The credential name a slot value carries: a bare `@config.<knob>` string, or a `{ credential, scope? }` object.
pub fn slot_credential(value: &Value) -> Option<&str> {
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

pub fn auth_uses_user_credentials(auth: &Map<String, Value>) -> bool {
    let is_user = |value: &Value| slot_credential(value).is_some()
        && value.as_object().and_then(|slot| slot.get("scope")).and_then(Value::as_str) == Some("user");
    let scheme = auth.get("scheme").and_then(Value::as_str).unwrap_or("");
    credential_slots(scheme).iter().filter_map(|key| auth.get(*key)).any(is_user)
        || (scheme == "token-exchange" && token_body_fields(auth).any(|(_, value)| is_user(value)))
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

fn is_redis(items: &[Value]) -> bool {
    items.first().and_then(Value::as_str) == Some("redis")
}

fn is_postgres(items: &[Value]) -> bool {
    items.first().and_then(Value::as_str) == Some("postgres")
}

/// The index of a driver call's first options object: after `origin` and `command` for redis, after `origin`, `sql`
/// and `params` for postgres.
fn driver_options_from(items: &[Value]) -> usize {
    if is_postgres(items) { 4 } else { 3 }
}

/// The `auth` credential slot of a `redis` or `postgres` call: the option may sit in either trailing object.
fn redis_auth_slots(items: &[Value]) -> impl Iterator<Item = &Value> {
    items.iter().skip(driver_options_from(items)).filter_map(Value::as_object).filter_map(|options| options.get("auth"))
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
    each_oauth_authorize(expr, &mut |items| {
        if let Some(client) = items.get(1) {
            check_oauth_client(client, secrets, path, out);
        }
    });
    each_scrypt_salt(expr, &mut |salt| {
        let is_secret_knob = salt
            .as_str()
            .and_then(config_knob)
            .is_some_and(|knob| secrets.contains(&knob));
        if !is_secret_knob {
            out.push(SecretFlowViolation::CredentialNotSecret {
                path: path.to_string(),
                slot: SCRYPT_SALT_SLOT.to_string(),
            });
        }
    });
    each_credential_effect(expr, &mut |head, items| {
        let credential = if head == "credential-status" {
            let slot = items
                .get(1)
                .and_then(|selection| serde_json::from_value::<crate::value::Value>(selection.clone()).ok())
                .and_then(|selection| {
                    crate::http_wire::parse_credential_status_slot(&selection).ok()
                });
            let Some(slot) = slot else {
                out.push(SecretFlowViolation::CredentialStatusSlotInvalid {
                    path: path.to_string(),
                });
                return;
            };
            Some(slot.name)
        } else {
            items.get(1).and_then(Value::as_str).map(str::to_string)
        };
        let is_secret_knob = credential
            .as_deref()
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

/// `oauth-authorize`: its first argument is an OAuth client object whose `clientId` / `clientSecret` are credential slots.
fn is_oauth_authorize(items: &[Value]) -> bool {
    items.first().and_then(Value::as_str) == Some("oauth-authorize")
}

/// The credential slots of an OAuth client object (`oauth-client` auth, `oauth-authorize`).
const OAUTH_CLIENT_SLOTS: [&str; 2] = ["clientId", "clientSecret"];

fn check_oauth_client(client: &Value, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    let Some(client) = client.as_object() else { return };
    for slot in OAUTH_CLIENT_SLOTS {
        let Some(value) = client.get(slot) else { continue };
        let is_secret_knob = slot_credential(value)
            .and_then(config_knob)
            .is_some_and(|knob| secrets.contains(&knob));
        if !is_secret_knob {
            out.push(SecretFlowViolation::CredentialNotSecret {
                path: path.to_string(),
                slot: format!("client.{slot}"),
            });
        }
    }
}

/// Reads of a secret inside an OAuth client object other than its credential slots.
fn scan_oauth_client_reads(client: &Value, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    match client {
        Value::Object(fields) => {
            for (key, value) in fields {
                if !OAUTH_CLIENT_SLOTS.contains(&key.as_str()) {
                    scan_reads(value, secrets, path, out);
                }
            }
        }
        other => scan_reads(other, secrets, path, out),
    }
}

fn each_http_call<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'a [Value])) {
    match expr {
        Value::Array(items) => {
            if is_http(items) || is_redis(items) || is_postgres(items) {
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

fn each_oauth_authorize<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'a [Value])) {
    match expr {
        Value::Array(items) => {
            if is_oauth_authorize(items) {
                visit(items);
            }
            for item in items {
                each_oauth_authorize(item, visit);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                each_oauth_authorize(value, visit);
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

/// Every credential slot of an `http` call, named by slot, in the order the secret-flow rules check them.
fn credential_slot_values(items: &[Value]) -> Vec<(String, &Value)> {
    let mut out = Vec::new();
    if is_redis(items) || is_postgres(items) {
        let name = if is_postgres(items) { POSTGRES_AUTH_SLOT } else { REDIS_AUTH_SLOT };
        out.extend(redis_auth_slots(items).map(|slot| (name.to_string(), slot)));
        return out;
    }
    let Some(Value::Object(auth)) = http_request(items).and_then(|r| r.get("auth")) else {
        return out;
    };
    let Some(scheme) = auth.get("scheme").and_then(Value::as_str) else {
        return out;
    };
    if scheme == "oauth-client" {
        let clients = auth
            .get("client")
            .into_iter()
            .chain(auth.get("clients").and_then(Value::as_object).into_iter().flat_map(Map::values));
        for client in clients.filter_map(Value::as_object) {
            for slot in OAUTH_CLIENT_SLOTS {
                if let Some(value) = client.get(slot) {
                    out.push((format!("client.{slot}"), value));
                }
            }
        }
        return out;
    }
    for slot in credential_slots(scheme) {
        if let Some(value) = auth.get(*slot) {
            out.push(((*slot).to_string(), value));
        }
    }
    if scheme == "token-exchange" {
        for (name, value) in token_body_fields(auth) {
            if is_slot_object(value) {
                out.push((format!("body.{name}"), value));
            }
        }
        if let Some(credential) = auth.get("rotate").and_then(Value::as_object).and_then(|rotate| rotate.get("credential")) {
            out.push(("rotate.credential".to_string(), credential));
        }
    }
    out
}

/// Every credential slot value of every `http` call in `expr`, as written, with its slot name, in walk order.
/// Read the credential name out of one with [`slot_credential`].
pub fn http_credential_slots(expr: &Value) -> Vec<(String, &Value)> {
    let mut out = Vec::new();
    each_http_call(expr, &mut |items| out.extend(credential_slot_values(items)));
    out
}

/// The credential slot of every `credential-status` and `credential-forget`, and the salt of every `hash/scrypt`, in
/// `expr`, as written.
fn effect_credential_slots(expr: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    each_credential_effect(expr, &mut |_, items| out.extend(items.get(1)));
    each_scrypt_salt(expr, &mut |salt| out.push(salt));
    out
}

/// The slot name of a `hash/scrypt` salt in a [`SecretFlowViolation::CredentialNotSecret`].
pub const SCRYPT_SALT_SLOT: &str = "hash/scrypt.salt";

/// Every `["hash/scrypt", input, salt]` salt argument in `expr`: a credential slot the kernel resolves itself.
fn each_scrypt_salt<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'a Value)) {
    match expr {
        Value::Array(items) => {
            if items.first().and_then(Value::as_str) == Some("hash/scrypt") {
                if let Some(salt) = items.get(2) {
                    visit(salt);
                }
            }
            for item in items {
                each_scrypt_salt(item, visit);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                each_scrypt_salt(value, visit);
            }
        }
        _ => {}
    }
}

fn check_credential_slots(items: &[Value], secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    for (slot, value) in credential_slot_values(items) {
        let is_secret_knob = slot_credential(value)
            .and_then(config_knob)
            .is_some_and(|knob| secrets.contains(&knob));
        if !is_secret_knob {
            out.push(SecretFlowViolation::CredentialNotSecret {
                path: path.to_string(),
                slot,
            });
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
            let redis = is_redis(items) || is_postgres(items);
            let options_from = driver_options_from(items);
            let credential = credential_effect(items).is_some();
            let authorize = is_oauth_authorize(items);
            let scrypt = items.first().and_then(Value::as_str) == Some("hash/scrypt");
            for (index, item) in items.iter().enumerate() {
                match (http, item) {
                    _ if scrypt && index == 2 && item.as_str().and_then(config_knob).is_some_and(|knob| secrets.contains(&knob)) => {}
                    _ if authorize && index == 1 => scan_oauth_client_reads(item, secrets, path, out),
                    (true, Value::Object(request)) => scan_request_reads(request, secrets, path, out),
                    (_, Value::Object(options)) if redis && index >= options_from => scan_redis_option_reads(options, secrets, path, out),
                    _ if credential && index == 1 => {
                        if matches!(items.first().and_then(Value::as_str), Some("credential-status")) {
                            if let Value::Object(selection) = item {
                                for (key, value) in selection {
                                    let allowed_leaf = key == "credential"
                                        && value
                                            .as_str()
                                            .and_then(config_knob)
                                            .is_some_and(|knob| secrets.contains(&knob));
                                    if !allowed_leaf {
                                        scan_reads(value, secrets, path, out);
                                    }
                                }
                            }
                        }
                    }
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

/// A `redis` call's option object: its `auth` credential slot is the one place a secret knob may be read.
fn scan_redis_option_reads(options: &Map<String, Value>, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    for (key, value) in options {
        match (key.as_str(), value) {
            ("auth", Value::String(_)) => {}
            ("auth", Value::Object(slot)) => {
                for (name, entry) in slot {
                    if name != "credential" {
                        scan_reads(entry, secrets, path, out);
                    }
                }
            }
            _ => scan_reads(value, secrets, path, out),
        }
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
                        (Some("oauth-client"), "client", client) => scan_oauth_client_reads(client, secrets, path, out),
                        (Some("oauth-client"), "clients", Value::Object(clients)) => {
                            for client in clients.values() {
                                scan_oauth_client_reads(client, secrets, path, out);
                            }
                        }
                        (Some("token-exchange"), "body", Value::Object(body)) => {
                            for fields in body.values() {
                                for (_, field) in fields.as_object().into_iter().flatten() {
                                    if !is_slot_object(field) {
                                        scan_reads(field, secrets, path, out);
                                    }
                                }
                            }
                        }
                        (Some("token-exchange"), "rotate", Value::Object(rotate)) => {
                            for (key, value) in rotate {
                                if key != "credential" {
                                    scan_reads(value, secrets, path, out);
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

/// A credential name a program declares: a `secret`-typed config knob read by an `http` auth credential slot or named by
/// a `credential-status` or `credential-forget` effect or the salt of a `hash/scrypt`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramCredential {
    /// The orbital that declares the knob.
    pub group: String,
    /// The knob's default: the env var / credential name.
    pub env_var: String,
    /// The knob's `@description`.
    pub description: String,
}

/// The credentials `program` declares, in walk order (orbital, trait, slot), each `(group, env_var)` once. A slot
/// counts when it reads `@config.<knob>` of a `secret` knob with a string default, or, once inline has replaced the
/// read with the knob's value, is a literal equal to the default of a `secret` knob of the same trait.
pub fn program_credentials(program: &Value) -> Vec<ProgramCredential> {
    let mut out: Vec<ProgramCredential> = Vec::new();
    let orbitals = program.get("orbitals").and_then(Value::as_array).into_iter().flatten();
    for orbital in orbitals {
        let group = orbital.get("name").and_then(Value::as_str).unwrap_or_default();
        for trait_def in orbital.get("traits").and_then(Value::as_array).into_iter().flatten() {
            let Some(config) = trait_def.get("config").and_then(Value::as_object) else { continue };
            let is_secret = |field: &Value| field.get("type").and_then(Value::as_str) == Some("secret");
            let read_slots = effect_credential_slots(trait_def);
            for slot in http_credential_slots(trait_def).into_iter().map(|(_, slot)| slot).chain(read_slots) {
                let Some(credential) = slot_credential(slot) else { continue };
                let Some(field) = config_knob(credential).and_then(|knob| config.get(knob)).filter(|f| is_secret(f)) else {
                    continue;
                };
                let Some(env_var) = field.get("default").and_then(Value::as_str).filter(|d| !d.is_empty()) else { continue };
                if out.iter().any(|c| c.group == group && c.env_var == env_var) {
                    continue;
                }
                out.push(ProgramCredential {
                    group: group.to_string(),
                    env_var: env_var.to_string(),
                    description: field.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                });
            }
        }
    }
    out
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
            json!({ "scheme": "jwt-bearer", "key": "@config.plain" }),
            json!({ "scheme": "vapid", "privateKey": "@config.plain" }),
            json!({ "scheme": "aws-sigv4", "accessKeyId": "@config.plain", "secretAccessKey": "@config.apiKey" }),
            json!({ "scheme": "aws-sigv4", "accessKeyId": "@config.apiKey", "secretAccessKey": "@config.apiKey", "sessionToken": "@config.plain" }),
            json!({ "scheme": "basic", "user": "alice", "credential": "@config.apiKey" }),
            json!({ "scheme": "oauth2-client", "tokenUrl": "https://a.example", "clientId": "@config.apiKey", "clientSecret": "@payload.s" }),
        ] {
            let found = codes(&program(config(), call(json!({ "auth": auth.clone() }))));
            assert_eq!(found, vec![CREDENTIAL_NOT_SECRET_CODE], "{auth}");
        }
    }

    fn redis_call(options: Value) -> Value {
        json!([["redis", "redis://h", ["PING"], options, { "emit": { "success": "OK" } }]])
    }

    #[test]
    fn a_redis_auth_slot_must_be_a_secret_knob_and_may_not_leak_it_elsewhere() {
        for auth in [json!("@config.plain"), json!("LITERAL"), json!({ "credential": "@config.plain", "scope": "user" })] {
            let found = codes(&program(config(), redis_call(json!({ "auth": auth.clone() }))));
            assert_eq!(found, vec![CREDENTIAL_NOT_SECRET_CODE], "{auth}");
        }
        let leaked = codes(&program(config(), redis_call(json!({ "auth": "@config.apiKey", "user": "@config.apiKey" }))));
        assert!(leaked.contains(&SECRET_OUTSIDE_AUTH_CODE), "{leaked:?}");
    }

    #[test]
    fn postgres_auth_is_a_secret_slot_after_the_params_and_other_reads_of_it_are_outside_auth() {
        let call = |options: Value| json!([["postgres", "postgres://h", "SELECT 1", [], options, { "emit": { "success": "OK" } }]]);
        let bad = codes(&program(config(), call(json!({ "auth": "@config.plain" }))));
        assert_eq!(bad, vec![CREDENTIAL_NOT_SECRET_CODE]);
        let leaked = codes(&program(config(), call(json!({ "auth": "@config.apiKey", "user": "@config.apiKey" }))));
        assert!(leaked.contains(&SECRET_OUTSIDE_AUTH_CODE), "{leaked:?}");
        assert!(codes(&program(config(), call(json!({ "auth": "@config.apiKey", "database": "main" })))).is_empty());
    }

    #[test]
    fn control_a_secret_knob_in_a_redis_auth_slot_is_clean() {
        for auth in [json!("@config.apiKey"), json!({ "credential": "@config.apiKey", "scope": "app" })] {
            assert!(codes(&program(config(), redis_call(json!({ "auth": auth.clone(), "db": 1 })))).is_empty(), "{auth}");
        }
        assert!(codes(&program(config(), redis_call(json!({ "db": 1 })))).is_empty());
    }

    #[test]
    fn control_secret_knobs_in_every_credential_slot_are_clean() {
        for auth in [
            json!({ "scheme": "bearer", "credential": "@config.apiKey" }),
            json!({ "scheme": "jwt-bearer", "key": "@config.apiKey" }),
            json!({ "scheme": "vapid", "privateKey": {"credential":"@config.apiKey","scope":"user"} }),
            json!({ "scheme": "aws-sigv4", "accessKeyId": "@config.apiKey", "secretAccessKey": {"credential":"@config.apiKey","scope":"user"}, "sessionToken":"@config.apiKey" }),
            json!({ "scheme": "basic", "user": "@config.apiKey", "credential": "@config.apiKey" }),
            json!({ "scheme": "api-key", "credential": "@config.apiKey", "name": "k", "in": "header" }),
            json!({ "scheme": "hmac-sha256", "credential": "@config.apiKey", "name": "S", "signs": "@payload.m", "encoding": "hex" }),
        ] {
            assert!(codes(&program(config(), call(json!({ "auth": auth.clone() })))).is_empty(), "{auth}");
        }
    }

    fn rotating(credential: Value) -> Value {
        call(json!({ "auth": {
            "scheme": "token-exchange", "tokenUrl": "https://a.example", "grant": "refresh_token",
            "body": { "form": { "refresh_token": { "credential": "@config.apiKey" } } },
            "rotate": { "field": "refresh_token", "credential": credential }
        } }))
    }

    #[test]
    fn a_rotate_credential_is_an_auth_slot_that_must_be_a_secret_knob() {
        for ok in [json!("@config.apiKey"), json!({ "credential": "@config.apiKey", "scope": "user" })] {
            assert!(codes(&program(config(), rotating(ok.clone()))).is_empty(), "{ok}");
        }
        for bad in [json!("@config.plain"), json!("LITERAL"), json!({ "credential": "@config.plain" })] {
            let found = secret_flow_violations(&program(config(), rotating(bad.clone())));
            assert_eq!(found.len(), 1, "{bad}");
            assert!(matches!(&found[0], SecretFlowViolation::CredentialNotSecret { slot, .. } if slot == "rotate.credential"), "{bad}");
        }
    }

    #[test]
    fn a_secret_read_in_another_rotate_key_is_outside_auth() {
        let p = program(config(), call(json!({ "auth": {
            "scheme": "token-exchange", "tokenUrl": "https://a.example", "grant": "refresh_token",
            "body": { "form": { "refresh_token": { "credential": "@config.apiKey" } } },
            "rotate": { "field": "@config.apiKey", "credential": "@config.apiKey" }
        } })));
        assert_eq!(codes(&p), vec![SECRET_OUTSIDE_AUTH_CODE]);
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
    fn a_scrypt_salt_is_a_legal_secret_sink_and_declares_the_credential_it_names() {
        let hashed = |salt: &str| json!([["let", [["h", ["hash/scrypt", "@payload.x", salt]]], ["emit", "OK", { "h": "@h" }]]]);
        assert!(codes(&program(config(), hashed("@config.apiKey"))).is_empty());
        let declared = program_credentials(&json!({ "orbitals": [{ "name": "KeysOrbital", "traits": [{
            "config": { "apiKey": { "type": "secret", "default": "ALMADAR_API_KEY_SALT", "description": "salt" } },
            "stateMachine": { "transitions": [{ "effects": hashed("@config.apiKey") }] }
        }] }] }));
        assert_eq!(declared.len(), 1);
        assert_eq!((declared[0].group.as_str(), declared[0].env_var.as_str()), ("KeysOrbital", "ALMADAR_API_KEY_SALT"));
        for head in ["credential-status", "credential-forget"] {
            let probed = program_credentials(&json!({ "orbitals": [{ "name": "ProbeOrbital", "traits": [{
                "config": { "token": { "type": "secret", "default": "PROBED_TOKEN", "description": "probed" } },
                "stateMachine": { "transitions": [{ "effects": [[head, { "credential": "@config.token", "scope": "app" }]] }] }
            }] }] }));
            assert_eq!(probed.len(), 1, "{head}: a secret knob a credential effect names is a declared credential");
            assert_eq!(probed[0].env_var, "PROBED_TOKEN");
        }
        let unread = program_credentials(&json!({ "orbitals": [{ "name": "KeysOrbital", "traits": [{
            "config": { "apiKey": { "type": "secret", "default": "ALMADAR_API_KEY_SALT" } },
            "stateMachine": { "transitions": [{ "effects": [["set", "@entity.x", 1]] }] }
        }] }] }));
        assert!(unread.is_empty(), "a secret knob nothing reads is not declared");
        assert_eq!(codes(&program(config(), hashed("@config.plain"))), vec![CREDENTIAL_NOT_SECRET_CODE]);
        assert_eq!(codes(&program(config(), hashed("LITERAL_SALT"))), vec![CREDENTIAL_NOT_SECRET_CODE]);
        let leaked = codes(&program(config(), json!([["hash/scrypt", "@config.apiKey", "@config.apiKey"]])));
        assert_eq!(leaked, vec![SECRET_OUTSIDE_AUTH_CODE], "the secret as the hashed input leaks it");
        assert_eq!(codes(&program(config(), json!([["set", "@entity.x", "@config.apiKey"]]))), vec![SECRET_OUTSIDE_AUTH_CODE]);
    }

    #[test]
    fn a_program_without_orbitals_has_no_violations() {
        assert!(secret_flow_violations(&json!({})).is_empty());
    }

    fn oauth_client(id: &str, secret: &str) -> Value {
        json!({ "tag": "google", "mode": "real", "issuer": "https://idp.example.com", "clientId": id, "clientSecret": secret, "clientAuth": "post" })
    }

    fn oauth_call(client: Value, code: &str) -> Value {
        json!([["http", "POST", "oauth-endpoint", "token",
            { "auth": { "scheme": "oauth-client", "op": "code", "client": client, "code": code, "state": "@payload.state" } },
            { "emit": { "success": "OK" } }]])
    }

    #[test]
    fn oauth_client_and_authorize_slots_must_be_secret_knobs_and_nothing_else_may_read_one() {
        let clean = program(config(), oauth_call(oauth_client("@config.apiKey", "@config.apiKey"), "@payload.code"));
        assert!(codes(&clean).is_empty(), "{:?}", secret_flow_violations(&clean));
        let bad = secret_flow_violations(&program(config(), oauth_call(oauth_client("@config.plain", "@config.apiKey"), "@payload.code")));
        assert!(matches!(&bad[..], [SecretFlowViolation::CredentialNotSecret { slot, .. }] if slot == "client.clientId"), "{bad:?}");
        let leaked = codes(&program(config(), oauth_call(oauth_client("@config.apiKey", "@config.apiKey"), "@config.apiKey")));
        assert_eq!(leaked, vec![SECRET_OUTSIDE_AUTH_CODE]);
        let authorize = |client: Value| program(config(), json!([["oauth-authorize", client, { "redirectUri": "https://a/cb" }, { "emit": { "success": "OK" } }]]));
        assert!(codes(&authorize(oauth_client("@config.apiKey", "@config.apiKey"))).is_empty());
        let bad = secret_flow_violations(&authorize(oauth_client("@config.apiKey", "LITERAL")));
        assert!(matches!(&bad[..], [SecretFlowViolation::CredentialNotSecret { slot, .. }] if slot == "client.clientSecret"), "{bad:?}");
    }

    #[test]
    fn http_credential_slots_lists_the_slots_of_every_http_call_and_only_those() {
        let effects = json!([
            ["http", "GET", "https://a.example", "/s", { "auth": { "scheme": "bearer", "credential": "STRIPE" } }, { "emit": { "success": "OK" } }],
            ["if", true, ["http", "GET", "https://a.example", "/t", { "auth": { "scheme": "basic", "user": "u", "credential": { "credential": "DB", "scope": "user" } } }, { "emit": { "success": "OK" } }]],
            ["http", "GET", "https://a.example", "/open", {}, { "emit": { "success": "OK" } }]
        ]);
        let slots = http_credential_slots(&effects);
        let named: Vec<(&str, Option<&str>)> = slots.iter().map(|(slot, value)| (slot.as_str(), slot_credential(value))).collect();
        assert_eq!(named, vec![("credential", Some("STRIPE")), ("user", Some("u")), ("credential", Some("DB"))]);
        assert!(http_credential_slots(&json!([["set", "@entity.x", 1]])).is_empty());
    }

    #[test]
    fn program_credentials_lists_the_secret_knobs_the_http_auth_slots_read() {
        let gh = json!({ "name": "GithubOrbital", "traits": [{
            "config": {
                "token": { "type": "secret", "default": "GITHUB_TOKEN", "description": "The token." },
                "unused": { "type": "secret", "default": "UNUSED" },
                "plain": { "type": "string", "default": "PLAIN" }
            },
            "stateMachine": { "transitions": [{ "effects": [
                ["http", "GET", "https://a.example", "/a", { "auth": { "scheme": "bearer", "credential": "@config.token" } }, { "emit": { "success": "OK" } }],
                ["http", "GET", "https://a.example", "/b", { "auth": { "scheme": "bearer", "credential": "@config.token" } }, { "emit": { "success": "OK" } }],
                ["http", "GET", "https://a.example", "/c", { "auth": { "scheme": "bearer", "credential": "@config.plain" } }, { "emit": { "success": "OK" } }]
            ] }] }
        }] });
        let other = json!({ "name": "Other", "traits": [{
            "config": { "key": { "type": "secret", "default": "GITHUB_TOKEN" } },
            "stateMachine": { "transitions": [{ "effects": [
                ["http", "GET", "https://a.example", "/a", { "auth": { "scheme": "api-key", "credential": "@config.key" } }, { "emit": { "success": "OK" } }]
            ] }] }
        }] });
        let found = program_credentials(&json!({ "orbitals": [gh, other] }));
        let named: Vec<(&str, &str, &str)> = found.iter().map(|c| (c.group.as_str(), c.env_var.as_str(), c.description.as_str())).collect();
        assert_eq!(named, vec![("GithubOrbital", "GITHUB_TOKEN", "The token."), ("Other", "GITHUB_TOKEN", "")]);
        assert!(program_credentials(&json!({ "orbitals": [] })).is_empty());
    }

    #[test]
    fn program_credentials_never_match_a_literal_to_a_knob_default_since_resolve_keeps_secret_reads() {
        let inlined = json!({ "orbitals": [{ "name": "TinyOrbital", "traits": [{
            "config": {
                "token": { "type": "secret", "default": "MY_GH_TOKEN", "description": "The token." },
                "other": { "type": "string", "default": "LITERAL_ONLY" }
            },
            "stateMachine": { "transitions": [{ "effects": [
                ["http", "GET", "https://a.example", "/a", { "auth": { "scheme": "bearer", "credential": "MY_GH_TOKEN" } }, { "emit": { "success": "OK" } }],
                ["http", "GET", "https://a.example", "/b", { "auth": { "scheme": "bearer", "credential": "LITERAL_ONLY" } }, { "emit": { "success": "OK" } }]
            ] }] }
        }] }] });
        assert!(program_credentials(&inlined).is_empty());
    }
}
