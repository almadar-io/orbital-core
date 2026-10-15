//! The HTTP seam, shared by every provider and by the kernel's sans-I/O `IoRequest::Http`: the `http` effect
//! (`runtime::http_request`) builds a request and reads a response; the transport moves the bytes
//! (`docs/Almadar_Embedded_Kernel.md`, "The `http` effect").

use std::borrow::Cow;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The bytes on the wire; empty for no body.
    pub body: Vec<u8>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
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
}

pub trait Transport: Send + Sync {
    /// A non-2xx status is a response, not an error.
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;
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

/// A credential by name; `None` when the host holds no value. `Credentials` (orbital-integrations) implements it.
pub trait SecretSource: Send + Sync {
    fn secret(&self, name: &str) -> Result<Option<String>, String>;
    /// Keep a derived secret (an OAuth token) until `ttl_ms` has passed.
    fn store_secret(&self, _name: &str, _value: &str, _ttl_ms: u64) -> Result<(), String> {
        Err("this host keeps no derived secrets".to_string())
    }
}
