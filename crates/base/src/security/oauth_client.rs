//! The declaration an `oauth-client` auth (and `oauth-authorize`) carries: which operation, which client, which
//! inputs. Secret slots (`clientId`, `clientSecret`) are credential NAMES the host resolves, never values.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::language::registry::{oauth_client_keys, oauth_vocabulary, EmbeddedOAuthOp};
use crate::oauth_ask::OAuthProvider;
use crate::oauth_wire::ClientAuth;
use crate::http_wire::CredentialSlot;

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
    /// Every op the operator table declares on `http`, in its order.
    pub fn words() -> Vec<&'static str> {
        oauth_vocabulary().ops.iter().map(|op| op.name.as_str()).collect()
    }

    /// Every input key any op takes, in the table's order.
    pub fn all_inputs() -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = Vec::new();
        for op in &oauth_vocabulary().ops {
            for key in op.required.iter().chain(op.optional.iter()) {
                if !keys.contains(&key.as_str()) {
                    keys.push(key.as_str());
                }
            }
        }
        keys
    }

    fn declared(self) -> &'static EmbeddedOAuthOp {
        oauth_vocabulary()
            .ops
            .iter()
            .find(|op| op.name == self.name())
            .unwrap_or_else(|| panic!("the operator table declares no oauth-client op `{}`", self.name()))
    }

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

    /// The `oauth-endpoint` word the op is sent to (the table's `endpoint`); `discovery` goes to a literal origin and path.
    pub fn endpoint(self) -> Option<OAuthEndpointKind> {
        self.declared().endpoint.as_deref().map(|word| {
            OAuthEndpointKind::parse(word).unwrap_or_else(|| panic!("the operator table sends `{}` to an unknown endpoint `{word}`", self.name()))
        })
    }

    /// The input keys the op requires, then those it may carry, as the operator table declares them.
    pub fn inputs(self) -> (Vec<&'static str>, Vec<&'static str>) {
        let op = self.declared();
        (op.required.iter().map(String::as_str).collect(), op.optional.iter().map(String::as_str).collect())
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
    /// The words that may follow the endpoint target, as the operator table declares them.
    pub fn words() -> Vec<&'static str> {
        oauth_vocabulary().endpoint_kinds.iter().map(String::as_str).collect()
    }

    /// The word that stands where an origin goes: the URL comes from the client's metadata.
    pub fn target() -> &'static str {
        oauth_vocabulary().endpoint_target.as_str()
    }

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
    /// The keys a client object may carry (sorted), read from the `client` parameter of `oauth-authorize`.
    pub fn keys() -> Vec<&'static str> {
        oauth_client_keys()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const OPS: [OAuthOp; 6] = [OAuthOp::Discovery, OAuthOp::Code, OAuthOp::Connect, OAuthOp::Refresh, OAuthOp::Revoke, OAuthOp::Userinfo];

    #[test]
    fn the_op_words_and_endpoint_words_are_the_tables_and_name_every_variant() {
        assert_eq!(OAuthOp::words(), OPS.map(OAuthOp::name));
        for op in OPS {
            assert_eq!(OAuthOp::parse(op.name()), Some(op));
        }
        assert_eq!(OAuthOp::parse("introspect"), None, "control: a word the table does not declare");
        assert_eq!(OAuthEndpointKind::words(), ["token", "revocation", "userinfo"]);
        for word in OAuthEndpointKind::words() {
            assert_eq!(OAuthEndpointKind::parse(word).map(OAuthEndpointKind::name), Some(word));
        }
        assert_eq!(OAuthEndpointKind::target(), "oauth-endpoint");
    }

    #[test]
    fn each_op_reads_its_inputs_and_endpoint_from_the_table() {
        assert_eq!(OAuthOp::Discovery.inputs(), (vec![], vec![]));
        assert_eq!(OAuthOp::Code.inputs(), (vec!["code", "state"], vec!["subject"]));
        assert_eq!(OAuthOp::Connect.inputs(), (vec!["code", "state", "provider"], vec![]));
        assert_eq!(OAuthOp::Refresh.inputs(), (vec!["refreshToken"], vec![]));
        assert_eq!(OAuthOp::Revoke.inputs(), (vec!["token"], vec![]));
        assert_eq!(OAuthOp::Userinfo.inputs(), (vec!["accessToken"], vec![]));
        assert_eq!(OAuthOp::Discovery.endpoint(), None);
        for op in [OAuthOp::Code, OAuthOp::Connect, OAuthOp::Refresh] {
            assert_eq!(op.endpoint(), Some(OAuthEndpointKind::Token));
        }
        assert_eq!(OAuthOp::Revoke.endpoint(), Some(OAuthEndpointKind::Revocation));
        assert_eq!(OAuthOp::Userinfo.endpoint(), Some(OAuthEndpointKind::Userinfo));
        assert_eq!(OAuthOp::all_inputs(), ["code", "state", "subject", "provider", "refreshToken", "token", "accessToken"]);
    }

    #[test]
    fn the_client_keys_are_the_fields_of_the_oauth_authorize_client() {
        assert_eq!(OAuthClientDecl::keys(), ["clientAuth", "clientId", "clientSecret", "endpoints", "issuer", "mode", "tag"]);
    }
}
