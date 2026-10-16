//! The pure OIDC / OAuth 2 processor: request planning and response checks with no transport. Every network leg
//! (discovery, code exchange, refresh, revocation, userinfo) is a request this module builds and a response it checks;
//! whoever owns the wire (a provider's `Transport`, the core `http` driver) sends it. Failures carry the coarse text
//! `openid-client` raises (`unexpected HTTP response status code`, ...), never a token or a vendor body.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::codec::form_urlencode_value;
use crate::runtime::http_service::{HttpRequest, HttpResponse};

use std::time::Duration;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const FORM_TYPE: &str = "application/x-www-form-urlencoded;charset=UTF-8";
const CLOCK_TOLERANCE_S: f64 = 30.0;

pub mod ask;
pub mod client;
pub mod store;

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

/// A failed check: the fixed message `openid-client` raises, never a token or vendor body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcError {
    pub kind: OidcErrorKind,
    pub message: String,
}

impl fmt::Display for OidcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OidcError {}

pub enum OidcServer {
    Issuer(String),
    Static(Map<String, Value>),
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

#[derive(Clone, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<f64>,
    pub scope: Vec<String>,
    pub id_token_subject: Option<String>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
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

pub struct OidcProcessor {
    provider: String,
    client_id: String,
    client_secret: String,
    client_auth: ClientAuth,
}

impl fmt::Debug for OidcProcessor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcProcessor")
            .field("provider", &self.provider)
            .field("client_auth", &self.client_auth)
            .finish_non_exhaustive()
    }
}

type Failure = OidcError;

fn fail(message: &str) -> Failure {
    OidcError { kind: OidcErrorKind::Invalid, message: message.to_string() }
}

fn with_kind(kind: OidcErrorKind, message: &str) -> Failure {
    OidcError { kind, message: message.to_string() }
}
fn plain(message: impl Into<String>) -> Failure {
    OidcError { kind: OidcErrorKind::Invalid, message: message.into() }
}

fn invalid_response() -> Failure {
    fail("invalid response encountered")
}
fn parse_error() -> Failure {
    fail("parsing error occured")
}
fn timestamp_error() -> Failure {
    fail("JWT timestamp claim value failed validation")
}
fn claim_error() -> Failure {
    fail("unexpected JWT claim value encountered")
}
fn status_error() -> Failure {
    fail("unexpected HTTP response status code")
}
fn content_type_error() -> Failure {
    fail("unexpected response content-type")
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_urlencode_value(k), form_urlencode_value(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `Response.headers.get('content-type')?.split(';')[0]`.
fn content_type(response: &HttpResponse) -> Option<String> {
    response
        .header("content-type")
        .map(|v| v.split(';').next().unwrap_or_default().to_string())
}

/// `assertApplicationJson` + `response.json()`: a top-level object, or the failure `openid-client` reports.
fn json_body(response: &HttpResponse) -> Result<Map<String, Value>, Failure> {
    match serde_json::from_str::<Value>(&response.text()) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(invalid_response()),
        Err(_) => {
            if content_type(response).as_deref() == Some("application/json") {
                Err(parse_error())
            } else {
                Err(content_type_error())
            }
        }
    }
}

/// Whether a `WWW-Authenticate` header parses as a challenge (scheme token, then nothing or whitespace/comma).
pub fn has_challenge(response: &HttpResponse) -> bool {
    let Some(header) = response.header("www-authenticate") else {
        return false;
    };
    let is_token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    let scheme_len = header.chars().take_while(|c| is_token(*c)).count();
    scheme_len > 0
        && header[scheme_len..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || c == ',')
}

fn string_of<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// `assertString`: a non-empty string, else an invalid response.
fn non_empty_string(map: &Map<String, Value>, key: &str) -> Result<String, Failure> {
    match map.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(invalid_response()),
    }
}

/// JS `parseFloat` on a string: the longest numeric prefix, else NaN.
pub fn js_parse_float(text: &str) -> f64 {
    let s = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let bytes = s.as_bytes();
    let mut i = 0;
    let sign = match bytes.first() {
        Some(b'-') => {
            i = 1;
            -1.0
        }
        Some(b'+') => {
            i = 1;
            1.0
        }
        _ => 1.0,
    };
    if s[i..].starts_with("Infinity") {
        return sign * f64::INFINITY;
    }
    let digits = |from: usize| bytes[from..].iter().take_while(|b| b.is_ascii_digit()).count();
    let int_len = digits(i);
    let mut end = i + int_len;
    let mut frac_len = 0;
    if bytes.get(end) == Some(&b'.') {
        frac_len = digits(end + 1);
        if int_len > 0 || frac_len > 0 {
            end += 1 + frac_len;
        }
    }
    if int_len == 0 && frac_len == 0 {
        return f64::NAN;
    }
    if matches!(bytes.get(end), Some(b'e') | Some(b'E')) {
        let mut k = end + 1;
        if matches!(bytes.get(k), Some(b'+') | Some(b'-')) {
            k += 1;
        }
        let exp_len = digits(k);
        if exp_len > 0 {
            end = k + exp_len;
        }
    }
    s[i..end].parse::<f64>().map_or(f64::NAN, |v| sign * v)
}

/// `resolveEndpoint`: the named endpoint of the server metadata, which must be an https URL.
fn resolve_endpoint(meta: &Map<String, Value>, name: &str) -> Result<Url, Failure> {
    let url = string_of(meta, name)
        .and_then(|text| Url::parse(text).ok())
        .ok_or_else(|| {
            with_kind(
                OidcErrorKind::Metadata,
                &format!("authorization server metadata does not contain a valid \"as.{name}\""),
            )
        })?;
    check_https(&url)?;
    Ok(url)
}

fn check_https(url: &Url) -> Result<(), Failure> {
    if url.scheme() != "https" {
        return Err(with_kind(OidcErrorKind::Metadata, "only requests to HTTPS are allowed"));
    }
    Ok(())
}

fn b64u_text(part: &str) -> Result<String, Failure> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .map_err(|_| parse_error())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

enum AlgRule<'a> {
    /// ID tokens: the issuer's advertised algorithms, else RS256.
    IdToken(Option<&'a Vec<Value>>),
    /// A signed UserInfo response: the issuer's advertised algorithms, or no way to check.
    UserInfo(Option<&'a Vec<Value>>),
}

struct Jwt {
    claims: Map<String, Value>,
}

/// `validateJwt`: structure, algorithm and timestamps. The signature is not checked, as `oauth4webapi` does not
/// for responses received straight from the issuer over TLS.
fn validate_jwt(jws: &str, alg: &AlgRule, now_s: f64) -> Result<Jwt, Failure> {
    let parts: Vec<&str> = jws.split('.').collect();
    if parts.len() == 5 {
        return Err(fail("unsupported operation"));
    }
    if parts.len() != 3 {
        return Err(invalid_response());
    }
    let header = match serde_json::from_str::<Value>(&b64u_text(parts[0])?) {
        Ok(Value::Object(map)) => map,
        Ok(_) => return Err(invalid_response()),
        Err(_) => return Err(parse_error()),
    };
    let header_alg = header.get("alg").cloned().unwrap_or(Value::Null);
    match alg {
        AlgRule::IdToken(Some(list)) | AlgRule::UserInfo(Some(list)) => {
            if !list.contains(&header_alg) {
                return Err(invalid_response());
            }
        }
        AlgRule::IdToken(None) => {
            if header_alg != Value::String("RS256".to_string()) {
                return Err(invalid_response());
            }
        }
        AlgRule::UserInfo(None) => return Err(plain(
            "missing client or server configuration to verify used JWT \"alg\" header parameter",
        )),
    }
    if header.contains_key("crit") {
        return Err(fail("unsupported operation"));
    }
    let claims = match serde_json::from_str::<Value>(&b64u_text(parts[1])?) {
        Ok(Value::Object(map)) => map,
        Ok(_) => return Err(invalid_response()),
        Err(_) => return Err(parse_error()),
    };
    if let Some(exp) = claims.get("exp") {
        let exp = exp.as_f64().ok_or_else(invalid_response)?;
        if exp <= now_s - CLOCK_TOLERANCE_S {
            return Err(timestamp_error());
        }
    }
    if let Some(iat) = claims.get("iat") {
        iat.as_f64().ok_or_else(invalid_response)?;
    }
    if let Some(iss) = claims.get("iss") {
        if !iss.is_string() {
            return Err(invalid_response());
        }
    }
    if let Some(nbf) = claims.get("nbf") {
        let nbf = nbf.as_f64().ok_or_else(invalid_response)?;
        if nbf > now_s + CLOCK_TOLERANCE_S {
            return Err(timestamp_error());
        }
    }
    if let Some(aud) = claims.get("aud") {
        if !aud.is_string() && !aud.is_array() {
            return Err(invalid_response());
        }
    }
    Ok(Jwt { claims })
}

fn validate_issuer(meta: &Map<String, Value>, claims: &Map<String, Value>) -> Result<(), Failure> {
    if claims.get("iss") != meta.get("issuer") || claims.get("iss").is_none() {
        return Err(claim_error());
    }
    Ok(())
}

fn validate_audience(client_id: &str, claims: &Map<String, Value>) -> Result<(), Failure> {
    let ok = match claims.get("aud") {
        Some(Value::Array(list)) => list.iter().any(|a| a.as_str() == Some(client_id)),
        Some(Value::String(aud)) => aud == client_id,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(claim_error())
    }
}

/// The URL of the discovery document: `<issuer path>/.well-known/openid-configuration`, a doubled slash collapsed.
pub fn discovery_url(issuer: &Url) -> Url {
    let mut url = issuer.clone();
    let path = format!("{}/.well-known/openid-configuration", url.path());
    url.set_path(&path.replacen("//", "/", 1));
    url
}


/// Borrowed form pairs of a token-style POST.
type FormPairs<'a> = Vec<(&'a str, &'a str)>;

impl OidcProcessor {
    pub fn new(provider: &str, client_id: &str, client_secret: &str) -> Self {
        Self {
            provider: provider.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            client_auth: ClientAuth::Post,
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn with_client_auth(mut self, client_auth: ClientAuth) -> Self {
        self.client_auth = client_auth;
        self
    }

    /// The discovery GET for `issuer`; the issuer must be an https URL.
    pub fn discovery_request(&self, issuer: &str) -> Result<HttpRequest, Failure> {
        let server = Url::parse(issuer).map_err(|_| with_kind(OidcErrorKind::Metadata, "Invalid URL"))?;
        check_https(&server)?;
        Ok(HttpRequest {
            method: "GET".into(),
            url: discovery_url(&server).to_string(),
            headers: vec![("Accept".into(), "application/json".into())],
            body: Vec::new(),
            timeout: Some(DISCOVERY_TIMEOUT),
        })
    }

    /// Check a discovery reply against the expected `issuer`: status, JSON object, matching `issuer`.
    pub fn process_discovery(
        &self,
        issuer: &str,
        response: &HttpResponse,
    ) -> Result<Map<String, Value>, Failure> {
        let server = Url::parse(issuer).map_err(|_| with_kind(OidcErrorKind::Metadata, "Invalid URL"))?;
        let metadata = |e: Failure| OidcError { kind: OidcErrorKind::Metadata, ..e };
        if response.status != 200 {
            return Err(metadata(status_error()));
        }
        let meta = json_body(response).map_err(metadata)?;
        let advertised = non_empty_string(&meta, "issuer").map_err(metadata)?;
        let advertised = Url::parse(&advertised).map_err(|_| with_kind(OidcErrorKind::Metadata, "Invalid URL"))?;
        if advertised.as_str() != server.as_str() {
            return Err(with_kind(
                OidcErrorKind::Metadata,
                "discovered metadata issuer does not match the expected issuer",
            ));
        }
        Ok(meta)
    }

    /// Build the authorization URL and the pending grant (PKCE verifier, subject) the caller stores under `state`.
    pub fn authorize(
        &self,
        meta: &Map<String, Value>,
        redirect_uri: &str,
        scopes: &[String],
        subject: Option<&str>,
        extra: &[(&str, &str)],
        random: impl Fn(usize) -> Vec<u8>,
    ) -> Result<(String, String, Pending), Failure> {
        let state = URL_SAFE_NO_PAD.encode(random(32));
        let verifier = URL_SAFE_NO_PAD.encode(random(32));
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut endpoint = resolve_endpoint(meta, "authorization_endpoint")?;

        let scope = scopes.join(" ");
        let mut params: Vec<(String, String)> = extra
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        for (k, v) in [
            ("redirect_uri", redirect_uri),
            ("scope", scope.as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ] {
            params.push((k.to_string(), v.to_string()));
        }
        if !params.iter().any(|(k, _)| k == "client_id") {
            params.push(("client_id".into(), self.client_id.clone()));
        }
        if !params
            .iter()
            .any(|(k, _)| k == "request_uri" || k == "request")
            && !params.iter().any(|(k, _)| k == "response_type")
        {
            params.push(("response_type".into(), "code".into()));
        }
        let mut all: Vec<(String, String)> = endpoint
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        all.extend(params);
        endpoint.set_query(None);
        endpoint.query_pairs_mut().extend_pairs(all);

        let pending = Pending {
            provider: self.provider.clone(),
            redirect_uri: redirect_uri.to_string(),
            verifier,
            subject: subject.map(str::to_string),
            scopes: scopes.to_vec(),
        };
        Ok((endpoint.to_string(), state, pending))
    }


    /// The grant's redirect URI must parse, before any network leg.
    pub fn check_grant_redirect(&self, grant: &Pending) -> Result<(), Failure> {
        Url::parse(&grant.redirect_uri).map(|_| ()).map_err(|_| with_kind(OidcErrorKind::Input, "Invalid URL"))
    }

    /// The code-exchange POST for a grant taken from the pending store, after the callback checks.
    pub fn exchange_request(
        &self,
        meta: &Map<String, Value>,
        grant: &Pending,
        code: &str,
        state: &str,
    ) -> Result<HttpRequest, Failure> {
        if grant.provider != self.provider {
            return Err(plain(format!(
                "grant was issued by {}, not {}",
                grant.provider, self.provider
            )));
        }
        let mut callback = Url::parse(&grant.redirect_uri).map_err(|_| with_kind(OidcErrorKind::Input, "Invalid URL"))?;

        let mut query: Vec<(String, String)> = callback
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .filter(|(k, _)| k != "code" && k != "state")
            .collect();
        query.push(("code".into(), code.to_string()));
        query.push(("state".into(), state.to_string()));
        let has = |name: &str| query.iter().any(|(k, _)| k == name);
        if has("response") {
            return Err(invalid_response());
        }
        let iss = query
            .iter()
            .find(|(k, _)| k == "iss")
            .map(|(_, v)| v.clone());
        let iss_required = meta
            .get("authorization_response_iss_parameter_supported")
            .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)));
        match &iss {
            None if iss_required => return Err(invalid_response()),
            Some(iss) if !iss.is_empty() && Some(iss.as_str()) != string_of(meta, "issuer") => {
                return Err(invalid_response())
            }
            _ => {}
        }
        if has("error") {
            return Err(with_kind(OidcErrorKind::Rejected, "authorization response from the server is an error"));
        }

        callback.set_query(None);
        callback.set_fragment(None);
        let endpoint = resolve_endpoint(meta, "token_endpoint")?;
        let redirect = callback.to_string();
        let mut body = vec![("redirect_uri", redirect.as_str()), ("code", code)];
        body.push(("code_verifier", grant.verifier.as_str()));
        self.token_request(
            &endpoint,
            "authorization_code",
            body,
        )
    }

    pub fn refresh_request(
        &self,
        meta: &Map<String, Value>,
        refresh_token: &str,
    ) -> Result<HttpRequest, Failure> {
        if refresh_token.is_empty() {
            return Err(with_kind(OidcErrorKind::Input, "\"refreshToken\" must not be empty"));
        }
        let endpoint = resolve_endpoint(meta, "token_endpoint")?;
        self.token_request(&endpoint, "refresh_token", vec![("refresh_token", refresh_token)])
    }

    pub fn revoke_request(
        &self,
        meta: &Map<String, Value>,
        token: &str,
    ) -> Result<HttpRequest, Failure> {
        if token.is_empty() {
            return Err(with_kind(OidcErrorKind::Input, "\"token\" must not be empty"));
        }
        let endpoint = resolve_endpoint(meta, "revocation_endpoint")?;
        let (mut headers, pairs) = self.client_credentials(vec![("token", token)]);
        headers.insert(0, ("Accept".into(), "*/*".into()));
        headers.push(("Content-Type".into(), FORM_TYPE.into()));
        Ok(HttpRequest {
            method: "POST".into(),
            url: endpoint.to_string(),
            headers,
            body: form_body(&pairs).into_bytes(),
            timeout: None,
        })
    }

    pub fn process_revoke(&self, response: &HttpResponse) -> Result<(), Failure> {
        check_body_error(response, 200)
    }

    pub fn userinfo_request(
        &self,
        meta: &Map<String, Value>,
        access_token: &str,
    ) -> Result<HttpRequest, Failure> {
        if access_token.is_empty() {
            return Err(with_kind(OidcErrorKind::Input, "\"accessToken\" must not be empty"));
        }
        let endpoint = resolve_endpoint(meta, "userinfo_endpoint")?;
        Ok(HttpRequest {
            method: "GET".into(),
            url: endpoint.to_string(),
            headers: vec![
                ("Accept".into(), "application/json, application/jwt".into()),
                ("Authorization".into(), format!("Bearer {access_token}")),
            ],
            body: Vec::new(),
            timeout: None,
        })
    }

    /// The UserInfo claims; `expected_subject` is the ID token's `sub` when known.
    pub fn process_userinfo(
        &self,
        meta: &Map<String, Value>,
        response: &HttpResponse,
        expected_subject: Option<&str>,
        now_ms: i64,
    ) -> Result<Map<String, Value>, Failure> {
        if has_challenge(response) {
            return Err(with_kind(
                OidcErrorKind::Rejected,
                "server responded with a challenge in the WWW-Authenticate HTTP Header",
            ));
        }
        if response.status != 200 {
            return Err(OidcError { kind: OidcErrorKind::Rejected, ..status_error() });
        }
        let info = if content_type(response).as_deref() == Some("application/jwt") {
            let algs = meta
                .get("userinfo_signing_alg_values_supported")
                .and_then(Value::as_array);
            let jwt = validate_jwt(
                &response.text(),
                &AlgRule::UserInfo(algs),
                (now_ms / 1000) as f64,
            )?;
            if jwt.claims.contains_key("aud") {
                validate_audience(&self.client_id, &jwt.claims)?;
            }
            if jwt.claims.contains_key("iss") {
                validate_issuer(meta, &jwt.claims)?;
            }
            jwt.claims
        } else {
            json_body(response)?
        };
        let sub = non_empty_string(&info, "sub")?;
        if let Some(expected) = expected_subject {
            if sub != expected {
                return Err(with_kind(OidcErrorKind::Mismatch, "unexpected JSON attribute value encountered"));
            }
        }
        Ok(info)
    }

    /// The client's proof for a token-style POST: the header (basic) and the form pairs, with the pairs extended.
    fn client_credentials<'a>(
        &'a self,
        mut pairs: Vec<(&'a str, &'a str)>,
    ) -> (Vec<(String, String)>, FormPairs<'a>) {
        let mut headers = Vec::new();
        match self.client_auth {
            ClientAuth::Post => {
                pairs.push(("client_id", &self.client_id));
                pairs.push(("client_secret", &self.client_secret));
            }
            ClientAuth::Basic => {
                let credential = format!(
                    "{}:{}",
                    form_urlencode_value(&self.client_id),
                    form_urlencode_value(&self.client_secret)
                );
                headers.push((
                    "Authorization".to_string(),
                    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(credential)),
                ));
            }
            ClientAuth::None => pairs.push(("client_id", &self.client_id)),
        }
        (headers, pairs)
    }

    /// A token-endpoint POST.
    fn token_request<'a>(
        &'a self,
        endpoint: &Url,
        grant_type: &'a str,
        mut pairs: Vec<(&'a str, &'a str)>,
    ) -> Result<HttpRequest, Failure> {
        pairs.push(("grant_type", grant_type));
        let (mut headers, pairs) = self.client_credentials(pairs);
        headers.insert(0, ("Accept".into(), "application/json".into()));
        headers.push(("Content-Type".into(), FORM_TYPE.into()));
        Ok(HttpRequest {
            method: "POST".into(),
            url: endpoint.to_string(),
            headers,
            body: form_body(&pairs).into_bytes(),
            timeout: None,
        })
    }

    /// `processGenericAccessTokenResponse` + `tokensOf`.
    pub fn process_tokens(
        &self,
        meta: &Map<String, Value>,
        response: &HttpResponse,
        now_ms: i64,
        code_grant: bool,
    ) -> Result<Tokens, Failure> {
        check_body_error(response, 200)?;
        let json = json_body(response)?;
        let access_token = non_empty_string(&json, "access_token")?;
        let token_type = non_empty_string(&json, "token_type")?.to_lowercase();

        let expires_in = match json.get("expires_in") {
            None => None,
            Some(value) => {
                let parsed = match value {
                    Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                    Value::String(s) => js_parse_float(s),
                    Value::Null => f64::NAN,
                    other => js_parse_float(&other.to_string()),
                };
                if !parsed.is_finite() || parsed < 0.0 {
                    return Err(invalid_response());
                }
                Some(parsed)
            }
        };
        let refresh_token = match json.get("refresh_token") {
            None => None,
            Some(_) => Some(non_empty_string(&json, "refresh_token")?),
        };
        let scope = match json.get("scope") {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => return Err(invalid_response()),
        };

        let mut id_token_subject = None;
        if json.contains_key("id_token") {
            let id_token = non_empty_string(&json, "id_token")?;
            let algs = meta
                .get("id_token_signing_alg_values_supported")
                .and_then(Value::as_array);
            let jwt = validate_jwt(&id_token, &AlgRule::IdToken(algs), (now_ms / 1000) as f64)?;
            for claim in ["aud", "exp", "iat", "iss", "sub"] {
                if !jwt.claims.contains_key(claim) {
                    return Err(invalid_response());
                }
            }
            validate_issuer(meta, &jwt.claims)?;
            validate_audience(&self.client_id, &jwt.claims)?;
            if let Some(Value::Array(aud)) = jwt.claims.get("aud") {
                if aud.len() != 1 {
                    match jwt.claims.get("azp") {
                        None => return Err(claim_error()),
                        Some(azp) if azp.as_str() != Some(self.client_id.as_str()) => {
                            return Err(claim_error())
                        }
                        Some(_) => {}
                    }
                }
            }
            if let Some(auth_time) = jwt.claims.get("auth_time") {
                if !auth_time.as_f64().is_some_and(f64::is_finite) {
                    return Err(invalid_response());
                }
            }
            if code_grant && jwt.claims.contains_key("nonce") {
                return Err(claim_error());
            }
            id_token_subject = jwt
                .claims
                .get("sub")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if token_type != "bearer" && token_type != "dpop" {
            return Err(fail("unsupported operation"));
        }

        Ok(Tokens {
            access_token,
            refresh_token,
            expires_in,
            scope: scope
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            id_token_subject,
        })
    }
}

/// `checkOAuthBodyError`: a status other than `expected` is a challenge, an OAuth error body, or just wrong.
fn check_body_error(response: &HttpResponse, expected: u16) -> Result<(), Failure> {
    body_error(response, expected).map_err(|e| OidcError { kind: OidcErrorKind::Rejected, ..e })
}

fn body_error(response: &HttpResponse, expected: u16) -> Result<(), Failure> {
    if response.status == expected {
        return Ok(());
    }
    if has_challenge(response) {
        return Err(fail(
            "server responded with a challenge in the WWW-Authenticate HTTP Header",
        ));
    }
    if (400..500).contains(&response.status) {
        if content_type(response).as_deref() != Some("application/json") {
            return Err(content_type_error());
        }
        if let Ok(Value::Object(body)) = serde_json::from_str::<Value>(&response.text()) {
            if matches!(body.get("error"), Some(Value::String(e)) if !e.is_empty()) {
                return Err(fail("server responded with an error in the response body"));
            }
        }
    }
    Err(status_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(value: &str) -> HttpResponse {
        HttpResponse {
            status: 401,
            headers: vec![("WWW-Authenticate".into(), value.into())],
            body: Vec::new(),
        }
    }

    #[test]
    fn the_discovery_document_sits_under_the_issuer_path() {
        let url = |issuer: &str| discovery_url(&Url::parse(issuer).expect("issuer")).to_string();
        assert_eq!(
            url("https://accounts.google.com"),
            "https://accounts.google.com/.well-known/openid-configuration"
        );
        assert_eq!(
            url("https://idp.example.com/tenant"),
            "https://idp.example.com/tenant/.well-known/openid-configuration"
        );
        assert_eq!(
            url("https://idp.example.com/tenant/"),
            "https://idp.example.com/tenant/.well-known/openid-configuration"
        );
    }

    #[test]
    fn parse_float_reads_a_numeric_prefix_like_js() {
        assert_eq!(js_parse_float("120"), 120.0);
        assert_eq!(js_parse_float(" 3.5e2xyz"), 350.0);
        assert_eq!(js_parse_float(".5"), 0.5);
        assert_eq!(js_parse_float("-7"), -7.0);
        assert_eq!(js_parse_float("5."), 5.0);
        assert_eq!(js_parse_float("1e"), 1.0);
        assert_eq!(js_parse_float("1e+"), 1.0);
        assert_eq!(js_parse_float("+Infinity"), f64::INFINITY);
        assert_eq!(js_parse_float("-Infinityx"), f64::NEG_INFINITY);
        assert!(js_parse_float("abc").is_nan());
        assert!(js_parse_float("").is_nan());
        assert!(js_parse_float(".").is_nan());
        assert!(js_parse_float("-").is_nan());
        assert!(js_parse_float("e5").is_nan());
    }

    #[test]
    fn a_challenge_needs_a_scheme_token_first() {
        assert!(has_challenge(&challenge("Bearer error=\"invalid_token\"")));
        assert!(has_challenge(&challenge("Bearer")));
        assert!(!has_challenge(&challenge("")));
        assert!(!has_challenge(&challenge("=oops")));
        assert!(!has_challenge(&HttpResponse { status: 401, headers: vec![], body: Vec::new() }));
    }
}
