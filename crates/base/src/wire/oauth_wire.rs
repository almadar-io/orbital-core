#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use std::fmt;


/// What a failed check says about the exchange, so callers classify without reading the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OidcErrorKind {
    /// The server metadata or a URL it names is unusable.
    Metadata,
    /// The server answered with a rejection (status, challenge, OAuth error body).
    Rejected,
    /// The server's reply is malformed or fails a claim check.
    Invalid,
    /// UserInfo `sub` is not the subject the tokens were issued to.
    Mismatch,
    /// A caller-supplied input is unusable (empty token, bad URL).
    Input,
}

/// Server-held state of one authorization between `authorize` and the code exchange.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub provider: String,
    pub redirect_uri: String,
    pub verifier: String,
    pub subject: Option<String>,
    /// The scopes the authorization asked for; a mock grant hands them back with its tokens.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl fmt::Debug for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending")
            .field("provider", &self.provider)
            .field("redirect_uri", &self.redirect_uri)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

/// How the client proves itself at the token, revocation endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientAuth {
    /// `client_secret_post`: id and secret in the form body.
    #[default]
    Post,
    /// `client_secret_basic`: an `Authorization: Basic` header.
    Basic,
    /// A public client: only `client_id` in the body.
    None,
}