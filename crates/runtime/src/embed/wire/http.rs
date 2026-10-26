//! HTTP requests, responses, bodies and failures on the wire.

use super::*;


/// A request or reply body on the wire: text as text, any other bytes as base64, so a binary body survives JSON.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HttpBody {
    #[default]
    Empty,
    Text {
        text: String,
    },
    Base64 {
        data: String,
    },
}

impl std::fmt::Debug for HttpBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Text { text } => write!(f, "Text({} bytes)", text.len()),
            Self::Base64 { data } => write!(f, "Base64({} chars)", data.len()),
        }
    }
}

impl From<&[u8]> for HttpBody {
    fn from(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::Empty;
        }
        match std::str::from_utf8(bytes) {
            Ok(text) => Self::Text {
                text: text.to_string(),
            },
            Err(_) => Self::Base64 {
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        }
    }
}

impl From<HttpBody> for Vec<u8> {
    fn from(body: HttpBody) -> Self {
        match body {
            HttpBody::Empty => Vec::new(),
            HttpBody::Text { text } => text.into_bytes(),
            HttpBody::Base64 { data } => base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap_or_default(),
        }
    }
}

/// `HttpRequest` on the wire.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequestWire {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: HttpBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl std::fmt::Debug for HttpRequestWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&HttpRequest::from(self.clone()), f)
    }
}

impl From<&HttpRequest> for HttpRequestWire {
    fn from(r: &HttpRequest) -> Self {
        Self {
            method: r.method.clone(),
            url: r.url.clone(),
            headers: r.headers.clone(),
            body: HttpBody::from(r.body.as_slice()),
            timeout_ms: r.timeout.map(|d| d.as_millis() as u64),
        }
    }
}

impl From<HttpRequestWire> for HttpRequest {
    fn from(w: HttpRequestWire) -> Self {
        Self {
            method: w.method,
            url: w.url,
            headers: w.headers,
            body: w.body.into(),
            timeout: w.timeout_ms.map(std::time::Duration::from_millis),
        }
    }
}

/// `HttpResponse` on the wire. A non-2xx status is a reply, not a failure.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponseWire {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: HttpBody,
}

impl std::fmt::Debug for HttpResponseWire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&HttpResponse::from(self.clone()), f)
    }
}

impl From<&HttpResponse> for HttpResponseWire {
    fn from(r: &HttpResponse) -> Self {
        Self {
            status: r.status,
            headers: r.headers.clone(),
            body: HttpBody::from(r.body.as_slice()),
        }
    }
}

impl From<HttpResponseWire> for HttpResponse {
    fn from(w: HttpResponseWire) -> Self {
        Self {
            status: w.status,
            headers: w.headers,
            body: w.body.into(),
        }
    }
}

/// `TransportError` on the wire: the request produced no reply.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HttpFailureWire {
    TimedOut,
    Failed { message: String },
    Refused { message: String },
}

impl From<&TransportError> for HttpFailureWire {
    fn from(e: &TransportError) -> Self {
        match e {
            TransportError::TimedOut => Self::TimedOut,
            TransportError::Failed(message) => Self::Failed {
                message: message.clone(),
            },
            TransportError::Refused(message) => Self::Refused {
                message: message.clone(),
            },
        }
    }
}

impl From<HttpFailureWire> for TransportError {
    fn from(w: HttpFailureWire) -> Self {
        match w {
            HttpFailureWire::TimedOut => Self::TimedOut,
            HttpFailureWire::Failed { message } => Self::Failed(message),
            HttpFailureWire::Refused { message } => Self::Refused(message),
        }
    }
}
