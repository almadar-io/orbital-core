//! The declaration an `oauth-client` auth (and `oauth-authorize`) carries: which operation, which client, which
//! inputs. Secret slots (`clientId`, `clientSecret`) are credential NAMES the host resolves, never values.

use serde::{Deserialize, Serialize};

use super::ask::OAuthProvider;
use super::ClientAuth;
use crate::runtime::http_request::CredentialSlot;

/// The closed set of operations an `oauth-client` auth performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthOp {
    Discovery,
    Code,
    Connect,
    Refresh,
    Revoke,
    Userinfo,
}

impl OAuthOp {
    pub const WORDS: &'static [&'static str] = &["discovery", "code", "connect", "refresh", "revoke", "userinfo"];

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "discovery" => Some(Self::Discovery),
            "code" => Some(Self::Code),
            "connect" => Some(Self::Connect),
            "refresh" => Some(Self::Refresh),
            "revoke" => Some(Self::Revoke),
            "userinfo" => Some(Self::Userinfo),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Code => "code",
            Self::Connect => "connect",
            Self::Refresh => "refresh",
            Self::Revoke => "revoke",
            Self::Userinfo => "userinfo",
        }
    }

    /// The `oauth-endpoint` word the op is sent to; `discovery` goes to a literal origin and path.
    pub fn endpoint(self) -> Option<OAuthEndpointKind> {
        match self {
            Self::Discovery => None,
            Self::Code | Self::Connect | Self::Refresh => Some(OAuthEndpointKind::Token),
            Self::Revoke => Some(OAuthEndpointKind::Revocation),
            Self::Userinfo => Some(OAuthEndpointKind::Userinfo),
        }
    }

    /// The input keys the op requires, then those it may carry.
    pub fn inputs(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::Discovery => (&[], &[]),
            Self::Code => (&["code", "state"], &["subject"]),
            Self::Connect => (&["code", "state", "provider"], &[]),
            Self::Refresh => (&["refreshToken"], &[]),
            Self::Revoke => (&["token"], &[]),
            Self::Userinfo => (&["accessToken"], &[]),
        }
    }
}

/// The `oauth-endpoint` target words: the URL comes from the client's metadata, not from the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthEndpointKind {
    Token,
    Revocation,
    Userinfo,
}

impl OAuthEndpointKind {
    pub const WORDS: &'static [&'static str] = &["token", "revocation", "userinfo"];
    pub const TARGET: &'static str = "oauth-endpoint";

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "token" => Some(Self::Token),
            "revocation" => Some(Self::Revocation),
            "userinfo" => Some(Self::Userinfo),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::Revocation => "revocation",
            Self::Userinfo => "userinfo",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthMode {
    Real,
    Mock,
}

/// Where the authorization server is described: a discovered issuer, or the endpoints themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OAuthServerDecl {
    Issuer(String),
    Endpoints {
        authorization: String,
        token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revocation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        userinfo: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientDecl {
    pub tag: OAuthProvider,
    pub mode: OAuthMode,
    pub server: OAuthServerDecl,
    pub client_id: CredentialSlot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<CredentialSlot>,
    pub client_auth: ClientAuth,
}

impl OAuthClientDecl {
    pub const KEYS: &'static [&'static str] = &["tag", "mode", "issuer", "endpoints", "clientId", "clientSecret", "clientAuth"];
}

/// The caller-supplied inputs of one op (request inputs, not credentials).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<OAuthProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
}
