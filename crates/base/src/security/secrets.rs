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
        "token-exchange" => &["clientId", "clientSecret"],
        "jwt-bearer" => &["key"],
        "aws-sigv4" => &["accessKeyId", "secretAccessKey", "sessionToken"],
        "vapid" => &["privateKey"],
        _ => &[],
    }
}

/// The credential keys of an OAuth client object: the `client` / `clients.<name>` of an `oauth-client` auth and the
/// client operand of `oauth-authorize`.
pub const OAUTH_CLIENT_SLOTS: &[&str] = &["clientId", "clientSecret"];

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
    each_call(expr, "oauth-authorize", &mut |items| {
        let mut slots = Vec::new();
        if let Some(client) = authorize_client(items) {
            push_client_slots("oauth-authorize", client, &mut slots);
        }
        for (slot, value) in slots {
            let is_secret_knob = slot_credential(value).and_then(config_knob).is_some_and(|knob| secrets.contains(&knob));
            if !is_secret_knob {
                out.push(SecretFlowViolation::CredentialNotSecret { path: path.to_string(), slot });
            }
        }
    });
    each_operator_credential(expr, &mut |slot, operand| {
        let is_secret_knob = operand
            .as_str()
            .and_then(config_knob)
            .is_some_and(|knob| secrets.contains(&knob));
        if !is_secret_knob {
            out.push(SecretFlowViolation::CredentialNotSecret {
                path: path.to_string(),
                slot: slot.to_string(),
            });
        }
    });
    each_os_send_segment(expr, &mut |operand| {
        let is_secret_knob = operand
            .as_str()
            .and_then(config_knob)
            .is_some_and(|knob| secrets.contains(&knob));
        if !is_secret_knob {
            out.push(SecretFlowViolation::CredentialNotSecret {
                path: path.to_string(),
                slot: OS_SEND_SEGMENT_SLOT.to_string(),
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

fn each_http_call<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'a [Value])) {
    each_call(expr, "http", visit);
}

/// Every call form in `expr` whose head is `head`.
fn each_call<'a>(expr: &'a Value, head: &str, visit: &mut dyn FnMut(&'a [Value])) {
    match expr {
        Value::Array(items) => {
            if items.first().and_then(Value::as_str) == Some(head) {
                visit(items);
            }
            for item in items {
                each_call(item, head, visit);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                each_call(value, head, visit);
            }
        }
        _ => {}
    }
}

/// The client object of an `oauth-authorize` call form.
fn authorize_client(items: &[Value]) -> Option<&Map<String, Value>> {
    items.get(1).and_then(Value::as_object)
}

fn push_client_slots<'a>(prefix: &str, client: &'a Map<String, Value>, out: &mut Vec<(String, &'a Value)>) {
    for slot in OAUTH_CLIENT_SLOTS {
        if let Some(value) = client.get(*slot) {
            out.push((format!("{prefix}.{slot}"), value));
        }
    }
}

fn oauth_client_slots<'a>(auth: &'a Map<String, Value>, out: &mut Vec<(String, &'a Value)>) {
    if let Some(client) = auth.get("client").and_then(Value::as_object) {
        push_client_slots("client", client, out);
    }
    for (name, client) in auth.get("clients").and_then(Value::as_object).into_iter().flatten() {
        if let Some(client) = client.as_object() {
            push_client_slots(&format!("clients.{name}"), client, out);
        }
    }
}

fn scan_client_reads(client: &Map<String, Value>, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    for (key, value) in client {
        let allowed_leaf = OAUTH_CLIENT_SLOTS.contains(&key.as_str())
            && slot_credential(value).and_then(config_knob).is_some_and(|knob| secrets.contains(&knob));
        if !allowed_leaf {
            scan_reads(value, secrets, path, out);
        }
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
    let Some(Value::Object(auth)) = http_request(items).and_then(|r| r.get("auth")) else {
        return out;
    };
    let Some(scheme) = auth.get("scheme").and_then(Value::as_str) else {
        return out;
    };
    for slot in credential_slots(scheme) {
        if let Some(value) = auth.get(*slot) {
            out.push(((*slot).to_string(), value));
        }
    }
    if scheme == "oauth-client" {
        oauth_client_slots(auth, &mut out);
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

/// The credential slot of every `credential-status` and `credential-forget`, and the credential operand of every
/// operator in [`CREDENTIAL_OPERATORS`], in `expr`, as written.
fn effect_credential_slots(expr: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    each_credential_effect(expr, &mut |_, items| out.extend(items.get(1)));
    each_operator_credential(expr, &mut |_, operand| out.push(operand));
    each_os_send_segment(expr, &mut |operand| out.push(operand));
    each_call(expr, "oauth-authorize", &mut |items| {
        let mut slots = Vec::new();
        if let Some(client) = authorize_client(items) {
            push_client_slots("oauth-authorize", client, &mut slots);
        }
        out.extend(slots.into_iter().map(|(_, value)| value));
    });
    out
}

/// An operator operand that names a credential the kernel resolves itself: the evaluation holds the value, the program
/// never receives it. `operand` is the index in the call form, the operator name at 0.
pub struct CredentialOperand {
    pub operator: &'static str,
    pub operand: usize,
    pub slot: &'static str,
}

/// Every operator whose operand is a credential slot (EXEC-SECRET: the host resolves the name at the boundary, never a
/// program value, never journaled). The kernel, the secret-flow rules and the credential declarations read this one table.
pub const CREDENTIAL_OPERATORS: &[CredentialOperand] = &[
    CredentialOperand { operator: "hash/scrypt", operand: 2, slot: "hash/scrypt.salt" },
    CredentialOperand { operator: "hmac/sha256", operand: 1, slot: "hmac/sha256.key" },
    CredentialOperand { operator: "ecdsa/p256-sign", operand: 1, slot: "ecdsa/p256-sign.key" },
    CredentialOperand { operator: "rsa/sign", operand: 1, slot: "rsa/sign.key" },
    CredentialOperand { operator: "hash/pbkdf2", operand: 1, slot: "hash/pbkdf2.password" },
    CredentialOperand { operator: "hash/md5-credential", operand: 1, slot: "hash/md5-credential.credential" },
    CredentialOperand { operator: "credential/byte-length", operand: 1, slot: "credential/byte-length.credential" },
];

/// The slot name of a credential segment of an `os/send` payload in a [`SecretFlowViolation::CredentialNotSecret`].
pub const OS_SEND_SEGMENT_SLOT: &str = "os/send.segment";

/// The index of the data operand of an `os/send` call form, the operator name at 0.
const OS_SEND_DATA: usize = 2;

/// The segments of the data operand of an `os/send` call: a list of `{ bytes, credential }` objects. A segment is its
/// bytes followed by the value of the credential it names, which the host fills in at the boundary; an empty credential
/// name is a segment of bytes alone.
fn os_send_segments(items: &[Value]) -> Option<&[Value]> {
    if items.first().and_then(Value::as_str) != Some("os/send") {
        return None;
    }
    items.get(OS_SEND_DATA).and_then(Value::as_array).map(Vec::as_slice)
}

/// The credential name operand of a segment, when the segment is a `{ credential }` object.
fn segment_credential(segment: &Value) -> Option<&Value> {
    segment.as_object().and_then(|fields| fields.get("credential"))
}

/// Whether a segment's credential operand names no credential: the empty literal.
fn is_no_credential(operand: &Value) -> bool {
    operand.as_str() == Some("")
}

/// Every credential segment of every `os/send` payload in `expr`: a credential slot the host resolves at the boundary.
/// A segment that names no credential is bytes alone and is not a slot.
fn each_os_send_segment<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'a Value)) {
    match expr {
        Value::Array(items) => {
            for segment in os_send_segments(items).into_iter().flatten() {
                if let Some(credential) = segment_credential(segment).filter(|operand| !is_no_credential(operand)) {
                    visit(credential);
                }
            }
            for item in items {
                each_os_send_segment(item, visit);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                each_os_send_segment(value, visit);
            }
        }
        _ => {}
    }
}

/// The credential operand of a call form whose head is in [`CREDENTIAL_OPERATORS`].
pub fn credential_operand<'a>(items: &'a [Value]) -> Option<(&'static str, &'a Value)> {
    let head = items.first().and_then(Value::as_str)?;
    let entry = CREDENTIAL_OPERATORS.iter().find(|entry| entry.operator == head)?;
    items.get(entry.operand).map(|operand| (entry.slot, operand))
}

fn credential_operand_index(items: &[Value]) -> Option<usize> {
    let head = items.first().and_then(Value::as_str)?;
    CREDENTIAL_OPERATORS.iter().find(|entry| entry.operator == head).map(|entry| entry.operand)
}

/// Every credential operand of every operator in [`CREDENTIAL_OPERATORS`] in `expr`, with its slot name.
fn each_operator_credential<'a>(expr: &'a Value, visit: &mut dyn FnMut(&'static str, &'a Value)) {
    match expr {
        Value::Array(items) => {
            if let Some((slot, operand)) = credential_operand(items) {
                visit(slot, operand);
            }
            for item in items {
                each_operator_credential(item, visit);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                each_operator_credential(value, visit);
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
            let credential = credential_effect(items).is_some();
            let slot_index = credential_operand_index(items);
            let segments = os_send_segments(items).is_some();
            let authorize = items.first().and_then(Value::as_str) == Some("oauth-authorize");
            for (index, item) in items.iter().enumerate() {
                match (http, item) {
                    (_, Value::Object(client)) if authorize && index == 1 => scan_client_reads(client, secrets, path, out),
                    _ if segments && index == OS_SEND_DATA => scan_segment_reads(item, secrets, path, out),
                    _ if slot_index == Some(index) && item.as_str().and_then(config_knob).is_some_and(|knob| secrets.contains(&knob)) => {}
                    (true, Value::Object(request)) => scan_request_reads(request, secrets, path, out),
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

fn scan_segment_reads(segments: &Value, secrets: &[&str], path: &str, out: &mut Vec<SecretFlowViolation>) {
    for segment in segments.as_array().into_iter().flatten() {
        match segment.as_object().filter(|fields| fields.contains_key("credential")) {
            Some(fields) => {
                for (key, value) in fields {
                    let allowed_leaf = key == "credential"
                        && (is_no_credential(value)
                            || value.as_str().and_then(config_knob).is_some_and(|knob| secrets.contains(&knob)));
                    if !allowed_leaf {
                        scan_reads(value, secrets, path, out);
                    }
                }
            }
            None => scan_reads(segment, secrets, path, out),
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
                        (Some("token-exchange"), "body", Value::Object(body)) => {
                            for fields in body.values() {
                                for (_, field) in fields.as_object().into_iter().flatten() {
                                    if !is_slot_object(field) {
                                        scan_reads(field, secrets, path, out);
                                    }
                                }
                            }
                        }
                        (Some("oauth-client"), "client", Value::Object(client)) => scan_client_reads(client, secrets, path, out),
                        (Some("oauth-client"), "clients", Value::Object(clients)) => {
                            for client in clients.values() {
                                match client {
                                    Value::Object(client) => scan_client_reads(client, secrets, path, out),
                                    other => scan_reads(other, secrets, path, out),
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
        ] {
            let found = codes(&program(config(), call(json!({ "auth": auth.clone() }))));
            assert_eq!(found, vec![CREDENTIAL_NOT_SECRET_CODE], "{auth}");
        }
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

    fn oauth_client(secret: &str) -> Value {
        json!({ "tag": "google", "mode": "real", "issuer": "https://a.example", "clientId": "@config.apiKey", "clientSecret": secret, "clientAuth": "post" })
    }

    #[test]
    fn an_oauth_client_secret_is_accepted_in_client_clients_and_oauth_authorize() {
        let one = call(json!({ "auth": { "scheme": "oauth-client", "op": "refresh", "client": oauth_client("@config.apiKey"), "refreshToken": "@payload.t" } }));
        assert!(codes(&program(config(), one)).is_empty());
        let many = call(json!({ "auth": { "scheme": "oauth-client", "op": "code", "clients": { "google": oauth_client("@config.apiKey"), "github": oauth_client("@config.apiKey") }, "code": "@payload.c" } }));
        assert!(codes(&program(config(), many)).is_empty());
        let authorize = json!([["oauth-authorize", oauth_client("@config.apiKey"), { "redirectUri": "https://a/cb" }, { "emit": { "success": "OK" } }]]);
        assert!(codes(&program(config(), authorize)).is_empty());
    }

    #[test]
    fn an_oauth_client_slot_that_is_not_a_secret_knob_is_reported_with_its_slot() {
        let one = call(json!({ "auth": { "scheme": "oauth-client", "op": "refresh", "client": oauth_client("@config.plain") } }));
        assert_eq!(codes(&program(config(), one)), vec![CREDENTIAL_NOT_SECRET_CODE]);
        let many = call(json!({ "auth": { "scheme": "oauth-client", "op": "code", "clients": { "g": oauth_client("LITERAL") } } }));
        let violations = secret_flow_violations(&program(config(), many));
        assert!(matches!(&violations[..], [SecretFlowViolation::CredentialNotSecret { slot, .. }] if slot == "clients.g.clientSecret"));
        let authorize = json!([["oauth-authorize", oauth_client("@config.plain"), { "redirectUri": "https://a/cb" }]]);
        let violations = secret_flow_violations(&program(config(), authorize));
        assert!(matches!(&violations[..], [SecretFlowViolation::CredentialNotSecret { slot, .. }] if slot == "oauth-authorize.clientSecret"));
    }

    #[test]
    fn an_oauth_secret_read_outside_the_client_slots_is_still_outside_auth() {
        let mut client = oauth_client("@config.apiKey");
        client["issuer"] = json!("@config.apiKey");
        let one = call(json!({ "auth": { "scheme": "oauth-client", "op": "refresh", "client": client.clone() } }));
        assert_eq!(codes(&program(config(), one)), vec![SECRET_OUTSIDE_AUTH_CODE]);
        let authorize = json!([["oauth-authorize", client, { "redirectUri": "https://a/cb" }]]);
        assert_eq!(codes(&program(config(), authorize)), vec![SECRET_OUTSIDE_AUTH_CODE]);
        let logged = json!([["emit", "LOG", { "secret": "@config.apiKey" }]]);
        assert_eq!(codes(&program(config(), logged)), vec![SECRET_OUTSIDE_AUTH_CODE]);
        let other_field = call(json!({ "auth": { "scheme": "oauth-client", "op": "refresh", "client": oauth_client("@config.apiKey"), "refreshToken": "@config.apiKey" } }));
        assert_eq!(codes(&program(config(), other_field)), vec![SECRET_OUTSIDE_AUTH_CODE]);
    }

    #[test]
    fn oauth_client_slots_are_declared_as_program_credentials() {
        let program = json!({ "orbitals": [{ "name": "o", "traits": [{
            "config": { "id": { "type": "secret", "default": "CID" }, "sec": { "type": "secret", "default": "CSEC" } },
            "stateMachine": { "transitions": [{ "effects": [
                ["oauth-authorize", { "tag": "google", "clientId": "@config.id", "clientSecret": "@config.sec" }, { "redirectUri": "https://a/cb" }]
            ] }] }
        }] }] });
        let envs: Vec<_> = program_credentials(&program).into_iter().map(|c| c.env_var).collect();
        assert_eq!(envs, vec!["CID".to_string(), "CSEC".to_string()]);
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
    fn every_credential_operator_takes_a_secret_knob_in_its_slot_and_nowhere_else() {
        for entry in CREDENTIAL_OPERATORS {
            let call = |operand: &str| {
                let mut items = vec![json!(entry.operator)];
                items.extend((1..=entry.operand.max(2)).map(|i| if i == entry.operand { json!(operand) } else { json!("@payload.m") }));
                json!([["let", [["v", Value::Array(items)]], ["emit", "OK", { "v": "@v" }]]])
            };
            let op = entry.operator;
            assert!(codes(&program(config(), call("@config.apiKey"))).is_empty(), "{op}: a secret knob is the slot");
            for bad in ["@config.plain", "LITERAL", "@payload.name"] {
                assert_eq!(codes(&program(config(), call(bad))), vec![CREDENTIAL_NOT_SECRET_CODE], "{op} {bad}");
            }
            let declared = program_credentials(&json!({ "orbitals": [{ "name": "O", "traits": [{
                "config": { "apiKey": { "type": "secret", "default": "SLOT_CREDENTIAL", "description": "d" } },
                "stateMachine": { "transitions": [{ "effects": call("@config.apiKey") }] }
            }] }] }));
            assert_eq!(declared.len(), 1, "{op}: the credential the slot names is declared");
            assert_eq!(declared[0].env_var, "SLOT_CREDENTIAL");
        }
        let leaked = json!([["hmac/sha256", "@config.apiKey", "@config.apiKey", "hex"]]);
        assert_eq!(codes(&program(config(), leaked)), vec![SECRET_OUTSIDE_AUTH_CODE], "the secret as the message leaks it");
        let control = json!([["hmac/sha256", "@config.apiKey", "@payload.m", "hex"]]);
        assert!(codes(&program(config(), control)).is_empty());
    }

    #[test]
    fn an_os_send_credential_segment_takes_a_secret_knob_and_nowhere_else() {
        let send = |credential: Value| {
            json!([["os/send", "@payload.sock", [
                { "bytes": ["bytes/from-string", "AUTH "], "credential": "" },
                { "bytes": ["bytes/from-string", "x"], "credential": credential }
            ], { "key": "s" }]])
        };
        assert!(codes(&program(config(), send(json!("@config.apiKey")))).is_empty(), "a secret knob is the slot");
        assert!(codes(&program(config(), send(json!("")))).is_empty(), "the empty name is bytes alone");
        for bad in ["@config.plain", "LITERAL", "@payload.name"] {
            assert_eq!(codes(&program(config(), send(json!(bad)))), vec![CREDENTIAL_NOT_SECRET_CODE], "{bad}");
        }
        let leaking = json!([["os/send", "@payload.sock", [{ "bytes": "@config.apiKey", "credential": "" }], { "key": "s" }]]);
        assert_eq!(codes(&program(config(), leaking)), vec![SECRET_OUTSIDE_AUTH_CODE], "a secret knob in a segment's bytes is a read");
        let beside = json!([["os/send", "@payload.sock", [{ "bytes": "@config.apiKey", "credential": "@config.apiKey" }], { "key": "s" }]]);
        assert_eq!(codes(&program(config(), beside)), vec![SECRET_OUTSIDE_AUTH_CODE], "the credential slot does not legalise the bytes");
        let declared = |credential: Value| {
            program_credentials(&json!({ "orbitals": [{ "name": "O", "traits": [{
                "config": { "apiKey": { "type": "secret", "default": "SEND_CREDENTIAL", "description": "d" } },
                "stateMachine": { "transitions": [{ "effects": send(credential) }] }
            }] }] }))
        };
        assert_eq!(declared(json!("@config.apiKey")).iter().map(|c| c.env_var.as_str()).collect::<Vec<_>>(), vec!["SEND_CREDENTIAL"]);
        assert!(declared(json!("")).is_empty(), "bytes alone declare nothing");
    }

    #[test]
    fn a_program_without_orbitals_has_no_violations() {
        assert!(secret_flow_violations(&json!({})).is_empty());
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
