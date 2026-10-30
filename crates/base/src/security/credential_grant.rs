//! The OAuth authorization-code exchange as pure functions: the token request a host sends and the reading of its
//! answer. No I/O and no clock; the caller posts the request and supplies `now`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::codec::{base64_encode, form_urlencode, form_urlencode_value};
use crate::secret::Secret;

/// An access token is treated as expired this long before the provider says it is.
pub const EXPIRY_SKEW_MS: i64 = 60_000;

/// How the token endpoint authenticates the client.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAuth {
    Body,
    Basic,
    None,
}

/// Why a token exchange did not yield a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantErrorKind {
    InvalidGrant,
    InvalidClient,
    /// The endpoint named another OAuth error, or the request could not be built.
    Refused,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantError {
    pub kind: GrantErrorKind,
    pub message: String,
}

impl GrantError {
    fn new(kind: GrantErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// What the exchange is made with. `client_secret` is read only by `Body` and `Basic`.
pub struct CodeExchange<'a> {
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub verifier: Option<&'a str>,
    pub client_id: &'a str,
    pub client_secret: Option<&'a str>,
    pub client_auth: ClientAuth,
}

/// A form POST to the token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormRequest {
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// What a refresh is made with: the stored refresh token and the client that was granted it.
pub struct RefreshExchange<'a> {
    pub refresh_token: &'a str,
    pub client_id: &'a str,
    pub client_secret: Option<&'a str>,
    pub client_auth: ClientAuth,
}

pub fn authorization_code_request(exchange: &CodeExchange<'_>) -> Result<FormRequest, GrantError> {
    let mut pairs = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), exchange.code.to_string()),
        (
            "redirect_uri".to_string(),
            exchange.redirect_uri.to_string(),
        ),
    ];
    if let Some(verifier) = exchange.verifier {
        pairs.push(("code_verifier".to_string(), verifier.to_string()));
    }
    client_form(
        pairs,
        exchange.client_id,
        exchange.client_secret,
        exchange.client_auth,
    )
}

pub fn refresh_token_request(exchange: &RefreshExchange<'_>) -> Result<FormRequest, GrantError> {
    let pairs = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        (
            "refresh_token".to_string(),
            exchange.refresh_token.to_string(),
        ),
    ];
    client_form(
        pairs,
        exchange.client_id,
        exchange.client_secret,
        exchange.client_auth,
    )
}

fn client_form(
    mut pairs: Vec<(String, String)>,
    client_id: &str,
    client_secret: Option<&str>,
    client_auth: ClientAuth,
) -> Result<FormRequest, GrantError> {
    let mut headers = vec![
        (
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    match client_auth {
        ClientAuth::Body => {
            pairs.push(("client_id".to_string(), client_id.to_string()));
            if let Some(secret) = client_secret {
                pairs.push(("client_secret".to_string(), secret.to_string()));
            }
        }
        ClientAuth::None => pairs.push(("client_id".to_string(), client_id.to_string())),
        ClientAuth::Basic => {
            let secret = client_secret.ok_or_else(|| {
                GrantError::new(
                    GrantErrorKind::Refused,
                    "basic client authentication needs a client secret",
                )
            })?;
            let pair = format!(
                "{}:{}",
                form_urlencode_value(client_id),
                form_urlencode_value(secret)
            );
            headers.push((
                "Authorization".to_string(),
                format!("Basic {}", base64_encode(pair.as_bytes())),
            ));
        }
    }
    Ok(FormRequest {
        headers,
        body: form_urlencode(&pairs),
    })
}

/// A successful exchange. `expires_at` is epoch milliseconds, already skewed by [`EXPIRY_SKEW_MS`]; `label` is the
/// non-secret account name the provider's `id_token` carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenGrant {
    pub access: Secret<String>,
    pub refresh: Option<Secret<String>>,
    pub expires_at: Option<i64>,
    pub label: Option<String>,
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: Option<String>,
    token_type: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    id_token: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct IdClaims {
    email: Option<String>,
}

fn label_of(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: IdClaims = serde_json::from_slice(&bytes).ok()?;
    claims.email.filter(|email| !email.is_empty())
}

pub fn parse_token_response(
    status: u16,
    body: &str,
    now_ms: i64,
) -> Result<TokenGrant, GrantError> {
    let malformed = |message: &str| GrantError::new(GrantErrorKind::Malformed, message);
    let parsed: TokenBody = serde_json::from_str(body)
        .map_err(|_| malformed("the token endpoint did not answer a token JSON object"))?;
    if let Some(error) = parsed.error {
        let kind = match error.as_str() {
            "invalid_grant" => GrantErrorKind::InvalidGrant,
            "invalid_client" => GrantErrorKind::InvalidClient,
            _ => GrantErrorKind::Refused,
        };
        return Err(GrantError::new(
            kind,
            format!("the token endpoint answered `{error}`"),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(malformed(&format!(
            "the token endpoint answered HTTP {status} without an OAuth error"
        )));
    }
    let access = parsed
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| malformed("the token response carries no access_token"))?;
    match parsed.token_type {
        Some(kind) if kind.eq_ignore_ascii_case("bearer") => {}
        _ => return Err(malformed("the token response is not of token_type bearer")),
    }
    let expires_at = parsed.expires_in.map(|seconds| {
        let lifetime = i64::try_from(seconds)
            .unwrap_or(i64::MAX / 2_000)
            .saturating_mul(1_000);
        now_ms
            .saturating_add(lifetime)
            .saturating_sub(EXPIRY_SKEW_MS)
    });
    Ok(TokenGrant {
        access: Secret::new(access),
        refresh: parsed
            .refresh_token
            .filter(|token| !token.is_empty())
            .map(Secret::new),
        expires_at,
        label: parsed.id_token.as_deref().and_then(label_of),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(auth: ClientAuth, secret: Option<&'static str>) -> CodeExchange<'static> {
        CodeExchange {
            code: "c o/d+e&=",
            redirect_uri: "https://app.example/cb?x=1",
            verifier: Some("ver~ifier"),
            client_id: "id:1",
            client_secret: secret,
            client_auth: auth,
        }
    }

    fn header<'a>(request: &'a FormRequest, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_form_body_escapes_reserved_characters_and_keeps_the_pair_order() {
        let request =
            authorization_code_request(&exchange(ClientAuth::Body, Some("s&=ecret"))).unwrap();
        assert_eq!(
            request.body,
            "grant_type=authorization_code&code=c+o%2Fd%2Be%26%3D&redirect_uri=https%3A%2F%2Fapp.example%2Fcb%3Fx%3D1&code_verifier=ver%7Eifier&client_id=id%3A1&client_secret=s%26%3Decret"
        );
        assert_eq!(
            header(&request, "Content-Type"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(header(&request, "Authorization"), None);
    }

    #[test]
    fn the_refresh_form_carries_the_stored_token_and_shares_the_client_auth_path() {
        let refresh = |auth, secret| {
            refresh_token_request(&RefreshExchange {
                refresh_token: "r/1",
                client_id: "id:1",
                client_secret: secret,
                client_auth: auth,
            })
        };
        let body = refresh(ClientAuth::Body, Some("s&"));
        assert_eq!(
            body.unwrap().body,
            "grant_type=refresh_token&refresh_token=r%2F1&client_id=id%3A1&client_secret=s%26"
        );
        let basic = refresh(ClientAuth::Basic, Some("s:"));
        let basic = basic.unwrap();
        assert_eq!(basic.body, "grant_type=refresh_token&refresh_token=r%2F1");
        assert_eq!(
            header(&basic, "Authorization"),
            Some(format!("Basic {}", base64_encode(b"id%3A1:s%3A")).as_str())
        );
        assert_eq!(
            refresh(ClientAuth::Basic, None).unwrap_err().kind,
            GrantErrorKind::Refused
        );
    }

    #[test]
    fn basic_auth_moves_the_client_into_the_header_and_out_of_the_body() {
        let request =
            authorization_code_request(&exchange(ClientAuth::Basic, Some("s:ecret"))).unwrap();
        let expected = format!("Basic {}", base64_encode(b"id%3A1:s%3Aecret"));
        assert_eq!(header(&request, "Authorization"), Some(expected.as_str()));
        assert!(!request.body.contains("client_id") && !request.body.contains("client_secret"));
    }

    #[test]
    fn basic_auth_without_a_secret_is_refused() {
        let error = authorization_code_request(&exchange(ClientAuth::Basic, None)).unwrap_err();
        assert_eq!(error.kind, GrantErrorKind::Refused);
    }

    #[test]
    fn none_sends_the_client_id_only_and_a_missing_verifier_is_omitted() {
        let mut ex = exchange(ClientAuth::None, Some("ignored"));
        ex.verifier = None;
        let request = authorization_code_request(&ex).unwrap();
        assert!(request.body.ends_with("&client_id=id%3A1"));
        assert!(!request.body.contains("ignored") && !request.body.contains("code_verifier"));
        assert_eq!(header(&request, "Authorization"), None);
    }

    #[test]
    fn body_auth_without_a_secret_sends_the_id_alone() {
        let request = authorization_code_request(&exchange(ClientAuth::Body, None)).unwrap();
        assert!(request.body.ends_with("&client_id=id%3A1"));
    }

    fn id_token(claims: &str) -> String {
        let enc =
            |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes());
        format!("{}.{}.sig", enc("{}"), enc(claims))
    }

    #[test]
    fn a_full_response_yields_the_tokens_the_skewed_expiry_and_the_id_token_label() {
        let body = format!(
            r#"{{"access_token":"A1","token_type":"Bearer","refresh_token":"R1","expires_in":3600,"id_token":"{}"}}"#,
            id_token(r#"{"email":"ada@example.com"}"#)
        );
        let grant = parse_token_response(200, &body, 1_000_000).unwrap();
        assert_eq!(grant.access.expose(), "A1");
        assert_eq!(
            grant.refresh.as_ref().map(|r| r.expose().as_str()),
            Some("R1")
        );
        assert_eq!(
            grant.expires_at,
            Some(1_000_000 + 3_600_000 - EXPIRY_SKEW_MS)
        );
        assert_eq!(grant.label.as_deref(), Some("ada@example.com"));
        assert!(!format!("{grant:?}").contains("A1"));
    }

    #[test]
    fn a_minimal_response_has_no_refresh_expiry_or_label_and_the_type_is_case_insensitive() {
        let grant =
            parse_token_response(200, r#"{"access_token":"A","token_type":"bEaReR"}"#, 5).unwrap();
        assert_eq!(
            (grant.refresh, grant.expires_at, grant.label),
            (None, None, None)
        );
    }

    #[test]
    fn an_unreadable_id_token_gives_no_label_and_does_not_fail_the_exchange() {
        let body = r#"{"access_token":"A","token_type":"bearer","id_token":"not-a-jwt"}"#;
        assert_eq!(parse_token_response(200, body, 0).unwrap().label, None);
    }

    #[test]
    fn a_short_lifetime_is_already_expired_by_the_skew() {
        let grant = parse_token_response(
            200,
            r#"{"access_token":"A","token_type":"bearer","expires_in":30}"#,
            10_000,
        )
        .unwrap();
        assert_eq!(grant.expires_at, Some(10_000 + 30_000 - EXPIRY_SKEW_MS));
    }

    #[test]
    fn oauth_errors_map_to_their_kinds_whatever_the_status() {
        let kind = |status, body: &str| parse_token_response(status, body, 0).unwrap_err().kind;
        assert_eq!(
            kind(400, r#"{"error":"invalid_grant"}"#),
            GrantErrorKind::InvalidGrant
        );
        assert_eq!(
            kind(401, r#"{"error":"invalid_client","error_description":"x"}"#),
            GrantErrorKind::InvalidClient
        );
        assert_eq!(
            kind(200, r#"{"error":"invalid_grant"}"#),
            GrantErrorKind::InvalidGrant
        );
        assert_eq!(
            kind(400, r#"{"error":"invalid_scope"}"#),
            GrantErrorKind::Refused
        );
    }

    #[test]
    fn malformed_answers_are_named_malformed_and_never_echo_the_body() {
        let bad = [
            (200, "not json"),
            (200, r#"{"token_type":"bearer"}"#),
            (200, r#"{"access_token":"","token_type":"bearer"}"#),
            (200, r#"{"access_token":"SECRET-A"}"#),
            (200, r#"{"access_token":"SECRET-A","token_type":"mac"}"#),
            (
                200,
                r#"{"access_token":"SECRET-A","token_type":"bearer","expires_in":"soon"}"#,
            ),
            (500, r#"{"access_token":"SECRET-A","token_type":"bearer"}"#),
            (502, "<html>bad gateway</html>"),
        ];
        for (status, body) in bad {
            let error = parse_token_response(status, body, 0).unwrap_err();
            assert_eq!(error.kind, GrantErrorKind::Malformed, "{body}");
            assert!(!error.message.contains("SECRET-A"));
        }
    }
}
