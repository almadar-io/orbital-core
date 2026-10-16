use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use serde::Deserialize;
use serde::Serialize;
use crate::value::Value;
use crate::value::ValueMap;
use crate::ir::types::OirType;


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

/// Whose credential a slot names: the app's, or the signed-in principal's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretScope {
    #[default]
    App,
    User,
}

impl SecretScope {
    pub fn is_app(&self) -> bool {
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
    pub const ENTRIES: &'static str = "retry.on entries are 1xx, 408, 5xx, 429, timeout, failed or an integer status code from 100 through 599";

    pub fn status(code: u64) -> Option<Self> {
        (100..=599).contains(&code).then_some(Self::Status(code as u16))
    }

    pub fn tag(self) -> Option<&'static str> {
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

    pub fn from_tag(tag: &str) -> Option<Self> {
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

    pub fn matches(self, outcome: RetryOutcome) -> bool {
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
pub enum RetryOutcome {
    Status(u16),
    Timeout,
    Failed,
}

impl HttpRetry {
    pub const KEYS: &'static [&'static str] = &["max", "on", "backoffMs", "scheduleMs", "noResponseMax"];

    pub fn next_delay(&self, attempt: &mut u32, outcome: RetryOutcome) -> Option<u64> {
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

pub fn invalid<T>(message: impl Into<String>) -> Result<T, HttpRefusal> {
    Err(HttpRefusal::Invalid(message.into()))
}

pub fn closed_keys(map: &ValueMap, allowed: &[&str], what: &str) -> Result<(), HttpRefusal> {
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(key) => invalid(format!("{what} has no key `{key}`")),
        None => Ok(()),
    }
}

pub fn text_field(map: &ValueMap, key: &str, what: &str) -> Result<String, HttpRefusal> {
    match map.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => invalid(format!("{what}.{key} must be a string")),
    }
}

pub fn parse_slot_object(map: &ValueMap, what: &str) -> Result<CredentialSlot, HttpRefusal> {
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

pub fn whole_number(value: Option<&Value>, what: &str) -> Result<Option<u64>, HttpRefusal> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) if *n >= 0.0 && n.fract() == 0.0 && *n <= u64::MAX as f64 => Ok(Some(*n as u64)),
        Some(_) => invalid(format!("{what} must be a whole number")),
    }
}

pub fn is_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric()
        || matches!(byte, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'))
}

pub fn is_header_value(value: &str) -> bool {
    value.bytes().all(|byte| byte == b'\t' || (byte >= b' ' && byte != 127))
}

pub fn check_header(name: &str, value: &str) -> Result<(), HttpRefusal> {
    if !is_header_name(name) {
        return invalid("invalid HTTP header name");
    }
    if !is_header_value(value) {
        return invalid(format!("header `{name}` must be single-line without control characters"));
    }
    Ok(())
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
    pub fn whole(&self, what: &str, missing: &str) -> Result<Option<u64>, HttpRefusal> {
        match self {
            Self::Literal(value) => whole_number(Some(value), what)?
                .map(Some)
                .ok_or_else(|| HttpRefusal::Invalid(missing.into())),
            Self::Typed(ty) if crate::type_compat::oir_type_satisfies(ty, &OirType::Number) => Ok(None),
            Self::Unresolved => Ok(None),
            _ => invalid(format!("{what} must be a whole number")),
        }
    }

    pub fn list(&self, inners: &[OirType], message: &str) -> Result<Option<Vec<Self>>, HttpRefusal> {
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
                    crate::type_compat::oir_type_satisfies(ty, &OirType::Array { inner: Box::new(inner.clone()) })
                }) =>
            {
                Ok(None)
            }
            Self::Unresolved => Ok(None),
            _ => invalid(message),
        }
    }

    pub fn rule(&self) -> Result<Option<RetryOn>, HttpRefusal> {
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
                if crate::type_compat::oir_type_satisfies(ty, &OirType::String)
                    || crate::type_compat::oir_type_satisfies(ty, &OirType::Number) =>
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

pub fn credential(
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    slot: &CredentialSlot,
) -> Result<String, HttpRefusal> {
    secrets(slot).ok_or_else(|| HttpRefusal::CredentialMissing(slot.name.clone()))
}

pub fn v4_public(ip: Ipv4Addr) -> bool {
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

pub fn v6_public(ip: Ipv6Addr) -> bool {
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