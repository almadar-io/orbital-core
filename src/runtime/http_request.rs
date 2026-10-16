//! The `http` effect's request builder and its sans-I/O step machine (`docs/Almadar_Embedded_Kernel.md`,
//! "The `http` effect", H.3, H.3b, H.3c). Pure: no clock, env, randomness or I/O. The kernel, the JS interpreter
//! (through wasm) and the generated TS shell only answer the driver's asks.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::codec;
use crate::embed::wire::{HttpFailureWire, HttpRequestWire, HttpResponseWire, Settled};
use crate::evaluator::{value_to_json, BindingContext, Evaluator, Value, ValueMap};
use crate::ir::OirType;
use crate::oauth::client::{OAuthClientDecl, OAuthEndpointKind, OAuthInputs, OAuthMode, OAuthOp, OAuthServerDecl};
use crate::oauth::ask::{OAuthAsk, OAuthFailureReason, OAuthProvider, OAuthReply};
use crate::runtime::oauth_flow::{FlowAnswer, FlowAsk, FlowFailure, FlowStep, OAuthEnv, OAuthFlow, OAuthHeld};
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
    WebPush(WebPushBody),
    Multipart {
        kind: MultipartKind,
        parts: Vec<MultipartPart>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebPushBody {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub payload: String,
    pub ttl: u64,
    pub urgency: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormEncoding {
    #[default]
    Html,
    Rfc3986,
}

/// How a query string escapes its pairs: `uri` is `encodeURIComponent`, `rfc3986` also escapes `!'()*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryEncoding {
    #[default]
    Uri,
    Rfc3986,
}

/// What a token-exchange call does with a 401 resource reply: hand it back (`reply`) or refresh once and retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnUnauthorized {
    #[default]
    Reply,
    Refresh,
}

/// One `retry.on` entry: a class tag, or one exact HTTP status code (100-599).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOn {
    Informational,
    RequestTimeout,
    Server,
    TooManyRequests,
    Timeout,
    Failed,
    Status(u16),
}

impl RetryOn {
    const ENTRIES: &'static str = "retry.on entries are 1xx, 408, 5xx, 429, timeout, failed or an integer status code from 100 through 599";

    fn status(code: u64) -> Option<Self> {
        (100..=599).contains(&code).then_some(Self::Status(code as u16))
    }

    fn tag(self) -> Option<&'static str> {
        match self {
            Self::Informational => Some("1xx"),
            Self::RequestTimeout => Some("408"),
            Self::Server => Some("5xx"),
            Self::TooManyRequests => Some("429"),
            Self::Timeout => Some("timeout"),
            Self::Failed => Some("failed"),
            Self::Status(_) => None,
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        [
            Self::Informational,
            Self::RequestTimeout,
            Self::Server,
            Self::TooManyRequests,
            Self::Timeout,
            Self::Failed,
        ]
        .into_iter()
        .find(|rule| rule.tag() == Some(tag))
    }

    fn matches(self, outcome: RetryOutcome) -> bool {
        match (self, outcome) {
            (Self::Informational, RetryOutcome::Status(status)) => (100..200).contains(&status),
            (Self::RequestTimeout, RetryOutcome::Status(status)) => status == 408,
            (Self::Server, RetryOutcome::Status(status)) => (500..600).contains(&status),
            (Self::TooManyRequests, RetryOutcome::Status(status)) => status == 429,
            (Self::Status(code), RetryOutcome::Status(status)) => status == code,
            (Self::Timeout, RetryOutcome::Timeout) | (Self::Failed, RetryOutcome::Failed) => true,
            _ => false,
        }
    }
}

impl Serialize for RetryOn {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Status(code) => serializer.serialize_u16(*code),
            other => serializer.serialize_str(other.tag().expect("class entries have a tag")),
        }
    }
}

impl<'de> Deserialize<'de> for RetryOn {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::String(tag) => {
                Self::from_tag(&tag).ok_or_else(|| serde::de::Error::custom(Self::ENTRIES))
            }
            serde_json::Value::Number(n) => match n.as_u64().and_then(Self::status) {
                Some(rule) => Ok(rule),
                None => Err(serde::de::Error::custom(Self::ENTRIES)),
            },
            _ => Err(serde::de::Error::custom(Self::ENTRIES)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RetryDelay {
    Backoff { #[serde(rename = "backoffMs")] backoff_ms: u64 },
    Schedule { #[serde(rename = "scheduleMs")] schedule_ms: Vec<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRetry {
    pub max: u32,
    pub on: Vec<RetryOn>,
    #[serde(flatten)]
    pub delay: RetryDelay,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_response_max: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum RetryOperand {
    Literal(Value),
    Typed(OirType),
    Items(Vec<RetryOperand>),
    Unresolved,
}

#[derive(Clone, Copy)]
enum RetryOutcome {
    Status(u16),
    Timeout,
    Failed,
}

impl HttpRetry {
    pub const KEYS: &'static [&'static str] = &["max", "on", "backoffMs", "scheduleMs", "noResponseMax"];

    fn next_delay(&self, attempt: &mut u32, outcome: RetryOutcome) -> Option<u64> {
        if *attempt >= self.max
            || (matches!(outcome, RetryOutcome::Timeout | RetryOutcome::Failed)
                && self.no_response_max.is_some_and(|max| *attempt >= max))
            || !self.on.iter().any(|rule| rule.matches(outcome))
        {
            return None;
        }
        let delay = match &self.delay {
            RetryDelay::Schedule { schedule_ms } => *schedule_ms.get(*attempt as usize)?,
            RetryDelay::Backoff { backoff_ms } => match 1u64.checked_shl(*attempt) {
                Some(multiplier) => backoff_ms.saturating_mul(multiplier),
                None if *backoff_ms == 0 => 0,
                None => u64::MAX,
            },
        };
        *attempt += 1;
        Some(delay)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRequest {
    pub headers: Vec<(String, String)>,
    pub timeout_ms: Option<u64>,
    pub retry: Option<HttpRetry>,
}

impl GrantRequest {
    pub const KEYS: &'static [&'static str] = &["headers", "timeoutMs", "retry"];

    pub fn is_driver_owned_header(name: &str) -> bool {
        name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("content-type")
    }
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
    JwtAssertion {
        key: CredentialSlot,
        alg: codec::jwt::JwtAlg,
        claims: Value,
    },
}

/// Where the exchanged token is kept as a host secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreAs {
    pub name: String,
    #[serde(default, skip_serializing_if = "SecretScope::is_app")]
    pub scope: SecretScope,
}

impl StoreAs {
    fn slot(&self) -> CredentialSlot {
        CredentialSlot { name: self.name.clone(), scope: self.scope }
    }
}

/// A refresh grant whose reply carries a replacement refresh token: after a 2xx reply, `field` of the reply
/// replaces the host-held `credential` (a durable write) before the access token is cached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRotate {
    pub field: String,
    pub credential: CredentialSlot,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_request: Option<GrantRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<TokenRotate>,
    #[serde(default, skip_serializing_if = "is_reply")]
    pub on_unauthorized: OnUnauthorized,
}

fn is_reply(value: &OnUnauthorized) -> bool {
    *value == OnUnauthorized::Reply
}

fn parse_on_unauthorized(map: &ValueMap) -> Result<OnUnauthorized, HttpRefusal> {
    match map.get("onUnauthorized") {
        None => Ok(OnUnauthorized::Reply),
        Some(Value::String(s)) if s == "reply" => Ok(OnUnauthorized::Reply),
        Some(Value::String(s)) if s == "refresh" => Ok(OnUnauthorized::Refresh),
        Some(_) => invalid("auth.onUnauthorized must be reply or refresh"),
    }
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
    #[serde(rename = "aws-sigv4")]
    AwsSigV4 {
        access_key_id: CredentialSlot,
        secret_access_key: CredentialSlot,
        session_token: Option<CredentialSlot>,
        region: String,
        service: String,
    },
    #[serde(rename = "vapid")]
    Vapid {
        private_key: CredentialSlot,
        public_key: String,
        subject: String,
    },
    #[serde(rename = "hmac-sha256")]
    HmacSha256 {
        credential: CredentialSlot,
        name: String,
        signs: String,
        encoding: DigestEncoding,
    },
    /// One OAuth operation the driver runs end to end (`oauth_flow`); the host only persists.
    #[serde(rename = "oauth-client")]
    OAuthClient {
        op: OAuthOp,
        /// The client the call runs as. With `clients`, the first of them (only its tag is used before a grant is taken).
        client: OAuthClientDecl,
        /// `op: code` only: the declared clients, one per provider tag; the pending grant's provider selects one.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        clients: Vec<OAuthClientDecl>,
        inputs: OAuthInputs,
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
    pub query_encoding: QueryEncoding,
    #[serde(default)]
    pub retry: Option<HttpRetry>,
    #[serde(default)]
    pub presign: Option<u64>,
    #[serde(default)]
    pub delivery: HttpDelivery,
}

/// How the built request reaches the network: an ordinary exchange, or the host's fire-and-forget beacon port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpDelivery {
    #[default]
    Fetch,
    Beacon,
}

/// How a host took a beacon: the browser's queue accepted it, one keepalive fetch answered `status`, or the host
/// has no beacon port at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via")]
pub enum BeaconOutcome {
    #[serde(rename = "queue")]
    Queued,
    #[serde(rename = "fetch")]
    Fetched { status: u16 },
    #[serde(rename = "unavailable")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpCallInput {
    pub method: HttpMethod,
    pub origin: String,
    pub path: String,
    pub request: Value,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub now_ms: Option<f64>,
    /// `beacon` reads `origin` as the absolute URL and `request` as `{ text }`; `path` is unused.
    #[serde(default)]
    pub delivery: HttpDelivery,
    /// The signed-in user an `oauth-client` call acts for; set by the host's kernel, never by the program.
    #[serde(default)]
    pub principal: Option<String>,
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

#[derive(Debug)]
pub struct OriginParts {
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
pub fn split_url(url: &str) -> Option<(OriginParts, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let (authority, remainder) = rest.split_at(end);
    if authority.is_empty() || authority.contains(['@', ' ']) {
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
        match authority.split_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, default_port),
        }
    };
    if host.is_empty() || host == "[]" {
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

pub fn parse_origin(origin: &str) -> Option<OriginParts> {
    match split_url(origin)? {
        (parts, "") => Some(parts),
        _ => None,
    }
}

/// `scheme://host[:port][/prefix]`: the origin and its path prefix (empty, or `/a/b` with no trailing slash).
pub fn parse_origin_prefix(origin: &str) -> Option<(OriginParts, String)> {
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

pub fn parse_credential_status_slot(selection: &Value) -> Result<CredentialSlot, HttpRefusal> {
    let message = "credential-status: expected { credential, scope: app | user }";
    match selection {
        Value::String(name) => Ok(CredentialSlot {
            name: name.clone(),
            scope: SecretScope::User,
        }),
        Value::Object(slot) if slot.contains_key("scope") => {
            parse_slot_object(slot, "credential-status")
                .map_err(|_| HttpRefusal::Invalid(message.to_string()))
        }
        _ => invalid(message),
    }
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

fn parse_token_rotate(value: &Value, grant: TokenGrant) -> Result<TokenRotate, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth.rotate must be { field, credential }");
    };
    closed_keys(map, &["field", "credential"], "auth.rotate")?;
    if grant != TokenGrant::RefreshToken {
        return invalid("auth.rotate needs auth.grant: refresh_token");
    }
    let Some(field) = non_empty_text(map, "field", "auth.rotate")? else {
        return invalid("auth.rotate.field is required");
    };
    let credential = slot_field(map, "credential", "auth.rotate")?;
    Ok(TokenRotate { field, credential })
}

fn parse_token_exchange(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(
        map,
        &[
            "scheme", "tokenUrl", "grant", "body", "clientAuth", "clientId", "clientSecret", "accessField",
            "expiresField", "expiresMarginMs", "storeAs", "grantRequest", "rotate", "onUnauthorized",
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
    let mut exchange = TokenExchange {
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
        store_as: StoreAs { name: String::new(), scope: SecretScope::App },
        grant_request: map.get("grantRequest").map(parse_grant_request).transpose()?,
        rotate: map.get("rotate").map(|value| parse_token_rotate(value, grant)).transpose()?,
        on_unauthorized: parse_on_unauthorized(map)?,
    };
    if let Some(rotate) = &exchange.rotate {
        let body_slot = exchange.fields.iter().find_map(|(name, field)| match field {
            TokenField::Slot(slot) if name == "refresh_token" => Some(slot),
            _ => None,
        });
        if body_slot != Some(&rotate.credential) {
            return invalid("auth.rotate.credential must be the credential slot of auth.body.refresh_token");
        }
    }
    exchange.store_as = match map.get("storeAs") {
        Some(value) => parse_store_as(value)?,
        None => default_exchange_cache(&exchange, "token-exchange")?,
    };
    check_exchange_cache_scope(&exchange)?;
    Ok(exchange)
}

fn check_exchange_cache_scope(exchange: &TokenExchange) -> Result<(), HttpRefusal> {
    if exchange.store_as.scope == SecretScope::App && exchange_slots(exchange).iter().any(|slot| slot.scope == SecretScope::User) {
        return invalid("a token exchange using user credentials requires a user-scoped cache");
    }
    Ok(())
}

fn default_exchange_cache(exchange: &TokenExchange, prefix: &str) -> Result<StoreAs, HttpRefusal> {
    let slots = exchange_slots(exchange);
    let scope = if slots.iter().any(|slot| slot.scope == SecretScope::User) { SecretScope::User } else { SecretScope::App };
    let mut names = Vec::new();
    for slot in &slots {
        if !names.contains(&slot.name.as_str()) { names.push(slot.name.as_str()); }
    }
    names.sort_unstable();
    let mut fields = std::collections::BTreeMap::new();
    for (name, field) in &exchange.fields {
        let identity = match field {
            TokenField::Literal(value) => {
                let literal = match exchange.body_kind {
                    TokenBodyKind::Form => scalar_text(value, name)?.map_or(serde_json::Value::Null, serde_json::Value::String),
                    TokenBodyKind::Json => value_to_json(value),
                };
                serde_json::json!({"literal":literal})
            }
            TokenField::Slot(slot) => serde_json::json!({"slot":slot}),
            TokenField::JwtAssertion { key, alg, claims } => serde_json::json!({"assertion":{"key":key,"alg":alg,"claims":value_to_json(claims)}}),
        };
        fields.insert(name, identity);
    }
    let identity = serde_json::json!({
        "tokenUrl":exchange.token_url,"grant":exchange.grant,"bodyKind":exchange.body_kind,"fields":fields,
        "clientAuth":exchange.client_auth,"clientId":exchange.client_id,"clientSecret":exchange.client_secret,
        "accessField":exchange.access_field,"expiresField":exchange.expires_field,"expiresMarginMs":exchange.expires_margin_ms,"scope":scope,
    });
    let bytes = serde_json::to_vec(&crate::util::canonical_json(&identity))
        .map_err(|_| HttpRefusal::Invalid("invalid token-exchange cache identity".into()))?;
    Ok(StoreAs { name:format!("{prefix}:{}|{}|{}",exchange.token_url,names.join(","),codec::hex(&codec::sha256(&bytes))), scope })
}

/// `oauth2-client` is the client-credentials form of the token exchange.
fn parse_oauth2_client(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(map, &["scheme", "tokenUrl", "clientId", "clientSecret", "scope", "grantRequest", "onUnauthorized"], "auth")?;
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
    let mut exchange = TokenExchange {
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
        grant_request: map.get("grantRequest").map(parse_grant_request).transpose()?,
        rotate: None,
        on_unauthorized: parse_on_unauthorized(map)?,
    };
    exchange.store_as = default_exchange_cache(&exchange, "oauth2")?;
    Ok(exchange)
}

fn parse_jwt_bearer(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(map, &["scheme", "key", "alg", "claims", "tokenUrl", "grant", "scope", "expiresMarginMs", "storeAs", "grantRequest", "onUnauthorized"], "auth")?;
    let key = slot_field(map, "key", "auth")?;
    let alg = codec::jwt::JwtAlg::parse(&text_field(map, "alg", "auth")?)
        .ok_or_else(|| HttpRefusal::Invalid("auth.alg must be RS256 or ES256".to_string()))?;
    let Some(Value::Object(claims)) = map.get("claims") else {
        return invalid("auth.claims must be an object");
    };
    if claims.contains_key("iss") {
        return invalid("auth.claims.iss comes from the service-account credential");
    }
    let token_url = text_field(map, "tokenUrl", "auth")?;
    let grant = non_empty_text(map, "grant", "auth")?
        .unwrap_or_else(|| "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string());
    let mut form = ValueMap::new();
    form.insert("grant_type".to_string(), Value::String(grant));
    if let Some(scope) = non_empty_text(map, "scope", "auth")? {
        form.insert("scope".to_string(), Value::String(scope));
    }
    let mut body = ValueMap::new();
    body.insert("form".to_string(), Value::Object(form));
    let mut exchange = ValueMap::new();
    exchange.insert("scheme".to_string(), Value::String("token-exchange".to_string()));
    exchange.insert("tokenUrl".to_string(), Value::String(token_url.clone()));
    exchange.insert("body".to_string(), Value::Object(body));
    for name in ["expiresMarginMs", "storeAs", "grantRequest", "onUnauthorized"] {
        if let Some(value) = map.get(name) {
            exchange.insert(name.to_string(), value.clone());
        }
    }
    let mut ex = parse_token_exchange(&exchange)?;
    if !map.contains_key("storeAs") {
        let identity_claims = claims.iter()
            .filter(|(name, _)| !matches!(name.as_str(), "iat" | "exp"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut identity = ex.clone();
        identity.fields.insert(1, ("assertion".into(), TokenField::JwtAssertion {
            key: key.clone(), alg, claims: Value::Object(identity_claims),
        }));
        ex.store_as = default_exchange_cache(&identity, "jwt-bearer")?;
    }
    ex.fields.insert(1, ("assertion".to_string(), TokenField::JwtAssertion {
        key, alg, claims: Value::Object(claims.clone()),
    }));
    check_exchange_cache_scope(&ex)?;
    Ok(ex)
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
            let name = text_field(map, "name", "auth")?;
            if location == ApiKeyIn::Header && !is_header_name(&name) {
                return invalid("invalid HTTP auth header name");
            }
            Ok(HttpAuth::ApiKey {
                credential: slot_field(map, "credential", "auth")?,
                name,
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
        "jwt-bearer" => Ok(HttpAuth::TokenExchange(parse_jwt_bearer(map)?)),
        "aws-sigv4" => {
            closed_keys(map, &["scheme", "accessKeyId", "secretAccessKey", "sessionToken", "region", "service"], "auth")?;
            Ok(HttpAuth::AwsSigV4 {
                access_key_id: slot_field(map, "accessKeyId", "auth")?,
                secret_access_key: slot_field(map, "secretAccessKey", "auth")?,
                session_token: map.get("sessionToken").filter(|v| !v.is_null()).map(|_| slot_field(map, "sessionToken", "auth")).transpose()?,
                region: non_empty_text(map, "region", "auth")?.ok_or_else(|| HttpRefusal::Invalid("auth.region is required".into()))?,
                service: non_empty_text(map, "service", "auth")?.ok_or_else(|| HttpRefusal::Invalid("auth.service is required".into()))?,
            })
        }
        "vapid" => {
            closed_keys(map, &["scheme", "privateKey", "publicKey", "subject"], "auth")?;
            Ok(HttpAuth::Vapid {
                private_key: slot_field(map, "privateKey", "auth")?,
                public_key: text_field(map, "publicKey", "auth")?,
                subject: text_field(map, "subject", "auth")?,
            })
        }
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
            let name = text_field(map, "name", "auth")?;
            if !is_header_name(&name) {
                return invalid("invalid HTTP auth header name");
            }
            Ok(HttpAuth::HmacSha256 {
                credential: slot_field(map, "credential", "auth")?,
                name,
                signs: text_field(map, "signs", "auth")?,
                encoding,
            })
        }
        "oauth-client" => parse_oauth_client_auth(map),
        other => invalid(format!("unknown auth scheme `{other}`")),
    }
}

/// An `oauth-client` client object: `{ tag, mode, issuer XOR endpoints, clientId, clientSecret?, clientAuth }`.
pub fn parse_oauth_client(value: &Value) -> Result<OAuthClientDecl, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth.client must be an object");
    };
    closed_keys(map, OAuthClientDecl::KEYS, "auth.client")?;
    let tag = text_field(map, "tag", "auth.client")?;
    let tag = OAuthProvider::parse(&tag).ok_or_else(|| HttpRefusal::Invalid(format!("auth.client.tag must be google, github or auth0, not `{tag}`")))?;
    let mode = match text_field(map, "mode", "auth.client")?.as_str() {
        "real" => OAuthMode::Real,
        "mock" => OAuthMode::Mock,
        other => return invalid(format!("auth.client.mode must be real or mock, not `{other}`")),
    };
    let client_auth = match text_field(map, "clientAuth", "auth.client")?.as_str() {
        "basic" => crate::oauth::ClientAuth::Basic,
        "post" => crate::oauth::ClientAuth::Post,
        "none" => crate::oauth::ClientAuth::None,
        other => return invalid(format!("auth.client.clientAuth must be basic, post or none, not `{other}`")),
    };
    let server = match (map.get("issuer"), map.get("endpoints")) {
        (Some(Value::String(issuer)), None) if !issuer.is_empty() => OAuthServerDecl::Issuer(issuer.clone()),
        (None, Some(Value::Object(endpoints))) => {
            closed_keys(endpoints, &["authorization", "token", "revocation", "userinfo"], "auth.client.endpoints")?;
            let optional = |key: &str| -> Result<Option<String>, HttpRefusal> {
                if endpoints.contains_key(key) { text_field(endpoints, key, "auth.client.endpoints").map(Some) } else { Ok(None) }
            };
            OAuthServerDecl::Endpoints {
                authorization: text_field(endpoints, "authorization", "auth.client.endpoints")?,
                token: text_field(endpoints, "token", "auth.client.endpoints")?,
                revocation: optional("revocation")?,
                userinfo: optional("userinfo")?,
            }
        }
        _ => return invalid("auth.client needs exactly one of issuer (a URL) or endpoints (an object)"),
    };
    Ok(OAuthClientDecl {
        tag,
        mode,
        server,
        client_id: slot_field(map, "clientId", "auth.client")?,
        client_secret: map.get("clientSecret").map(|_| slot_field(map, "clientSecret", "auth.client")).transpose()?,
        client_auth,
    })
}

fn parse_oauth_client_auth(map: &ValueMap) -> Result<HttpAuth, HttpRefusal> {
    let op_word = text_field(map, "op", "auth")?;
    let op = OAuthOp::parse(&op_word).ok_or_else(|| HttpRefusal::Invalid(format!("auth.op must be one of {}, not `{op_word}`", OAuthOp::WORDS.join(", "))))?;
    let (required, optional) = op.inputs();
    let mut allowed: Vec<&str> = vec!["scheme", "op", "client"];
    if op == OAuthOp::Code {
        allowed.push("clients");
    }
    allowed.extend(required);
    allowed.extend(optional);
    closed_keys(map, &allowed, "auth")?;
    let (client, clients) = match (map.get("client"), map.get("clients")) {
        (Some(client), None) => (parse_oauth_client(client)?, Vec::new()),
        (None, Some(Value::Object(declared))) => {
            let mut clients = Vec::new();
            for (tag, entry) in declared {
                let client = parse_oauth_client(entry)?;
                if client.tag.name() != tag {
                    return invalid(format!("auth.clients.{tag} declares a client tagged `{}`", client.tag.name()));
                }
                clients.push(client);
            }
            let Some(first) = clients.first().cloned() else {
                return invalid("auth.clients declares no client");
            };
            (first, clients)
        }
        (None, Some(_)) => return invalid("auth.clients must be an object of clients keyed by provider tag"),
        (Some(_), Some(_)) => return invalid("an oauth-client auth has `client` or `clients`, not both"),
        (None, None) => return invalid("auth.client is required"),
    };
    for key in required {
        if !map.contains_key(*key) {
            return invalid(format!("auth op `{}` needs `{key}`", op.name()));
        }
    }
    let input = |key: &str| -> Result<Option<String>, HttpRefusal> {
        if map.contains_key(key) { text_field(map, key, "auth").map(Some) } else { Ok(None) }
    };
    let provider = match input("provider")? {
        Some(word) => Some(OAuthProvider::parse(&word).ok_or_else(|| HttpRefusal::Invalid(format!("auth.provider must be google, github or auth0, not `{word}`")))?),
        None => None,
    };
    let inputs = OAuthInputs {
        code: input("code")?,
        state: input("state")?,
        subject: input("subject")?,
        provider,
        refresh_token: input("refreshToken")?,
        token: input("token")?,
        access_token: input("accessToken")?,
    };
    Ok(HttpAuth::OAuthClient { op, client, clients, inputs })
}

pub fn is_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric()
        || matches!(byte, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'))
}

pub fn is_header_value(value: &str) -> bool {
    value.bytes().all(|byte| byte == b'\t' || (byte >= b' ' && byte != 127))
}

fn check_header(name: &str, value: &str) -> Result<(), HttpRefusal> {
    if !is_header_name(name) {
        return invalid("invalid HTTP header name");
    }
    if !is_header_value(value) {
        return invalid(format!("header `{name}` must be single-line without control characters"));
    }
    Ok(())
}

fn parse_webpush(value: &Value, target: &str) -> Result<HttpBodySpec, HttpRefusal> {
    let Value::Object(map) = value else { return invalid("webpush must be an object") };
    closed_keys(map, &["subscription", "payload", "ttl", "urgency"], "webpush")?;
    let Some(Value::Object(subscription)) = map.get("subscription") else { return invalid("webpush.subscription must be an object") };
    closed_keys(subscription, &["endpoint", "keys"], "webpush.subscription")?;
    let endpoint = text_field(subscription, "endpoint", "webpush.subscription")?;
    if endpoint != target { return invalid("webpush subscription.endpoint must equal the declared HTTP URL") }
    let Some(Value::Object(keys)) = subscription.get("keys") else { return invalid("webpush.subscription.keys must be an object") };
    closed_keys(keys, &["p256dh", "auth"], "webpush.subscription.keys")?;
    let urgency = map.get("urgency").map(|_| text_field(map,"urgency","webpush")).transpose()?.unwrap_or_else(|| "normal".into());
    if !["very-low","low","normal","high"].contains(&urgency.as_str()) { return invalid("invalid webpush.urgency") }
    Ok(HttpBodySpec::WebPush(WebPushBody {
        endpoint, p256dh:text_field(keys,"p256dh","webpush.subscription.keys")?,
        auth:text_field(keys,"auth","webpush.subscription.keys")?,
        payload:text_field(map,"payload","webpush")?,
        ttl:whole_number(map.get("ttl"),"webpush.ttl")?.unwrap_or(2_419_200), urgency,
    }))
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
                        Value::String(v) if is_header_name(name) && is_header_value(v) => {
                            headers.push((name.clone(), v.clone()))
                        }
                        _ => return invalid("multipart part header needs a token name and a single-line string value"),
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

pub fn parse_http_headers(value: Option<&Value>) -> Result<Vec<(String, String)>, HttpRefusal> {
    match value {
        Some(Value::Object(headers)) => {
            let mut out = Vec::new();
            for (name, value) in headers {
                match value {
                    Value::String(s) => {
                        check_header(name, s)?;
                        out.push((name.clone(), s.clone()));
                    }
                    _ => return invalid("HTTP header values must be strings"),
                }
            }
            Ok(out)
        }
        Some(_) => invalid("headers must be an object"),
        None => Ok(Vec::new()),
    }
}

pub fn parse_timeout_ms(value: Option<&Value>) -> Result<Option<u64>, HttpRefusal> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) if *n >= 0.0 && n.fract() == 0.0 => Ok(Some(*n as u64)),
        Some(_) => invalid("timeoutMs must be a whole number"),
    }
}

pub fn parse_grant_request(value: &Value) -> Result<GrantRequest, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth.grantRequest must be an object");
    };
    closed_keys(map, GrantRequest::KEYS, "auth.grantRequest")?;
    let headers = parse_http_headers(map.get("headers"))?;
    if headers.iter().any(|(name, _)| GrantRequest::is_driver_owned_header(name)) {
        return invalid("grant Authorization and Content-Type headers are kernel-owned");
    }
    Ok(GrantRequest {
        headers,
        timeout_ms: parse_timeout_ms(map.get("timeoutMs"))?,
        retry: map.get("retry").map(parse_retry).transpose()?,
    })
}

pub fn parse_retry(value: &Value) -> Result<HttpRetry, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("retry must be an object");
    };
    let fields: Vec<_> = map
        .iter()
        .map(|(name, value)| (name.clone(), RetryOperand::Literal(value.clone())))
        .collect();
    HttpRetry::check_partial(&fields)?.ok_or_else(|| HttpRefusal::Invalid("retry must contain concrete values".into()))
}

impl RetryOperand {
    fn whole(&self, what: &str, missing: &str) -> Result<Option<u64>, HttpRefusal> {
        match self {
            Self::Literal(value) => whole_number(Some(value), what)?
                .map(Some)
                .ok_or_else(|| HttpRefusal::Invalid(missing.into())),
            Self::Typed(ty) if crate::oir_type_satisfies(ty, &OirType::Number) => Ok(None),
            Self::Unresolved => Ok(None),
            _ => invalid(format!("{what} must be a whole number")),
        }
    }

    fn list(&self, inners: &[OirType], message: &str) -> Result<Option<Vec<Self>>, HttpRefusal> {
        match self {
            Self::Literal(Value::Array(values)) => {
                Ok(Some(values.iter().cloned().map(Self::Literal).collect()))
            }
            Self::Items(values) => Ok(Some(values.clone())),
            Self::Typed(OirType::Tuple { items }) => {
                Ok(Some(items.iter().cloned().map(Self::Typed).collect()))
            }
            Self::Typed(OirType::Optional { inner: actual }) => {
                Self::Typed(actual.as_ref().clone()).list(inners, message)
            }
            Self::Typed(ty)
                if inners.iter().any(|inner| {
                    crate::oir_type_satisfies(ty, &OirType::Array { inner: Box::new(inner.clone()) })
                }) =>
            {
                Ok(None)
            }
            Self::Unresolved => Ok(None),
            _ => invalid(message),
        }
    }

    fn rule(&self) -> Result<Option<RetryOn>, HttpRefusal> {
        match self {
            Self::Literal(Value::String(tag)) => match RetryOn::from_tag(tag) {
                Some(rule) => Ok(Some(rule)),
                None => invalid(RetryOn::ENTRIES),
            },
            Self::Literal(Value::Number(n)) => {
                match (n.fract() == 0.0 && *n >= 0.0).then(|| RetryOn::status(*n as u64)).flatten() {
                    Some(rule) => Ok(Some(rule)),
                    None => invalid(RetryOn::ENTRIES),
                }
            }
            Self::Typed(ty)
                if crate::oir_type_satisfies(ty, &OirType::String)
                    || crate::oir_type_satisfies(ty, &OirType::Number) =>
            {
                Ok(None)
            }
            Self::Unresolved => Ok(None),
            _ => invalid(RetryOn::ENTRIES),
        }
    }
}

impl HttpRetry {
    pub fn check_partial(fields: &[(String, RetryOperand)]) -> Result<Option<Self>, HttpRefusal> {
        if let Some((name, _)) = fields
            .iter()
            .find(|(name, _)| !Self::KEYS.contains(&name.as_str()))
        {
            return invalid(format!("retry has no key `{name}`"));
        }
        let get = |key: &str| {
            fields.iter().find(|(name, _)| name == key).map(|(_, value)| value)
        };
        let max_message = "retry.max must be a whole number of at least 1";
        let Some(raw_max) = get("max") else {
            return invalid(max_message);
        };
        let max = match raw_max.whole("retry.max", max_message)? {
            Some(value) if (1..=u64::from(u32::MAX)).contains(&value) => Some(value as u32),
            Some(_) => return invalid(max_message),
            None => None,
        };
        let on_message = "retry.on must be a non-empty list";
        let Some(raw_on) = get("on") else {
            return invalid(on_message);
        };
        let on = match raw_on.list(&[OirType::String, OirType::Number], on_message)? {
            Some(values) => {
                if values.is_empty() {
                    return invalid(on_message);
                }
                let mut rules = Vec::new();
                let mut complete = true;
                for value in values {
                    match value.rule()? {
                        Some(rule) => {
                            if !rules.contains(&rule) {
                                rules.push(rule);
                            }
                        }
                        None => complete = false,
                    }
                }
                complete.then_some(rules)
            }
            None => None,
        };
        let schedule_message = "retry.scheduleMs must contain exactly max whole-number delays";
        let (delay, schedule_len) = match (get("backoffMs"), get("scheduleMs")) {
            (Some(value), None) => (
                value
                    .whole("retry.backoffMs", "retry.backoffMs is required")?
                    .map(|backoff_ms| RetryDelay::Backoff { backoff_ms }),
                None,
            ),
            (None, Some(value)) => match value.list(&[OirType::Number], schedule_message)? {
                Some(values) => {
                    let Ok(length) = u32::try_from(values.len()) else {
                        return invalid(schedule_message);
                    };
                    if length == 0 || max.is_some_and(|max| max != length) {
                        return invalid(schedule_message);
                    }
                    let mut schedule_ms = Vec::new();
                    let mut complete = true;
                    for value in values {
                        match value.whole(
                            "retry.scheduleMs entry",
                            "retry.scheduleMs entries must be whole numbers",
                        )? {
                            Some(value) => schedule_ms.push(value),
                            None => complete = false,
                        }
                    }
                    (
                        complete.then_some(RetryDelay::Schedule { schedule_ms }),
                        Some(length),
                    )
                }
                None => (None, None),
            },
            _ => return invalid("retry needs exactly one of backoffMs or scheduleMs"),
        };
        let cap_message = "retry.noResponseMax must be a whole number from zero through max";
        let no_response_max = match get("noResponseMax") {
            None => Some(None),
            Some(value) => match value.whole("retry.noResponseMax", cap_message)? {
                Some(value)
                    if value <= u64::from(u32::MAX)
                        && max.is_none_or(|max| value <= u64::from(max))
                        && schedule_len.is_none_or(|length| value <= u64::from(length)) =>
                {
                    Some(Some(value as u32))
                }
                Some(_) => return invalid(cap_message),
                None => None,
            },
        };
        Ok(match (max, on, delay, no_response_max) {
            (Some(max), Some(on), Some(delay), Some(no_response_max)) => Some(Self {
                max,
                on,
                delay,
                no_response_max,
            }),
            _ => None,
        })
    }
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

const OAUTH_ENDPOINT_STAND_IN: &str = "https://oauth-endpoint.invalid";

/// `(http METHOD oauth-endpoint WORD { auth: { scheme: oauth-client, … } })`: the URL is the client's metadata
/// endpoint named by WORD, so the call has no origin of its own and egress is checked on each built request.
fn parse_oauth_endpoint_call(method: HttpMethod, word: &str, request: &Value) -> Result<HttpCallSpec, HttpRefusal> {
    let Some(kind) = OAuthEndpointKind::parse(word) else {
        return invalid(format!("oauth-endpoint must be followed by one of {}, not `{word}`", OAuthEndpointKind::WORDS.join(", ")));
    };
    let mut spec = parse_http_call(method, OAUTH_ENDPOINT_STAND_IN, &format!("/{word}"), request)?;
    match &spec.auth {
        Some(HttpAuth::OAuthClient { op, .. }) if op.endpoint() == Some(kind) => {}
        Some(HttpAuth::OAuthClient { op, .. }) => {
            return invalid(format!("oauth-endpoint {word} does not serve op `{}`", op.name()))
        }
        _ => return invalid("oauth-endpoint needs an oauth-client auth"),
    }
    let expected = if kind == OAuthEndpointKind::Userinfo { HttpMethod::Get } else { HttpMethod::Post };
    if method != expected {
        return invalid(format!("oauth-endpoint {word} is called with {}", expected.as_str()));
    }
    spec.origin = OAuthEndpointKind::TARGET.to_string();
    spec.path = word.to_string();
    Ok(spec)
}

pub fn parse_http_call(
    method: HttpMethod,
    origin: &str,
    path: &str,
    request: &Value,
) -> Result<HttpCallSpec, HttpRefusal> {
    if origin == OAuthEndpointKind::TARGET {
        return parse_oauth_endpoint_call(method, path, request);
    }
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
            "paginate", "stream", "retry", "formEncoding", "queryEncoding", "presign", "webpush",
        ],
        "request",
    )?;
    let query_encoding = match map.get("queryEncoding") {
        None => QueryEncoding::Uri,
        Some(Value::String(s)) if s == "uri" => QueryEncoding::Uri,
        Some(Value::String(s)) if s == "rfc3986" => QueryEncoding::Rfc3986,
        Some(_) => return invalid("queryEncoding must be uri or rfc3986"),
    };

    let query = match map.get("query") {
        Some(value) => pairs_of(value, "query")?,
        None => Vec::new(),
    };
    let headers = parse_http_headers(map.get("headers"))?;

    let bodies: Vec<&str> = ["json", "form", "text", "bytes", "multipart", "webpush"]
        .into_iter()
        .filter(|k| map.contains_key(*k))
        .collect();
    if bodies.len() > 1 {
        return invalid("a request carries at most one of json, form, text, bytes, multipart, webpush");
    }
    if !bodies.is_empty() && method == HttpMethod::Get {
        return invalid("a GET request carries no body");
    }
    let body = match bodies.first().copied() {
        None => HttpBodySpec::Empty,
        Some("webpush") => parse_webpush(&map["webpush"], &with_query(format!("{origin}{path}"), &encode_query(&query, query_encoding)))?,
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
    if let Some(HttpAuth::OAuthClient { op, .. }) = &auth {
        if let Some(extra) = map.keys().find(|key| key.as_str() != "auth") {
            return invalid(format!("an oauth-client request carries only `auth`, not `{extra}`"));
        }
        let endpoint_call = origin == OAUTH_ENDPOINT_STAND_IN;
        if !endpoint_call && (*op != OAuthOp::Discovery || method != HttpMethod::Get) {
            return invalid("an oauth-client op other than discovery is sent to `oauth-endpoint`; discovery is a GET");
        }
    }
    let timeout_ms = parse_timeout_ms(map.get("timeoutMs"))?;
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
    let presign = match map.get("presign") {
        None => None,
        Some(Value::Object(options)) => {
            closed_keys(options, &["expiresSec"], "presign")?;
            let seconds = whole_number(options.get("expiresSec"), "presign.expiresSec")?
                .filter(|n| (1..=604800).contains(n))
                .ok_or_else(|| HttpRefusal::Invalid("presign.expiresSec must be a whole number from 1 to 604800".into()))?;
            if !matches!(auth, Some(HttpAuth::AwsSigV4 { .. })) {
                return invalid("presign requires aws-sigv4 auth");
            }
            if paginate.is_some() || stream.is_some() || retry.is_some() || !matches!(body, HttpBodySpec::Empty) {
                return invalid("presign has no body, pagination, stream or retry");
            }
            Some(seconds)
        }
        Some(_) => return invalid("presign must be an object"),
    };

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
        query_encoding,
        retry,
        presign,
        delivery: HttpDelivery::Fetch,
    })
}

/// A `browser/beacon` call: `url` must be an absolute http(s) URL without a fragment and `request` is exactly
/// `{ text }`. The result is an ordinary POST spec whose delivery is the beacon port, so egress and the request
/// itself come from the same builder as every other call.
pub fn parse_beacon_call(url: &str, request: &Value) -> Result<HttpCallSpec, HttpRefusal> {
    let Some((_, remainder)) = split_url(url) else {
        return invalid(format!("beacon URL `{url}` is not an absolute http(s) URL"));
    };
    if remainder.contains('#') {
        return invalid("a beacon URL must not carry a fragment");
    }
    let origin = &url[..url.len() - remainder.len()];
    let path = if remainder.is_empty() { "/".to_string() } else if remainder.starts_with('?') { format!("/{remainder}") } else { remainder.to_string() };
    let Value::Object(map) = request else {
        return invalid("a beacon request is an object carrying `text`");
    };
    closed_keys(map, &["text"], "beacon request")?;
    let Some(Value::String(text)) = map.get("text") else {
        return invalid("a beacon request needs `text`, a string");
    };
    let mut body = ValueMap::new();
    body.insert("text".to_string(), Value::String(text.clone()));
    body.insert(
        "headers".to_string(),
        Value::Object(ValueMap::from([("Content-Type".to_string(), Value::String("text/plain".to_string()))])),
    );
    let mut spec = parse_http_call(HttpMethod::Post, origin, &path, &Value::Object(body))?;
    spec.delivery = HttpDelivery::Beacon;
    Ok(spec)
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
        TokenField::Slot(slot) | TokenField::JwtAssertion { key: slot, .. } => Some(slot),
        TokenField::Literal(_) => None,
    });
    for slot in ex.client_id.iter().chain(ex.client_secret.iter()).chain(body) {
        if !out.contains(slot) {
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
        Some(HttpAuth::TokenExchange(ex)) => vec![ex.store_as.slot()],
        Some(HttpAuth::Vapid { private_key, .. }) => vec![private_key.clone()],
        Some(HttpAuth::OAuthClient { clients, .. }) if !clients.is_empty() => Vec::new(),
        Some(HttpAuth::OAuthClient { client, .. }) if client.mode == OAuthMode::Mock => Vec::new(),
        Some(HttpAuth::OAuthClient { client, .. }) => {
            std::iter::once(client.client_id.clone()).chain(client.client_secret.clone()).collect()
        }
        Some(HttpAuth::AwsSigV4 { access_key_id, secret_access_key, session_token, .. }) =>
            [Some(access_key_id.clone()), Some(secret_access_key.clone()), session_token.clone()].into_iter().flatten().collect(),
    }
}

/// Credential names `build_request`'s lookup must answer, in ask order.
pub fn credentials_needed(spec: &HttpCallSpec) -> Vec<String> {
    credential_slots_needed(spec).into_iter().map(|slot| slot.name).collect()
}

fn credential(
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    slot: &CredentialSlot,
) -> Result<String, HttpRefusal> {
    secrets(slot).ok_or_else(|| HttpRefusal::CredentialMissing(slot.name.clone()))
}

fn encode_query(pairs: &[(String, String)], encoding: QueryEncoding) -> String {
    let escape = match encoding {
        QueryEncoding::Uri => codec::encode_uri_component,
        QueryEncoding::Rfc3986 => codec::encode_rfc3986,
    };
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
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
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
) -> Result<HttpRequest, HttpRefusal> {
    build_request_with_boundary(spec, secrets, egress, page_token, None)
}

/// [`build_request`] with the multipart boundary the driver drew from the host's randomness.
pub fn build_request_with_boundary(
    spec: &HttpCallSpec,
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
    boundary: Option<&str>,
) -> Result<HttpRequest, HttpRefusal> {
    build_request_with_metadata(spec, secrets, egress, page_token, boundary, None, None)
}

fn build_request_with_metadata(
    spec: &HttpCallSpec,
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
    boundary: Option<&str>,
    now_ms: Option<f64>,
    webpush_body: Option<&[u8]>,
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
        Some(HttpAuth::OAuthClient { .. }) => {}
        Some(HttpAuth::AwsSigV4 { .. }) => {}
        Some(HttpAuth::Vapid { .. }) => {}
        Some(HttpAuth::Bearer { credential: name }) => {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", credential(secrets, name)?),
            ));
        }
        Some(HttpAuth::ApiKey {
            credential: name,
            name: key,
            location,
        }) => {
            let value = credential(secrets, name)?;
            match location {
                ApiKeyIn::Header => headers.push((key.clone(), value)),
                ApiKeyIn::Query => query.push((key.clone(), value)),
            }
        }
        Some(HttpAuth::Basic { user, credential: name }) => {
            let pair = format!("{}:{}", credential(secrets, user)?, credential(secrets, name)?);
            headers.push((
                "Authorization".to_string(),
                format!("Basic {}", codec::base64_encode(pair.as_bytes())),
            ));
        }
        Some(HttpAuth::TokenExchange(ex)) => {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", credential(secrets, &ex.store_as.slot())?),
            ));
        }
        Some(HttpAuth::HmacSha256 {
            credential: name,
            name: header,
            signs,
            encoding,
        }) => {
            let mac = codec::hmac_sha256(credential(secrets, name)?.as_bytes(), signs.as_bytes());
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
    let mut url = with_query(url, &encode_query(&query, spec.query_encoding));
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
        HttpBodySpec::WebPush(push) => {
            if url != push.endpoint { return invalid("webpush subscription.endpoint must equal the declared HTTP URL") }
            for (name, _) in &headers {
                if ["content-type","content-encoding","content-length","ttl","urgency"].iter().any(|owned| name.eq_ignore_ascii_case(owned)) {
                    return invalid("webpush encryption headers are kernel-owned");
                }
            }
            let bytes = webpush_body.ok_or_else(|| HttpRefusal::Invalid("webpush needs journaled driver entropy".into()))?;
            headers.extend([
                ("TTL".into(),push.ttl.to_string()), ("Urgency".into(),push.urgency.clone()),
                ("Content-Length".into(),bytes.len().to_string()), ("Content-Encoding".into(),"aes128gcm".into()),
            ]);
            (Some("application/octet-stream".into()),bytes.to_vec())
        }
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

    if let Some(HttpAuth::AwsSigV4 { access_key_id, secret_access_key, session_token, region, service }) = &spec.auth {
        let stamp = dispatch_stamp(now_ms, "aws-sigv4")?;
        for (name, _) in &headers {
            if ["host", "authorization", "x-amz-date", "x-amz-content-sha256", "x-amz-security-token"].iter().any(|reserved| name.eq_ignore_ascii_case(reserved)) {
                return invalid("aws-sigv4 signing headers are kernel-owned");
            }
        }
        let signer = codec::sigv4::Signer {
            access_key_id: credential(secrets, access_key_id)?,
            secret_access_key: credential(secrets, secret_access_key)?,
            session_token: session_token.as_ref().map(|slot| credential(secrets, slot)).transpose()?,
            region: region.clone(), service: service.clone(),
        };
        let parsed = url::Url::parse(&url).map_err(|_| HttpRefusal::Invalid("invalid signing URL".into()))?;
        let host = &parsed[url::Position::BeforeHost..url::Position::AfterPort];
        let query: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        let target = codec::sigv4::Target { method: spec.method.as_str(), host, path: parsed.path(), query: &query };
        if let Some(seconds) = spec.presign {
            if !headers.is_empty() { return invalid("presign cannot require headers not carried in its URL"); }
            let signed_query = signer.presign_query(&target, &seconds.to_string(), Some(codec::sigv4::UNSIGNED_PAYLOAD), stamp);
            url = format!("{}?{signed_query}", &parsed[..url::Position::AfterPath]);
        } else {
            headers.extend(signer.sign_request(&target, &headers, &body, stamp));
        }
    }
    if let Some(HttpAuth::Vapid { private_key, public_key, subject }) = &spec.auth {
        if !url.starts_with("https://android.googleapis.com/gcm/send") {
            if headers.iter().any(|(name,_)| name.eq_ignore_ascii_case("authorization")) { return invalid("vapid authorization is kernel-owned") }
            let stamp = dispatch_stamp(now_ms,"vapid")?;
            let (_,authority) = url.split_once("://").ok_or_else(|| HttpRefusal::Invalid("invalid VAPID URL".into()))?;
            let host = authority.split(['/', '?', '#']).next().unwrap_or("");
            let scheme = url.split_once("://").map(|(scheme,_)| scheme).unwrap_or("");
            let audience = format!("{}://{}",scheme.to_ascii_lowercase(),host.to_ascii_lowercase());
            let jwt = codec::webpush::vapid_jwt(&audience,subject,public_key,&credential(secrets,private_key)?,stamp.div_euclid(1000)+43_200)
                .map_err(|error| HttpRefusal::Invalid(error.0))?;
            headers.push(("Authorization".into(),format!("vapid t={jwt}, k={public_key}")));
        }
    }
    for (name, value) in &headers {
        check_header(name, value)?;
    }

    Ok(HttpRequest {
        method: spec.method.as_str().to_string(),
        url,
        headers,
        body,
        timeout: spec.timeout_ms.map(Duration::from_millis),
    })
}

fn dispatch_stamp(now_ms: Option<f64>, scheme: &str) -> Result<i64, HttpRefusal> {
    now_ms.filter(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= 8_640_000_000_000_000.0)
        .map(|n| n as i64)
        .ok_or_else(|| HttpRefusal::Invalid(format!("{scheme} requires the dispatch nowMs stamp")))
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
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
) -> Result<HttpRequest, HttpRefusal> {
    let HttpAuth::TokenExchange(ex) = auth else {
        return invalid("not a token-exchange auth");
    };
    if split_url(&ex.token_url).is_none() {
        return invalid(format!("tokenUrl `{}` is not an http(s) URL", ex.token_url));
    }
    let id = ex.client_id.as_ref().map(|slot| credential(secrets, slot)).transpose()?;
    let secret = ex.client_secret.as_ref().map(|slot| credential(secrets, slot)).transpose()?;
    let mut fields: Vec<(String, Value)> = Vec::new();
    if let Some(grant_type) = ex.grant.grant_type() {
        fields.push(("grant_type".to_string(), Value::String(grant_type.to_string())));
    }
    for (name, field) in &ex.fields {
        let value = match field {
            TokenField::Literal(value) => value.clone(),
            TokenField::Slot(slot) => Value::String(credential(secrets, slot)?),
            TokenField::JwtAssertion { key, alg, claims } => {
                let raw = credential(secrets, key)?;
                let parsed = codec::jwt::ServiceAccountKey::parse(&raw).or_else(|_| {
                    let bytes = codec::lenient::base64_decode_lenient(&raw);
                    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid credential".to_string())?;
                    codec::jwt::ServiceAccountKey::parse(text)
                }).map_err(|_| HttpRefusal::Invalid("invalid service-account credential".to_string()))?;
                let pem = parsed.private_key.filter(|pem| !pem.is_empty())
                    .ok_or_else(|| HttpRefusal::Invalid("No key or keyFile set.".to_string()))?;
                let issuer = parsed.client_email.filter(|email| !email.is_empty())
                    .ok_or_else(|| HttpRefusal::Invalid("email is required.".to_string()))?;
                let mut payload = serde_json::Map::new();
                payload.insert("iss".to_string(), serde_json::Value::String(issuer));
                let Value::Object(claims) = claims else {
                    return invalid("auth.claims must be an object");
                };
                for (name, value) in claims {
                    payload.insert(name.clone(), value_to_json(value));
                }
                let payload = serde_json::Value::Object(payload).to_string();
                let assertion = codec::jwt::compact(*alg, &payload, &pem)
                    .map_err(|_| HttpRefusal::Invalid("invalid service-account signing key".to_string()))?;
                Value::String(assertion)
            }
        };
        fields.push((name.clone(), value));
    }
    let mut headers = ex.grant_request.as_ref().map(|request| request.headers.clone()).unwrap_or_default();
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
        timeout: ex.grant_request.as_ref().and_then(|request| request.timeout_ms).map(Duration::from_millis),
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

/// The replacement refresh token a rotating grant reply carries in `field`.
fn parse_rotated_token(resp: &HttpResponse, field: &str) -> Result<String, String> {
    let json: serde_json::Value =
        serde_json::from_slice(&resp.body).map_err(|e| format!("token reply is not JSON: {e}"))?;
    json.get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("token reply has no {field}"))
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
        ("phase", Value::Null),
        ("grantStatus", Value::Null),
        ("grantBody", Value::Null),
        ("code", Value::Null),
        ("reason", Value::Null),
    ])
}

fn set_field(mut payload: Value, name: &str, value: Value) -> Value {
    if let Value::Object(map) = &mut payload {
        map.insert(name.to_string(), value);
    }
    payload
}

/// A failure of the OAuth flow: the fixed safe message, its public `code` and the closed `reason`.
pub fn oauth_failure_payload(error: &str, code: &str, reason: &str) -> Value {
    let payload = failure_value(error.to_string(), "oauth", None);
    let payload = set_field(payload, "code", Value::String(code.to_string()));
    set_field(payload, "reason", Value::String(reason.to_string()))
}

/// The host has no port for a capability the call needs.
pub fn unavailable_payload(message: &str) -> Value {
    failure_value(message.to_string(), "unavailable", None)
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
    /// The built request of a beacon call, for the host's fire-and-forget port.
    Beacon { request: HttpRequest },
    /// Replace the host-held credential `name` with `value`, durably (a rotated refresh token).
    SecretRotate {
        name: String,
        value: String,
        scope: SecretScope,
    },
    /// `bytes` of host CSPRNG output (journaled by the kernel, so a replay is deterministic).
    Random { bytes: u32 },
    /// What an `oauth-client` call needs its host to persist (pending grants, connections, metadata, clock).
    OAuth { ask: OAuthAsk },
}

#[derive(Clone, PartialEq)]
pub enum HttpAnswer {
    Secret(Result<Option<String>, String>),
    SecretSet(Result<(), String>),
    SecretRotate(Result<(), String>),
    Http(Result<HttpResponse, TransportError>),
    Beacon(Result<BeaconOutcome, TransportError>),
    Random(Result<Vec<u8>, String>),
    OAuth(Result<OAuthReply, String>),
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
            Self::SecretRotate { name, scope, .. } => f
                .debug_struct("SecretRotate")
                .field("name", name)
                .field("value", &"<redacted>")
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
            Self::Beacon { request } => f.debug_struct("Beacon").field("request", request).finish(),
            Self::Random { bytes } => f.debug_struct("Random").field("bytes", bytes).finish(),
            Self::OAuth { ask } => f.debug_struct("OAuth").field("ask", ask).finish(),
        }
    }
}

impl std::fmt::Debug for HttpAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret(Ok(Some(_))) => f.write_str("Secret(Ok(Some(<redacted>)))"),
            Self::Secret(other) => f.debug_tuple("Secret").field(other).finish(),
            Self::SecretSet(r) => f.debug_tuple("SecretSet").field(r).finish(),
            Self::SecretRotate(r) => f.debug_tuple("SecretRotate").field(r).finish(),
            Self::Http(r) => f.debug_tuple("Http").field(r).finish(),
            Self::Beacon(r) => f.debug_tuple("Beacon").field(r).finish(),
            Self::Random(Ok(_)) => f.write_str("Random(Ok(<redacted>))"),
            Self::Random(Err(message)) => f.debug_tuple("Random").field(&Err::<(), _>(message)).finish(),
            Self::OAuth(r) => f.debug_tuple("OAuth").field(r).finish(),
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
    SecretRotate {
        name: String,
        value: String,
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
    Beacon { request: HttpRequestWire },
    Random { bytes: u32 },
    #[serde(rename = "oauth")]
    OAuth { ask: OAuthAsk },
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
            HttpAsk::SecretRotate { name, value, scope } => Self::SecretRotate {
                name: name.clone(),
                value: value.clone(),
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
            HttpAsk::Beacon { request } => Self::Beacon { request: request.into() },
            HttpAsk::Random { bytes } => Self::Random { bytes: *bytes },
            HttpAsk::OAuth { ask } => Self::OAuth { ask: ask.clone() },
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
            HttpAskWire::SecretRotate { name, value, scope } => Self::SecretRotate { name, value, scope },
            HttpAskWire::Http { request, delay_ms } => Self::Http {
                request: request.into(),
                delay_ms,
            },
            HttpAskWire::HttpStream { request, framing } => Self::HttpStream {
                request: request.into(),
                framing,
            },
            HttpAskWire::Beacon { request } => Self::Beacon { request: request.into() },
            HttpAskWire::Random { bytes } => Self::Random { bytes },
            HttpAskWire::OAuth { ask } => Self::OAuth { ask },
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
    SecretRotate { result: Settled<(), String> },
    Http { result: Settled<HttpResponseWire, HttpFailureWire> },
    Beacon { result: Settled<BeaconOutcome, HttpFailureWire> },
    Random { result: Settled<String, String> },
    #[serde(rename = "oauth")]
    OAuth { result: Settled<OAuthReply, String> },
}

impl From<&HttpAnswer> for HttpAnswerWire {
    fn from(answer: &HttpAnswer) -> Self {
        match answer {
            HttpAnswer::Secret(r) => Self::Secret { result: r.clone().into() },
            HttpAnswer::SecretSet(r) => Self::SecretSet { result: r.clone().into() },
            HttpAnswer::SecretRotate(r) => Self::SecretRotate { result: r.clone().into() },
            HttpAnswer::Http(r) => Self::Http {
                result: r
                    .as_ref()
                    .map(HttpResponseWire::from)
                    .map_err(HttpFailureWire::from)
                    .into(),
            },
            HttpAnswer::Beacon(r) => Self::Beacon {
                result: r.as_ref().map(Clone::clone).map_err(HttpFailureWire::from).into(),
            },
            HttpAnswer::Random(r) => Self::Random {
                result: r.as_ref().map(|bytes| codec::base64_encode(bytes)).map_err(Clone::clone).into(),
            },
            HttpAnswer::OAuth(r) => Self::OAuth { result: r.clone().into() },
        }
    }
}

impl TryFrom<HttpAnswerWire> for HttpAnswer {
    type Error = String;

    fn try_from(wire: HttpAnswerWire) -> Result<Self, String> {
        Ok(match wire {
            HttpAnswerWire::Secret { result } => Self::Secret(result.into()),
            HttpAnswerWire::SecretSet { result } => Self::SecretSet(result.into()),
            HttpAnswerWire::SecretRotate { result } => Self::SecretRotate(result.into()),
            HttpAnswerWire::Http { result } => {
                let result: Result<HttpResponseWire, HttpFailureWire> = result.into();
                Self::Http(result.map(HttpResponse::from).map_err(TransportError::from))
            }
            HttpAnswerWire::Beacon { result } => {
                let result: Result<BeaconOutcome, HttpFailureWire> = result.into();
                Self::Beacon(result.map_err(TransportError::from))
            }
            HttpAnswerWire::OAuth { result } => Self::OAuth(result.into()),
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

/// Private credentials and built requests, never serialized or printed with the driver.
#[derive(Clone, PartialEq, Default)]
pub struct HeldSecrets {
    credentials: Vec<(CredentialSlot, String)>,
    grant_request: Option<HttpRequest>,
    resource_request: Option<HttpRequest>,
    oauth: OAuthHeld,
}

impl std::fmt::Debug for HeldSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.credentials.iter().map(|(slot, _)| &slot.name)).finish()
    }
}

impl HeldSecrets {
    fn get(&self, slot: &CredentialSlot) -> Option<&str> {
        self.credentials.iter().find(|(held, _)| held == slot).map(|(_, v)| v.as_str())
    }

    fn hold(&mut self, slot: CredentialSlot, value: String) {
        self.credentials.retain(|(held, _)| held != &slot);
        self.credentials.push((slot, value));
    }

    fn forget(&mut self, slot: &CredentialSlot) {
        self.credentials.retain(|(held, _)| held != slot);
    }

    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty()
            && self.grant_request.is_none()
            && self.resource_request.is_none()
            && self.oauth.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum Phase {
    Credentials { pending: Vec<CredentialSlot>, awaiting: Option<CredentialSlot> },
    GrantSecrets { pending: Vec<CredentialSlot>, awaiting: Option<CredentialSlot> },
    GrantReply,
    GrantRotated,
    GrantStored,
    Boundary,
    WebPushEntropy,
    OAuth,
    Reply,
    Finished,
}

/// An `oauth-client` call's declaration with its resolved credentials.
struct OAuthCall {
    op: OAuthOp,
    client: OAuthClientDecl,
    clients: Vec<OAuthClientDecl>,
    inputs: OAuthInputs,
    id: String,
    secret: String,
}

/// One `http` call as a pure step machine: `start` yields the first ask, `resume` feeds each answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpCallDriver {
    spec: HttpCallSpec,
    #[serde(default)]
    now_ms: Option<f64>,
    #[serde(default)]
    webpush_body: Option<Vec<u8>>,
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
    #[serde(default)]
    grant_retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_delay_ms: Option<u64>,
    /// The lifetime of the access token waiting behind a rotation; the token itself is a held secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_ttl_ms: Option<u64>,
    /// The `@user` id the kernel sets for an `oauth-client` call; never the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth: Option<OAuthFlow>,
}

pub(crate) fn with_key(mut payload: Value, key: &Option<String>) -> Value {
    if let Value::Object(map) = &mut payload {
        map.insert("key".to_string(), key.clone().map_or(Value::Null, Value::String));
    }
    payload
}

const BOUNDARY_BYTES: u32 = 16;

impl HttpCallDriver {
    pub fn start(spec: HttpCallSpec, egress: Vec<String>, key: Option<String>) -> (HttpCallDriver, HttpStep) {
        Self::start_stamped(spec, egress, key, None)
    }

    pub fn start_stamped(spec: HttpCallSpec, egress: Vec<String>, key: Option<String>, now_ms: Option<f64>) -> (HttpCallDriver, HttpStep) {
        Self::start_as(spec, egress, key, now_ms, None)
    }

    /// [`Self::start_stamped`] for the signed-in user `principal`: an `oauth-client` call with no credential to
    /// ask (a mock client) begins its flow inside `start`, so the principal must already be there.
    pub fn start_as(
        spec: HttpCallSpec,
        egress: Vec<String>,
        key: Option<String>,
        now_ms: Option<f64>,
        principal: Option<String>,
    ) -> (HttpCallDriver, HttpStep) {
        let pending: Vec<CredentialSlot> = credential_slots_needed(&spec).into_iter().rev().collect();
        let mut driver = HttpCallDriver {
            spec,
            now_ms,
            webpush_body: None,
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
            grant_retries: 0,
            pending_delay_ms: None,
            pending_ttl_ms: None,
            principal,
            oauth: None,
        };
        let step = if origin_allowed(&driver.spec.origin, &driver.egress) || driver.spec.origin == OAuthEndpointKind::TARGET {
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

    /// The signed-in user an `oauth-client` call acts for. Set by the kernel before the call's first answer.
    pub fn set_principal(&mut self, principal: Option<String>) {
        self.principal = principal;
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
                    Ok(Some(value)) => self.secrets.hold(slot, value),
                    Ok(None) if matches!(self.spec.auth, Some(HttpAuth::OAuthClient { .. })) => {
                        return self.fail_oauth(OAuthFailureReason::UnconfiguredClient)
                    }
                    Ok(None) if self.cache_slot().as_ref() == Some(&slot) && !self.granted => self.oauth_miss = true,
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
                    Ok(Some(value)) => self.secrets.hold(slot, value),
                    Ok(None) => return self.refuse(HttpRefusal::CredentialMissing(slot.name)),
                    Err(message) => return self.fail(TransportError::Failed(message)),
                }
                self.phase = Phase::GrantSecrets { pending, awaiting: None };
                self.advance()
            }
            (Phase::GrantReply, HttpAnswer::Http(Err(e))) => self.on_transport_error(e),
            (Phase::GrantReply, HttpAnswer::Http(Ok(resp))) => self.on_grant_reply(resp),
            (Phase::GrantRotated, HttpAnswer::SecretRotate(Ok(()))) => self.store_granted_token(),
            (Phase::GrantRotated, HttpAnswer::SecretRotate(Err(message))) => {
                self.fail(TransportError::Failed(message))
            }
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
            (Phase::WebPushEntropy, HttpAnswer::Random(Ok(bytes))) if bytes.len() == 48 => {
                let HttpBodySpec::WebPush(push) = &self.spec.body else { return self.unexpected() };
                let mut key = [0;32]; key.copy_from_slice(&bytes[..32]);
                let mut salt = [0;16]; salt.copy_from_slice(&bytes[32..]);
                if codec::webpush::ephemeral_private_key(&key).is_err() { return HttpStep::Ask(HttpAsk::Random { bytes:48 }) }
                match codec::webpush::encrypt_aes128gcm(&push.p256dh,&push.auth,push.payload.as_bytes(),&key,&salt) {
                    Ok(body) => { self.webpush_body = Some(body); self.send_page() }
                    Err(error) => self.refuse(HttpRefusal::Invalid(error.0)),
                }
            }
            (Phase::WebPushEntropy, HttpAnswer::Random(Ok(bytes))) => self.fail(TransportError::Failed(format!("the host answered {} random bytes, asked for 48",bytes.len()))),
            (Phase::WebPushEntropy, HttpAnswer::Random(Err(message))) => self.fail(TransportError::Failed(message)),
            (Phase::OAuth, HttpAnswer::OAuth(result)) => self.resume_oauth(FlowAnswer::OAuth(result)),
            (Phase::OAuth, HttpAnswer::Http(result)) => self.resume_oauth(FlowAnswer::Http(result)),
            (Phase::OAuth, HttpAnswer::Random(result)) => self.resume_oauth(FlowAnswer::Random(result)),
            (Phase::OAuth, HttpAnswer::Secret(result)) => self.resume_oauth(FlowAnswer::Secret(result)),
            (Phase::Reply, HttpAnswer::Http(Err(e))) => self.on_transport_error(e),
            (Phase::Reply, HttpAnswer::Http(Ok(resp))) => self.on_reply(resp),
            (Phase::Reply, HttpAnswer::Beacon(result)) => self.on_beacon(result),
            _ => self.unexpected(),
        }
    }

    fn exchange(&self) -> Option<&TokenExchange> {
        match &self.spec.auth {
            Some(HttpAuth::TokenExchange(ex)) => Some(ex),
            _ => None,
        }
    }

    fn cache_slot(&self) -> Option<CredentialSlot> {
        self.exchange().map(|ex| ex.store_as.slot())
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
        let payload = self.with_phase(failure_payload(&err));
        self.done(Err(payload))
    }

    fn fail_grant(&mut self, err: TransportError, rejected: Option<&HttpResponse>) -> HttpStep {
        let payload = self.with_phase(failure_payload(&err));
        let payload = match rejected {
            Some(resp) => {
                let payload = set_field(payload, "grantStatus", Value::Number(f64::from(resp.status)));
                set_field(payload, "grantBody", Value::String(resp.text().into_owned()))
            }
            None => payload,
        };
        self.done(Err(payload))
    }

    /// Names the leg a failure belongs to: the grant exchange or the authored request.
    fn with_phase(&self, payload: Value) -> Value {
        let phase = match self.phase {
            Phase::GrantReply | Phase::GrantRotated | Phase::GrantStored => "grant",
            Phase::Reply => "request",
            _ => return payload,
        };
        set_field(payload, "phase", Value::String(phase.to_string()))
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
                None => self.send_grant(self.pending_delay_ms),
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
            .filter(|slot| self.secrets.get(slot).is_none())
            .collect();
        pending.reverse();
        pending
    }

    fn begin_grant(&mut self) -> HttpStep {
        if self.exchange().is_none() {
            return self.unexpected();
        }
        self.grant_retries = 0;
        self.pending_delay_ms = None;
        self.secrets.grant_request = None;
        self.secrets.resource_request = None;
        let pending = self.missing_exchange_slots();
        self.phase = Phase::GrantSecrets { pending, awaiting: None };
        self.advance()
    }

    fn lookup(&self) -> impl Fn(&CredentialSlot) -> Option<String> + '_ {
        move |slot| self.secrets.get(slot).map(str::to_string)
    }

    fn send_grant(&mut self, delay_ms: Option<u64>) -> HttpStep {
        self.pending_delay_ms = delay_ms;
        let Some(auth) = self.spec.auth.clone() else {
            return self.unexpected();
        };
        let pending = self.missing_exchange_slots();
        if !pending.is_empty() {
            self.phase = Phase::GrantSecrets { pending, awaiting: None };
            return self.advance();
        }
        let built = match self.secrets.grant_request.clone() {
            Some(request) => Ok(request),
            None => token_exchange_request(&auth, &self.lookup()),
        };
        let request = match built {
            Ok(request) => request,
            Err(refusal) => return self.refuse(refusal),
        };
        if !origin_allowed(&request.url, &self.egress) {
            let url = request.url;
            return self.refuse(HttpRefusal::EgressDenied(url));
        }
        self.secrets.grant_request = Some(request.clone());
        self.phase = Phase::GrantReply;
        HttpStep::Ask(HttpAsk::Http {
            request,
            delay_ms,
        })
    }

    fn on_grant_reply(&mut self, resp: HttpResponse) -> HttpStep {
        if let Some(step) = self.retry_step(RetryOutcome::Status(resp.status)) {
            return step;
        }
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
            Err(message) => {
                let rejected = (!resp.is_success()).then_some(&resp);
                self.fail_grant(TransportError::Failed(format!("token exchange: {message}")), rejected)
            }
            Ok((token, ttl_ms)) => {
                let rotate = self.exchange().and_then(|ex| ex.rotate.clone());
                let replacement = match &rotate {
                    Some(rotate) => match parse_rotated_token(&resp, &rotate.field) {
                        Ok(value) => Some(value),
                        Err(message) => {
                            return self.fail_grant(TransportError::Failed(format!("token exchange: {message}")), None)
                        }
                    },
                    None => None,
                };
                self.secrets.hold(store.slot(), token);
                self.granted = true;
                self.oauth_miss = false;
                self.pending_delay_ms = None;
                match (rotate, replacement) {
                    (Some(rotate), Some(value)) => {
                        self.secrets.hold(rotate.credential.clone(), value.clone());
                        self.pending_ttl_ms = Some(ttl_ms);
                        self.phase = Phase::GrantRotated;
                        HttpStep::Ask(HttpAsk::SecretRotate {
                            name: rotate.credential.name,
                            value,
                            scope: rotate.credential.scope,
                        })
                    }
                    _ => {
                        self.pending_ttl_ms = Some(ttl_ms);
                        self.store_granted_token()
                    }
                }
            }
        }
    }

    /// Caches the held access token under `storeAs` (after any rotation has been written).
    fn store_granted_token(&mut self) -> HttpStep {
        let Some(store) = self.exchange().map(|ex| ex.store_as.clone()) else {
            return self.unexpected();
        };
        let (Some(token), Some(ttl_ms)) = (self.secrets.get(&store.slot()).map(str::to_string), self.pending_ttl_ms.take()) else {
            return self.unexpected();
        };
        self.phase = Phase::GrantStored;
        HttpStep::Ask(HttpAsk::SecretSet {
            name: store.name,
            value: token,
            ttl_ms,
            scope: store.scope,
        })
    }

    fn send_page(&mut self) -> HttpStep {
        self.send_request(self.pending_delay_ms)
    }

    fn send_request(&mut self, delay_ms: Option<u64>) -> HttpStep {
        self.pending_delay_ms = delay_ms;
        let pending: Vec<CredentialSlot> = credential_slots_needed(&self.spec)
            .into_iter()
            .rev()
            .filter(|slot| self.secrets.get(slot).is_none())
            .collect();
        if !pending.is_empty() {
            self.phase = Phase::Credentials { pending, awaiting: None };
            return self.advance();
        }
        if matches!(self.spec.auth, Some(HttpAuth::OAuthClient { .. })) {
            return self.begin_oauth();
        }
        if matches!(self.spec.body, HttpBodySpec::Multipart { .. }) && self.boundary.is_none() {
            self.phase = Phase::Boundary;
            return HttpStep::Ask(HttpAsk::Random { bytes: BOUNDARY_BYTES });
        }
        if matches!(self.spec.body, HttpBodySpec::WebPush(_)) && self.webpush_body.is_none() {
            self.phase = Phase::WebPushEntropy;
            return HttpStep::Ask(HttpAsk::Random { bytes:48 });
        }
        let built = match self.secrets.resource_request.clone() {
            Some(request) => Ok(request),
            None => build_request_with_metadata(
                &self.spec,
                &self.lookup(),
                &self.egress,
                self.page_token.as_deref(),
                self.boundary.as_deref(),
                self.now_ms,
                self.webpush_body.as_deref(),
            ),
        };
        let request = match built {
            Ok(request) => request,
            Err(refusal) => return self.refuse(refusal),
        };
        self.secrets.resource_request = Some(request.clone());
        if self.spec.presign.is_some() {
            let mut payload = ValueMap::new();
            payload.insert("url".into(), Value::String(request.url));
            return self.done(Ok(Value::Object(payload)));
        }
        self.phase = Phase::Reply;
        if self.spec.delivery == HttpDelivery::Beacon {
            return HttpStep::Ask(HttpAsk::Beacon { request });
        }
        match self.spec.stream {
            Some(framing) => HttpStep::Ask(HttpAsk::HttpStream { request, framing }),
            None => HttpStep::Ask(HttpAsk::Http { request, delay_ms }),
        }
    }

    fn fail_oauth(&mut self, reason: OAuthFailureReason) -> HttpStep {
        self.done(Err(oauth_failure_payload(reason.message(), reason.code(), reason.as_str())))
    }

    /// The call's declaration and resolved credentials; `selected` is the `clients` entry a taken grant chose.
    fn oauth_env(&self, selected: Option<usize>) -> Option<OAuthCall> {
        let Some(HttpAuth::OAuthClient { op, client, clients, inputs }) = &self.spec.auth else {
            return None;
        };
        let effective = match selected {
            Some(index) => clients.get(index)?,
            None => client,
        };
        let (id, secret) = if effective.mode == OAuthMode::Mock {
            (String::new(), String::new())
        } else if !clients.is_empty() {
            (
                self.secrets.oauth.client_id.clone().unwrap_or_default(),
                self.secrets.oauth.client_secret.clone().unwrap_or_default(),
            )
        } else {
            let id = self.secrets.get(&effective.client_id)?.to_string();
            let secret = effective
                .client_secret
                .as_ref()
                .and_then(|slot| self.secrets.get(slot))
                .unwrap_or_default()
                .to_string();
            (id, secret)
        };
        Some(OAuthCall { op: *op, client: effective.clone(), clients: clients.clone(), inputs: inputs.clone(), id, secret })
    }

    fn begin_oauth(&mut self) -> HttpStep {
        let Some(call) = self.oauth_env(None) else {
            return self.fail_oauth(OAuthFailureReason::UnconfiguredClient);
        };
        if call.clients.is_empty() && call.client.mode == OAuthMode::Real && call.client.client_secret.is_some() && call.secret.is_empty() {
            return self.fail_oauth(OAuthFailureReason::UnconfiguredClient);
        }
        self.phase = Phase::OAuth;
        let url = format!("{}{}", self.spec.origin, self.spec.path);
        let env = OAuthEnv {
            op: call.op,
            client: &call.client,
            clients: &call.clients,
            inputs: &call.inputs,
            client_id: &call.id,
            client_secret: &call.secret,
            egress: &self.egress,
            authored_url: &url,
        };
        let mut flow = OAuthFlow::new(self.principal.clone());
        let step = flow.start(&env, &mut self.secrets.oauth);
        self.oauth = Some(flow);
        self.oauth_step(step)
    }

    fn resume_oauth(&mut self, answer: FlowAnswer) -> HttpStep {
        let Some(mut flow) = self.oauth.take() else {
            return self.unexpected();
        };
        let Some(call) = self.oauth_env(flow.selected()) else {
            return self.fail_oauth(OAuthFailureReason::StoreUnavailable);
        };
        let url = format!("{}{}", self.spec.origin, self.spec.path);
        let env = OAuthEnv {
            op: call.op,
            client: &call.client,
            clients: &call.clients,
            inputs: &call.inputs,
            client_id: &call.id,
            client_secret: &call.secret,
            egress: &self.egress,
            authored_url: &url,
        };
        let step = flow.resume(&env, &mut self.secrets.oauth, answer);
        self.oauth = Some(flow);
        self.oauth_step(step)
    }

    fn oauth_step(&mut self, step: FlowStep) -> HttpStep {
        match step {
            FlowStep::Ask(FlowAsk::OAuth(ask)) => HttpStep::Ask(HttpAsk::OAuth { ask }),
            FlowStep::Ask(FlowAsk::Http(request)) => HttpStep::Ask(HttpAsk::Http { request, delay_ms: None }),
            FlowStep::Ask(FlowAsk::Random { bytes }) => HttpStep::Ask(HttpAsk::Random { bytes }),
            FlowStep::Ask(FlowAsk::Secret { name, scope }) => HttpStep::Ask(HttpAsk::Secret { name, scope }),
            FlowStep::Done(Ok(payload)) => self.done(Ok(payload)),
            FlowStep::Done(Err(FlowFailure::Reason(reason))) => self.fail_oauth(reason),
            FlowStep::Done(Err(FlowFailure::Refused(refusal))) => self.refuse(refusal),
            FlowStep::Done(Err(FlowFailure::Transport(error))) => self.fail(error),
        }
    }

    fn next_retry_delay(&mut self, outcome: RetryOutcome) -> Option<u64> {
        let (retry, attempts) = match self.phase {
            Phase::GrantReply => {
                let Some(HttpAuth::TokenExchange(exchange)) = &self.spec.auth else {
                    return None;
                };
                (exchange.grant_request.as_ref()?.retry.as_ref()?, &mut self.grant_retries)
            }
            Phase::Reply => (self.spec.retry.as_ref()?, &mut self.retries),
            _ => return None,
        };
        retry.next_delay(attempts, outcome)
    }

    fn retry_step(&mut self, outcome: RetryOutcome) -> Option<HttpStep> {
        let grant = matches!(self.phase, Phase::GrantReply);
        let delay = self.next_retry_delay(outcome)?;
        Some(if grant { self.send_grant(Some(delay)) } else { self.send_request(Some(delay)) })
    }

    fn on_transport_error(&mut self, err: TransportError) -> HttpStep {
        let outcome = match &err {
            TransportError::TimedOut => Some(RetryOutcome::Timeout),
            TransportError::Failed(_) => Some(RetryOutcome::Failed),
            TransportError::Refused(_) => None,
        };
        if let Some(step) = outcome.and_then(|outcome| self.retry_step(outcome)) {
            return step;
        }
        self.fail(err)
    }

    fn on_beacon(&mut self, result: Result<BeaconOutcome, TransportError>) -> HttpStep {
        let (sent, via, status) = match result {
            Ok(BeaconOutcome::Unavailable { message }) => return self.done(Err(unavailable_payload(&message))),
            Ok(BeaconOutcome::Queued) => (true, "queue", Value::Null),
            Ok(BeaconOutcome::Fetched { status }) => ((200..300).contains(&status), "fetch", Value::Number(f64::from(status))),
            Err(TransportError::TimedOut | TransportError::Failed(_)) => (false, "fetch", Value::Null),
            Err(err @ TransportError::Refused(_)) => return self.fail(err),
        };
        self.done(Ok(object(vec![
            ("sent", Value::Bool(sent)),
            ("via", Value::String(via.to_string())),
            ("status", status),
        ])))
    }

    fn on_reply(&mut self, resp: HttpResponse) -> HttpStep {
        if let Some(step) = self.retry_step(RetryOutcome::Status(resp.status)) {
            return step;
        }
        if resp.status == 401 && !self.refreshed && self.exchange().is_some_and(|ex| ex.on_unauthorized == OnUnauthorized::Refresh) {
            if let Some(slot) = self.cache_slot() {
                self.refreshed = true;
                self.secrets.forget(&slot);
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
                self.pending_delay_ms = None;
                self.secrets.resource_request = None;
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
