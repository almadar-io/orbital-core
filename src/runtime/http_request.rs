//! The `http` effect's request builder and its sans-I/O step machine (`docs/Almadar_Embedded_Kernel.md`,
//! "The `http` effect", H.3, H.3b, H.3c). Pure: no clock, env, randomness or I/O. The kernel, the JS interpreter
//! (through wasm) and the generated TS shell only answer the driver's asks.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::codec;
use crate::embed::wire::{HttpFailureWire, HttpRequestWire, HttpResponseWire, Settled};
use crate::evaluator::{value_to_json, BindingContext, Evaluator, Value, ValueMap};
use crate::runtime::http_service::{HttpRequest, HttpResponse, TransportError};
use crate::schema::SExpression;
use crate::str_utils::js_number_string;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamFraming {
    Sse,
    JsonLines,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseAs {
    Text,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DigestEncoding {
    Hex,
    Base64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyIn {
    Header,
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageStyle {
    #[serde(rename = "cursor")]
    Cursor,
    #[serde(rename = "offset")]
    Offset,
    #[serde(rename = "page")]
    Page,
    #[serde(rename = "link-header")]
    LinkHeader,
}

/// Whose credential a slot names: the app's, or the signed-in principal's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretScope {
    #[default]
    App,
    User,
}

impl SecretScope {
    fn is_app(&self) -> bool {
        *self == Self::App
    }
}

/// A credential slot: a credential NAME the host resolves, plus whose it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialSlot {
    pub name: String,
    #[serde(default, skip_serializing_if = "SecretScope::is_app")]
    pub scope: SecretScope,
}

impl From<&str> for CredentialSlot {
    fn from(name: &str) -> Self {
        Self {
            name: name.to_string(),
            scope: SecretScope::App,
        }
    }
}

impl From<String> for CredentialSlot {
    fn from(name: String) -> Self {
        Self {
            name,
            scope: SecretScope::App,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultipartKind {
    Related,
    FormData,
}

impl MultipartKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Related => "related",
            Self::FormData => "form-data",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PartContent {
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultipartPart {
    pub headers: Vec<(String, String)>,
    pub content: PartContent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HttpBodySpec {
    Empty,
    Json(String),
    Form(Vec<(String, String)>),
    Text(String),
    Bytes(Vec<u8>),
    Multipart {
        kind: MultipartKind,
        parts: Vec<MultipartPart>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormEncoding {
    #[default]
    Html,
    Rfc3986,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryOn {
    #[serde(rename = "5xx")]
    Server,
    #[serde(rename = "429")]
    TooManyRequests,
    #[serde(rename = "timeout")]
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRetry {
    pub max: u32,
    pub on: Vec<RetryOn>,
    pub backoff_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenGrant {
    ClientCredentials,
    RefreshToken,
    AuthorizationCode,
    Custom,
}

impl TokenGrant {
    fn grant_type(self) -> Option<&'static str> {
        match self {
            Self::ClientCredentials => Some("client_credentials"),
            Self::RefreshToken => Some("refresh_token"),
            Self::AuthorizationCode => Some("authorization_code"),
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenBodyKind {
    Json,
    Form,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientAuth {
    Basic,
    Post,
}

/// A token-exchange body field: a literal, or a credential slot the host resolves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenField {
    Literal(Value),
    Slot(CredentialSlot),
}

/// Where the exchanged token is kept as a host secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreAs {
    pub name: String,
    #[serde(default, skip_serializing_if = "SecretScope::is_app")]
    pub scope: SecretScope,
}

/// The one token exchange: `oauth2-client` lowers to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenExchange {
    pub token_url: String,
    pub grant: TokenGrant,
    pub body_kind: TokenBodyKind,
    pub fields: Vec<(String, TokenField)>,
    pub client_auth: Option<ClientAuth>,
    pub client_id: Option<CredentialSlot>,
    pub client_secret: Option<CredentialSlot>,
    pub access_field: String,
    pub expires_field: Option<String>,
    pub expires_margin_ms: u64,
    pub store_as: StoreAs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all_fields = "camelCase")]
pub enum HttpAuth {
    #[serde(rename = "bearer")]
    Bearer { credential: CredentialSlot },
    #[serde(rename = "api-key")]
    ApiKey {
        credential: CredentialSlot,
        name: String,
        location: ApiKeyIn,
    },
    #[serde(rename = "basic")]
    Basic {
        user: CredentialSlot,
        credential: CredentialSlot,
    },
    #[serde(rename = "token-exchange")]
    TokenExchange(TokenExchange),
    #[serde(rename = "hmac-sha256")]
    HmacSha256 {
        credential: CredentialSlot,
        name: String,
        signs: String,
        encoding: DigestEncoding,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpPaginate {
    pub style: PageStyle,
    pub param: Option<String>,
    pub next: Value,
    pub collect: Value,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpCallSpec {
    pub method: HttpMethod,
    pub origin: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: HttpBodySpec,
    pub auth: Option<HttpAuth>,
    pub timeout_ms: Option<u64>,
    pub response_as: ResponseAs,
    pub paginate: Option<HttpPaginate>,
    pub stream: Option<StreamFraming>,
    #[serde(default)]
    pub form_encoding: FormEncoding,
    #[serde(default)]
    pub retry: Option<HttpRetry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpRefusal {
    EgressDenied(String),
    OriginEscape(String),
    CredentialMissing(String),
    Invalid(String),
}

impl HttpRefusal {
    pub fn message(&self) -> String {
        match self {
            Self::EgressDenied(origin) => format!("egress denied: {origin}"),
            Self::OriginEscape(path) => format!("request leaves the declared origin: {path}"),
            Self::CredentialMissing(name) => format!("credential missing: {name}"),
            Self::Invalid(message) => message.clone(),
        }
    }
}

struct OriginParts {
    scheme: String,
    host: String,
    port: u16,
}

impl PartialEq for OriginParts {
    fn eq(&self, other: &Self) -> bool {
        self.scheme == other.scheme && self.host == other.host && self.port == other.port
    }
}

/// Splits `scheme://authority` off `url`; the rest (path, query) is returned as is.
fn split_url(url: &str) -> Option<(OriginParts, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let (authority, remainder) = rest.split_at(end);
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let close = after.find(']')?;
        let host = &authority[..close + 2];
        match &after[close + 1..] {
            "" => (host, default_port),
            tail => (host, tail.strip_prefix(':')?.parse().ok()?),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, default_port),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some((
        OriginParts {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
        },
        remainder,
    ))
}

fn parse_origin(origin: &str) -> Option<OriginParts> {
    match split_url(origin)? {
        (parts, "") => Some(parts),
        _ => None,
    }
}

/// `scheme://host[:port][/prefix]`: the origin and its path prefix (empty, or `/a/b` with no trailing slash).
fn parse_origin_prefix(origin: &str) -> Option<(OriginParts, String)> {
    let (parts, rest) = split_url(origin)?;
    if rest.contains(['?', '#', '\\']) {
        return None;
    }
    let prefix = rest.trim_end_matches('/');
    if prefix.contains("//") || prefix.split('/').any(|segment| segment == "." || segment == "..") {
        return None;
    }
    Some((parts, prefix.to_string()))
}

/// Exact scheme + host + port match against an `egress` entry.
pub fn origin_allowed(url_or_origin: &str, egress: &[String]) -> bool {
    let Some((target, _)) = split_url(url_or_origin) else {
        return false;
    };
    egress
        .iter()
        .filter_map(|entry| parse_origin(entry))
        .any(|allowed| allowed == target)
}

fn invalid<T>(message: impl Into<String>) -> Result<T, HttpRefusal> {
    Err(HttpRefusal::Invalid(message.into()))
}

fn scalar_text(value: &Value, what: &str) -> Result<Option<String>, HttpRefusal> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        Value::Number(n) => Ok(Some(js_number_string(*n))),
        Value::Bool(b) => Ok(Some(b.to_string())),
        _ => invalid(format!("{what} must be a string, number, boolean, null or a list of them")),
    }
}

fn pairs_of(value: &Value, what: &str) -> Result<Vec<(String, String)>, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid(format!("{what} must be an object"));
    };
    let mut out = Vec::new();
    for (key, entry) in map {
        match entry {
            Value::Array(items) => {
                for item in items {
                    if let Some(text) = scalar_text(item, what)? {
                        out.push((key.clone(), text));
                    }
                }
            }
            other => {
                if let Some(text) = scalar_text(other, what)? {
                    out.push((key.clone(), text));
                }
            }
        }
    }
    Ok(out)
}

fn closed_keys(map: &ValueMap, allowed: &[&str], what: &str) -> Result<(), HttpRefusal> {
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(key) => invalid(format!("{what} has no key `{key}`")),
        None => Ok(()),
    }
}

fn text_field(map: &ValueMap, key: &str, what: &str) -> Result<String, HttpRefusal> {
    match map.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => invalid(format!("{what}.{key} must be a string")),
    }
}

fn parse_slot_object(map: &ValueMap, what: &str) -> Result<CredentialSlot, HttpRefusal> {
    closed_keys(map, &["credential", "scope"], what)?;
    let name = text_field(map, "credential", what)?;
    let scope = match map.get("scope") {
        None => SecretScope::App,
        Some(Value::String(s)) if s == "app" => SecretScope::App,
        Some(Value::String(s)) if s == "user" => SecretScope::User,
        Some(_) => return invalid(format!("{what}.scope must be app or user")),
    };
    Ok(CredentialSlot { name, scope })
}

fn slot_field(map: &ValueMap, key: &str, what: &str) -> Result<CredentialSlot, HttpRefusal> {
    let at = format!("{what}.{key}");
    match map.get(key) {
        Some(Value::String(name)) => Ok(name.as_str().into()),
        Some(Value::Object(slot)) => parse_slot_object(slot, &at),
        _ => invalid(format!("{at} must be a credential name or {{ credential, scope }}")),
    }
}

fn whole_number(value: Option<&Value>, what: &str) -> Result<Option<u64>, HttpRefusal> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) if *n >= 0.0 && n.fract() == 0.0 && *n <= u64::MAX as f64 => Ok(Some(*n as u64)),
        Some(_) => invalid(format!("{what} must be a whole number")),
    }
}

fn non_empty_text(map: &ValueMap, key: &str, what: &str) -> Result<Option<String>, HttpRefusal> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => invalid(format!("{what}.{key} must be a non-empty string")),
    }
}

fn parse_token_body(value: Option<&Value>) -> Result<(TokenBodyKind, Vec<(String, TokenField)>), HttpRefusal> {
    let Some(value) = value else {
        return Ok((TokenBodyKind::Form, Vec::new()));
    };
    let Value::Object(body) = value else {
        return invalid("auth.body must be { json: {...} } or { form: {...} }");
    };
    closed_keys(body, &["json", "form"], "auth.body")?;
    let (kind, fields) = match (body.get("json"), body.get("form")) {
        (Some(fields), None) => (TokenBodyKind::Json, fields),
        (None, Some(fields)) => (TokenBodyKind::Form, fields),
        _ => return invalid("auth.body carries exactly one of json, form"),
    };
    let Value::Object(fields) = fields else {
        return invalid("auth.body fields must be an object");
    };
    let mut out = Vec::new();
    for (name, field) in fields {
        let at = format!("auth.body.{name}");
        out.push((
            name.clone(),
            match field {
                Value::Object(slot) if slot.contains_key("credential") => {
                    TokenField::Slot(parse_slot_object(slot, &at)?)
                }
                Value::Null if kind == TokenBodyKind::Form => continue,
                Value::String(_) | Value::Number(_) | Value::Bool(_) => TokenField::Literal(field.clone()),
                other if kind == TokenBodyKind::Json => TokenField::Literal(other.clone()),
                _ => return invalid(format!("{at} must be a string, number, boolean or credential slot in a form body")),
            },
        ));
    }
    Ok((kind, out))
}

fn parse_store_as(value: &Value) -> Result<StoreAs, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth.storeAs must be an object");
    };
    closed_keys(map, &["name", "scope"], "auth.storeAs")?;
    let Some(name) = non_empty_text(map, "name", "auth.storeAs")? else {
        return invalid("auth.storeAs.name is required");
    };
    let scope = match map.get("scope") {
        None => SecretScope::App,
        Some(Value::String(s)) if s == "app" => SecretScope::App,
        Some(Value::String(s)) if s == "user" => SecretScope::User,
        Some(_) => return invalid("auth.storeAs.scope must be app or user"),
    };
    Ok(StoreAs { name, scope })
}

fn parse_token_exchange(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(
        map,
        &[
            "scheme", "tokenUrl", "grant", "body", "clientAuth", "clientId", "clientSecret", "accessField",
            "expiresField", "expiresMarginMs", "storeAs",
        ],
        "auth",
    )?;
    let token_url = text_field(map, "tokenUrl", "auth")?;
    if split_url(&token_url).is_none() {
        return invalid(format!("tokenUrl `{token_url}` is not an http(s) URL"));
    }
    let grant = match map.get("grant") {
        None => TokenGrant::Custom,
        Some(Value::String(s)) => match s.as_str() {
            "client_credentials" => TokenGrant::ClientCredentials,
            "refresh_token" => TokenGrant::RefreshToken,
            "authorization_code" => TokenGrant::AuthorizationCode,
            "custom" => TokenGrant::Custom,
            other => return invalid(format!("auth.grant must be client_credentials, refresh_token, authorization_code or custom, not `{other}`")),
        },
        Some(_) => return invalid("auth.grant must be a string"),
    };
    let (body_kind, fields) = parse_token_body(map.get("body"))?;
    let has_field = |name: &str| fields.iter().any(|(n, _)| n == name);
    if grant.grant_type().is_some() && has_field("grant_type") {
        return invalid("auth.body.grant_type is set by auth.grant; use grant: custom to send your own");
    }
    match grant {
        TokenGrant::RefreshToken if !has_field("refresh_token") => {
            return invalid("a refresh_token grant needs auth.body.refresh_token")
        }
        TokenGrant::AuthorizationCode if !has_field("code") => {
            return invalid("an authorization_code grant needs auth.body.code")
        }
        _ => {}
    }
    let client_auth = match map.get("clientAuth") {
        None => None,
        Some(Value::String(s)) if s == "basic" => Some(ClientAuth::Basic),
        Some(Value::String(s)) if s == "post" => Some(ClientAuth::Post),
        Some(_) => return invalid("auth.clientAuth must be basic or post"),
    };
    let (client_id, client_secret) = if client_auth.is_some() {
        (
            Some(slot_field(map, "clientId", "auth")?),
            Some(slot_field(map, "clientSecret", "auth")?),
        )
    } else if map.contains_key("clientId") || map.contains_key("clientSecret") {
        return invalid("auth.clientId and auth.clientSecret need auth.clientAuth");
    } else {
        (None, None)
    };
    let access_field = non_empty_text(map, "accessField", "auth")?.unwrap_or_else(|| "access_token".to_string());
    let expires_field = match map.get("expiresField") {
        None => Some("expires_in".to_string()),
        Some(Value::Null) => None,
        Some(_) => non_empty_text(map, "expiresField", "auth")?,
    };
    let expires_margin_ms = whole_number(map.get("expiresMarginMs"), "auth.expiresMarginMs")?.unwrap_or(0);
    let store_as = match map.get("storeAs") {
        Some(value) => parse_store_as(value)?,
        None => {
            let mut names: Vec<&str> = Vec::new();
            let slots = client_id.iter().chain(client_secret.iter()).chain(fields.iter().filter_map(|(_, f)| match f {
                TokenField::Slot(slot) => Some(slot),
                TokenField::Literal(_) => None,
            }));
            for slot in slots {
                if !names.contains(&slot.name.as_str()) {
                    names.push(&slot.name);
                }
            }
            StoreAs {
                name: format!("token-exchange:{token_url}|{}", names.join(",")),
                scope: SecretScope::App,
            }
        }
    };
    Ok(TokenExchange {
        token_url,
        grant,
        body_kind,
        fields,
        client_auth,
        client_id,
        client_secret,
        access_field,
        expires_field,
        expires_margin_ms,
        store_as,
    })
}

/// `oauth2-client` is the client-credentials form of the token exchange.
fn parse_oauth2_client(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(map, &["scheme", "tokenUrl", "clientId", "clientSecret", "scope"], "auth")?;
    let token_url = text_field(map, "tokenUrl", "auth")?;
    if split_url(&token_url).is_none() {
        return invalid(format!("tokenUrl `{token_url}` is not an http(s) URL"));
    }
    let client_id = slot_field(map, "clientId", "auth")?;
    let fields = match map.get("scope") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(scope)) => vec![("scope".to_string(), TokenField::Literal(Value::String(scope.clone())))],
        Some(_) => return invalid("auth.scope must be a string"),
    };
    Ok(TokenExchange {
        store_as: StoreAs {
            name: format!("oauth2:{token_url}|{}", client_id.name),
            scope: SecretScope::App,
        },
        token_url,
        grant: TokenGrant::ClientCredentials,
        body_kind: TokenBodyKind::Form,
        fields,
        client_auth: Some(ClientAuth::Basic),
        client_id: Some(client_id),
        client_secret: Some(slot_field(map, "clientSecret", "auth")?),
        access_field: "access_token".to_string(),
        expires_field: Some("expires_in".to_string()),
        expires_margin_ms: 0,
    })
}

fn parse_auth(value: &Value) -> Result<HttpAuth, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth must be an object");
    };
    let scheme = text_field(map, "scheme", "auth")?;
    match scheme.as_str() {
        "bearer" => {
            closed_keys(map, &["scheme", "credential"], "auth")?;
            Ok(HttpAuth::Bearer {
                credential: slot_field(map, "credential", "auth")?,
            })
        }
        "api-key" => {
            closed_keys(map, &["scheme", "credential", "name", "in"], "auth")?;
            let location = match text_field(map, "in", "auth")?.as_str() {
                "header" => ApiKeyIn::Header,
                "query" => ApiKeyIn::Query,
                other => return invalid(format!("auth.in must be header or query, not `{other}`")),
            };
            Ok(HttpAuth::ApiKey {
                credential: slot_field(map, "credential", "auth")?,
                name: text_field(map, "name", "auth")?,
                location,
            })
        }
        "basic" => {
            closed_keys(map, &["scheme", "user", "credential"], "auth")?;
            Ok(HttpAuth::Basic {
                user: slot_field(map, "user", "auth")?,
                credential: slot_field(map, "credential", "auth")?,
            })
        }
        "oauth2-client" => Ok(HttpAuth::TokenExchange(parse_oauth2_client(map)?)),
        "token-exchange" => Ok(HttpAuth::TokenExchange(parse_token_exchange(map)?)),
        "hmac-sha256" => {
            closed_keys(
                map,
                &["scheme", "credential", "name", "signs", "encoding"],
                "auth",
            )?;
            let encoding = match text_field(map, "encoding", "auth")?.as_str() {
                "hex" => DigestEncoding::Hex,
                "base64" => DigestEncoding::Base64,
                other => return invalid(format!("auth.encoding must be hex or base64, not `{other}`")),
            };
            Ok(HttpAuth::HmacSha256 {
                credential: slot_field(map, "credential", "auth")?,
                name: text_field(map, "name", "auth")?,
                signs: text_field(map, "signs", "auth")?,
                encoding,
            })
        }
        other => invalid(format!("unknown auth scheme `{other}`")),
    }
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\r', '\n', ':'])
}

fn parse_multipart(value: &Value) -> Result<HttpBodySpec, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("multipart must be an object");
    };
    closed_keys(map, &["kind", "parts"], "multipart")?;
    let kind = match text_field(map, "kind", "multipart")?.as_str() {
        "related" => MultipartKind::Related,
        "form-data" => MultipartKind::FormData,
        other => return invalid(format!("multipart.kind must be related or form-data, not `{other}`")),
    };
    let Some(Value::Array(raw)) = map.get("parts") else {
        return invalid("multipart.parts must be a list");
    };
    if raw.is_empty() {
        return invalid("multipart.parts needs at least one part");
    }
    let mut parts = Vec::new();
    for raw_part in raw {
        let Value::Object(part) = raw_part else {
            return invalid("a multipart part must be an object");
        };
        closed_keys(part, &["headers", "text", "bytes"], "multipart part")?;
        let mut headers = Vec::new();
        match part.get("headers") {
            None => {}
            Some(Value::Object(map)) => {
                for (name, value) in map {
                    match value {
                        Value::String(v) if is_header_name(name) && !v.contains(['\r', '\n']) => {
                            headers.push((name.clone(), v.clone()))
                        }
                        _ => return invalid(format!("multipart part header `{name}` is not a valid single-line header")),
                    }
                }
            }
            Some(_) => return invalid("multipart part headers must be an object"),
        }
        let content = match (part.get("text"), part.get("bytes")) {
            (Some(Value::String(text)), None) => PartContent::Text(text.clone()),
            (None, Some(Value::String(b64))) => match codec::base64_decode(b64) {
                Ok(bytes) => PartContent::Bytes(bytes),
                Err(e) => return invalid(format!("multipart part bytes are not base64: {e}")),
            },
            _ => return invalid("a multipart part carries exactly one of text, bytes (a string)"),
        };
        parts.push(MultipartPart { headers, content });
    }
    Ok(HttpBodySpec::Multipart { kind, parts })
}

fn parse_retry(value: &Value) -> Result<HttpRetry, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("retry must be an object");
    };
    closed_keys(map, &["max", "on", "backoffMs"], "retry")?;
    let max = match whole_number(map.get("max"), "retry.max")? {
        Some(n) if (1..=u64::from(u32::MAX)).contains(&n) => n as u32,
        _ => return invalid("retry.max must be a whole number of at least 1"),
    };
    let Some(Value::Array(raw)) = map.get("on") else {
        return invalid("retry.on must be a non-empty list");
    };
    let mut on = Vec::new();
    for entry in raw {
        let rule = match entry {
            Value::String(s) if s == "5xx" => RetryOn::Server,
            Value::String(s) if s == "429" => RetryOn::TooManyRequests,
            Value::String(s) if s == "timeout" => RetryOn::Timeout,
            _ => return invalid("retry.on entries are 5xx, 429 or timeout"),
        };
        if !on.contains(&rule) {
            on.push(rule);
        }
    }
    if on.is_empty() {
        return invalid("retry.on must be a non-empty list");
    }
    let Some(backoff_ms) = whole_number(map.get("backoffMs"), "retry.backoffMs")? else {
        return invalid("retry.backoffMs is required");
    };
    Ok(HttpRetry { max, on, backoff_ms })
}

fn parse_paginate(value: &Value) -> Result<HttpPaginate, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("paginate must be an object");
    };
    closed_keys(map, &["style", "param", "next", "collect", "max"], "paginate")?;
    let style = match text_field(map, "style", "paginate")?.as_str() {
        "cursor" => PageStyle::Cursor,
        "offset" => PageStyle::Offset,
        "page" => PageStyle::Page,
        "link-header" => PageStyle::LinkHeader,
        other => return invalid(format!("unknown paginate style `{other}`")),
    };
    let param = match map.get("param") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return invalid("paginate.param must be a string"),
    };
    if style != PageStyle::LinkHeader && param.is_none() {
        return invalid("paginate.param is required unless the style is link-header");
    }
    let next = map.get("next").cloned().unwrap_or(Value::Null);
    if style != PageStyle::LinkHeader && next.is_null() {
        return invalid("paginate.next is required unless the style is link-header");
    }
    let Some(collect) = map.get("collect").cloned() else {
        return invalid("paginate.collect is required");
    };
    let max = match map.get("max") {
        Some(Value::Number(n)) if *n >= 1.0 && n.fract() == 0.0 && *n <= f64::from(u32::MAX) => *n as u32,
        _ => return invalid("paginate.max must be a whole number of at least 1"),
    };
    Ok(HttpPaginate {
        style,
        param,
        next,
        collect,
        max,
    })
}

pub fn parse_http_call(
    method: HttpMethod,
    origin: &str,
    path: &str,
    request: &Value,
) -> Result<HttpCallSpec, HttpRefusal> {
    let Some((origin_parts, _)) = parse_origin_prefix(origin) else {
        return invalid(format!("origin `{origin}` is not scheme://host[:port][/prefix]"));
    };
    let origin = origin.trim_end_matches('/');
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        return Err(HttpRefusal::OriginEscape(path.to_string()));
    }
    if path.contains('#') {
        return invalid("path must not carry a fragment");
    }
    match split_url(&format!("{origin}{path}")) {
        Some((joined, _)) if joined == origin_parts => {}
        _ => return Err(HttpRefusal::OriginEscape(path.to_string())),
    }

    let empty = ValueMap::new();
    let map = match request {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ => return invalid("request must be an object"),
    };
    closed_keys(
        map,
        &[
            "query", "headers", "json", "form", "text", "bytes", "multipart", "auth", "timeoutMs", "responseAs",
            "paginate", "stream", "retry", "formEncoding",
        ],
        "request",
    )?;

    let query = match map.get("query") {
        Some(value) => pairs_of(value, "query")?,
        None => Vec::new(),
    };
    let headers = match map.get("headers") {
        Some(Value::Object(headers)) => {
            let mut out = Vec::new();
            for (name, value) in headers {
                match value {
                    Value::String(s) => out.push((name.clone(), s.clone())),
                    _ => return invalid(format!("header `{name}` must be a string")),
                }
            }
            out
        }
        Some(_) => return invalid("headers must be an object"),
        None => Vec::new(),
    };

    let bodies: Vec<&str> = ["json", "form", "text", "bytes", "multipart"]
        .into_iter()
        .filter(|k| map.contains_key(*k))
        .collect();
    if bodies.len() > 1 {
        return invalid("a request carries at most one of json, form, text, bytes, multipart");
    }
    if !bodies.is_empty() && method == HttpMethod::Get {
        return invalid("a GET request carries no body");
    }
    let body = match bodies.first().copied() {
        None => HttpBodySpec::Empty,
        Some("json") => match serde_json::to_string(&value_to_json(&map["json"])) {
            Ok(text) => HttpBodySpec::Json(text),
            Err(e) => return invalid(format!("json body: {e}")),
        },
        Some("form") => HttpBodySpec::Form(pairs_of(&map["form"], "form")?),
        Some("text") => match &map["text"] {
            Value::String(s) => HttpBodySpec::Text(s.clone()),
            _ => return invalid("text body must be a string"),
        },
        Some("bytes") => match &map["bytes"] {
            Value::String(s) => match codec::base64_decode(s) {
                Ok(bytes) => HttpBodySpec::Bytes(bytes),
                Err(e) => return invalid(format!("bytes body is not base64: {e}")),
            },
            _ => return invalid("bytes body must be a base64 string"),
        },
        Some(_) => {
            if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                return invalid("a multipart body sets its own Content-Type");
            }
            parse_multipart(&map["multipart"])?
        }
    };
    let form_encoding = match map.get("formEncoding") {
        None => FormEncoding::Html,
        Some(_) if !matches!(body, HttpBodySpec::Form(_)) => return invalid("formEncoding applies to a form body"),
        Some(Value::String(s)) if s == "html" => FormEncoding::Html,
        Some(Value::String(s)) if s == "rfc3986" => FormEncoding::Rfc3986,
        Some(_) => return invalid("formEncoding must be html or rfc3986"),
    };
    let retry = map.get("retry").map(parse_retry).transpose()?;

    let auth = map.get("auth").map(parse_auth).transpose()?;
    let timeout_ms = match map.get("timeoutMs") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
        Some(_) => return invalid("timeoutMs must be a whole number"),
    };
    let response_as = match map.get("responseAs") {
        None => ResponseAs::Text,
        Some(Value::String(s)) if s == "text" => ResponseAs::Text,
        Some(Value::String(s)) if s == "bytes" => ResponseAs::Bytes,
        Some(_) => return invalid("responseAs must be text or bytes"),
    };
    let paginate = map.get("paginate").map(parse_paginate).transpose()?;
    let stream = match map.get("stream") {
        None => None,
        Some(Value::String(s)) if s == "sse" => Some(StreamFraming::Sse),
        Some(Value::String(s)) if s == "json-lines" => Some(StreamFraming::JsonLines),
        Some(_) => return invalid("stream must be sse or json-lines"),
    };
    if paginate.is_some() && stream.is_some() {
        return invalid("a request is paginated or streamed, not both");
    }
    if retry.is_some() && stream.is_some() {
        return invalid("a streamed request is not retried");
    }

    Ok(HttpCallSpec {
        method,
        origin: origin.to_string(),
        path: path.to_string(),
        query,
        headers,
        body,
        auth,
        timeout_ms,
        response_as,
        paginate,
        stream,
        form_encoding,
        retry,
    })
}

/// The host-secret name a token exchange keeps its token under.
pub fn token_cache_name(auth: &HttpAuth) -> Option<String> {
    match auth {
        HttpAuth::TokenExchange(ex) => Some(ex.store_as.name.clone()),
        _ => None,
    }
}

/// The slots the exchange itself needs, in ask order: the client id and secret, then the body's slots.
fn exchange_slots(ex: &TokenExchange) -> Vec<CredentialSlot> {
    let mut out: Vec<CredentialSlot> = Vec::new();
    let body = ex.fields.iter().filter_map(|(_, field)| match field {
        TokenField::Slot(slot) => Some(slot),
        TokenField::Literal(_) => None,
    });
    for slot in ex.client_id.iter().chain(ex.client_secret.iter()).chain(body) {
        if !out.iter().any(|held| held.name == slot.name) {
            out.push(slot.clone());
        }
    }
    out
}

/// Credential slots `build_request`'s lookup must answer, in ask order. For a token exchange that is the cached token.
pub fn credential_slots_needed(spec: &HttpCallSpec) -> Vec<CredentialSlot> {
    match &spec.auth {
        None => Vec::new(),
        Some(HttpAuth::Bearer { credential })
        | Some(HttpAuth::ApiKey { credential, .. })
        | Some(HttpAuth::HmacSha256 { credential, .. }) => vec![credential.clone()],
        Some(HttpAuth::Basic { user, credential }) => vec![user.clone(), credential.clone()],
        Some(HttpAuth::TokenExchange(ex)) => vec![CredentialSlot {
            name: ex.store_as.name.clone(),
            scope: ex.store_as.scope,
        }],
    }
}

/// Credential names `build_request`'s lookup must answer, in ask order.
pub fn credentials_needed(spec: &HttpCallSpec) -> Vec<String> {
    credential_slots_needed(spec).into_iter().map(|slot| slot.name).collect()
}

fn credential(
    secrets: &dyn Fn(&str) -> Option<String>,
    name: &str,
) -> Result<String, HttpRefusal> {
    secrets(name).ok_or_else(|| HttpRefusal::CredentialMissing(name.to_string()))
}

fn encode_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                codec::encode_uri_component(k),
                codec::encode_uri_component(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn with_query(url: String, query: &str) -> String {
    if query.is_empty() {
        url
    } else if url.contains('?') {
        format!("{url}&{query}")
    } else {
        format!("{url}?{query}")
    }
}

fn resolve_page_url(origin: &str, token: &str) -> Result<String, HttpRefusal> {
    let Some((expected, prefix)) = parse_origin_prefix(origin) else {
        return invalid(format!("origin `{origin}` is not scheme://host[:port][/prefix]"));
    };
    if token.starts_with('/') && !token.starts_with("//") && !token.contains('\\') {
        return Ok(format!("{}{token}", &origin[..origin.len() - prefix.len()]));
    }
    match split_url(token) {
        Some((parts, _)) if parts == expected => Ok(token.to_string()),
        _ => Err(HttpRefusal::OriginEscape(token.to_string())),
    }
}

pub fn build_request(
    spec: &HttpCallSpec,
    secrets: &dyn Fn(&str) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
) -> Result<HttpRequest, HttpRefusal> {
    build_request_with_boundary(spec, secrets, egress, page_token, None)
}

/// [`build_request`] with the multipart boundary the driver drew from the host's randomness.
pub fn build_request_with_boundary(
    spec: &HttpCallSpec,
    secrets: &dyn Fn(&str) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
    boundary: Option<&str>,
) -> Result<HttpRequest, HttpRefusal> {
    if !origin_allowed(&spec.origin, egress) {
        return Err(HttpRefusal::EgressDenied(spec.origin.clone()));
    }

    let mut query = spec.query.clone();
    let mut url = None;
    if let (Some(token), Some(paginate)) = (page_token, &spec.paginate) {
        if paginate.style == PageStyle::LinkHeader {
            url = Some(resolve_page_url(&spec.origin, token)?);
            query.clear();
        } else if let Some(param) = &paginate.param {
            query.retain(|(k, _)| k != param);
            query.push((param.clone(), token.to_string()));
        }
    }
    let mut headers = spec.headers.clone();

    match &spec.auth {
        None => {}
        Some(HttpAuth::Bearer { credential: name }) => {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", credential(secrets, &name.name)?),
            ));
        }
        Some(HttpAuth::ApiKey {
            credential: name,
            name: key,
            location,
        }) => {
            let value = credential(secrets, &name.name)?;
            match location {
                ApiKeyIn::Header => headers.push((key.clone(), value)),
                ApiKeyIn::Query => query.push((key.clone(), value)),
            }
        }
        Some(HttpAuth::Basic { user, credential: name }) => {
            let pair = format!("{}:{}", credential(secrets, &user.name)?, credential(secrets, &name.name)?);
            headers.push((
                "Authorization".to_string(),
                format!("Basic {}", codec::base64_encode(pair.as_bytes())),
            ));
        }
        Some(auth @ HttpAuth::TokenExchange(_)) => {
            let cache = token_cache_name(auth).unwrap_or_default();
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", credential(secrets, &cache)?),
            ));
        }
        Some(HttpAuth::HmacSha256 {
            credential: name,
            name: header,
            signs,
            encoding,
        }) => {
            let mac = codec::hmac_sha256(credential(secrets, &name.name)?.as_bytes(), signs.as_bytes());
            let value = match encoding {
                DigestEncoding::Hex => codec::hex(&mac),
                DigestEncoding::Base64 => codec::base64_encode(&mac),
            };
            headers.push((header.clone(), value));
        }
    }

    let url = match url {
        Some(url) => url,
        None => format!("{}{}", spec.origin, spec.path),
    };
    let url = with_query(url, &encode_query(&query));
    match (split_url(&url), parse_origin_prefix(&spec.origin)) {
        (Some((joined, _)), Some((expected, _))) if joined == expected => {}
        _ => return Err(HttpRefusal::OriginEscape(spec.path.clone())),
    }

    let (content_type, body) = match &spec.body {
        HttpBodySpec::Empty => (None, Vec::new()),
        HttpBodySpec::Json(text) => (Some("application/json".to_string()), text.clone().into_bytes()),
        HttpBodySpec::Form(pairs) => (
            Some("application/x-www-form-urlencoded".to_string()),
            match spec.form_encoding {
                FormEncoding::Html => codec::form_urlencode(pairs),
                FormEncoding::Rfc3986 => pairs
                    .iter()
                    .map(|(k, v)| format!("{}={}", codec::encode_rfc3986(k), codec::encode_rfc3986(v)))
                    .collect::<Vec<_>>()
                    .join("&"),
            }
            .into_bytes(),
        ),
        HttpBodySpec::Text(text) => (Some("text/plain; charset=utf-8".to_string()), text.clone().into_bytes()),
        HttpBodySpec::Bytes(bytes) => (Some("application/octet-stream".to_string()), bytes.clone()),
        HttpBodySpec::Multipart { kind, parts } => {
            let Some(boundary) = boundary else {
                return invalid("a multipart body needs a boundary from the driver");
            };
            (
                Some(format!("multipart/{}; boundary={boundary}", kind.as_str())),
                multipart_body(parts, boundary),
            )
        }
    };
    if let Some(content_type) = content_type {
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
            headers.push(("Content-Type".to_string(), content_type));
        }
    }

    Ok(HttpRequest {
        method: spec.method.as_str().to_string(),
        url,
        headers,
        body,
        timeout: spec.timeout_ms.map(Duration::from_millis),
    })
}

fn multipart_body(parts: &[MultipartPart], boundary: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        for (name, value) in &part.headers {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        match &part.content {
            PartContent::Text(text) => out.extend_from_slice(text.as_bytes()),
            PartContent::Bytes(bytes) => out.extend_from_slice(bytes),
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// The request that exchanges credentials for a token (`oauth2-client` is its client-credentials form).
pub fn token_exchange_request(
    auth: &HttpAuth,
    secrets: &dyn Fn(&str) -> Option<String>,
) -> Result<HttpRequest, HttpRefusal> {
    let HttpAuth::TokenExchange(ex) = auth else {
        return invalid("not a token-exchange auth");
    };
    if split_url(&ex.token_url).is_none() {
        return invalid(format!("tokenUrl `{}` is not an http(s) URL", ex.token_url));
    }
    let id = ex.client_id.as_ref().map(|slot| credential(secrets, &slot.name)).transpose()?;
    let secret = ex.client_secret.as_ref().map(|slot| credential(secrets, &slot.name)).transpose()?;
    let mut fields: Vec<(String, Value)> = Vec::new();
    if let Some(grant_type) = ex.grant.grant_type() {
        fields.push(("grant_type".to_string(), Value::String(grant_type.to_string())));
    }
    for (name, field) in &ex.fields {
        let value = match field {
            TokenField::Literal(value) => value.clone(),
            TokenField::Slot(slot) => Value::String(credential(secrets, &slot.name)?),
        };
        fields.push((name.clone(), value));
    }
    let mut headers = Vec::new();
    if let (Some(id), Some(secret)) = (id, secret) {
        match ex.client_auth {
            Some(ClientAuth::Basic) => headers.push((
                "Authorization".to_string(),
                format!("Basic {}", codec::base64_encode(format!("{id}:{secret}").as_bytes())),
            )),
            Some(ClientAuth::Post) => {
                fields.push(("client_id".to_string(), Value::String(id)));
                fields.push(("client_secret".to_string(), Value::String(secret)));
            }
            None => {}
        }
    }
    let (content_type, body) = match ex.body_kind {
        TokenBodyKind::Form => {
            let mut pairs = Vec::new();
            for (name, value) in &fields {
                if let Some(text) = scalar_text(value, name)? {
                    pairs.push((name.clone(), text));
                }
            }
            ("application/x-www-form-urlencoded", codec::form_urlencode(&pairs))
        }
        TokenBodyKind::Json => {
            let object: serde_json::Map<String, serde_json::Value> =
                fields.iter().map(|(name, value)| (name.clone(), value_to_json(value))).collect();
            match serde_json::to_string(&object) {
                Ok(text) => ("application/json", text),
                Err(e) => return invalid(format!("token exchange body: {e}")),
            }
        }
    };
    headers.push(("Content-Type".to_string(), content_type.to_string()));
    Ok(HttpRequest {
        method: "POST".to_string(),
        url: ex.token_url.clone(),
        headers,
        body: body.into_bytes(),
        timeout: None,
    })
}

/// `(token, ttl_ms)` from a token reply. The expiry is optional: without it the ttl is 0, which the host reads as
/// "do not cache" (the kernel has no clock to age an entry). The margin is taken off the lifetime; a lifetime
/// that does not outlast it is stored for 0 ms too.
pub fn parse_token_reply(
    resp: &HttpResponse,
    access_field: &str,
    expires_field: Option<&str>,
    margin_ms: u64,
) -> Result<(String, u64), String> {
    if !resp.is_success() {
        return Err(format!("token request failed with status {}", resp.status));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&resp.body).map_err(|e| format!("token reply is not JSON: {e}"))?;
    let token = json
        .get(access_field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("token reply has no {access_field}"))?;
    let ttl_ms = match expires_field.and_then(|field| json.get(field)) {
        None | Some(serde_json::Value::Null) => 0,
        Some(value) => {
            let seconds = value
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| format!("token reply has an invalid {}", expires_field.unwrap_or_default()))?;
            ((seconds * 1000.0) as u64).saturating_sub(margin_ms)
        }
    };
    Ok((token.to_string(), ttl_ms))
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    Value::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub fn reply_payload(resp: &HttpResponse, as_: ResponseAs) -> Value {
    let mut headers = ValueMap::new();
    for (name, value) in &resp.headers {
        let name = name.to_ascii_lowercase();
        let joined = match headers.get(&name) {
            Some(Value::String(prev)) => format!("{prev}, {value}"),
            _ => value.clone(),
        };
        headers.insert(name, Value::String(joined));
    }
    let body = match as_ {
        ResponseAs::Text => resp.text().into_owned(),
        ResponseAs::Bytes => codec::base64_encode(&resp.body),
    };
    object(vec![
        ("status", Value::Number(f64::from(resp.status))),
        ("headers", Value::Object(headers)),
        ("body", Value::String(body)),
    ])
}

fn failure_value(error: String, kind: &str, credential: Option<String>) -> Value {
    object(vec![
        ("error", Value::String(error)),
        ("kind", Value::String(kind.to_string())),
        ("credential", credential.map_or(Value::Null, Value::String)),
    ])
}

pub fn failure_payload(err: &TransportError) -> Value {
    match err {
        TransportError::TimedOut => failure_value("request timed out".to_string(), "timeout", None),
        TransportError::Failed(message) => failure_value(message.clone(), "failed", None),
        TransportError::Refused(message) => failure_value(message.clone(), "refused", None),
    }
}

/// A call refused before it was sent. A missing credential is `unconfigured` and names it; the rest are `refused`.
pub fn refusal_payload(r: &HttpRefusal) -> Value {
    match r {
        HttpRefusal::CredentialMissing(name) => failure_value(r.message(), "unconfigured", Some(name.clone())),
        _ => failure_value(r.message(), "refused", None),
    }
}

/// The URL of the `rel="next"` entry across the `Link` headers.
pub fn next_link(resp: &HttpResponse) -> Option<String> {
    for (name, value) in &resp.headers {
        if !name.eq_ignore_ascii_case("link") {
            continue;
        }
        for entry in split_link_entries(value) {
            let Some((target, params)) = entry.trim().split_once('>') else {
                continue;
            };
            let Some(url) = target.trim().strip_prefix('<') else {
                continue;
            };
            let is_next = params.split(';').any(|param| {
                match param.trim().split_once('=') {
                    Some((key, rel)) if key.trim().eq_ignore_ascii_case("rel") => rel
                        .trim()
                        .trim_matches('"')
                        .split_whitespace()
                        .any(|r| r.eq_ignore_ascii_case("next")),
                    _ => false,
                }
            });
            if is_next {
                return Some(url.to_string());
            }
        }
    }
    None
}

fn split_link_entries(value: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut depth = false;
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in value.char_indices() {
        match c {
            '<' if !quoted => depth = true,
            '>' if !quoted => depth = false,
            '"' => quoted = !quoted,
            ',' if !depth && !quoted => {
                entries.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    entries.push(&value[start..]);
    entries
}

fn stream_event(data: String, event: Option<String>, id: Option<String>) -> Value {
    object(vec![
        ("data", Value::String(data)),
        ("event", event.map_or(Value::Null, Value::String)),
        ("id", id.map_or(Value::Null, Value::String)),
    ])
}

fn sse_block(block: &str) -> Option<Value> {
    let mut data: Option<Vec<&str>> = None;
    let mut event = None;
    let mut id = None;
    for line in block.split('\n') {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => data.get_or_insert_with(Vec::new).push(value),
            "event" => event = Some(value.to_string()),
            "id" => id = Some(value.to_string()),
            _ => {}
        }
    }
    data.map(|lines| stream_event(lines.join("\n"), event, id))
}

fn json_line(line: &str) -> Option<Value> {
    let line = line.trim_end_matches('\r');
    if line.is_empty() {
        None
    } else {
        Some(stream_event(line.to_string(), None, None))
    }
}

/// Frames `chunk` appended to `carry`; whatever is not yet a whole frame stays in `carry`.
pub fn stream_events(framing: StreamFraming, chunk: &str, carry: &mut String) -> Vec<Value> {
    carry.push_str(chunk);
    match framing {
        StreamFraming::Sse => {
            let normalized = carry.replace("\r\n", "\n");
            let Some(end) = normalized.rfind("\n\n") else {
                *carry = normalized;
                return Vec::new();
            };
            let events = normalized[..end].split("\n\n").filter_map(sse_block).collect();
            *carry = normalized[end + 2..].to_string();
            events
        }
        StreamFraming::JsonLines => {
            let Some(end) = carry.rfind('\n') else {
                return Vec::new();
            };
            let events = carry[..end].split('\n').filter_map(json_line).collect();
            *carry = carry[end + 1..].to_string();
            events
        }
    }
}

/// Frames `chunk` appended to `carry` and stamps each event with the call's `key` (`null` when unkeyed): the one
/// place a stream event gets it, for the driver and for a host holding only an `HttpStream` ask.
pub fn frame_stream(framing: StreamFraming, key: Option<&str>, chunk: &str, carry: &mut String) -> Vec<Value> {
    let key = key.map(str::to_string);
    stream_events(framing, chunk, carry)
        .into_iter()
        .map(|e| with_key(e, &key))
        .collect()
}

/// The keyed events the unterminated tail in `carry` makes at end of body.
pub fn frame_stream_finish(framing: StreamFraming, key: Option<&str>, carry: &mut String) -> Vec<Value> {
    let key = key.map(str::to_string);
    stream_finish(framing, carry)
        .into_iter()
        .map(|e| with_key(e, &key))
        .collect()
}

/// The final unterminated frame at EOF.
pub fn stream_finish(framing: StreamFraming, carry: &mut String) -> Vec<Value> {
    let rest = std::mem::take(carry);
    match framing {
        StreamFraming::Sse => sse_block(&rest.replace("\r\n", "\n")).into_iter().collect(),
        StreamFraming::JsonLines => json_line(&rest).into_iter().collect(),
    }
}

fn v4_public(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)))
}

fn v6_public(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return v4_public(mapped);
    }
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || first & 0xfe00 == 0xfc00
        || first & 0xffc0 == 0xfe80)
}

/// Whether a resolved address is a public destination; an IPv4-mapped IPv6 address is classified by its IPv4.
pub fn is_public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_public(v4),
        IpAddr::V6(v6) => v6_public(v6),
    }
}

// --- the step machine -------------------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
pub enum HttpAsk {
    Secret {
        name: String,
        scope: SecretScope,
    },
    SecretSet {
        name: String,
        value: String,
        ttl_ms: u64,
        scope: SecretScope,
    },
    /// `delay_ms` is how long the host waits before sending (a retry's backoff); `None` sends now.
    Http {
        request: HttpRequest,
        delay_ms: Option<u64>,
    },
    HttpStream {
        request: HttpRequest,
        framing: StreamFraming,
    },
    /// `bytes` of host CSPRNG output (journaled by the kernel, so a replay is deterministic).
    Random { bytes: u32 },
}

#[derive(Clone, PartialEq)]
pub enum HttpAnswer {
    Secret(Result<Option<String>, String>),
    SecretSet(Result<(), String>),
    Http(Result<HttpResponse, TransportError>),
    Random(Result<Vec<u8>, String>),
}

impl std::fmt::Debug for HttpAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret { name, scope } => f.debug_struct("Secret").field("name", name).field("scope", scope).finish(),
            Self::SecretSet { name, ttl_ms, scope, .. } => f
                .debug_struct("SecretSet")
                .field("name", name)
                .field("value", &"<redacted>")
                .field("ttl_ms", ttl_ms)
                .field("scope", scope)
                .finish(),
            Self::Http { request, delay_ms } => f
                .debug_struct("Http")
                .field("request", request)
                .field("delay_ms", delay_ms)
                .finish(),
            Self::HttpStream { request, framing } => f
                .debug_struct("HttpStream")
                .field("request", request)
                .field("framing", framing)
                .finish(),
            Self::Random { bytes } => f.debug_struct("Random").field("bytes", bytes).finish(),
        }
    }
}

impl std::fmt::Debug for HttpAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret(Ok(Some(_))) => f.write_str("Secret(Ok(Some(<redacted>)))"),
            Self::Secret(other) => f.debug_tuple("Secret").field(other).finish(),
            Self::SecretSet(r) => f.debug_tuple("SecretSet").field(r).finish(),
            Self::Http(r) => f.debug_tuple("Http").field(r).finish(),
            Self::Random(Ok(_)) => f.write_str("Random(Ok(<redacted>))"),
            Self::Random(Err(message)) => f.debug_tuple("Random").field(&Err::<(), _>(message)).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HttpStep {
    Ask(HttpAsk),
    /// `Ok` = success payload, `Err` = failure payload (both carry `key`).
    Done(Result<Value, Value>),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum HttpAskWire {
    Secret {
        name: String,
        #[serde(default, skip_serializing_if = "SecretScope::is_app")]
        scope: SecretScope,
    },
    #[serde(rename_all = "camelCase")]
    SecretSet {
        name: String,
        value: String,
        ttl_ms: u64,
        #[serde(default, skip_serializing_if = "SecretScope::is_app")]
        scope: SecretScope,
    },
    #[serde(rename_all = "camelCase")]
    Http {
        request: HttpRequestWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay_ms: Option<u64>,
    },
    HttpStream { request: HttpRequestWire, framing: StreamFraming },
    Random { bytes: u32 },
}

impl From<&HttpAsk> for HttpAskWire {
    fn from(ask: &HttpAsk) -> Self {
        match ask {
            HttpAsk::Secret { name, scope } => Self::Secret {
                name: name.clone(),
                scope: *scope,
            },
            HttpAsk::SecretSet {
                name,
                value,
                ttl_ms,
                scope,
            } => Self::SecretSet {
                name: name.clone(),
                value: value.clone(),
                ttl_ms: *ttl_ms,
                scope: *scope,
            },
            HttpAsk::Http { request, delay_ms } => Self::Http {
                request: request.into(),
                delay_ms: *delay_ms,
            },
            HttpAsk::HttpStream { request, framing } => Self::HttpStream {
                request: request.into(),
                framing: *framing,
            },
            HttpAsk::Random { bytes } => Self::Random { bytes: *bytes },
        }
    }
}

impl From<HttpAskWire> for HttpAsk {
    fn from(wire: HttpAskWire) -> Self {
        match wire {
            HttpAskWire::Secret { name, scope } => Self::Secret { name, scope },
            HttpAskWire::SecretSet {
                name,
                value,
                ttl_ms,
                scope,
            } => Self::SecretSet {
                name,
                value,
                ttl_ms,
                scope,
            },
            HttpAskWire::Http { request, delay_ms } => Self::Http {
                request: request.into(),
                delay_ms,
            },
            HttpAskWire::HttpStream { request, framing } => Self::HttpStream {
                request: request.into(),
                framing,
            },
            HttpAskWire::Random { bytes } => Self::Random { bytes },
        }
    }
}

impl Serialize for HttpAsk {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        HttpAskWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HttpAsk {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        HttpAskWire::deserialize(deserializer).map(Self::from)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum HttpAnswerWire {
    Secret { result: Settled<Option<String>, String> },
    SecretSet { result: Settled<(), String> },
    Http { result: Settled<HttpResponseWire, HttpFailureWire> },
    Random { result: Settled<String, String> },
}

impl From<&HttpAnswer> for HttpAnswerWire {
    fn from(answer: &HttpAnswer) -> Self {
        match answer {
            HttpAnswer::Secret(r) => Self::Secret { result: r.clone().into() },
            HttpAnswer::SecretSet(r) => Self::SecretSet { result: r.clone().into() },
            HttpAnswer::Http(r) => Self::Http {
                result: r
                    .as_ref()
                    .map(HttpResponseWire::from)
                    .map_err(HttpFailureWire::from)
                    .into(),
            },
            HttpAnswer::Random(r) => Self::Random {
                result: r.as_ref().map(|bytes| codec::base64_encode(bytes)).map_err(Clone::clone).into(),
            },
        }
    }
}

impl TryFrom<HttpAnswerWire> for HttpAnswer {
    type Error = String;

    fn try_from(wire: HttpAnswerWire) -> Result<Self, String> {
        Ok(match wire {
            HttpAnswerWire::Secret { result } => Self::Secret(result.into()),
            HttpAnswerWire::SecretSet { result } => Self::SecretSet(result.into()),
            HttpAnswerWire::Http { result } => {
                let result: Result<HttpResponseWire, HttpFailureWire> = result.into();
                Self::Http(result.map(HttpResponse::from).map_err(TransportError::from))
            }
            HttpAnswerWire::Random { result } => {
                let result: Result<String, String> = result.into();
                Self::Random(match result {
                    Ok(text) => Ok(codec::base64_decode(&text).map_err(|e| format!("random bytes are not base64: {e}"))?),
                    Err(message) => Err(message),
                })
            }
        })
    }
}

impl Serialize for HttpAnswer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        HttpAnswerWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HttpAnswer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        HttpAnswerWire::deserialize(deserializer)
            .and_then(|wire| Self::try_from(wire).map_err(serde::de::Error::custom))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum HttpStepWire {
    Ask { ask: HttpAsk },
    Done { result: Settled<Value, Value> },
}

impl Serialize for HttpStep {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Ask(ask) => HttpStepWire::Ask { ask: ask.clone() },
            Self::Done(result) => HttpStepWire::Done {
                result: result.clone().into(),
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HttpStep {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match HttpStepWire::deserialize(deserializer)? {
            HttpStepWire::Ask { ask } => Self::Ask(ask),
            HttpStepWire::Done { result } => Self::Done(result.into()),
        })
    }
}

/// The credential values a call has been answered with. They live in memory only: never serialized with the
/// driver, never printed (`Debug` lists names). A driver revived without them asks for the ones it needs again.
#[derive(Clone, PartialEq, Default)]
pub struct HeldSecrets(Vec<(String, String)>);

impl std::fmt::Debug for HeldSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.iter().map(|(name, _)| name)).finish()
    }
}

impl HeldSecrets {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn hold(&mut self, name: String, value: String) {
        self.0.retain(|(n, _)| n != &name);
        self.0.push((name, value));
    }

    fn forget(&mut self, name: &str) {
        self.0.retain(|(n, _)| n != name);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum Phase {
    Credentials { pending: Vec<CredentialSlot>, awaiting: Option<CredentialSlot> },
    GrantSecrets { pending: Vec<CredentialSlot>, awaiting: Option<CredentialSlot> },
    GrantReply,
    GrantStored,
    Boundary,
    Reply,
    Finished,
}

/// One `http` call as a pure step machine: `start` yields the first ask, `resume` feeds each answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpCallDriver {
    spec: HttpCallSpec,
    egress: Vec<String>,
    key: Option<String>,
    phase: Phase,
    #[serde(skip)]
    secrets: HeldSecrets,
    /// Where an embedding host (wasm) keeps this call's [`HeldSecrets`] between steps; opaque to the driver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault: Option<u64>,
    oauth_miss: bool,
    granted: bool,
    refreshed: bool,
    pages: u32,
    items: Vec<Value>,
    page_token: Option<String>,
    #[serde(default)]
    boundary: Option<String>,
    /// Retries spent on the request in flight; a new page starts again at 0.
    #[serde(default)]
    retries: u32,
}

fn with_key(mut payload: Value, key: &Option<String>) -> Value {
    if let Value::Object(map) = &mut payload {
        map.insert("key".to_string(), key.clone().map_or(Value::Null, Value::String));
    }
    payload
}

const BOUNDARY_BYTES: u32 = 16;

impl HttpCallDriver {
    pub fn start(spec: HttpCallSpec, egress: Vec<String>, key: Option<String>) -> (HttpCallDriver, HttpStep) {
        let pending: Vec<CredentialSlot> = credential_slots_needed(&spec).into_iter().rev().collect();
        let mut driver = HttpCallDriver {
            spec,
            egress,
            key,
            phase: Phase::Credentials {
                pending,
                awaiting: None,
            },
            secrets: HeldSecrets::default(),
            vault: None,
            oauth_miss: false,
            granted: false,
            refreshed: false,
            pages: 0,
            items: Vec::new(),
            page_token: None,
            boundary: None,
            retries: 0,
        };
        let step = if origin_allowed(&driver.spec.origin, &driver.egress) {
            driver.advance()
        } else {
            let origin = driver.spec.origin.clone();
            driver.refuse(HttpRefusal::EgressDenied(origin))
        };
        (driver, step)
    }

    /// The finished step of a call refused before it could start (it did not parse): the refusal's failure payload
    /// with the call's `key`.
    pub fn refused(refusal: &HttpRefusal, key: Option<&str>) -> HttpStep {
        HttpStep::Done(Err(with_key(refusal_payload(refusal), &key.map(str::to_string))))
    }

    /// Hands the held credential values to an embedding host that keeps them outside the serialized driver.
    pub fn take_secrets(&mut self) -> HeldSecrets {
        std::mem::take(&mut self.secrets)
    }

    pub fn restore_secrets(&mut self, secrets: HeldSecrets) {
        self.secrets = secrets;
    }

    pub fn vault(&self) -> Option<u64> {
        self.vault
    }

    pub fn set_vault(&mut self, vault: Option<u64>) {
        self.vault = vault;
    }

    /// Frames a stream chunk; each event carries the call's `key`.
    pub fn stream_events(&self, chunk: &str, carry: &mut String) -> Vec<Value> {
        match self.spec.stream {
            Some(framing) => frame_stream(framing, self.key.as_deref(), chunk, carry),
            None => Vec::new(),
        }
    }

    pub fn stream_finish(&self, carry: &mut String) -> Vec<Value> {
        match self.spec.stream {
            Some(framing) => frame_stream_finish(framing, self.key.as_deref(), carry),
            None => Vec::new(),
        }
    }

    pub fn resume(&mut self, answer: HttpAnswer) -> HttpStep {
        match (self.phase.clone(), answer) {
            (Phase::Credentials { pending, awaiting }, HttpAnswer::Secret(result)) => {
                let Some(slot) = awaiting else {
                    return self.unexpected();
                };
                match result {
                    Ok(Some(value)) => self.secrets.hold(slot.name, value),
                    Ok(None) if self.is_cache_name(&slot.name) && !self.granted => self.oauth_miss = true,
                    Ok(None) => return self.refuse(HttpRefusal::CredentialMissing(slot.name)),
                    Err(message) => return self.fail(TransportError::Failed(message)),
                }
                self.phase = Phase::Credentials { pending, awaiting: None };
                self.advance()
            }
            (Phase::GrantSecrets { pending, awaiting }, HttpAnswer::Secret(result)) => {
                let Some(slot) = awaiting else {
                    return self.unexpected();
                };
                match result {
                    Ok(Some(value)) => self.secrets.hold(slot.name, value),
                    Ok(None) => return self.refuse(HttpRefusal::CredentialMissing(slot.name)),
                    Err(message) => return self.fail(TransportError::Failed(message)),
                }
                self.phase = Phase::GrantSecrets { pending, awaiting: None };
                self.advance()
            }
            (Phase::GrantReply, HttpAnswer::Http(Err(e))) => self.fail(e),
            (Phase::GrantReply, HttpAnswer::Http(Ok(resp))) => self.on_grant_reply(resp),
            (Phase::GrantStored, HttpAnswer::SecretSet(Ok(()))) => self.send_page(),
            (Phase::GrantStored, HttpAnswer::SecretSet(Err(message))) => {
                self.fail(TransportError::Failed(message))
            }
            (Phase::Boundary, HttpAnswer::Random(Ok(bytes))) if bytes.len() == BOUNDARY_BYTES as usize => {
                self.boundary = Some(format!("almadar-{}", codec::hex(&bytes)));
                self.send_page()
            }
            (Phase::Boundary, HttpAnswer::Random(Ok(bytes))) => self.fail(TransportError::Failed(format!(
                "the host answered {} random bytes, asked for {BOUNDARY_BYTES}",
                bytes.len()
            ))),
            (Phase::Boundary, HttpAnswer::Random(Err(message))) => self.fail(TransportError::Failed(message)),
            (Phase::Reply, HttpAnswer::Http(Err(e))) => self.on_transport_error(e),
            (Phase::Reply, HttpAnswer::Http(Ok(resp))) => self.on_reply(resp),
            _ => self.unexpected(),
        }
    }

    fn exchange(&self) -> Option<&TokenExchange> {
        match &self.spec.auth {
            Some(HttpAuth::TokenExchange(ex)) => Some(ex),
            _ => None,
        }
    }

    fn cache_name(&self) -> Option<String> {
        self.spec.auth.as_ref().and_then(token_cache_name)
    }

    fn is_cache_name(&self, name: &str) -> bool {
        self.cache_name().as_deref() == Some(name)
    }

    fn unexpected(&mut self) -> HttpStep {
        self.fail(TransportError::Failed("unexpected answer for the http call".to_string()))
    }

    fn done(&mut self, result: Result<Value, Value>) -> HttpStep {
        self.phase = Phase::Finished;
        self.secrets = HeldSecrets::default();
        HttpStep::Done(result.map(|v| with_key(v, &self.key)).map_err(|v| with_key(v, &self.key)))
    }

    fn fail(&mut self, err: TransportError) -> HttpStep {
        self.done(Err(failure_payload(&err)))
    }

    fn refuse(&mut self, refusal: HttpRefusal) -> HttpStep {
        self.done(Err(refusal_payload(&refusal)))
    }

    fn advance(&mut self) -> HttpStep {
        match self.phase.clone() {
            Phase::Credentials { mut pending, .. } => match pending.pop() {
                Some(slot) => {
                    self.phase = Phase::Credentials {
                        pending,
                        awaiting: Some(slot.clone()),
                    };
                    HttpStep::Ask(HttpAsk::Secret {
                        name: slot.name,
                        scope: slot.scope,
                    })
                }
                None if self.oauth_miss => self.begin_grant(),
                None => self.send_page(),
            },
            Phase::GrantSecrets { mut pending, .. } => match pending.pop() {
                Some(slot) => {
                    self.phase = Phase::GrantSecrets {
                        pending,
                        awaiting: Some(slot.clone()),
                    };
                    HttpStep::Ask(HttpAsk::Secret {
                        name: slot.name,
                        scope: slot.scope,
                    })
                }
                None => self.send_grant(),
            },
            _ => self.unexpected(),
        }
    }

    fn missing_exchange_slots(&self) -> Vec<CredentialSlot> {
        let Some(ex) = self.exchange() else {
            return Vec::new();
        };
        let mut pending: Vec<CredentialSlot> = exchange_slots(ex)
            .into_iter()
            .filter(|slot| self.secrets.get(&slot.name).is_none())
            .collect();
        pending.reverse();
        pending
    }

    fn begin_grant(&mut self) -> HttpStep {
        if self.exchange().is_none() {
            return self.unexpected();
        }
        let pending = self.missing_exchange_slots();
        self.phase = Phase::GrantSecrets { pending, awaiting: None };
        self.advance()
    }

    fn lookup(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |name| self.secrets.get(name).map(str::to_string)
    }

    fn send_grant(&mut self) -> HttpStep {
        let Some(auth) = self.spec.auth.clone() else {
            return self.unexpected();
        };
        let pending = self.missing_exchange_slots();
        if !pending.is_empty() {
            self.phase = Phase::GrantSecrets { pending, awaiting: None };
            return self.advance();
        }
        let built = token_exchange_request(&auth, &self.lookup());
        let request = match built {
            Ok(request) => request,
            Err(refusal) => return self.refuse(refusal),
        };
        if !origin_allowed(&request.url, &self.egress) {
            let url = request.url;
            return self.refuse(HttpRefusal::EgressDenied(url));
        }
        self.phase = Phase::GrantReply;
        HttpStep::Ask(HttpAsk::Http {
            request,
            delay_ms: None,
        })
    }

    fn on_grant_reply(&mut self, resp: HttpResponse) -> HttpStep {
        let Some((access_field, expires_field, margin, store)) = self.exchange().map(|ex| {
            (
                ex.access_field.clone(),
                ex.expires_field.clone(),
                ex.expires_margin_ms,
                ex.store_as.clone(),
            )
        }) else {
            return self.unexpected();
        };
        match parse_token_reply(&resp, &access_field, expires_field.as_deref(), margin) {
            Err(message) => self.fail(TransportError::Failed(format!("token exchange: {message}"))),
            Ok((token, ttl_ms)) => {
                self.secrets.hold(store.name.clone(), token.clone());
                self.granted = true;
                self.oauth_miss = false;
                self.phase = Phase::GrantStored;
                HttpStep::Ask(HttpAsk::SecretSet {
                    name: store.name,
                    value: token,
                    ttl_ms,
                    scope: store.scope,
                })
            }
        }
    }

    fn send_page(&mut self) -> HttpStep {
        self.send_request(None)
    }

    fn send_request(&mut self, delay_ms: Option<u64>) -> HttpStep {
        let pending: Vec<CredentialSlot> = credential_slots_needed(&self.spec)
            .into_iter()
            .rev()
            .filter(|slot| self.secrets.get(&slot.name).is_none())
            .collect();
        if !pending.is_empty() {
            self.phase = Phase::Credentials { pending, awaiting: None };
            return self.advance();
        }
        if matches!(self.spec.body, HttpBodySpec::Multipart { .. }) && self.boundary.is_none() {
            self.phase = Phase::Boundary;
            return HttpStep::Ask(HttpAsk::Random { bytes: BOUNDARY_BYTES });
        }
        let built = build_request_with_boundary(
            &self.spec,
            &self.lookup(),
            &self.egress,
            self.page_token.as_deref(),
            self.boundary.as_deref(),
        );
        let request = match built {
            Ok(request) => request,
            Err(refusal) => return self.refuse(refusal),
        };
        self.phase = Phase::Reply;
        match self.spec.stream {
            Some(framing) => HttpStep::Ask(HttpAsk::HttpStream { request, framing }),
            None => HttpStep::Ask(HttpAsk::Http { request, delay_ms }),
        }
    }

    /// The backoff before the next attempt if `matched` is a retryable outcome with attempts left.
    fn next_retry_delay(&mut self, matches_rule: impl Fn(RetryOn) -> bool) -> Option<u64> {
        let retry = self.spec.retry.as_ref()?;
        if self.retries >= retry.max || !retry.on.iter().copied().any(matches_rule) {
            return None;
        }
        let delay = retry
            .backoff_ms
            .saturating_mul(1u64 << self.retries.min(63));
        self.retries += 1;
        Some(delay)
    }

    fn on_transport_error(&mut self, err: TransportError) -> HttpStep {
        if err == TransportError::TimedOut {
            if let Some(delay) = self.next_retry_delay(|rule| rule == RetryOn::Timeout) {
                return self.send_request(Some(delay));
            }
        }
        self.fail(err)
    }

    fn on_reply(&mut self, resp: HttpResponse) -> HttpStep {
        let status = resp.status;
        let retry_rule = |rule: RetryOn| match rule {
            RetryOn::Server => (500..600).contains(&status),
            RetryOn::TooManyRequests => status == 429,
            RetryOn::Timeout => false,
        };
        if let Some(delay) = self.next_retry_delay(retry_rule) {
            return self.send_request(Some(delay));
        }
        if resp.status == 401 && !self.refreshed {
            if let Some(name) = self.cache_name() {
                self.refreshed = true;
                self.secrets.forget(&name);
                return self.begin_grant();
            }
        }
        let payload = reply_payload(&resp, self.spec.response_as);
        let Some(paginate) = self.spec.paginate.clone() else {
            return self.done(Ok(payload));
        };
        self.pages += 1;
        if !resp.is_success() {
            return self.finish_pages(payload);
        }
        let mut ctx = BindingContext::new();
        ctx.set_local("response", payload.clone());
        let evaluator = Evaluator::new();
        let collected = match evaluator.evaluate(&SExpression::from(paginate.collect.clone()), &ctx) {
            Ok(Value::Array(items)) => items,
            Ok(_) => return self.fail(TransportError::Failed("paginate.collect did not produce a list".to_string())),
            Err(e) => return self.fail(TransportError::Failed(format!("paginate.collect: {e}"))),
        };
        self.items.extend(collected);
        if self.pages >= paginate.max {
            return self.finish_pages(payload);
        }
        let token = if paginate.style == PageStyle::LinkHeader {
            next_link(&resp)
        } else {
            match evaluator.evaluate(&SExpression::from(paginate.next.clone()), &ctx) {
                Ok(Value::Null) => None,
                Ok(Value::String(s)) => Some(s),
                Ok(Value::Number(n)) => Some(js_number_string(n)),
                Ok(_) => {
                    return self.fail(TransportError::Failed(
                        "paginate.next did not produce a string, number or null".to_string(),
                    ))
                }
                Err(e) => return self.fail(TransportError::Failed(format!("paginate.next: {e}"))),
            }
        };
        match token {
            None => self.finish_pages(payload),
            Some(token) => {
                self.page_token = Some(token);
                self.retries = 0;
                self.send_page()
            }
        }
    }

    fn finish_pages(&mut self, last: Value) -> HttpStep {
        let paged = object(vec![
            ("pages", Value::Number(f64::from(self.pages))),
            ("items", Value::Array(std::mem::take(&mut self.items))),
            ("last", last),
        ]);
        self.done(Ok(paged))
    }
}
