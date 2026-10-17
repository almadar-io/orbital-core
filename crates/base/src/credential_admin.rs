//! Credential administration, owned by the kernel: the role gate, request validation, the declared-name check, the
//! catalog (the registry's remaining services plus the credentials the running program declares), last-4 masking, and
//! the shapes `list`, `set`, `remove`, `test`, `mode` and `rotate` answer with. The host supplies only the raw store
//! and mode-flag asks (`IoRequest::CredentialStore*` / `CredentialMode*`); everything decided here is pure.

use crate::secrets::ProgramCredential;
use crate::value::{Value, ValueMap};

/// The code of a refused role gate.
pub const FORBIDDEN_CODE: &str = "AUTH_ERROR";
/// The code of a request that fails its parameter contract.
pub const VALIDATION_CODE: &str = "VALIDATION_ERROR";
/// Comma-separated roles allowed to change credentials.
pub const ADMIN_ROLES_VAR: &str = "ALMADAR_CREDENTIAL_ADMIN_ROLES";
/// `store` makes declared credentials resolve only through the store.
pub const SOURCE_VAR: &str = "ALMADAR_CREDENTIALS_SOURCE";
/// `mock` here selects mock mode when the host has no mode flag of its own.
pub const MODE_ENV_VAR: &str = "ALMADAR_INTEGRATIONS_MODE";
/// `true` here selects mock mode when the host has no mode flag of its own.
pub const MOCK_DATA_VAR: &str = "USE_MOCK_DATA";
/// A reachable Firestore emulator.
pub const FIRESTORE_EMULATOR_VAR: &str = "FIRESTORE_EMULATOR_HOST";
/// The Firestore targets that make the `knowledge` service reachable.
pub const FIRESTORE_TARGET_VARS: [&str; 3] = [FIRESTORE_EMULATOR_VAR, "FIREBASE_PROJECT_ID", "FIREBASE_SERVICE_ACCOUNT_PATH"];
/// The message of a host that holds no credential store.
pub const NO_STORE: &str = "No credential store is installed on this host";
/// The message of a host with no mode flag.
pub const NO_MANAGER: &str = "No integration manager is installed on this host";

const DEFAULT_ADMIN_ROLES: [&str; 2] = ["admin", "owner"];
const UNMOCKED: [&str; 2] = ["credentials", "workspace"];
const FIRESTORE_SERVICES: [&str; 1] = ["knowledge"];
const FIRESTORE_NEEDS: &str = "FIRESTORE_EMULATOR_HOST (start the Firebase emulators), or FIREBASE_PROJECT_ID / FIREBASE_SERVICE_ACCOUNT_PATH";

/// One credential administration effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CredentialAdminOp {
    #[serde(rename = "credential-list")]
    List,
    #[serde(rename = "credential-set")]
    Set,
    #[serde(rename = "credential-remove")]
    Remove,
    #[serde(rename = "credential-test")]
    Test,
    #[serde(rename = "credential-mode")]
    Mode,
    #[serde(rename = "credential-rotate")]
    Rotate,
}

impl CredentialAdminOp {
    pub const ALL: [CredentialAdminOp; 6] = [
        Self::List,
        Self::Set,
        Self::Remove,
        Self::Test,
        Self::Mode,
        Self::Rotate,
    ];

    /// The surface name, `credential-<verb>`.
    pub fn name(self) -> &'static str {
        match self {
            Self::List => "credential-list",
            Self::Set => "credential-set",
            Self::Remove => "credential-remove",
            Self::Test => "credential-test",
            Self::Mode => "credential-mode",
            Self::Rotate => "credential-rotate",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.name() == name)
    }

    /// The verb a refusal names (`set` in `forbidden: "set" requires ...`).
    pub fn verb(self) -> &'static str {
        &self.name()["credential-".len()..]
    }

    /// Whether the caller's role must be an admin role. `list` stays open (masked last-4 only).
    pub fn gated(self) -> bool {
        matches!(self, Self::Set | Self::Remove | Self::Test | Self::Mode | Self::Rotate)
    }

    /// The request's `(required, optional)` string parameters, in contract order.
    pub fn params(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::List => (&[], &["service"]),
            Self::Set => (&["service", "envVar", "value"], &[]),
            Self::Remove => (&["service", "envVar"], &[]),
            Self::Test => (&["service"], &[]),
            Self::Mode => (&["mode"], &[]),
            Self::Rotate => (&[], &[]),
        }
    }
}

/// A refused request: the message and, for a gate or contract refusal, its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: Option<&'static str>,
    pub message: String,
}

impl Refusal {
    pub fn plain(message: impl Into<String>) -> Self {
        Self { code: None, message: message.into() }
    }

    fn coded(code: &'static str, message: impl Into<String>) -> Self {
        Self { code: Some(code), message: message.into() }
    }
}

/// A request that passed its parameter contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    pub service: Option<String>,
    pub env_var: Option<String>,
    pub value: Option<String>,
    pub mode: Option<String>,
}

fn js_type(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        _ => "object",
    }
}

/// Check the request against the effect's closed string parameters: missing required ones, wrongly typed ones, then
/// unknown ones, each with the contract's message.
pub fn parse_request(op: CredentialAdminOp, request: &Value) -> Result<Request, Refusal> {
    let empty = ValueMap::new();
    let fields = match request {
        Value::Object(fields) => fields,
        Value::Null => &empty,
        other => {
            return Err(Refusal::coded(
                VALIDATION_CODE,
                format!("Validation failed for {}: request: Expected object, got {}", op.name(), js_type(other)),
            ))
        }
    };
    let (required, optional) = op.params();
    let mut problems: Vec<String> = Vec::new();
    for name in required.iter().chain(optional) {
        match fields.get(*name) {
            None | Some(Value::Null) if required.contains(name) => {
                problems.push(format!("{name}: Missing required parameter: {name}"));
            }
            None | Some(Value::Null) => {}
            Some(Value::String(_)) => {}
            Some(other) => problems.push(format!("{name}: Expected string, got {}", js_type(other))),
        }
    }
    let declared: Vec<&str> = required.iter().chain(optional).copied().collect();
    for name in fields.keys().filter(|name| !declared.contains(&name.as_str())) {
        problems.push(format!("{name}: Unknown parameter: {name} (declared: {})", declared.join(", ")));
    }
    if !problems.is_empty() {
        return Err(Refusal::coded(
            VALIDATION_CODE,
            format!("Validation failed for {}: {}", op.name(), problems.join("; ")),
        ));
    }
    let text = |name: &str| fields.get(name).and_then(Value::as_str).map(str::to_string);
    Ok(Request { service: text("service"), env_var: text("envVar"), value: text("value"), mode: text("mode") })
}

/// The roles allowed to change credentials: the comma-separated `raw` list, or `admin, owner` when it names none.
pub fn admin_roles(raw: Option<&str>) -> Vec<String> {
    let roles: Vec<String> = raw
        .unwrap_or_default()
        .split(',')
        .map(|role| role.trim().to_string())
        .filter(|role| !role.is_empty())
        .collect();
    if roles.is_empty() {
        DEFAULT_ADMIN_ROLES.iter().map(|role| (*role).to_string()).collect()
    } else {
        roles
    }
}

/// The role gate: a gated effect needs the caller's role among `allowed`, and fails closed without one.
pub fn gate(op: CredentialAdminOp, role: Option<&str>, allowed: &[String]) -> Result<(), Refusal> {
    if !op.gated() {
        return Ok(());
    }
    match role {
        Some(role) if !role.is_empty() && allowed.iter().any(|a| a == role) => Ok(()),
        _ => Err(Refusal::coded(
            FORBIDDEN_CODE,
            format!("forbidden: \"{}\" requires one of roles: {}", op.verb(), allowed.join(", ")),
        )),
    }
}

/// The last four UTF-16 code units of `value`: the only part of a credential a page may show.
pub fn last4(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    String::from_utf16_lossy(&units[units.len().saturating_sub(4)..])
}

/// A credential a service declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub env_var: String,
    pub required: bool,
    pub description: String,
    pub one_of: Option<String>,
}

/// The declared credentials and backends of the services a program may name, in registry order, then the program's own
/// groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Services with a status row, in order.
    pub listed: Vec<String>,
    /// Every group's declared credentials, in the order the entries list.
    pub declared: Vec<(String, Vec<Declared>)>,
    pub backends: Vec<(String, String)>,
}

impl Catalog {
    /// The catalog of a services registry (`{ services: { <name>: { backend, credentials: [...] } } }`): every service
    /// with a production, simulated or stub backend that is not unmocked gets a status row.
    pub fn from_registry(registry: &str) -> Result<Self, String> {
        let parsed: serde_json::Value = serde_json::from_str(registry).map_err(|e| e.to_string())?;
        let services = parsed
            .get("services")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "the services registry has no `services` map".to_string())?;
        let mut catalog = Catalog::default();
        for (name, service) in services {
            let backend = service.get("backend").and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
            if matches!(backend.as_str(), "production" | "simulated" | "stub") && !UNMOCKED.contains(&name.as_str()) {
                catalog.listed.push(name.clone());
            }
            catalog.backends.push((name.clone(), backend));
            let credentials = service.get("credentials").and_then(serde_json::Value::as_array).into_iter().flatten();
            let declared = credentials
                .filter_map(|c| {
                    Some(Declared {
                        env_var: c.get("envVar")?.as_str()?.to_string(),
                        required: c.get("required").and_then(serde_json::Value::as_bool).unwrap_or(false),
                        description: c.get("description").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                        one_of: c.get("oneOf").and_then(serde_json::Value::as_str).map(str::to_string),
                    })
                })
                .collect();
            catalog.declared.push((name.clone(), declared));
        }
        Ok(catalog)
    }

    /// This catalog with its listing and declaration order replaced (a caller whose own contract orders them).
    pub fn ordered(mut self, listed: Vec<String>, declared_order: Vec<String>) -> Self {
        let mut remaining = std::mem::take(&mut self.declared);
        let mut declared = Vec::new();
        for name in declared_order {
            if let Some(at) = remaining.iter().position(|(n, _)| *n == name) {
                declared.push(remaining.remove(at));
            }
        }
        self.declared = declared;
        self.listed = listed;
        self
    }

    /// This catalog plus the credentials a program declares, each group after the registry's services.
    pub fn with_program_credentials(mut self, credentials: &[ProgramCredential]) -> Self {
        for credential in credentials {
            let declared = Declared {
                env_var: credential.env_var.clone(),
                required: false,
                description: credential.description.clone(),
                one_of: None,
            };
            match self.declared.iter_mut().find(|(name, _)| *name == credential.group) {
                Some((_, list)) => {
                    if !list.iter().any(|d| d.env_var == declared.env_var) {
                        list.push(declared);
                    }
                }
                None => self.declared.push((credential.group.clone(), vec![declared])),
            }
        }
        self
    }

    pub fn declared_for(&self, service: &str) -> &[Declared] {
        self.declared
            .iter()
            .find(|(name, _)| name == service)
            .map_or(&[], |(_, list)| list.as_slice())
    }

    fn backend_of(&self, service: &str) -> &str {
        self.backends.iter().find(|(name, _)| name == service).map_or("", |(_, backend)| backend.as_str())
    }
}

/// Only a declared credential of the named service may be written or removed, so the page can never become an
/// arbitrary env-injection surface.
pub fn settable(catalog: &Catalog, service: &str, env_var: &str) -> Result<(), Refusal> {
    let declared = catalog.declared_for(service);
    if declared.is_empty() {
        return Err(Refusal::plain(format!("Service \"{service}\" declares no credentials")));
    }
    if !declared.iter().any(|c| c.env_var == env_var) {
        let valid: Vec<&str> = declared.iter().map(|c| c.env_var.as_str()).collect();
        return Err(Refusal::plain(format!(
            "\"{env_var}\" is not a declared credential of \"{service}\" (declared: {})",
            valid.join(", ")
        )));
    }
    Ok(())
}

/// A stored credential as the page may see it: never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEntry {
    pub service: String,
    pub env_var: String,
    pub last4: String,
}

/// What the store reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreView {
    pub enabled: bool,
    pub entries: Vec<StoredEntry>,
}

/// What `list` reads besides the catalog.
pub struct ListFacts<'a> {
    /// `None`: the host holds no store.
    pub store: Option<&'a StoreView>,
    /// `ALMADAR_CREDENTIALS_SOURCE == store`: env-only values are hidden.
    pub store_first: bool,
    /// The host's environment, non-empty values only.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Whether integrations answer with mock data.
    pub mock: bool,
}

/// Whether the host's mode flag (or, without one, the environment) selects mock data.
pub fn env_mock(env: &dyn Fn(&str) -> Option<String>) -> bool {
    env(MODE_ENV_VAR).as_deref() == Some("mock") || env(MOCK_DATA_VAR).as_deref() == Some("true")
}

pub fn mode_name(mock: bool) -> &'static str {
    if mock {
        "mock"
    } else {
        "real"
    }
}

fn object(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

/// The environment names `list_report` will read, in the order it reads them: a declared credential no stored row
/// holds (unless the store alone decides), and the Firestore targets when a Firestore-backed service is listed.
pub fn list_env_names(catalog: &Catalog, filter: Option<&str>, store: Option<&StoreView>, store_first: bool) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    };
    if !store_first {
        let held = |service: &str, env_var: &str| {
            store.is_some_and(|s| s.entries.iter().any(|e| e.service == service && e.env_var == env_var))
        };
        for (service, declared) in &catalog.declared {
            if filter.is_some_and(|f| f != service) {
                continue;
            }
            for d in declared.iter().filter(|d| !held(service, &d.env_var)) {
                push(&d.env_var);
            }
        }
    }
    let firestore_listed = catalog
        .listed
        .iter()
        .any(|s| filter.is_none_or(|f| f == s) && FIRESTORE_SERVICES.contains(&s.as_str()));
    if firestore_listed {
        for var in FIRESTORE_TARGET_VARS {
            push(var);
        }
    }
    names
}

/// `{ enabled, mode, services, entries }`: the masked inventory with each service's status.
pub fn list_report(catalog: &Catalog, filter: Option<&str>, facts: &ListFacts<'_>) -> Value {
    let mut stored: Vec<Option<&StoredEntry>> = facts.store.map(|s| s.entries.iter().map(Some).collect()).unwrap_or_default();
    let mut entries: Vec<Value> = Vec::new();
    let mut configured: Vec<(String, bool)> = Vec::new();
    for (service, declared) in &catalog.declared {
        if filter.is_some_and(|f| f != service) {
            continue;
        }
        for d in declared {
            let from_store = stored
                .iter_mut()
                .find(|e| e.is_some_and(|e| e.service == *service && e.env_var == d.env_var))
                .and_then(Option::take);
            let from_env = if facts.store_first || from_store.is_some() { None } else { (facts.env)(&d.env_var) };
            let source = if from_store.is_some() {
                "store"
            } else if from_env.is_some() {
                "env"
            } else {
                "none"
            };
            let last4 = match (from_store, &from_env) {
                (Some(entry), _) => entry.last4.clone(),
                (None, Some(value)) => self::last4(value),
                (None, None) => String::new(),
            };
            configured.push((format!("{service}\u{0}{}", d.env_var), source != "none"));
            entries.push(object(vec![
                ("id", Value::String(format!("{service}/{}", d.env_var))),
                ("service", text(service)),
                ("envVar", text(&d.env_var)),
                ("required", Value::Bool(d.required)),
                ("description", text(&d.description)),
                ("configured", Value::Bool(source != "none")),
                ("source", text(source)),
                ("last4", Value::String(last4)),
            ]));
        }
    }
    for entry in stored.into_iter().flatten() {
        if filter.is_some_and(|f| f != entry.service) {
            continue;
        }
        entries.push(object(vec![
            ("id", Value::String(format!("{}/{}", entry.service, entry.env_var))),
            ("service", text(&entry.service)),
            ("envVar", text(&entry.env_var)),
            ("required", Value::Bool(false)),
            ("description", text("Stored connection reference")),
            ("configured", Value::Bool(true)),
            ("source", text("store")),
            ("last4", text(&entry.last4)),
        ]));
    }
    let is_configured = |service: &str, env_var: &str| {
        configured.iter().any(|(key, ok)| *ok && *key == format!("{service}\u{0}{env_var}"))
    };
    let firestore_target = FIRESTORE_TARGET_VARS.iter().any(|var| (facts.env)(var).is_some());
    let emulator = (facts.env)(FIRESTORE_EMULATOR_VAR).is_some();
    let mut services: Vec<Value> = Vec::new();
    for service in &catalog.listed {
        if filter.is_some_and(|f| f != service) {
            continue;
        }
        let declared = catalog.declared_for(service);
        let on_firestore = FIRESTORE_SERVICES.contains(&service.as_str());
        let ready = if on_firestore {
            firestore_target
        } else {
            let mut groups: Vec<(&str, bool)> = Vec::new();
            for d in declared.iter().filter(|d| d.one_of.is_some()) {
                let group = d.one_of.as_deref().unwrap_or_default();
                let met = is_configured(service, &d.env_var);
                match groups.iter_mut().find(|(g, _)| *g == group) {
                    Some((_, any)) => *any |= met,
                    None => groups.push((group, met)),
                }
            }
            let requires = declared.iter().any(|d| d.required) || !groups.is_empty();
            requires && declared.iter().filter(|d| d.required).all(|d| is_configured(service, &d.env_var)) && groups.iter().all(|(_, met)| *met)
        };
        let mocked = facts.mock && ready;
        let status = if mocked {
            "mocked"
        } else if !ready {
            "not configured"
        } else if on_firestore && emulator {
            "emulated"
        } else {
            "real"
        };
        let needs = if ready {
            String::new()
        } else if on_firestore {
            FIRESTORE_NEEDS.to_string()
        } else {
            credential_needs(declared)
        };
        services.push(object(vec![
            ("id", text(service)),
            ("service", text(service)),
            ("configured", Value::Bool(ready)),
            ("mocked", Value::Bool(mocked)),
            ("status", text(status)),
            ("backend", text(catalog.backend_of(service))),
            ("needs", Value::String(needs)),
        ]));
    }
    object(vec![
        ("enabled", Value::Bool(facts.store.is_some_and(|s| s.enabled))),
        ("mode", text(mode_name(facts.mock))),
        ("services", Value::Array(services)),
        ("entries", Value::Array(entries)),
    ])
}

fn credential_needs(declared: &[Declared]) -> String {
    let mut parts: Vec<String> = declared.iter().filter(|d| d.required).map(|d| d.env_var.clone()).collect();
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for d in declared {
        if let Some(group) = &d.one_of {
            match groups.iter_mut().find(|(g, _)| g == group) {
                Some((_, vars)) => vars.push(d.env_var.clone()),
                None => groups.push((group.clone(), vec![d.env_var.clone()])),
            }
        }
    }
    parts.extend(groups.iter().map(|(_, vars)| format!("one of {}", vars.join(" / "))));
    parts.join(", ")
}

/// The credentials a `test` of `service` reads: the required ones, or, when it marks none as required (the secret
/// knobs a program declares), every declared one.
pub fn test_subjects(catalog: &Catalog, service: &str) -> Vec<String> {
    let declared = catalog.declared_for(service);
    let required: Vec<String> = declared.iter().filter(|c| c.required).map(|c| c.env_var.clone()).collect();
    if required.is_empty() {
        declared.iter().map(|c| c.env_var.clone()).collect()
    } else {
        required
    }
}

/// `{ service, configured, missing, probed, ok, message }`: credential presence only, never a live probe. A service is
/// configured when every required credential is held, or, when it marks none as required, when at least one declared
/// credential is held (a service that declares nothing has nothing to be missing). `resolves` answers whether a
/// credential name holds a non-empty value (the store first, then the environment).
pub fn test_report(catalog: &Catalog, service: &str, resolves: &dyn Fn(&str) -> bool) -> Value {
    let declared = catalog.declared_for(service);
    let any_required = declared.iter().any(|c| c.required);
    let subjects = test_subjects(catalog, service);
    let held: Vec<&String> = subjects.iter().filter(|name| resolves(name)).collect();
    let missing: Vec<String> = if any_required {
        subjects.iter().filter(|name| !resolves(name)).cloned().collect()
    } else if subjects.is_empty() || !held.is_empty() {
        Vec::new()
    } else {
        subjects.clone()
    };
    let configured = missing.is_empty();
    let message = if configured {
        "Credentials present (no live probe declared for this service)".to_string()
    } else if any_required {
        format!("Missing required credentials: {}", missing.join(", "))
    } else {
        format!("None of the declared credentials is set: {}", missing.join(", "))
    };
    object(vec![
        ("service", text(service)),
        ("configured", Value::Bool(configured)),
        ("missing", Value::Array(missing.into_iter().map(Value::String).collect())),
        ("probed", Value::Bool(false)),
        ("ok", Value::Bool(configured)),
        ("message", Value::String(message)),
    ])
}

/// `{ saved, service, envVar, last4 }`.
pub fn saved_report(service: &str, env_var: &str, value: &str) -> Value {
    object(vec![
        ("saved", Value::Bool(true)),
        ("service", text(service)),
        ("envVar", text(env_var)),
        ("last4", Value::String(last4(value))),
    ])
}

/// The mode a `credential-mode` request names.
pub fn requested_mode(mode: &str) -> Result<bool, Refusal> {
    match mode {
        "mock" => Ok(true),
        "real" => Ok(false),
        _ => Err(Refusal::plain("mode must be \"mock\" or \"real\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY: &str = r#"{ "services": {
        "credentials": { "backend": "production", "credentials": [{ "envVar": "MASTER", "required": false, "description": "key" }] },
        "apikeys": { "backend": "production", "credentials": [{ "envVar": "SALT", "required": true, "description": "salt" }] },
        "queue": { "backend": "simulated", "credentials": [{ "envVar": "Q_A", "oneOf": "q" }, { "envVar": "Q_B", "oneOf": "q" }] },
        "hosting": { "backend": "production", "credentials": [] },
        "maps": { "backend": "host-injected" },
        "knowledge": { "backend": "production" }
    } }"#;

    fn catalog() -> Catalog {
        Catalog::from_registry(REGISTRY).unwrap()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn facts<'a>(store: Option<&'a StoreView>, env: &'a dyn Fn(&str) -> Option<String>) -> ListFacts<'a> {
        ListFacts { store, store_first: false, env, mock: false }
    }

    fn field<'a>(value: &'a Value, path: &str) -> &'a Value {
        value.get_path(path).unwrap_or_else(|| panic!("{path} missing in {value:?}"))
    }

    #[test]
    fn the_gate_admits_an_admin_role_and_refuses_the_rest_closed() {
        let allowed = admin_roles(None);
        assert_eq!(allowed, ["admin", "owner"]);
        assert!(gate(CredentialAdminOp::Set, Some("owner"), &allowed).is_ok());
        for role in [None, Some(""), Some("viewer"), Some("Admin")] {
            let refusal = gate(CredentialAdminOp::Set, role, &allowed).unwrap_err();
            assert_eq!(refusal.code, Some(FORBIDDEN_CODE));
            assert_eq!(refusal.message, "forbidden: \"set\" requires one of roles: admin, owner");
        }
        assert!(gate(CredentialAdminOp::List, None, &allowed).is_ok(), "list is open");
        assert_eq!(admin_roles(Some(" ops , lead ,,")), ["ops", "lead"]);
        assert_eq!(admin_roles(Some(" , ")), ["admin", "owner"]);
    }

    #[test]
    fn a_request_is_checked_against_its_closed_string_contract() {
        let ok = parse_request(
            CredentialAdminOp::Set,
            &Value::Object(ValueMap::from([
                ("service".into(), text("apikeys")),
                ("envVar".into(), text("SALT")),
                ("value".into(), text("v")),
            ])),
        )
        .unwrap();
        assert_eq!(ok.env_var.as_deref(), Some("SALT"));
        let missing = parse_request(CredentialAdminOp::Set, &Value::Object(ValueMap::from([("service".into(), text("a"))]))).unwrap_err();
        assert_eq!(
            missing.message,
            "Validation failed for credential-set: envVar: Missing required parameter: envVar; value: Missing required parameter: value"
        );
        assert_eq!(missing.code, Some(VALIDATION_CODE));
        let typed = parse_request(CredentialAdminOp::Test, &Value::Object(ValueMap::from([("service".into(), Value::Number(5.0))]))).unwrap_err();
        assert_eq!(typed.message, "Validation failed for credential-test: service: Expected string, got number");
        let extra = parse_request(CredentialAdminOp::List, &Value::Object(ValueMap::from([("extra".into(), Value::Bool(true))]))).unwrap_err();
        assert_eq!(extra.message, "Validation failed for credential-list: extra: Unknown parameter: extra (declared: service)");
        assert!(parse_request(CredentialAdminOp::Rotate, &Value::Null).is_ok());
        assert!(parse_request(CredentialAdminOp::Rotate, &Value::Object(ValueMap::from([("force".into(), Value::Bool(true))]))).is_err());
        assert!(parse_request(CredentialAdminOp::List, &Value::String("x".into())).is_err());
    }

    #[test]
    fn only_a_declared_credential_is_settable_and_a_connection_ref_is_no_exception() {
        let catalog = catalog();
        assert!(settable(&catalog, "apikeys", "SALT").is_ok());
        assert_eq!(settable(&catalog, "apikeys", "OTHER").unwrap_err().message, "\"OTHER\" is not a declared credential of \"apikeys\" (declared: SALT)");
        assert_eq!(settable(&catalog, "hosting", "X").unwrap_err().message, "Service \"hosting\" declares no credentials");
        assert_eq!(settable(&catalog, "database", "REPORTS_DB_URL").unwrap_err().message, "Service \"database\" declares no credentials");
    }

    #[test]
    fn the_catalog_is_the_registry_then_the_program_groups() {
        let program = [
            ProgramCredential { group: "apikeys".into(), env_var: "EXTRA".into(), description: "d".into() },
            ProgramCredential { group: "LlmOrbital".into(), env_var: "OPENAI".into(), description: "o".into() },
            ProgramCredential { group: "LlmOrbital".into(), env_var: "OPENAI".into(), description: "o".into() },
        ];
        let catalog = catalog().with_program_credentials(&program);
        assert_eq!(catalog.declared_for("apikeys").iter().map(|d| d.env_var.as_str()).collect::<Vec<_>>(), ["SALT", "EXTRA"]);
        assert_eq!(catalog.declared_for("LlmOrbital").len(), 1);
        assert!(settable(&catalog, "LlmOrbital", "OPENAI").is_ok());
        assert_eq!(catalog.listed, ["apikeys", "queue", "hosting", "knowledge"], "credentials is unmocked; maps is host-injected");
    }

    #[test]
    fn masking_keeps_the_last_four_code_units_only() {
        assert_eq!(last4("sk_test_new_1111"), "1111");
        assert_eq!(last4("ab"), "ab");
        assert_eq!(last4(""), "");
        let report = saved_report("apikeys", "SALT", "secret-value-9876");
        assert_eq!(field(&report, "last4"), &Value::String("9876".into()));
        assert!(!format!("{report:?}").contains("secret-value"));
    }

    #[test]
    fn list_orders_store_then_env_then_none_and_hides_env_under_store_first() {
        let catalog = catalog();
        let store = StoreView {
            enabled: true,
            entries: vec![
                StoredEntry { service: "apikeys".into(), env_var: "SALT".into(), last4: "1234".into() },
                StoredEntry { service: "database".into(), env_var: "ORDERS_DB_URL".into(), last4: "ders".into() },
            ],
        };
        let env = |name: &str| (name == "Q_A").then(|| "env-value-0042".to_string());
        let report = list_report(&catalog, None, &facts(Some(&store), &env));
        let entries = field(&report, "entries").as_array().unwrap();
        let by_id = |id: &str| entries.iter().find(|e| field(e, "id") == &Value::String(id.into())).unwrap();
        assert_eq!(field(by_id("apikeys/SALT"), "source"), &Value::String("store".into()));
        assert_eq!(field(by_id("apikeys/SALT"), "last4"), &Value::String("1234".into()));
        assert_eq!(field(by_id("queue/Q_A"), "source"), &Value::String("env".into()));
        assert_eq!(field(by_id("queue/Q_A"), "last4"), &Value::String("0042".into()));
        assert_eq!(field(by_id("queue/Q_B"), "source"), &Value::String("none".into()));
        assert_eq!(field(by_id("database/ORDERS_DB_URL"), "description"), &Value::String("Stored connection reference".into()));
        assert_eq!(field(&report, "enabled"), &Value::Bool(true));
        let hidden = list_report(&catalog, Some("queue"), &ListFacts { store: Some(&store), store_first: true, env: &env, mock: false });
        assert_eq!(field(&hidden.get_path("entries").unwrap().as_array().unwrap()[0], "source"), &Value::String("none".into()));
        let none = list_report(&catalog, None, &facts(None, &no_env));
        assert_eq!(field(&none, "enabled"), &Value::Bool(false));
        assert!(none.get_path("entries").unwrap().as_array().unwrap().iter().all(|e| field(e, "source") == &Value::String("none".into())));
    }

    #[test]
    fn a_service_is_configured_when_every_required_credential_and_one_of_group_is_met() {
        let catalog = catalog();
        let status = |report: &Value, id: &str| {
            let rows = field(report, "services").as_array().unwrap().clone();
            let row = rows.into_iter().find(|r| field(r, "id") == &Value::String(id.into())).unwrap();
            (field(&row, "status").clone(), field(&row, "needs").clone())
        };
        let store = StoreView { enabled: true, entries: vec![StoredEntry { service: "apikeys".into(), env_var: "SALT".into(), last4: "1234".into() }] };
        let report = list_report(&catalog, None, &facts(Some(&store), &no_env));
        assert_eq!(status(&report, "apikeys"), (Value::String("real".into()), Value::String(String::new())));
        assert_eq!(status(&report, "queue"), (Value::String("not configured".into()), Value::String("one of Q_A / Q_B".into())));
        assert_eq!(status(&report, "hosting").0, Value::String("not configured".into()), "nothing declared, nothing to be configured");
        let queue_env = |name: &str| (name == "Q_B").then(|| "x".to_string());
        assert_eq!(status(&list_report(&catalog, None, &facts(Some(&store), &queue_env)), "queue").0, Value::String("real".into()));
        let missing = list_report(&catalog, None, &facts(None, &no_env));
        assert_eq!(status(&missing, "apikeys"), (Value::String("not configured".into()), Value::String("SALT".into())));
        let mock = list_report(&catalog, None, &ListFacts { store: Some(&store), store_first: false, env: &no_env, mock: true });
        assert_eq!(status(&mock, "apikeys").0, Value::String("mocked".into()));
        assert_eq!(status(&mock, "queue").0, Value::String("not configured".into()), "an unconfigured service is never mocked");
        let emulator = |name: &str| (name == FIRESTORE_EMULATOR_VAR).then(|| "localhost:8080".to_string());
        assert_eq!(status(&list_report(&catalog, None, &facts(None, &emulator)), "knowledge").0, Value::String("emulated".into()));
        assert_eq!(status(&list_report(&catalog, None, &facts(None, &no_env)), "knowledge").1, Value::String(FIRESTORE_NEEDS.into()));
    }

    #[test]
    fn test_reports_presence_only_and_names_what_is_missing() {
        let catalog = catalog();
        let present = test_report(&catalog, "apikeys", &|name| name == "SALT");
        assert_eq!(field(&present, "ok"), &Value::Bool(true));
        assert_eq!(field(&present, "probed"), &Value::Bool(false));
        let absent = test_report(&catalog, "apikeys", &|_| false);
        assert_eq!(field(&absent, "message"), &Value::String("Missing required credentials: SALT".into()));
        assert_eq!(field(&absent, "missing"), &Value::Array(vec![Value::String("SALT".into())]));
        let unknown = test_report(&catalog, "nonexistent", &|_| false);
        assert_eq!(field(&unknown, "ok"), &Value::Bool(true));
    }

    #[test]
    fn a_service_with_only_optional_credentials_needs_one_of_them() {
        let catalog = catalog().with_program_credentials(&[
            ProgramCredential { group: "MailOrbital".into(), env_var: "RESEND".into(), description: String::new() },
            ProgramCredential { group: "MailOrbital".into(), env_var: "SENDGRID".into(), description: String::new() },
        ]);
        assert_eq!(test_subjects(&catalog, "MailOrbital"), ["RESEND", "SENDGRID"]);
        assert_eq!(test_subjects(&catalog, "apikeys"), ["SALT"], "a required credential narrows the subjects");
        assert!(test_subjects(&catalog, "nothing-declared").is_empty());
        let none = test_report(&catalog, "MailOrbital", &|_| false);
        assert_eq!(field(&none, "ok"), &Value::Bool(false));
        assert_eq!(field(&none, "message"), &Value::String("None of the declared credentials is set: RESEND, SENDGRID".into()));
        assert_eq!(field(&none, "missing"), &Value::Array(vec![Value::String("RESEND".into()), Value::String("SENDGRID".into())]));
        let one = test_report(&catalog, "MailOrbital", &|name| name == "SENDGRID");
        assert_eq!(field(&one, "ok"), &Value::Bool(true));
        assert_eq!(field(&one, "missing"), &Value::Array(Vec::new()));
        assert_eq!(field(&test_report(&catalog, "nothing-declared", &|_| false), "ok"), &Value::Bool(true));
    }

    #[test]
    fn the_mode_request_is_exactly_mock_or_real() {
        assert_eq!(requested_mode("mock"), Ok(true));
        assert_eq!(requested_mode("real"), Ok(false));
        assert_eq!(requested_mode("hybrid").unwrap_err().message, "mode must be \"mock\" or \"real\"");
        let mock_env = |name: &str| (name == MOCK_DATA_VAR).then(|| "true".to_string());
        assert!(env_mock(&mock_env));
        assert!(!env_mock(&no_env));
    }

    #[test]
    fn every_op_round_trips_its_name() {
        for op in CredentialAdminOp::ALL {
            assert_eq!(CredentialAdminOp::from_name(op.name()), Some(op));
        }
        assert_eq!(CredentialAdminOp::from_name("credential-status"), None);
        assert_eq!(CredentialAdminOp::Mode.verb(), "mode");
    }
}
