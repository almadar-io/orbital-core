//! The `http` effect's request builder and its sans-I/O step machine (`docs/Almadar_Embedded_Kernel.md`,
//! "The `http` effect", H.3, H.3b, H.3c). Pure: no clock, env, randomness or I/O. The kernel, the JS interpreter
//! (through wasm) and the generated TS shell only answer the driver's asks.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;


use std::time::Duration;

use serde::{Deserialize, Serialize};

use orbital_core_base::codec;
use orbital_core_base::json_value::value_to_json;
use orbital_core_eval::evaluator::context::BindingContext;
use orbital_core_eval::evaluator::Evaluator;
use orbital_core_base::value::{Value, ValueMap};

use orbital_core_base::oauth_client::{OAuthClientDecl, OAuthEndpointKind, OAuthInputs, OAuthMode, OAuthOp, OAuthServerDecl};
use orbital_core_base::oauth_ask::{OAuthFailureReason, OAuthProvider};
use crate::oauth_flow::{FlowAnswer, FlowAsk, FlowFailure, FlowStep, OAuthEnv, OAuthFlow, OAuthHeld};
use crate::http_service::{HttpRequest, HttpResponse, TransportError};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::str_utils::js_number_string;

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
        alg: orbital_core_base::jwt_alg::JwtAlg,
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
        /// Text prepended to the digest in the header value (`sha256=`); `None` sends the bare digest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
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
    /// How long the host waits before it sends the first request; a retry or a next page has its own delay.
    #[serde(default)]
    pub delay_ms: Option<u64>,
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

pub use crate::driver::{DriverAnswer as HttpAnswer, DriverAsk as HttpAsk, DriverStep as HttpStep};
#[allow(unused_imports)]
use orbital_core_base::http_wire::{RetryOutcome, invalid, closed_keys, text_field, parse_slot_object, whole_number, check_header, credential, v4_public, v6_public};
#[allow(unused_imports)]
pub use orbital_core_base::http_wire::{tcp_default_port, parse_egress_entry, split_tcp_url, HttpMethod, SecretScope, CredentialSlot, RetryOn, RetryDelay, HttpRetry, RetryOperand, GrantRequest, HttpRefusal, OriginParts, split_url, parse_origin, parse_origin_prefix, parse_credential_status_slot, is_header_name, is_header_value, parse_http_headers, parse_timeout_ms, parse_delay_ms, parse_grant_request, parse_retry, is_public_address};

mod parse;
mod auth;
mod request;
mod stream;
mod driver;
use parse::*;
use auth::*;
use request::*;
pub use auth::parse_oauth_client;
pub(crate) use driver::with_key;
pub use driver::{HeldSecrets, HttpCallDriver};
pub use parse::{
    credential_slots_needed, credentials_needed, origin_allowed, parse_beacon_call,
    parse_http_call, tcp_target_allowed, token_cache_name,
};
pub use request::{
    build_request, build_request_with_boundary, failure_payload, next_link, oauth_failure_payload,
    parse_token_reply, refusal_payload, reply_payload, token_exchange_request, unavailable_payload,
};
pub use stream::{frame_stream, frame_stream_finish, stream_events, stream_finish, StreamDecoder};
