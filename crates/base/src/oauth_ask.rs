//! What an OAuth flow asks its host to keep, and how a flow fails. The core `http` driver owns every request
//! (`oauth-client` auth); the host only persists: pending grants, provider connections, the subject index,
//! discovered metadata and the clock. Nothing here sends a byte (`docs/Almadar_Embedded_Kernel.md`, "OAuth").

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::oauth_wire::Pending;

/// The providers a connection can be kept for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Google,
    Github,
    Auth0,
}

impl OAuthProvider {
    pub const WORDS: &'static [&'static str] = &["google", "github", "auth0"];

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "google" => Some(Self::Google),
            "github" => Some(Self::Github),
            "auth0" => Some(Self::Auth0),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Github => "github",
            Self::Auth0 => "auth0",
        }
    }
}

/// A caller's connection to an OAuth provider (`OAuthConnectionRow`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthConnectionRow {
    pub provider: OAuthProvider,
    pub connected: bool,
    pub account: String,
    pub avatar_url: String,
    pub scope: Vec<String>,
    pub connected_at: String,
}

/// The exchanged tokens the host keeps for a connection; the program never reads them back.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthTokenGrant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: f64,
    pub token_type: String,
    pub scope: Option<Vec<String>>,
    pub subject: Option<String>,
}

impl fmt::Debug for OAuthTokenGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokenGrant")
            .field("expires_in", &self.expires_in)
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// One thing an OAuth flow needs its host to do. The kernel sets the `principal` of the carrying request.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OAuthAsk {
    /// Keep the pending grant under `state` for `ttl_ms` of the host's clock.
    PendingPut { state: String, grant: Pending, ttl_ms: u64 },
    /// Consume the grant under `state`, once.
    PendingTake { state: String },
    /// Whether the host has a connection store.
    ConnectionAvailable,
    ConnectionSave { provider: OAuthProvider, grant: OAuthTokenGrant },
    ConnectionStatus { provider: OAuthProvider },
    ConnectionRemove { provider: OAuthProvider },
    SubjectRemember { access_token: String, subject: String },
    SubjectLookup { access_token: String },
    SubjectForget { access_token: String },
    MetadataGet { issuer: String },
    MetadataPut { issuer: String, metadata: Map<String, Value> },
    /// The host clock, in epoch milliseconds.
    Clock,
    /// Mock mode: keep a minted token pair. The access token expires at `expires_at_ms`; the refresh token is indexed to it.
    MockTokenPut { access_token: String, refresh_token: String, expires_at_ms: i64, provider: OAuthProvider },
    /// Mock mode: exchange the access token behind `refresh_token` for `new_access_token`, carrying its subject
    /// over; answers whether the refresh token was known.
    MockTokenRefresh { refresh_token: String, new_access_token: String, expires_at_ms: i64 },
    /// Mock mode: drop an access or refresh token and its subject; answers whether either was known.
    MockTokenRevoke { token: String },
    /// Mock mode: whether the access token is held, expired (and is dropped) or valid, by the host clock; a valid one
    /// answers the provider that issued it.
    MockTokenCheck { access_token: String },
}

impl fmt::Debug for OAuthAsk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const HIDDEN: &str = "<redacted>";
        match self {
            Self::PendingPut { ttl_ms, .. } => f
                .debug_struct("PendingPut")
                .field("state", &HIDDEN)
                .field("grant", &HIDDEN)
                .field("ttl_ms", ttl_ms)
                .finish(),
            Self::PendingTake { .. } => f.debug_struct("PendingTake").field("state", &HIDDEN).finish(),
            Self::ConnectionAvailable => f.write_str("ConnectionAvailable"),
            Self::ConnectionSave { provider, .. } => f
                .debug_struct("ConnectionSave")
                .field("provider", provider)
                .field("grant", &HIDDEN)
                .finish(),
            Self::ConnectionStatus { provider } => f.debug_struct("ConnectionStatus").field("provider", provider).finish(),
            Self::ConnectionRemove { provider } => f.debug_struct("ConnectionRemove").field("provider", provider).finish(),
            Self::SubjectRemember { .. } => f.debug_struct("SubjectRemember").field("token", &HIDDEN).finish(),
            Self::SubjectLookup { .. } => f.debug_struct("SubjectLookup").field("token", &HIDDEN).finish(),
            Self::SubjectForget { .. } => f.debug_struct("SubjectForget").field("token", &HIDDEN).finish(),
            Self::MetadataGet { issuer } => f.debug_struct("MetadataGet").field("issuer", issuer).finish(),
            Self::MetadataPut { issuer, .. } => f.debug_struct("MetadataPut").field("issuer", issuer).finish(),
            Self::Clock => f.write_str("Clock"),
            Self::MockTokenPut { expires_at_ms, .. } => f.debug_struct("MockTokenPut").field("tokens", &HIDDEN).field("expires_at_ms", expires_at_ms).finish(),
            Self::MockTokenRefresh { expires_at_ms, .. } => f.debug_struct("MockTokenRefresh").field("tokens", &HIDDEN).field("expires_at_ms", expires_at_ms).finish(),
            Self::MockTokenRevoke { .. } => f.debug_struct("MockTokenRevoke").field("token", &HIDDEN).finish(),
            Self::MockTokenCheck { .. } => f.debug_struct("MockTokenCheck").field("token", &HIDDEN).finish(),
        }
    }
}

/// What taking a pending grant found.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "found", rename_all = "camelCase")]
pub enum PendingTake {
    Missing,
    Expired,
    Grant { grant: Pending },
}

impl fmt::Debug for PendingTake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("Missing"),
            Self::Expired => f.write_str("Expired"),
            Self::Grant { .. } => f.write_str("Grant(<redacted>)"),
        }
    }
}

/// What a mock access token's check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MockTokenState {
    Missing,
    Expired,
    Valid,
}

/// The host's answer to an [`OAuthAsk`]; the variant is closed per ask ([`OAuthAsk::accepts`]).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OAuthReply {
    Done,
    Flag { value: bool },
    Taken { taken: PendingTake },
    Row { row: OAuthConnectionRow },
    Subject { subject: Option<String> },
    Metadata { metadata: Option<Map<String, Value>> },
    Millis { value: i64 },
    MockToken { state: MockTokenState, provider: Option<OAuthProvider> },
}

impl fmt::Debug for OAuthReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Done => f.write_str("Done"),
            Self::Flag { value } => f.debug_tuple("Flag").field(value).finish(),
            Self::Taken { taken } => f.debug_tuple("Taken").field(taken).finish(),
            Self::Row { row } => f.debug_tuple("Row").field(row).finish(),
            Self::Subject { subject } => f.debug_tuple("Subject").field(&subject.as_ref().map(|_| "<redacted>")).finish(),
            Self::Metadata { metadata } => f.debug_tuple("Metadata").field(&metadata.is_some()).finish(),
            Self::Millis { value } => f.debug_tuple("Millis").field(value).finish(),
            Self::MockToken { state, provider } => f.debug_tuple("MockToken").field(state).field(provider).finish(),
        }
    }
}

impl OAuthAsk {
    /// Whether `reply` is a variant this ask can be answered with.
    pub fn accepts(&self, reply: &OAuthReply) -> bool {
        matches!(
            (self, reply),
            (Self::PendingPut { .. }, OAuthReply::Done)
                | (Self::PendingTake { .. }, OAuthReply::Taken { .. })
                | (Self::ConnectionAvailable, OAuthReply::Flag { .. })
                | (
                    Self::ConnectionSave { .. } | Self::ConnectionStatus { .. } | Self::ConnectionRemove { .. },
                    OAuthReply::Row { .. }
                )
                | (Self::SubjectRemember { .. } | Self::SubjectForget { .. } | Self::MetadataPut { .. }, OAuthReply::Done)
                | (Self::SubjectLookup { .. }, OAuthReply::Subject { .. })
                | (Self::MetadataGet { .. }, OAuthReply::Metadata { .. })
                | (Self::Clock, OAuthReply::Millis { .. })
                | (Self::MockTokenPut { .. }, OAuthReply::Done)
                | (Self::MockTokenRefresh { .. } | Self::MockTokenRevoke { .. }, OAuthReply::Flag { .. })
                | (Self::MockTokenCheck { .. }, OAuthReply::MockToken { .. })
        )
    }

    /// Whether the ask acts for a signed-in caller.
    pub fn needs_caller(&self) -> bool {
        matches!(
            self,
            Self::ConnectionSave { .. } | Self::ConnectionStatus { .. } | Self::ConnectionRemove { .. }
        )
    }
}

/// Why an OAuth flow failed: a closed set, each with a public code and one fixed safe message. A vendor body or
/// `Error.cause` never reaches a payload; only these do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthFailureReason {
    StateUnknown,
    StateExpired,
    SubjectMismatch,
    ProviderMismatch,
    NotSignedIn,
    StoreUnavailable,
    MetadataInvalid,
    ResponseInvalid,
    TokenRejected,
    UserinfoMismatch,
    UnconfiguredClient,
}

impl OAuthFailureReason {
    pub const ALL: [Self; 11] = [
        Self::StateUnknown,
        Self::StateExpired,
        Self::SubjectMismatch,
        Self::ProviderMismatch,
        Self::NotSignedIn,
        Self::StoreUnavailable,
        Self::MetadataInvalid,
        Self::ResponseInvalid,
        Self::TokenRejected,
        Self::UserinfoMismatch,
        Self::UnconfiguredClient,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::StateUnknown => "state-unknown",
            Self::StateExpired => "state-expired",
            Self::SubjectMismatch => "subject-mismatch",
            Self::ProviderMismatch => "provider-mismatch",
            Self::NotSignedIn => "not-signed-in",
            Self::StoreUnavailable => "store-unavailable",
            Self::MetadataInvalid => "metadata-invalid",
            Self::ResponseInvalid => "response-invalid",
            Self::TokenRejected => "token-rejected",
            Self::UserinfoMismatch => "userinfo-mismatch",
            Self::UnconfiguredClient => "unconfigured-client",
        }
    }

    /// The public error code of the failure (`AUTH_ERROR | VALIDATION_ERROR | NOT_CONFIGURED | SERVICE_ERROR`).
    pub fn code(self) -> &'static str {
        match self {
            Self::StateUnknown
            | Self::StateExpired
            | Self::SubjectMismatch
            | Self::ProviderMismatch
            | Self::NotSignedIn
            | Self::TokenRejected => "AUTH_ERROR",
            Self::StoreUnavailable | Self::UnconfiguredClient => "NOT_CONFIGURED",
            Self::MetadataInvalid | Self::ResponseInvalid | Self::UserinfoMismatch => "SERVICE_ERROR",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::StateUnknown => "Invalid or unknown state token",
            Self::StateExpired => "The state token has expired",
            Self::SubjectMismatch => {
                "OAuth completion refused: the completing subject is not the one that started the authorization"
            }
            Self::ProviderMismatch => "OAuth completion refused: the grant was issued for a different provider",
            Self::NotSignedIn => "A provider connection needs a signed-in caller",
            Self::StoreUnavailable => "No OAuth connection store is available on this host",
            Self::MetadataInvalid => "The authorization server metadata is unusable",
            Self::ResponseInvalid => "The authorization server sent an invalid response",
            Self::TokenRejected => "The authorization server rejected the request",
            Self::UserinfoMismatch => "The userinfo response does not match the subject the tokens were issued to",
            Self::UnconfiguredClient => "The OAuth client is not configured",
        }
    }
}

impl crate::oauth_wire::OidcErrorKind {
    /// The flow failure a failed response check is; `None` for a bad caller input, which is a refusal.
    pub fn reason(self) -> Option<OAuthFailureReason> {
        use crate::oauth_wire::OidcErrorKind::*;
        match self {
            Metadata => Some(OAuthFailureReason::MetadataInvalid),
            Rejected => Some(OAuthFailureReason::TokenRejected),
            Invalid => Some(OAuthFailureReason::ResponseInvalid),
            Mismatch => Some(OAuthFailureReason::UserinfoMismatch),
            Input => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_a_distinct_word_a_known_code_and_a_message_free_of_vendor_text() {
        let words: std::collections::HashSet<_> = OAuthFailureReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(words.len(), 11);
        for reason in OAuthFailureReason::ALL {
            assert!(["AUTH_ERROR", "NOT_CONFIGURED", "SERVICE_ERROR"].contains(&reason.code()));
            assert!(!reason.message().is_empty());
            let wire = serde_json::to_value(reason).expect("serializes");
            assert_eq!(wire, Value::String(reason.as_str().to_string()));
        }
    }

    #[test]
    fn debug_never_shows_a_token_state_or_verifier() {
        let grant = Pending {
            provider: "google".into(),
            redirect_uri: "https://app/cb".into(),
            verifier: "VERIFIER-SECRET".into(),
            subject: None,
            scopes: Vec::new(),
        };
        let put = OAuthAsk::PendingPut { state: "STATE-SECRET".into(), grant: grant.clone(), ttl_ms: 1 };
        let save = OAuthAsk::ConnectionSave {
            provider: OAuthProvider::Google,
            grant: OAuthTokenGrant {
                access_token: "ACCESS-SECRET".into(),
                refresh_token: Some("REFRESH-SECRET".into()),
                expires_in: 1.0,
                token_type: "bearer".into(),
                scope: None,
                subject: None,
            },
        };
        let taken = OAuthReply::Taken { taken: PendingTake::Grant { grant } };
        let shown = format!("{put:?} {save:?} {taken:?}");
        for secret in ["STATE-SECRET", "VERIFIER-SECRET", "ACCESS-SECRET", "REFRESH-SECRET"] {
            assert!(!shown.contains(secret), "{secret} leaked: {shown}");
        }
    }

    #[test]
    fn an_ask_accepts_only_its_own_reply() {
        let take = OAuthAsk::PendingTake { state: "s".into() };
        assert!(take.accepts(&OAuthReply::Taken { taken: PendingTake::Missing }));
        assert!(!take.accepts(&OAuthReply::Done));
        assert!(OAuthAsk::Clock.accepts(&OAuthReply::Millis { value: 1 }));
        assert!(!OAuthAsk::Clock.accepts(&OAuthReply::Flag { value: true }));
    }

    #[test]
    fn asks_round_trip_through_json() {
        let ask = OAuthAsk::SubjectLookup { access_token: "t".into() };
        let wire = serde_json::to_value(&ask).expect("serializes");
        assert_eq!(wire["ask"], "subjectLookup");
        assert_eq!(serde_json::from_value::<OAuthAsk>(wire).expect("parses"), ask);
    }
}
