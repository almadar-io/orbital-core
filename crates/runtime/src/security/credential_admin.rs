//! The shapes the credential store answers with, and the raw store asks the `credential-*` operators make. The host
//! supplies the store and the mode flag; the `std-credential-admin` program owns every rule over what they answer.
use serde::{Deserialize, Serialize};

use orbital_core_base::http_wire::{closed_keys, parse_slot_object, text_field, SecretScope};
use orbital_core_base::secret::Secret;
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_base::credential_admin::CredentialAdminOp;

use crate::resumable::IoPort;

/// One stored credential row as the store reports it: its service and credential name, and the last four characters of a
/// secret of eight or more, never its value.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredName {
    pub service: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last4: Option<String>,
}

/// The credential store's state: whether it can encrypt (a master key is present) and the rows it holds.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreListing {
    pub enabled: bool,
    pub entries: Vec<StoredName>,
}

/// One declared credential name the host's environment holds: the last four characters of a value of eight or more,
/// never the value.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvTail {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last4: Option<String>,
}

impl EnvTail {
    /// The tail of `value` held under `name`: `last4` only from eight characters up.
    pub fn of(name: &str, value: &str) -> Self {
        let count = value.chars().count();
        let last4 = (count >= 8).then(|| value.chars().skip(count - 4).collect());
        Self { name: name.to_string(), last4 }
    }
}

/// A host's answer to the environment-tails ask: the tails of the names its environment holds, or that it has no
/// environment to read (a browser, a host without one).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EnvTails {
    Held { tails: Vec<EnvTail> },
    Unsupported,
}

/// What `credential-status` reports of one credential: whether the host holds a value, and the non-secret account label
/// stored beside it (the email it was signed in as), never the value.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialStanding {
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl CredentialStanding {
    /// A standing with no label.
    pub fn of(connected: bool) -> Self {
        Self { connected, label: None }
    }
}

pub use orbital_core_base::credential_grant::{AssertionSpec, ClientAuth, JsonTokenSpec};

/// One `credential-connect` ask: the grant the host's credential store performs. `name`, `client_id`, `client_secret`,
/// `refresh_credential` and the assertion's `signing_key` name credentials the store resolves; `code` and `verifier` are the
/// single-use values, which never print. `grant` is `authorization_code`, `refresh_token` (the credential's own refresh
/// token, or the one `refresh_credential` names), `jwt-bearer` (`assertion`) or `json-token` (`json_token`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialGrant {
    pub name: String,
    pub principal: Option<String>,
    pub token_endpoint: String,
    pub grant: String,
    pub code: Secret<String>,
    pub redirect_uri: String,
    pub verifier: Option<Secret<String>>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub client_auth: ClientAuth,
    pub scopes: Vec<String>,
    pub refresh_credential: Option<String>,
    pub assertion: Option<AssertionSpec>,
    pub json_token: Option<JsonTokenSpec>,
}

/// Why a `credential-connect` did not connect.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectFailureKind {
    Unavailable,
    Unsigned,
    Denied,
    InvalidGrant,
    InvalidClient,
    Transport,
    Malformed,
    Refused,
}

impl ConnectFailureKind {
    /// The word a failure payload carries as `kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Unsigned => "unsigned",
            Self::Denied => "denied",
            Self::InvalidGrant => "invalid_grant",
            Self::InvalidClient => "invalid_client",
            Self::Transport => "transport",
            Self::Malformed => "malformed",
            Self::Refused => "refused",
        }
    }
}

/// A host's failure answer to a `credential-connect`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectFailure {
    pub kind: ConnectFailureKind,
    pub message: String,
}

impl ConnectFailure {
    pub fn new(kind: ConnectFailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

/// The message of a host whose credential store cannot connect.
pub const NO_CONNECT: &str = "this host holds no credential store that can connect";

const AUTH_CODE_KEYS: &[&str] = &["code", "redirectUri", "verifier"];
const CLIENT_KEYS: &[&str] = &["clientId", "clientSecret", "clientAuth"];
const ASSERTION_KEYS: &[&str] = &["signingKey", "issuer", "subject", "audience", "ttlSeconds"];
const JSON_TOKEN_KEYS: &[&str] = &[
    "clientIdField", "clientSecretField", "accessField", "refreshField", "expiresInField", "refreshEndpoint",
    "refreshBodyField",
];

impl CredentialGrant {
    /// The grant a `credential-connect` request object names, for `viewer` (the dispatch's signed-in user). Every grant
    /// names the keys it uses and refuses the keys of another grant.
    pub fn from_request(request: &Value, viewer: Option<&str>) -> Result<Self, ConnectFailure> {
        const WHAT: &str = "credential-connect";
        let refused = |message: String| ConnectFailure::new(ConnectFailureKind::Refused, format!("{WHAT}: {message}"));
        let by_refusal = |refusal: orbital_core_base::http_wire::HttpRefusal| ConnectFailure::new(ConnectFailureKind::Refused, refusal.message());
        let Value::Object(fields) = request else {
            return Err(refused("takes a request object".to_string()));
        };
        closed_keys(
            fields,
            &[
                "credential", "scope", "tokenEndpoint", "grant", "code", "redirectUri", "verifier", "clientId",
                "clientSecret", "clientAuth", "scopes", "refreshCredential", "signingKey", "issuer", "subject", "audience",
                "ttlSeconds", "clientIdField", "clientSecretField", "accessField", "refreshField", "expiresInField",
                "refreshEndpoint", "refreshBodyField",
            ],
            WHAT,
        )
        .map_err(by_refusal)?;
        let mut slot = ValueMap::new();
        for key in ["credential", "scope"] {
            if let Some(value) = fields.get(key) {
                slot.insert(key.into(), value.clone());
            }
        }
        let slot = parse_slot_object(&slot, WHAT).map_err(by_refusal)?;
        let principal = match slot.scope {
            SecretScope::App => None,
            SecretScope::User => match viewer {
                Some(viewer) => Some(viewer.to_string()),
                None => return Err(ConnectFailure::new(ConnectFailureKind::Unsigned, format!("{WHAT}: nobody is signed in"))),
            },
        };
        let text = |key: &str| text_field(fields, key, WHAT).map_err(by_refusal);
        let optional = |key: &str| fields.get(key).map(|_| text(key)).transpose();
        let grant = text("grant")?;
        let allowed: Vec<&str> = match grant.as_str() {
            "authorization_code" => [AUTH_CODE_KEYS, CLIENT_KEYS, &["scopes"]].concat(),
            "refresh_token" => [AUTH_CODE_KEYS, CLIENT_KEYS, &["scopes", "refreshCredential"]].concat(),
            "jwt-bearer" => [ASSERTION_KEYS, &["scopes"]].concat(),
            "json-token" => [JSON_TOKEN_KEYS, &["clientId", "clientSecret"]].concat(),
            other => return Err(refused(format!("grant `{other}` is not supported"))),
        };
        for key in fields.keys() {
            let shared = ["credential", "scope", "tokenEndpoint", "grant"].contains(&key.as_str());
            if !shared && !allowed.contains(&key.as_str()) {
                return Err(refused(format!("`{key}` is not a key of grant `{grant}`")));
            }
        }
        let uses_client = matches!(grant.as_str(), "authorization_code" | "refresh_token");
        let client_auth = if uses_client {
            match text("clientAuth")?.as_str() {
                "body" => ClientAuth::Body,
                "basic" => ClientAuth::Basic,
                "none" => ClientAuth::None,
                _ => return Err(refused("clientAuth must be body, basic or none".to_string())),
            }
        } else {
            ClientAuth::None
        };
        let scopes = match fields.get("scopes") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| refused("scopes must be a list of text".to_string()))?,
            Some(_) => return Err(refused("scopes must be a list of text".to_string())),
        };
        let code_text = |key: &str| -> Result<String, ConnectFailure> {
            if grant == "authorization_code" { text(key) } else { Ok(optional(key)?.unwrap_or_default()) }
        };
        let assertion = if grant == "jwt-bearer" {
            let ttl = match fields.get("ttlSeconds") {
                Some(Value::Number(seconds)) if seconds.fract() == 0.0 && *seconds >= 1.0 && *seconds <= f64::from(u32::MAX) => *seconds as u32,
                _ => return Err(refused("ttlSeconds must be a whole number of seconds, 1 or more".to_string())),
            };
            Some(AssertionSpec {
                signing_key: text("signingKey")?,
                issuer: optional("issuer")?,
                subject: optional("subject")?,
                audience: text("audience")?,
                ttl_seconds: ttl,
            })
        } else {
            None
        };
        let json_token = if grant == "json-token" {
            let spec = JsonTokenSpec {
                client_id_field: text("clientIdField")?,
                client_secret_field: text("clientSecretField")?,
                access_field: text("accessField")?,
                refresh_field: optional("refreshField")?,
                expires_in_field: optional("expiresInField")?,
                refresh_endpoint: optional("refreshEndpoint")?,
                refresh_body_field: optional("refreshBodyField")?,
            };
            if spec.refresh_endpoint.is_some() != spec.refresh_body_field.is_some() {
                return Err(refused("refreshEndpoint and refreshBodyField are declared together".to_string()));
            }
            if spec.refresh_endpoint.is_some() && spec.refresh_field.is_none() {
                return Err(refused("a refreshEndpoint needs the refreshField the answer carries the refresh token under".to_string()));
            }
            Some(spec)
        } else {
            None
        };
        let client_id = if grant == "jwt-bearer" || (uses_client && client_auth == ClientAuth::None) {
            optional("clientId")?.unwrap_or_default()
        } else {
            text("clientId")?
        };
        if grant == "json-token" && !fields.contains_key("clientSecret") {
            return Err(refused("grant `json-token` needs clientSecret".to_string()));
        }
        let refresh_credential = optional("refreshCredential")?;
        let code = code_text("code")?;
        let redirect_uri = code_text("redirectUri")?;
        Ok(Self {
            name: slot.name,
            principal,
            token_endpoint: text("tokenEndpoint")?,
            grant,
            code: Secret::new(code),
            redirect_uri,
            verifier: optional("verifier")?.map(Secret::new),
            client_id,
            client_secret: optional("clientSecret")?,
            client_auth,
            scopes,
            refresh_credential,
            assertion,
            json_token,
        })
    }
}

/// What a rotation did to the stored rows.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RotateCounts {
    #[cfg_attr(test, ts(type = "number"))]
    pub rotated: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub already_current: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub unreadable: u64,
}

fn object(pairs: Vec<(&str, Value)>) -> Value {
    Value::object(ValueMap::from_iter(pairs.into_iter().map(|(key, value)| (key.into(), value))))
}

fn text(request: &Value, op: CredentialAdminOp, key: &str) -> Result<String, String> {
    match request {
        Value::Object(fields) => match fields.get(key) {
            Some(Value::String(value)) => Ok(value.to_string()),
            Some(_) => Err(format!("{} needs {key} as text", op.name())),
            None => Err(format!("{} needs {key}", op.name())),
        },
        _ => Err(format!("{} takes a request object", op.name())),
    }
}

/// Ask the host's store or mode flag for `op`, answering the raw reply.
pub(crate) fn ask(port: &IoPort, op: CredentialAdminOp, request: &Value) -> Result<Value, String> {
    match op {
        CredentialAdminOp::List => {
            let listing = port.credential_store_list()?.unwrap_or(StoreListing { enabled: false, entries: Vec::new() });
            let entries = listing
                .entries
                .into_iter()
                .map(|entry| {
                    let mut row = vec![("service", Value::String(entry.service.into())), ("name", Value::String(entry.name.into()))];
                    if let Some(tail) = entry.last4 {
                        row.push(("last4", Value::String(tail.into())));
                    }
                    object(row)
                })
                .collect::<Vec<_>>();
            Ok(object(vec![("enabled", Value::Bool(listing.enabled)), ("entries", Value::array(entries))]))
        }
        CredentialAdminOp::Set => {
            port.credential_store_write(&text(request, op, "service")?, &text(request, op, "envVar")?, &text(request, op, "value")?)?;
            Ok(object(vec![("saved", Value::Bool(true))]))
        }
        CredentialAdminOp::Remove => Ok(object(vec![("removed", Value::Bool(port.credential_store_remove(&text(request, op, "envVar")?)?))])),
        CredentialAdminOp::Test => {
            let held = port.secret(&text(request, op, "envVar")?, None)?.is_some_and(|value| !value.expose().is_empty());
            Ok(object(vec![("held", Value::Bool(held))]))
        }
        CredentialAdminOp::Mode => {
            let mode = match request {
                Value::Object(fields) if fields.contains_key("mode") => port.credential_mode_write(&text(request, op, "mode")?)?,
                _ => port.credential_mode_read()?.unwrap_or_default(),
            };
            Ok(object(vec![("mode", Value::String(mode.into()))]))
        }
        CredentialAdminOp::Rotate => {
            let counts = port.credential_store_rotate()?;
            Ok(object(vec![
                ("rotated", Value::Number(counts.rotated as f64)),
                ("alreadyCurrent", Value::Number(counts.already_current as f64)),
                ("unreadable", Value::Number(counts.unreadable as f64)),
            ]))
        }
    }
}
