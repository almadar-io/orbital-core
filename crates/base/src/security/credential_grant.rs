//! The OAuth authorization-code exchange as pure functions: the token request a host sends and the reading of its
//! answer. No I/O and no clock; the caller posts the request and supplies `now`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::codec::{base64_encode, base64url_encode, form_urlencode, form_urlencode_value};
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

/// The declared claims of an RFC 7523 assertion: `iss` and `sub` only when named, `scope` the space-joined scopes when any,
/// `aud` always, `iat` the clock and `exp` that plus `ttl_seconds`. `signing_key` names the stored PKCS#8 RSA private key.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertionSpec {
    pub signing_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub audience: String,
    pub ttl_seconds: u32,
}

/// A JSON-body token endpoint declared field by field: the names the client id and secret travel under, the names the
/// answer carries the access token, refresh token and lifetime (seconds) under, and the optional refresh endpoint with the
/// name its body carries the refresh token under.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonTokenSpec {
    pub client_id_field: String,
    pub client_secret_field: String,
    pub access_field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_body_field: Option<String>,
}

pub const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

fn json_text(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// The signed RS256 assertion for `spec` at `now_ms` (epoch milliseconds, read as whole seconds). Claims are written in the
/// fixed order iss, sub, scope, aud, iat, exp. `seed` blinds the private operation; PKCS#1 v1.5 signatures do not depend on it.
pub fn assertion_jwt(
    spec: &AssertionSpec,
    scopes: &[String],
    signing_pem: &str,
    now_ms: i64,
    seed: [u8; 32],
) -> Result<Secret<String>, GrantError> {
    let iat = now_ms.div_euclid(1_000);
    let exp = iat.saturating_add(i64::from(spec.ttl_seconds));
    let mut claims = Vec::new();
    if let Some(issuer) = &spec.issuer {
        claims.push(format!("\"iss\":{}", json_text(issuer)));
    }
    if let Some(subject) = &spec.subject {
        claims.push(format!("\"sub\":{}", json_text(subject)));
    }
    if !scopes.is_empty() {
        claims.push(format!("\"scope\":{}", json_text(&scopes.join(" "))));
    }
    claims.push(format!("\"aud\":{}", json_text(&spec.audience)));
    claims.push(format!("\"iat\":{iat}"));
    claims.push(format!("\"exp\":{exp}"));
    let signing_input = format!(
        "{}.{}",
        base64url_encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        base64url_encode(format!("{{{}}}", claims.join(",")).as_bytes())
    );
    let signature = crate::codec::rsa_pkcs1_sha256_sign(signing_pem, signing_input.as_bytes(), seed)
        .map_err(|_| {
            GrantError::new(
                GrantErrorKind::Refused,
                "the signing credential is not a PKCS#8 RSA private key of at least 2048 bits",
            )
        })?;
    Ok(Secret::new(format!(
        "{signing_input}.{}",
        base64url_encode(&signature)
    )))
}

/// The RFC 7523 token request carrying `assertion`. The assertion authenticates the client, so no client credential rides.
pub fn jwt_bearer_request(assertion: &str) -> FormRequest {
    FormRequest {
        headers: form_headers(),
        body: form_urlencode(&[
            ("grant_type".to_string(), JWT_BEARER_GRANT_TYPE.to_string()),
            ("assertion".to_string(), assertion.to_string()),
        ]),
    }
}

fn json_headers() -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ]
}

/// The declared JSON-body mint request: `{ "<clientIdField>": id, "<clientSecretField>": secret }`.
pub fn json_token_request(spec: &JsonTokenSpec, client_id: &str, client_secret: &str) -> FormRequest {
    FormRequest {
        headers: json_headers(),
        body: format!(
            "{{{}:{},{}:{}}}",
            json_text(&spec.client_id_field),
            json_text(client_id),
            json_text(&spec.client_secret_field),
            json_text(client_secret)
        ),
    }
}

/// The declared JSON-body refresh request: `{ "<refreshBodyField>": refresh }`. `None` when the spec declares no refresh body field.
pub fn json_token_refresh_request(spec: &JsonTokenSpec, refresh_token: &str) -> Option<FormRequest> {
    let field = spec.refresh_body_field.as_ref()?;
    Some(FormRequest {
        headers: json_headers(),
        body: format!("{{{}:{}}}", json_text(field), json_text(refresh_token)),
    })
}

/// Read the answer of a declared JSON-body endpoint. A non-2xx status is `InvalidClient` for a mint and `InvalidGrant` for a
/// refresh when it is 400, 401 or 403, `Malformed` otherwise; the body is never echoed.
pub fn parse_json_token_response(
    spec: &JsonTokenSpec,
    refresh: bool,
    status: u16,
    body: &str,
    now_ms: i64,
) -> Result<TokenGrant, GrantError> {
    let malformed = |message: &str| GrantError::new(GrantErrorKind::Malformed, message);
    if !(200..300).contains(&status) {
        return Err(match status {
            400 | 401 | 403 if refresh => GrantError::new(
                GrantErrorKind::InvalidGrant,
                format!("the token endpoint answered HTTP {status}"),
            ),
            400 | 401 | 403 => GrantError::new(
                GrantErrorKind::InvalidClient,
                format!("the token endpoint answered HTTP {status}"),
            ),
            _ => malformed(&format!("the token endpoint answered HTTP {status}")),
        });
    }
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| malformed("the token endpoint did not answer a JSON object"))?;
    let fields = parsed
        .as_object()
        .ok_or_else(|| malformed("the token endpoint did not answer a JSON object"))?;
    let text_at = |field: &str| {
        fields
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
    };
    let access = text_at(&spec.access_field)
        .ok_or_else(|| malformed("the token response carries no access token"))?;
    let expires_at = match &spec.expires_in_field {
        None => None,
        Some(field) => match fields.get(field) {
            None => None,
            Some(value) => {
                let seconds = value
                    .as_u64()
                    .ok_or_else(|| malformed("the token response lifetime is not a number of seconds"))?;
                let lifetime = i64::try_from(seconds)
                    .unwrap_or(i64::MAX / 2_000)
                    .saturating_mul(1_000);
                Some(
                    now_ms
                        .saturating_add(lifetime)
                        .saturating_sub(EXPIRY_SKEW_MS),
                )
            }
        },
    };
    Ok(TokenGrant {
        access: Secret::new(access.to_string()),
        refresh: spec
            .refresh_field
            .as_deref()
            .and_then(text_at)
            .map(|token| Secret::new(token.to_string())),
        expires_at,
        label: None,
    })
}

fn form_headers() -> Vec<(String, String)> {
    vec![
        (
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ),
        ("Accept".to_string(), "application/json".to_string()),
    ]
}

fn client_form(
    mut pairs: Vec<(String, String)>,
    client_id: &str,
    client_secret: Option<&str>,
    client_auth: ClientAuth,
) -> Result<FormRequest, GrantError> {
    let mut headers = form_headers();
    match client_auth {
        ClientAuth::Body => {
            pairs.push(("client_id".to_string(), client_id.to_string()));
            if let Some(secret) = client_secret {
                pairs.push(("client_secret".to_string(), secret.to_string()));
            }
        }
        ClientAuth::None => {
            if !client_id.is_empty() {
                pairs.push(("client_id".to_string(), client_id.to_string()));
            }
        }
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
    fn none_without_a_client_id_sends_no_client_fields() {
        let refresh = |client_id| {
            refresh_token_request(&RefreshExchange {
                refresh_token: "r/1",
                client_id,
                client_secret: None,
                client_auth: ClientAuth::None,
            })
            .unwrap()
        };
        let firebase = refresh("");
        assert_eq!(firebase.body, "grant_type=refresh_token&refresh_token=r%2F1");
        assert_eq!(header(&firebase, "Authorization"), None);
        assert_eq!(
            refresh("pub:1").body,
            "grant_type=refresh_token&refresh_token=r%2F1&client_id=pub%3A1"
        );
        let mut ex = exchange(ClientAuth::None, None);
        ex.client_id = "";
        ex.verifier = None;
        assert!(!authorization_code_request(&ex).unwrap().body.contains("client_id"));
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
    fn jwt_fixture() -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/security/jwt-bearer.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
    }

    fn spec() -> AssertionSpec {
        AssertionSpec {
            signing_key: "SA_KEY".into(),
            issuer: Some("sa@proj.iam".into()),
            subject: Some("user@example.com".into()),
            audience: "https://oauth2.example/token".into(),
            ttl_seconds: 3600,
        }
    }

    #[test]
    fn the_assertion_matches_the_openssl_signed_known_answer_with_every_claim_declared() {
        let fx = jwt_fixture();
        let scopes = vec!["https://a/x".to_string(), "https://a/y".to_string()];
        let jwt = assertion_jwt(&spec(), &scopes, fx["privateKeyPem"].as_str().unwrap(), 1_700_000_000_999, [7; 32]).unwrap();
        assert_eq!(jwt.expose(), fx["full"].as_str().unwrap());
        let other_seed = assertion_jwt(&spec(), &scopes, fx["privateKeyPem"].as_str().unwrap(), 1_700_000_000_000, [9; 32]).unwrap();
        assert_eq!(other_seed.expose(), jwt.expose(), "the blinding seed and sub-second clock change nothing");
    }

    #[test]
    fn undeclared_claims_are_omitted_and_a_bad_key_is_refused_without_the_key_text() {
        let fx = jwt_fixture();
        let bare = AssertionSpec { issuer: None, subject: None, ..spec() };
        let jwt = assertion_jwt(&bare, &[], fx["privateKeyPem"].as_str().unwrap(), 1_700_000_000_000, [1; 32]).unwrap();
        assert_eq!(jwt.expose(), fx["minimal"].as_str().unwrap());
        let error = assertion_jwt(&spec(), &[], "-----BEGIN PRIVATE KEY-----\nSECRET-PEM\n-----END PRIVATE KEY-----", 0, [0; 32]).unwrap_err();
        assert_eq!(error.kind, GrantErrorKind::Refused);
        assert!(!error.message.contains("SECRET-PEM"));
    }

    #[test]
    fn the_jwt_bearer_form_is_byte_exact_and_carries_no_client() {
        let request = jwt_bearer_request("a.b.c");
        assert_eq!(request.body, "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=a.b.c");
        assert_eq!(header(&request, "Content-Type"), Some("application/x-www-form-urlencoded"));
        assert_eq!(header(&request, "Authorization"), None);
    }

    fn gocardless() -> JsonTokenSpec {
        JsonTokenSpec {
            client_id_field: "secret_id".into(),
            client_secret_field: "secret_key".into(),
            access_field: "access".into(),
            refresh_field: Some("refresh".into()),
            expires_in_field: Some("access_expires".into()),
            refresh_endpoint: Some("https://bank.example/token/refresh/".into()),
            refresh_body_field: Some("refresh".into()),
        }
    }

    #[test]
    fn the_json_token_bodies_are_byte_exact_and_escape_their_values() {
        let mint = json_token_request(&gocardless(), "id\"1", "se/cret\\");
        assert_eq!(mint.body, r#"{"secret_id":"id\"1","secret_key":"se/cret\\"}"#);
        assert_eq!(header(&mint, "Content-Type"), Some("application/json"));
        let refresh = json_token_refresh_request(&gocardless(), "r-1").unwrap();
        assert_eq!(refresh.body, r#"{"refresh":"r-1"}"#);
        let none = JsonTokenSpec { refresh_body_field: None, ..gocardless() };
        assert!(json_token_refresh_request(&none, "r").is_none());
    }

    #[test]
    fn the_json_token_answer_is_read_by_its_declared_fields_with_the_skewed_expiry() {
        let body = r#"{"access":"A1","access_expires":86400,"refresh":"R1","refresh_expires":2592000}"#;
        let grant = parse_json_token_response(&gocardless(), false, 200, body, 1_000_000).unwrap();
        assert_eq!(grant.access.expose(), "A1");
        assert_eq!(grant.refresh.as_ref().map(|r| r.expose().as_str()), Some("R1"));
        assert_eq!(grant.expires_at, Some(1_000_000 + 86_400_000 - EXPIRY_SKEW_MS));
        let refreshed = parse_json_token_response(&gocardless(), true, 200, r#"{"access":"A2","access_expires":60}"#, 0).unwrap();
        assert_eq!((refreshed.refresh, refreshed.access.expose().as_str()), (None, "A2"));
    }

    #[test]
    fn json_token_failures_are_typed_by_status_and_never_echo_the_body() {
        let kind = |refresh, status, body: &str| parse_json_token_response(&gocardless(), refresh, status, body, 0).unwrap_err().kind;
        assert_eq!(kind(false, 401, r#"{"detail":"bad"}"#), GrantErrorKind::InvalidClient);
        assert_eq!(kind(true, 401, r#"{"detail":"bad"}"#), GrantErrorKind::InvalidGrant);
        assert_eq!(kind(false, 502, "gateway"), GrantErrorKind::Malformed);
        for body in ["not json", "[]", r#"{"access":""}"#, r#"{"other":"SECRET-A"}"#, r#"{"access":"SECRET-A","access_expires":"soon"}"#] {
            let error = parse_json_token_response(&gocardless(), false, 200, body, 0).unwrap_err();
            assert_eq!(error.kind, GrantErrorKind::Malformed, "{body}");
            assert!(!error.message.contains("SECRET-A"));
        }
    }
}
