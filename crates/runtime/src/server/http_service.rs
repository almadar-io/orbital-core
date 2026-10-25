//! The HTTP seam, shared by every provider and by the kernel's sans-I/O `IoRequest::Http`: the `http` effect
//! (`runtime::http_request`) builds a request and reads a response; the transport moves the bytes
//! (`docs/Almadar_Embedded_Kernel.md`, "The `http` effect").
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::borrow::Cow;
use std::time::Duration;

#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The bytes on the wire; empty for no body.
    pub body: Vec<u8>,
    pub timeout: Option<Duration>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A request carries credentials (an `Authorization` header, an api-key query, a grant body), so its `Debug`
/// shows the method, the URL without its query, the header names and the body length, never a value.
impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let url = self.url.split(['?', '#']).next().unwrap_or(&self.url);
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &url)
            .field("header_names", &names)
            .field("body_len", &self.body.len())
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// A reply can carry a minted token (an OAuth reply) or a cookie, so its `Debug` shows the status, the header
/// names and the body length.
impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("header_names", &names)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl HttpRequest {
    /// The body as UTF-8 text, each invalid sequence becoming U+FFFD.
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// A request whose body is `text`, as its UTF-8 bytes.
    pub fn with_text_body(mut self, text: &str) -> Self {
        self.body = text.as_bytes().to_vec();
        self
    }
}

impl HttpResponse {
    /// The body as fetch's `res.text()` reads it: UTF-8, each invalid sequence becoming U+FFFD.
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn status_text(&self) -> &'static str {
        reason_phrase(self.status)
    }
}

/// The request never produced a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    TimedOut,
    Failed(String),
    /// The host refused before sending (a non-public resolved address, a redirect it does not follow).
    Refused(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => f.write_str("request timed out"),
            Self::Failed(message) | Self::Refused(message) => f.write_str(message),
        }
    }
}

pub trait Transport: Send + Sync {
    /// A non-2xx status is a response, not an error.
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;

    /// Deliver body bytes as they arrive; the returned response retains status and headers, with no body.
    /// In-memory transports may deliver their canned body as one chunk.
    fn stream(
        &self,
        request: &HttpRequest,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<(), TransportError>,
    ) -> Result<HttpResponse, TransportError> {
        let mut response = self.send(request)?;
        on_chunk(&response.body)?;
        response.body.clear();
        Ok(response)
    }
}

/// The standard reason phrase (`Response.statusText` in fetch) for a status code.
pub fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// A credential by name; `None` when the host holds no value. `Credentials` implements it.
pub trait SecretSource: Send + Sync {
    fn secret(&self, name: &str) -> Result<Option<String>, String>;
    /// Keep a derived secret (an OAuth token) until `ttl_ms` has passed.
    fn store_secret(&self, _name: &str, _value: &str, _ttl_ms: u64) -> Result<(), String> {
        Err("this host keeps no derived secrets".to_string())
    }
    /// The credential `name` of `principal` (a signed-in user's id), or the app-wide one when `None`.
    fn secret_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        match principal {
            None => self.secret(name),
            Some(_) => Err(NO_USER_CREDENTIALS.to_string()),
        }
    }
    /// The non-secret account label stored beside the credential `name` of `principal` (or the app-wide one), `None`
    /// when it has none or this host keeps none.
    fn label_of(&self, _name: &str, _principal: Option<&str>) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// [`Self::store_secret`] for `principal`'s credential.
    fn store_secret_of(
        &self,
        name: &str,
        value: &str,
        ttl_ms: u64,
        principal: Option<&str>,
    ) -> Result<(), String> {
        match principal {
            None => self.store_secret(name, value, ttl_ms),
            Some(_) => Err(NO_USER_CREDENTIALS.to_string()),
        }
    }
    /// Replace the credential `name` of `principal` (or the app-wide one when `None`) with `value`, durably: a
    /// rotated refresh token, unlike [`Self::store_secret_of`]'s expiring derived secret.
    fn rotate_secret_of(&self, _name: &str, _value: &str, _principal: Option<&str>) -> Result<(), String> {
        Err("this host keeps no rotatable credentials".to_string())
    }
    /// Answer what an `oauth-client` flow asks a host to keep (pending grants, connections, metadata, clock).
    fn oauth_of(
        &self,
        _ask: &orbital_core_base::oauth_ask::OAuthAsk,
        _principal: Option<&str>,
    ) -> Result<orbital_core_base::oauth_ask::OAuthReply, String> {
        Err("this host keeps no OAuth state".to_string())
    }
    /// Drop the credential `name` of `principal` (or the app-wide one when `None`).
    fn forget_of(&self, _name: &str, _principal: Option<&str>) -> Result<(), String> {
        Err("this host keeps no credentials to forget".to_string())
    }
    /// Keep `value` as the credential `name` of `principal` (or the app-wide one when `None`) until it is forgotten:
    /// a durable upsert, unlike [`Self::store_secret_of`]'s expiring derived secret and [`Self::rotate_secret_of`]'s
    /// replacement of a credential that already exists.
    fn persist_secret_of(&self, _name: &str, _value: &str, _principal: Option<&str>) -> Result<(), String> {
        Err("this host keeps no durable credentials".to_string())
    }
    /// Remove and return the derived secret `name` of `principal` (or the app-wide one when `None`): the one read
    /// of a one-use value such as a pending OAuth grant.
    fn take_secret_of(&self, _name: &str, _principal: Option<&str>) -> Result<Option<String>, String> {
        Err("this host keeps no derived secrets to take".to_string())
    }
    /// Whether [`Self::persist_secret_of`] keeps a value on this host.
    fn durable_credentials(&self) -> bool {
        false
    }
    /// The credential store's state (can it encrypt, which rows it holds), never a value; `None` when the host holds
    /// no store.
    fn credential_store_list(&self) -> Result<Option<crate::credential_admin::StoreListing>, String> {
        Ok(None)
    }
    /// The stored value of credential `name`, from the store only.
    fn credential_store_read(&self, _name: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Encrypt `value` into the store as credential `name` of `service`.
    fn credential_store_write(&self, _service: &str, _name: &str, _value: &str) -> Result<(), String> {
        Err(orbital_core_base::credential_admin::NO_STORE.to_string())
    }
    /// Delete the stored row of credential `name`; whether one existed.
    fn credential_store_remove(&self, _name: &str) -> Result<bool, String> {
        Err(orbital_core_base::credential_admin::NO_STORE.to_string())
    }
    /// Re-encrypt every stored row under the current master key.
    fn credential_store_rotate(&self) -> Result<crate::credential_admin::RotateCounts, String> {
        Err(orbital_core_base::credential_admin::NO_STORE.to_string())
    }
    /// The integrations mode flag (`mock` | `real`); `None` when the host keeps none.
    fn credential_mode(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Switch the integrations mode flag; the host may refuse. The mode in force afterwards.
    fn set_credential_mode(&self, _mode: &str) -> Result<String, String> {
        Err(orbital_core_base::credential_admin::NO_MANAGER.to_string())
    }
}

pub fn credential_status_connected(value: Option<&str>, principal: Option<&str>) -> bool {
    value.is_some_and(|value| principal.is_some() || !value.is_empty())
}

const NO_USER_CREDENTIALS: &str = "this host keeps no user-scoped credentials";
