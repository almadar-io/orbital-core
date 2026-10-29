//! Credential administration, owned by the kernel: the role gate, request validation, the declared-name check, the
//! catalog (the registry's remaining services plus the credentials the running program declares), last-4 masking, and
//! the shapes `list`, `set`, `remove`, `test`, `mode` and `rotate` answer with. The host supplies only the raw store
//! and mode-flag asks (`IoRequest::CredentialStore*` / `CredentialMode*`); everything decided here is pure.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

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
/// The host settings the kernel reads through the credential ask; under `store` source they still come from the host env.
pub const HOST_SETTING_VARS: [&str; 4] = [ADMIN_ROLES_VAR, SOURCE_VAR, MODE_ENV_VAR, MOCK_DATA_VAR];
/// The message of a host that holds no credential store.
pub const NO_STORE: &str = "No credential store is installed on this host";
/// The message of a host with no mode flag.
pub const NO_MANAGER: &str = "No integration manager is installed on this host";

const DEFAULT_ADMIN_ROLES: [&str; 2] = ["admin", "owner"];
const UNMOCKED: [&str; 2] = ["credentials", "workspace"];

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

    /// Whether the effect takes a request object before its options; `rotate` has no parameters, so it takes none.
    pub fn takes_request(self) -> bool {
        !matches!(self, Self::Rotate)
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
    let empty = ValueMap::default();
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

/// The catalog of the installed services registry plus the credentials the running program declares.
pub fn catalog(program: &[ProgramCredential]) -> Result<Catalog, Refusal> {
    let data = crate::language_data::language_data().map_err(|e| Refusal::plain(e.to_string()))?;
    Ok(Catalog::from_registry(&data.services).map_err(Refusal::plain)?.with_program_credentials(program))
}

/// `<service>: <ENV_VAR> (<description>)` for every required credential of `services` that `present` does not hold.
pub fn missing_required_credentials(
    catalog: &Catalog,
    services: &std::collections::BTreeSet<String>,
    present: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    services
        .iter()
        .flat_map(|service| {
            catalog
                .declared_for(service)
                .iter()
                .filter(|c| c.required && !present(&c.env_var))
                .map(move |c| format!("{service}: {} ({})", c.env_var, c.description))
        })
        .collect()
}

/// Whether `value` of `ALMADAR_CREDENTIALS_SOURCE` selects store-only resolution.
pub fn source_is_store(value: Option<&str>) -> bool {
    value == Some("store")
}

/// Whether a credential `name` is answered from the store alone under `store` source: every name but a host setting.
pub fn store_only_name(name: &str) -> bool {
    !HOST_SETTING_VARS.contains(&name)
}

/// One declared credential a boot copies from the host env into the store.
#[derive(Clone, PartialEq, Eq)]
pub struct SeedRow {
    pub service: String,
    pub env_var: String,
    pub value: String,
}

impl std::fmt::Debug for SeedRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedRow").field("service", &self.service).field("env_var", &self.env_var).finish_non_exhaustive()
    }
}

/// The declared credentials the store does not hold yet and the env does: what `store` source seeds at boot, so the
/// env stays the way to hand a deployment its first keys. A name declared by several services seeds once.
pub fn seed_plan(catalog: &Catalog, held: &dyn Fn(&str) -> bool, env: &dyn Fn(&str) -> Option<String>) -> Vec<SeedRow> {
    let set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
    seed_targets(catalog, held, &set)
        .into_iter()
        .filter_map(|(service, env_var)| env(&env_var).map(|value| SeedRow { service, env_var, value }))
        .collect()
}

/// The (service, name) pairs to seed: each declared name, once, that the store does not hold and the env sets.
fn seed_targets(catalog: &Catalog, held: &dyn Fn(&str) -> bool, set: &dyn Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut targets: Vec<(String, String)> = Vec::new();
    for (service, declared) in &catalog.declared {
        for d in declared {
            if targets.iter().any(|(_, env_var)| *env_var == d.env_var) || held(&d.env_var) || !set(&d.env_var) {
                continue;
            }
            targets.push((service.clone(), d.env_var.clone()));
        }
    }
    targets
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeedDeclared {
    service: String,
    env_var: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SeedTarget<'a> {
    service: &'a str,
    env_var: &'a str,
}

/// [`seed_plan`] over JSON for a host that drives its own store: `declared_json` is `[{ service, envVar }]`, `held_json`
/// the names the store holds (`string[]`), `present_json` the declared names the host env sets to a non-empty value
/// (`string[]`). The answer is the rows to seed (`[{ service, envVar }]`) in plan order; no value crosses either way.
pub fn seed_plan_json(declared_json: &str, held_json: &str, present_json: &str) -> Result<String, String> {
    let declared: Vec<SeedDeclared> = serde_json::from_str(declared_json).map_err(|e| format!("parse declared: {e}"))?;
    let held: Vec<String> = serde_json::from_str(held_json).map_err(|e| format!("parse held: {e}"))?;
    let present: Vec<String> = serde_json::from_str(present_json).map_err(|e| format!("parse present: {e}"))?;
    let credentials: Vec<ProgramCredential> = declared
        .into_iter()
        .map(|d| ProgramCredential { group: d.service, env_var: d.env_var, description: String::new() })
        .collect();
    let catalog = Catalog::default().with_program_credentials(&credentials);
    let targets = seed_targets(&catalog, &|name| held.iter().any(|h| h == name), &|name| present.iter().any(|p| p == name));
    let targets: Vec<SeedTarget<'_>> = targets.iter().map(|(service, env_var)| SeedTarget { service, env_var }).collect();
    serde_json::to_string(&targets).map_err(|e| e.to_string())
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
    Value::Object(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
}

fn text(value: &str) -> Value {
    Value::string(value.to_string())
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
                ("id", Value::string(format!("{service}/{}", d.env_var))),
                ("service", text(service)),
                ("envVar", text(&d.env_var)),
                ("required", Value::Bool(d.required)),
                ("description", text(&d.description)),
                ("configured", Value::Bool(source != "none")),
                ("source", text(source)),
                ("last4", Value::string(last4)),
            ]));
        }
    }
    for entry in stored.into_iter().flatten() {
        if filter.is_some_and(|f| f != entry.service) {
            continue;
        }
        entries.push(object(vec![
            ("id", Value::string(format!("{}/{}", entry.service, entry.env_var))),
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
    let mut services: Vec<Value> = Vec::new();
    for service in &catalog.listed {
        if filter.is_some_and(|f| f != service) {
            continue;
        }
        let declared = catalog.declared_for(service);
        let ready = {
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
        } else {
            "real"
        };
        let needs = if ready {
            String::new()
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
            ("needs", Value::string(needs)),
        ]));
    }
    object(vec![
        ("enabled", Value::Bool(facts.store.is_some_and(|s| s.enabled))),
        ("mode", text(mode_name(facts.mock))),
        ("services", Value::array(services)),
        ("entries", Value::array(entries)),
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
        ("missing", Value::Array(missing.into_iter().map(Value::string).collect())),
        ("probed", Value::Bool(false)),
        ("ok", Value::Bool(configured)),
        ("message", Value::string(message)),
    ])
}

/// `{ saved, service, envVar, last4 }`.
pub fn saved_report(service: &str, env_var: &str, value: &str) -> Value {
    object(vec![
        ("saved", Value::Bool(true)),
        ("service", text(service)),
        ("envVar", text(env_var)),
        ("last4", Value::string(last4(value))),
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
        "maps": { "backend": "host-injected" }
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
            &Value::object(ValueMap::from_iter([
                ("service".into(), text("apikeys")),
                ("envVar".into(), text("SALT")),
                ("value".into(), text("v")),
            ])),
        )
        .unwrap();
        assert_eq!(ok.env_var.as_deref(), Some("SALT"));
        let missing = parse_request(CredentialAdminOp::Set, &Value::object(ValueMap::from_iter([("service".into(), text("a"))]))).unwrap_err();
        assert_eq!(
            missing.message,
            "Validation failed for credential-set: envVar: Missing required parameter: envVar; value: Missing required parameter: value"
        );
        assert_eq!(missing.code, Some(VALIDATION_CODE));
        let typed = parse_request(CredentialAdminOp::Test, &Value::object(ValueMap::from_iter([("service".into(), Value::Number(5.0))]))).unwrap_err();
        assert_eq!(typed.message, "Validation failed for credential-test: service: Expected string, got number");
        let extra = parse_request(CredentialAdminOp::List, &Value::object(ValueMap::from_iter([("extra".into(), Value::Bool(true))]))).unwrap_err();
        assert_eq!(extra.message, "Validation failed for credential-list: extra: Unknown parameter: extra (declared: service)");
        assert!(parse_request(CredentialAdminOp::Rotate, &Value::Null).is_ok());
        assert!(parse_request(CredentialAdminOp::Rotate, &Value::object(ValueMap::from_iter([("force".into(), Value::Bool(true))]))).is_err());
        assert!(parse_request(CredentialAdminOp::List, &Value::string("x")).is_err());
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
    fn the_source_is_store_only_for_the_exact_word_and_a_host_setting_stays_on_the_env() {
        assert!(source_is_store(Some("store")));
        for other in [None, Some(""), Some("env"), Some("Store")] {
            assert!(!source_is_store(other), "{other:?}");
        }
        assert!(store_only_name("OPENAI_API_KEY"));
        for setting in HOST_SETTING_VARS {
            assert!(!store_only_name(setting), "{setting}");
        }
    }

    #[test]
    fn the_seed_plan_copies_declared_env_values_the_store_lacks_once_and_never_prints_them() {
        let catalog = catalog().with_program_credentials(&[ProgramCredential { group: "LlmOrbital".into(), env_var: "OPENAI".into(), description: "o".into() }]);
        let env = |name: &str| match name {
            "SALT" => Some("s3cret".to_string()),
            "OPENAI" => Some("sk-live".to_string()),
            "Q_A" => Some(String::new()),
            "UNDECLARED" => Some("x".to_string()),
            _ => None,
        };
        let rows = seed_plan(&catalog, &|name| name == "SALT", &env);
        assert_eq!(rows.iter().map(|r| (r.service.as_str(), r.env_var.as_str())).collect::<Vec<_>>(), [("LlmOrbital", "OPENAI")], "SALT is held, Q_A is empty, UNDECLARED is not declared");
        assert!(!format!("{rows:?}").contains("sk-live"));
        let all = seed_plan(&catalog, &|_| false, &env);
        assert_eq!(all.iter().map(|r| r.env_var.as_str()).collect::<Vec<_>>(), ["SALT", "OPENAI"]);
        assert!(seed_plan(&catalog, &|_| false, &|_| None).is_empty(), "control: nothing in the env, nothing to seed");
    }

    #[test]
    fn the_seed_plan_wire_takes_only_the_names_the_env_sets_and_names_the_rows_to_seed() {
        let declared = r#"[{"service":"stripe","envVar":"STRIPE_KEY"},{"service":"stripe","envVar":"STRIPE_WEBHOOK"},{"service":"other","envVar":"STRIPE_KEY"},{"service":"mail","envVar":"EMPTY"}]"#;
        let present = r#"["STRIPE_KEY","STRIPE_WEBHOOK","UNDECLARED"]"#;
        assert_eq!(seed_plan_json(declared, r#"["STRIPE_WEBHOOK"]"#, present).unwrap(), r#"[{"service":"stripe","envVar":"STRIPE_KEY"}]"#);
        assert_eq!(
            seed_plan_json(declared, "[]", present).unwrap(),
            r#"[{"service":"stripe","envVar":"STRIPE_KEY"},{"service":"stripe","envVar":"STRIPE_WEBHOOK"}]"#,
            "EMPTY is not set, UNDECLARED is not declared"
        );
        assert_eq!(seed_plan_json(declared, "[]", "[]").unwrap(), "[]", "control: nothing set, nothing to seed");
        assert!(seed_plan_json(declared, "[]", r#"{"STRIPE_KEY":"sk-env"}"#).unwrap_err().contains("present"), "an env map is refused: only names cross");
        assert!(seed_plan_json("not json", "[]", "[]").unwrap_err().contains("declared"));
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
        assert_eq!(catalog.listed, ["apikeys", "queue", "hosting"], "credentials is unmocked; maps is host-injected");
    }

    #[test]
    fn masking_keeps_the_last_four_code_units_only() {
        assert_eq!(last4("fake-new-1111"), "1111");
        assert_eq!(last4("ab"), "ab");
        assert_eq!(last4(""), "");
        let report = saved_report("apikeys", "SALT", "secret-value-9876");
        assert_eq!(field(&report, "last4"), &Value::string("9876"));
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
        let by_id = |id: &str| entries.iter().find(|e| field(e, "id") == &Value::string(id)).unwrap();
        assert_eq!(field(by_id("apikeys/SALT"), "source"), &Value::string("store"));
        assert_eq!(field(by_id("apikeys/SALT"), "last4"), &Value::string("1234"));
        assert_eq!(field(by_id("queue/Q_A"), "source"), &Value::string("env"));
        assert_eq!(field(by_id("queue/Q_A"), "last4"), &Value::string("0042"));
        assert_eq!(field(by_id("queue/Q_B"), "source"), &Value::string("none"));
        assert_eq!(field(by_id("database/ORDERS_DB_URL"), "description"), &Value::string("Stored connection reference"));
        assert_eq!(field(&report, "enabled"), &Value::Bool(true));
        let hidden = list_report(&catalog, Some("queue"), &ListFacts { store: Some(&store), store_first: true, env: &env, mock: false });
        assert_eq!(field(&hidden.get_path("entries").unwrap().as_array().unwrap()[0], "source"), &Value::string("none"));
        let none = list_report(&catalog, None, &facts(None, &no_env));
        assert_eq!(field(&none, "enabled"), &Value::Bool(false));
        assert!(none.get_path("entries").unwrap().as_array().unwrap().iter().all(|e| field(e, "source") == &Value::string("none")));
    }

    #[test]
    fn a_service_is_configured_when_every_required_credential_and_one_of_group_is_met() {
        let catalog = catalog();
        let status = |report: &Value, id: &str| {
            let rows = field(report, "services").as_array().unwrap().clone();
            let row = rows.into_iter().find(|r| field(r, "id") == &Value::string(id)).unwrap();
            (field(&row, "status").clone(), field(&row, "needs").clone())
        };
        let store = StoreView { enabled: true, entries: vec![StoredEntry { service: "apikeys".into(), env_var: "SALT".into(), last4: "1234".into() }] };
        let report = list_report(&catalog, None, &facts(Some(&store), &no_env));
        assert_eq!(status(&report, "apikeys"), (Value::string("real"), Value::string(String::new())));
        assert_eq!(status(&report, "queue"), (Value::string("not configured"), Value::string("one of Q_A / Q_B")));
        assert_eq!(status(&report, "hosting").0, Value::string("not configured"), "nothing declared, nothing to be configured");
        let queue_env = |name: &str| (name == "Q_B").then(|| "x".to_string());
        assert_eq!(status(&list_report(&catalog, None, &facts(Some(&store), &queue_env)), "queue").0, Value::string("real"));
        let missing = list_report(&catalog, None, &facts(None, &no_env));
        assert_eq!(status(&missing, "apikeys"), (Value::string("not configured"), Value::string("SALT")));
        let mock = list_report(&catalog, None, &ListFacts { store: Some(&store), store_first: false, env: &no_env, mock: true });
        assert_eq!(status(&mock, "apikeys").0, Value::string("mocked"));
        assert_eq!(status(&mock, "queue").0, Value::string("not configured"), "an unconfigured service is never mocked");
        assert!(list_report(&catalog, Some("knowledge"), &facts(None, &no_env)).get_path("services").and_then(|v| v.as_array().map(Vec::is_empty)).unwrap_or(false), "knowledge is no listed service");
    }

    #[test]
    fn test_reports_presence_only_and_names_what_is_missing() {
        let catalog = catalog();
        let present = test_report(&catalog, "apikeys", &|name| name == "SALT");
        assert_eq!(field(&present, "ok"), &Value::Bool(true));
        assert_eq!(field(&present, "probed"), &Value::Bool(false));
        let absent = test_report(&catalog, "apikeys", &|_| false);
        assert_eq!(field(&absent, "message"), &Value::string("Missing required credentials: SALT"));
        assert_eq!(field(&absent, "missing"), &Value::array(vec![Value::String("SALT".into())]));
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
        assert_eq!(field(&none, "message"), &Value::string("None of the declared credentials is set: RESEND, SENDGRID"));
        assert_eq!(field(&none, "missing"), &Value::array(vec![Value::String("RESEND".into()), Value::String("SENDGRID".into())]));
        let one = test_report(&catalog, "MailOrbital", &|name| name == "SENDGRID");
        assert_eq!(field(&one, "ok"), &Value::Bool(true));
        assert_eq!(field(&one, "missing"), &Value::array(Vec::new()));
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
    fn only_the_required_credentials_of_the_called_services_are_owed() {
        let catalog = Catalog::from_registry(
            r#"{ "services": {
                "stripe": { "backend": "production", "credentials": [
                    { "envVar": "STRIPE_SECRET_KEY", "required": true, "description": "API key" },
                    { "envVar": "STRIPE_WEBHOOK_SECRET", "required": false, "description": "webhooks" }] },
                "email": { "backend": "production", "credentials": [{ "envVar": "SMTP_URL", "required": true, "description": "mail" }] },
                "slack": { "backend": "production", "credentials": [{ "envVar": "SLACK_TOKEN", "required": true, "description": "bot" }] } } }"#,
        )
        .unwrap();
        let called: std::collections::BTreeSet<String> = ["email".to_string(), "stripe".to_string()].into();
        assert_eq!(
            missing_required_credentials(&catalog, &called, &|name| name == "SMTP_URL"),
            ["stripe: STRIPE_SECRET_KEY (API key)"]
        );
        assert!(missing_required_credentials(&catalog, &called, &|_| true).is_empty(), "control: everything held owes nothing");
        assert!(
            missing_required_credentials(&catalog, &std::collections::BTreeSet::new(), &|_| false).is_empty(),
            "control: a service the program never calls (slack) is not owed"
        );
        let unknown: std::collections::BTreeSet<String> = ["nowhere".to_string()].into();
        assert!(missing_required_credentials(&catalog, &unknown, &|_| false).is_empty(), "an undeclared service owes nothing");
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
