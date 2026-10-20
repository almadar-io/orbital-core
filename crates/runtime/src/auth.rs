//! Who a request is, natively: the twin of the server half of `@almadar/auth`. A bearer is verified by the app's provider
//! (Firebase ID tokens against Google's published x509 certificates, generic OIDC against the issuer's JWKS, or an
//! `sk_live_` API key against stored scrypt hashes), then becomes `@user` the way the program's `[identity]` entity says.
//! The tokens are RS256/ES256 JWS checked by `jwt::verify_compact`; the keys are fetched through the host's `http` answer
//! and kept in the credential source's TTL cache, so the kernel's egress and address rules apply to them like to any call.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use orbital_core_base::codec;
use orbital_core_base::policy::identity_entity_def;
use orbital_core_base::schema::types::{OrbitalSchema, Persistence};
use orbital_core_base::value::Value;
use serde_json::{json, Value as Json};

use crate::http_service::{HttpRequest, SecretSource};
use crate::jwt::{is_unsigned, verify_compact, verify_unsigned, JwtVerifyError, KeySet, VerifyPolicy};
use crate::resumable::{IoAnswer, IoRequest};

/// Who vouched for a verified bearer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Firebase,
    Oidc,
    ApiKey,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Firebase => "firebase",
            Provider::Oidc => "oidc",
            Provider::ApiKey => "apikey",
        }
    }
}

/// A token the configured provider has verified. `claims` is the token's own payload.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedUser {
    pub uid: String,
    pub provider: Provider,
    pub email: Option<String>,
    pub name: Option<String>,
    pub tenant: Option<String>,
    pub claims: Json,
}

/// Why a bearer was refused: a stable `reason` word and the provider's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError {
    pub reason: String,
    pub message: String,
}

impl AuthError {
    fn new(reason: &str, message: impl Into<String>) -> Self {
        Self { reason: reason.to_string(), message: message.into() }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<JwtVerifyError> for AuthError {
    fn from(error: JwtVerifyError) -> Self {
        Self::new(error.reason(), error.to_string())
    }
}

pub trait TokenVerifier: Send + Sync {
    fn provider(&self) -> Provider;
    /// `tenant` `None` = the app has no tenant; a tenant-scoped token is then refused.
    fn verify(&self, token: &str, tenant: Option<&str>) -> Result<VerifiedUser, AuthError>;
}

/// The host's answer to an ask: the native host in a server, a fixture in a test.
pub type HostAnswer = Arc<dyn Fn(&IoRequest) -> IoAnswer + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// A published document (a key set, a discovery document), fetched through the host and kept for its `max-age`.
#[derive(Clone)]
pub struct Feed {
    host: HostAnswer,
    cache: Arc<dyn SecretSource>,
    now_ms: Clock,
    /// The only origins this feed fetches from: what the configured provider needs, never the program's `egress`.
    allowed: Vec<String>,
}

/// `scheme://host[:port]` of an `http(s)` URL, lowercased; `None` for any other URL or one that carries credentials.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

const DEFAULT_MAX_AGE_SECS: u64 = 600;
/// A key id the cache does not know refetches the document at most this often.
const REFETCH_COOLDOWN_MS: i64 = 30_000;

fn max_age_secs(cache_control: Option<&str>) -> u64 {
    cache_control
        .into_iter()
        .flat_map(|value| value.split(','))
        .find_map(|part| part.trim().strip_prefix("max-age=").and_then(|n| n.trim().parse().ok()))
        .unwrap_or(DEFAULT_MAX_AGE_SECS)
}

impl Feed {
    /// A feed that fetches through `host` from `allowed` origins only.
    pub fn new(host: HostAnswer, cache: Arc<dyn SecretSource>, now_ms: Clock, allowed: Vec<String>) -> Self {
        Self { host, cache, now_ms, allowed }
    }

    fn permit(&self, url: &str) -> Result<(), AuthError> {
        match origin_of(url) {
            Some(origin) if self.allowed.contains(&origin) => Ok(()),
            _ => Err(AuthError::new("origin-not-allowed", format!("{url} is not an origin the configured sign-in provider uses"))),
        }
    }

    /// A POST of `body` to `url` through the host, uncached: the emulator's user lookup.
    fn post_json(&self, url: &str, body: &Json) -> Result<Json, AuthError> {
        self.permit(url)?;
        let request = HttpRequest {
            method: "POST".to_string(),
            url: url.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), "Bearer owner".to_string()),
            ],
            body: body.to_string().into_bytes(),
            timeout: Some(Duration::from_secs(10)),
        };
        match (self.host)(&IoRequest::Http { request, key: None, delay_ms: None, site: None }) {
            IoAnswer::Http(Ok(reply)) if reply.is_success() => {
                serde_json::from_slice(&reply.body).map_err(|e| AuthError::new("emulator-unavailable", format!("{url}: {e}")))
            }
            IoAnswer::Http(Ok(reply)) => Err(AuthError::new("emulator-unavailable", format!("{url} answered {}", reply.status))),
            other => Err(AuthError::new("emulator-unavailable", format!("{url}: {other:?}"))),
        }
    }

    fn slot(url: &str) -> String {
        format!("auth:feed:{url}")
    }

    /// The document at `url`: from the cache while it is fresh (and, with `fresh`, not younger than the refetch
    /// cooldown), else fetched. `(body, fetched_at_ms)`.
    fn get(&self, url: &str, refetch: bool) -> Result<(String, i64), AuthError> {
        self.permit(url)?;
        let now = (self.now_ms)();
        if let Ok(Some(held)) = self.cache.secret(&Self::slot(url)) {
            if let Ok(entry) = serde_json::from_str::<Json>(&held) {
                let at = entry["at"].as_i64().unwrap_or(0);
                let body = entry["body"].as_str().unwrap_or_default();
                if !body.is_empty() && !(refetch && now - at >= REFETCH_COOLDOWN_MS) {
                    return Ok((body.to_string(), at));
                }
            }
        }
        let request = HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: vec![("accept".to_string(), "application/json".to_string())],
            body: Vec::new(),
            timeout: Some(Duration::from_secs(10)),
        };
        match (self.host)(&IoRequest::Http { request, key: None, delay_ms: None, site: None }) {
            IoAnswer::Http(Ok(reply)) if reply.is_success() => {
                let body = reply.text().into_owned();
                let ttl_ms = max_age_secs(reply.header("cache-control")) * 1000;
                let entry = json!({ "at": now, "body": body }).to_string();
                let _ = self.cache.store_secret(&Self::slot(url), &entry, ttl_ms);
                Ok((body, now))
            }
            IoAnswer::Http(Ok(reply)) => Err(AuthError::new("keys-unavailable", format!("{url} answered {}", reply.status))),
            IoAnswer::Http(Err(error)) => Err(AuthError::new("keys-unavailable", format!("{url}: {error:?}"))),
            other => Err(AuthError::new("keys-unavailable", format!("{url}: unexpected host answer {other:?}"))),
        }
    }

    /// Verify with the keys `parse` reads from the document at `url`; a key id the cached keys lack gets one refetch.
    fn verify_with(
        &self,
        url: &str,
        parse: &dyn Fn(&str) -> Result<KeySet, JwtVerifyError>,
        check: &dyn Fn(&KeySet) -> Result<Json, JwtVerifyError>,
    ) -> Result<Json, AuthError> {
        let (body, _) = self.get(url, false)?;
        match check(&parse(&body)?) {
            Err(JwtVerifyError::UnknownKey(_)) => {
                let (body, _) = self.get(url, true)?;
                Ok(check(&parse(&body)?)?)
            }
            other => Ok(other?),
        }
    }
}

fn text_claim(claims: &Json, name: &str) -> Option<String> {
    claims.get(name).and_then(Json::as_str).map(str::to_string)
}

fn seconds(clock: &Clock) -> i64 {
    clock() / 1000
}

/// Generic OIDC: the issuer's discovery document names its JWKS; no tenants.
pub struct OidcVerifier {
    pub issuer: String,
    pub audience: String,
    feed: Feed,
    clock: Clock,
}

impl OidcVerifier {
    pub fn new(issuer: &str, audience: &str, feed: Feed, clock: Clock) -> Self {
        Self { issuer: issuer.to_string(), audience: audience.to_string(), feed, clock }
    }

    fn jwks_uri(&self) -> Result<String, AuthError> {
        let url = format!("{}/.well-known/openid-configuration", self.issuer.trim_end_matches('/'));
        let (body, _) = self.feed.get(&url, false)?;
        let doc: Json = serde_json::from_str(&body).map_err(|e| AuthError::new("keys-unavailable", format!("{url}: {e}")))?;
        if doc["issuer"].as_str() != Some(self.issuer.as_str()) {
            return Err(AuthError::new("keys-unavailable", format!("discovery issuer {} is not {}", doc["issuer"], self.issuer)));
        }
        doc["jwks_uri"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| AuthError::new("keys-unavailable", format!("{url} names no jwks_uri")))
    }
}

impl TokenVerifier for OidcVerifier {
    fn provider(&self) -> Provider {
        Provider::Oidc
    }

    fn verify(&self, token: &str, tenant: Option<&str>) -> Result<VerifiedUser, AuthError> {
        if let Some(tenant) = tenant {
            return Err(AuthError::new("tenant", format!("the oidc provider has no tenants, asked for {tenant}")));
        }
        let uri = self.jwks_uri()?;
        let policy = VerifyPolicy { issuer: &self.issuer, audience: &self.audience, now: seconds(&self.clock) };
        let claims = self.feed.verify_with(&uri, &|text| Ok(KeySet::from_jwks(text)?), &|keys| verify_compact(token, keys, &policy))?;
        Ok(VerifiedUser {
            uid: text_claim(&claims, "sub").unwrap_or_default(),
            provider: Provider::Oidc,
            email: text_claim(&claims, "email"),
            name: text_claim(&claims, "name"),
            tenant: None,
            claims,
        })
    }
}

/// Where Google publishes the x509 certificates that sign Firebase / Identity Platform ID tokens.
pub const FIREBASE_CERTS_URL: &str =
    "https://www.googleapis.com/robot/v1/metadata/x509/securetoken@system.gserviceaccount.com";

/// Firebase / Identity Platform ID tokens. `tenant` set: the token must belong to that tenant; `None`: a project token, and a
/// tenant token (a published app's end user) is refused.
pub struct FirebaseVerifier {
    pub project_id: String,
    pub certs_url: String,
    feed: Feed,
    clock: Clock,
    /// `FIREBASE_AUTH_EMULATOR_HOST` (`host:port`), when the Auth emulator is in use.
    emulator: Option<String>,
}

impl FirebaseVerifier {
    pub fn new(project_id: &str, certs_url: &str, feed: Feed, clock: Clock) -> Self {
        Self { project_id: project_id.to_string(), certs_url: certs_url.to_string(), feed, clock, emulator: None }
    }

    /// Talk to the Firebase Auth emulator at `host`, as the Admin SDK does when `FIREBASE_AUTH_EMULATOR_HOST` is set: an
    /// unsigned token is accepted (its claims are still checked, its absent signature is not), a signed one is verified as
    /// ever, and either way the emulator must know the user and not have disabled them, since an emulator token vouches
    /// for nothing by itself.
    pub fn with_emulator(mut self, host: Option<&str>) -> Self {
        self.emulator = host.map(str::to_string);
        self
    }

    fn emulator_knows(&self, uid: &str, tenant: Option<&str>, host: &str) -> Result<(), AuthError> {
        let scope = tenant.map_or(String::new(), |t| format!("/tenants/{t}"));
        let url = format!("http://{host}/identitytoolkit.googleapis.com/v1/projects/{}{scope}/accounts:lookup", self.project_id);
        let body = self.feed.post_json(&url, &json!({ "localId": [uid] }))?;
        let user = body["users"].as_array().and_then(|users| users.first());
        match user {
            None => Err(AuthError::new("user-not-found", format!("the emulator has no user {uid}"))),
            Some(user) if user["disabled"].as_bool() == Some(true) => Err(AuthError::new("user-disabled", format!("user {uid} is disabled"))),
            Some(_) => Ok(()),
        }
    }
}

impl TokenVerifier for FirebaseVerifier {
    fn provider(&self) -> Provider {
        Provider::Firebase
    }

    fn verify(&self, token: &str, tenant: Option<&str>) -> Result<VerifiedUser, AuthError> {
        let issuer = format!("https://securetoken.google.com/{}", self.project_id);
        let now = seconds(&self.clock);
        let policy = VerifyPolicy { issuer: &issuer, audience: &self.project_id, now };
        let claims = if self.emulator.is_some() && is_unsigned(token) {
            verify_unsigned(token, &policy)?
        } else {
            self.feed.verify_with(&self.certs_url, &|text| Ok(KeySet::from_x509_map(text)?), &|keys| verify_compact(token, keys, &policy))?
        };
        let auth_time = claims.get("auth_time").and_then(Json::as_f64);
        if !auth_time.is_some_and(|at| at <= now as f64) {
            return Err(AuthError::new("auth-time", "auth_time is missing or in the future"));
        }
        let token_tenant = claims.get("firebase").and_then(|f| f.get("tenant")).and_then(Json::as_str).map(str::to_string);
        if token_tenant.as_deref() != tenant {
            return Err(AuthError::new("tenant", format!("token tenant {token_tenant:?} is not {tenant:?}")));
        }
        let uid = text_claim(&claims, "sub").unwrap_or_default();
        if let Some(host) = &self.emulator {
            self.emulator_knows(&uid, token_tenant.as_deref(), host)?;
        }
        Ok(VerifiedUser {
            uid,
            provider: Provider::Firebase,
            email: text_claim(&claims, "email"),
            name: text_claim(&claims, "name"),
            tenant: token_tenant,
            claims,
        })
    }
}

/// Every API key starts with this, which is how a bearer is routed to the key verifier.
pub const API_KEY_PREFIX: &str = "sk_live_";

/// A stored key as the host's lookup reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyRecord {
    pub principal: String,
    pub revoked: bool,
    /// Epoch ms; absent for a key that never expires.
    pub expires_at: Option<i64>,
    pub claims: Json,
}

pub type KeyLookup = Arc<dyn Fn(&str) -> Result<Option<ApiKeyRecord>, String> + Send + Sync>;

/// The scrypt hash (hex) of an API key over the hex salt, the form stored and looked up.
pub fn hash_api_key(plaintext: &str, salt_hex: &str) -> Result<String, String> {
    let salt = codec::hex_decode(salt_hex).ok_or("the API key salt must be non-empty hex (ALMADAR_API_KEY_SALT)")?;
    let digest = codec::scrypt_api_key(plaintext.as_bytes(), &salt)?;
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

pub struct ApiKeyVerifier {
    salt: String,
    lookup: KeyLookup,
    clock: Clock,
}

impl ApiKeyVerifier {
    pub fn new(salt_hex: &str, lookup: KeyLookup, clock: Clock) -> Result<Self, String> {
        codec::hex_decode(salt_hex).ok_or("the API key salt must be non-empty hex (ALMADAR_API_KEY_SALT)")?;
        Ok(Self { salt: salt_hex.to_string(), lookup, clock })
    }
}

impl TokenVerifier for ApiKeyVerifier {
    fn provider(&self) -> Provider {
        Provider::ApiKey
    }

    fn verify(&self, token: &str, _tenant: Option<&str>) -> Result<VerifiedUser, AuthError> {
        if !token.starts_with(API_KEY_PREFIX) {
            return Err(AuthError::new("not-a-key", "not an API key"));
        }
        let hash = hash_api_key(token, &self.salt).map_err(|e| AuthError::new("salt", e))?;
        let record = (self.lookup)(&hash).map_err(|e| AuthError::new("lookup", e))?;
        let Some(record) = record else { return Err(AuthError::new("unknown", "unknown API key")) };
        if record.revoked {
            return Err(AuthError::new("revoked", "revoked API key"));
        }
        if record.expires_at.is_some_and(|at| at <= (self.clock)()) {
            return Err(AuthError::new("expired", "expired API key"));
        }
        Ok(VerifiedUser {
            uid: record.principal,
            provider: Provider::ApiKey,
            email: None,
            name: None,
            tenant: None,
            claims: record.claims,
        })
    }
}

/// The app's sign-in verifier, with `sk_live_...` bearers routed to its API keys instead.
pub struct WithApiKeys {
    pub sign_in: Arc<dyn TokenVerifier>,
    pub keys: Arc<dyn TokenVerifier>,
}

impl TokenVerifier for WithApiKeys {
    fn provider(&self) -> Provider {
        self.sign_in.provider()
    }

    fn verify(&self, token: &str, tenant: Option<&str>) -> Result<VerifiedUser, AuthError> {
        if token.starts_with(API_KEY_PREFIX) { self.keys.verify(token, tenant) } else { self.sign_in.verify(token, tenant) }
    }
}

/// The app's sign-in tenant of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tenant {
    /// The request belongs to an app with no sign-in at all: always refused.
    NoSignIn,
    /// An app or route with no tenant.
    None,
    App(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthOutcome {
    Verified(VerifiedUser),
    Refused { status: u16, error: String },
}

const BEARER_PREFIX: &str = "Bearer ";

/// Who a request's bearer is, or why it is refused. Framework-free: the server middleware calls it.
pub fn authenticate_bearer(verifier: &dyn TokenVerifier, authorization: Option<&str>, tenant: &Tenant) -> AuthOutcome {
    let refused = |error: &str| AuthOutcome::Refused { status: 401, error: error.to_string() };
    let Some(token) = authorization.and_then(|value| value.strip_prefix(BEARER_PREFIX)) else {
        return refused("Authorization header missing or malformed");
    };
    let tenant = match tenant {
        Tenant::NoSignIn => return refused("This app has no sign-in tenant"),
        Tenant::None => None,
        Tenant::App(id) => Some(id.as_str()),
    };
    match verifier.verify(token, tenant) {
        Ok(user) => AuthOutcome::Verified(user),
        Err(error) => {
            tracing::info!(target: orbital_core_base::logging::ns::SERVER_DATA, provider = verifier.provider().name(), reason = %error.reason, "auth:rejected");
            refused("Unauthorized")
        }
    }
}

// ----- the environment contract --------------------------------------------------------------------

pub type AuthEnv = HashMap<String, String>;

fn env_text<'a>(env: &'a AuthEnv, name: &str) -> Option<&'a str> {
    env.get(name).map(String::as_str).filter(|v| !v.is_empty())
}

/// What keeps the declared auth config from working in production, one line per problem (empty when none). The native
/// Firebase provider verifies against Google's public certificates, so it needs `FIREBASE_PROJECT_ID` and nothing secret.
pub fn auth_env_problems(env: &AuthEnv) -> Vec<String> {
    let provider = env_text(env, "AUTH_PROVIDER").unwrap_or("firebase");
    if provider != "firebase" && provider != "oidc" {
        return vec![format!("AUTH_PROVIDER must be one of firebase, oidc, got '{provider}'")];
    }
    let mut problems = Vec::new();
    if env_text(env, "FIREBASE_AUTH_EMULATOR_HOST").is_some() {
        problems.push("FIREBASE_AUTH_EMULATOR_HOST is set: the Auth emulator is for local dev only".to_string());
    }
    if provider == "firebase" {
        match env_text(env, "FIREBASE_PROJECT_ID") {
            None => problems.push("AUTH_PROVIDER=firebase needs FIREBASE_PROJECT_ID".to_string()),
            Some(project) if project.starts_with("demo-") => {
                problems.push(format!("FIREBASE_PROJECT_ID={project} is an emulator-only demo project"))
            }
            Some(_) => {}
        }
    } else {
        if env_text(env, "OIDC_ISSUER_URL").is_none() {
            problems.push("AUTH_PROVIDER=oidc needs OIDC_ISSUER_URL".to_string());
        }
        if env_text(env, "OIDC_AUDIENCE").is_none() {
            problems.push("AUTH_PROVIDER=oidc needs OIDC_AUDIENCE".to_string());
        }
    }
    problems
}

/// The app's declared provider: `AUTH_PROVIDER` (firebase | oidc, default firebase); `oidc` requires `OIDC_ISSUER_URL` and
/// `OIDC_AUDIENCE`, `firebase` requires `FIREBASE_PROJECT_ID`; a missing one fails at boot. The verifier fetches its keys
/// through `feed_host(allowed)`, `allowed` being the origins the provider needs and nothing else: Firebase, the origin of the
/// certificates (Google's, or `FIREBASE_CERTS_URL`) and the Auth emulator when one is set; OIDC, the issuer's origin and,
/// when the JWKS lives elsewhere, the origin `OIDC_JWKS_ORIGIN` names.
pub fn verifier_from_env(
    env: &AuthEnv,
    feed_host: &dyn Fn(&[String]) -> HostAnswer,
    cache: Arc<dyn SecretSource>,
    clock: Clock,
) -> Result<Arc<dyn TokenVerifier>, String> {
    let feed_for = |allowed: Vec<String>| Feed::new(feed_host(&allowed), cache.clone(), clock.clone(), allowed);
    match env_text(env, "AUTH_PROVIDER").unwrap_or("firebase") {
        "firebase" => {
            let project = env_text(env, "FIREBASE_PROJECT_ID").ok_or("@almadar/auth: AUTH_PROVIDER=firebase requires FIREBASE_PROJECT_ID")?;
            let certs = env_text(env, "FIREBASE_CERTS_URL").unwrap_or(FIREBASE_CERTS_URL);
            let emulator = env_text(env, "FIREBASE_AUTH_EMULATOR_HOST");
            let mut allowed = vec![origin_of(certs).ok_or_else(|| format!("@almadar/auth: FIREBASE_CERTS_URL {certs:?} is not an http(s) URL"))?];
            allowed.extend(emulator.map(|host| format!("http://{}", host.to_ascii_lowercase())));
            Ok(Arc::new(FirebaseVerifier::new(project, certs, feed_for(allowed), clock.clone()).with_emulator(emulator)))
        }
        "oidc" => {
            let issuer = env_text(env, "OIDC_ISSUER_URL").ok_or("@almadar/auth: AUTH_PROVIDER=oidc requires OIDC_ISSUER_URL")?;
            let audience = env_text(env, "OIDC_AUDIENCE").ok_or("@almadar/auth: AUTH_PROVIDER=oidc requires OIDC_AUDIENCE")?;
            let mut allowed = vec![origin_of(issuer).ok_or_else(|| format!("@almadar/auth: OIDC_ISSUER_URL {issuer:?} is not an http(s) URL"))?];
            if let Some(jwks) = env_text(env, "OIDC_JWKS_ORIGIN") {
                allowed.push(origin_of(jwks).ok_or_else(|| format!("@almadar/auth: OIDC_JWKS_ORIGIN {jwks:?} is not an http(s) origin"))?);
            }
            Ok(Arc::new(OidcVerifier::new(issuer, audience, feed_for(allowed), clock.clone())))
        }
        other => Err(format!("@almadar/auth: AUTH_PROVIDER must be one of firebase, oidc, got '{other}'")),
    }
}

// ----- the viewer -----------------------------------------------------------------------------------

/// Where `@user`'s fields come from, read off the program's `[identity]` entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentitySource {
    /// No identity entity: the token's own claims.
    None,
    /// `[runtime, identity]`: the entity's declared fields name the claims read.
    Claims(Vec<String>),
    /// `[persistent, identity]`: the row keyed by the verified uid.
    Directory(String),
}

pub fn identity_source_of(schema: &OrbitalSchema) -> IdentitySource {
    let Some(entity) = identity_entity_def(schema) else { return IdentitySource::None };
    if entity.persistence == Some(Persistence::Runtime) {
        IdentitySource::Claims(entity.fields.iter().filter_map(|f| f.name.clone()).collect())
    } else {
        IdentitySource::Directory(entity.name.clone())
    }
}

/// The identity of an unauthenticated viewer: a definite object, so a role predicate is a definite `false`.
pub fn anonymous_user() -> Value {
    json_to_value(&json!({ "id": "anonymous", "role": "anonymous", "permissions": [] }))
}

fn json_to_value(json: &Json) -> Value {
    serde_json::from_value(json.clone()).unwrap_or(Value::Null)
}

fn token_user(verified: &VerifiedUser) -> serde_json::Map<String, Json> {
    let mut user = serde_json::Map::new();
    user.insert("id".to_string(), json!(verified.uid));
    if let Some(email) = &verified.email {
        user.insert("email".to_string(), json!(email));
    }
    if let Some(name) = &verified.name {
        user.insert("name".to_string(), json!(name));
    }
    user
}

/// A directory lookup: the row of `entity` with `id`, when it exists.
pub type DirectoryLookup<'a> = &'a dyn Fn(&str, &str) -> Result<Option<Json>, String>;

/// The one place a verified token becomes `@user`. No token is the anonymous viewer; a directory user with no row yet carries
/// only the token's fields.
pub fn resolve_viewer(
    verified: Option<&VerifiedUser>,
    source: &IdentitySource,
    lookup: Option<DirectoryLookup<'_>>,
) -> Result<Value, String> {
    let Some(verified) = verified else { return Ok(anonymous_user()) };
    let user = match source {
        IdentitySource::None => {
            let mut raw = verified.claims.as_object().cloned().unwrap_or_default();
            raw.insert("uid".to_string(), json!(verified.uid));
            raw.remove("email");
            raw.remove("name");
            if let Some(email) = &verified.email {
                raw.insert("email".to_string(), json!(email));
            }
            if let Some(name) = &verified.name {
                raw.insert("name".to_string(), json!(name));
            }
            let id = raw
                .get("id")
                .or_else(|| raw.get("uid"))
                .and_then(Json::as_str)
                .filter(|id| !id.is_empty())
                .ok_or("@almadar/auth: a verified token carries no subject")?
                .to_string();
            let mut user = serde_json::Map::new();
            user.insert("id".to_string(), json!(id));
            for (key, value) in &raw {
                if matches!(key.as_str(), "id" | "name" | "displayName" | "email") {
                    continue;
                }
                user.insert(key.clone(), value.clone());
            }
            let name = raw.get("name").or_else(|| raw.get("displayName")).and_then(Json::as_str).filter(|n| !n.is_empty());
            if let Some(name) = name {
                user.insert("name".to_string(), json!(name));
            }
            if let Some(email) = raw.get("email").and_then(Json::as_str).filter(|e| !e.is_empty()) {
                user.insert("email".to_string(), json!(email));
            }
            user
        }
        IdentitySource::Claims(fields) => {
            let mut user = token_user(verified);
            for field in fields {
                if matches!(field.as_str(), "id" | "email" | "name") {
                    continue;
                }
                if let Some(value) = verified.claims.get(field) {
                    user.insert(field.clone(), value.clone());
                }
            }
            user
        }
        IdentitySource::Directory(entity) => {
            let lookup = lookup
                .ok_or_else(|| format!("@almadar/auth: [identity] entity {entity} is persistent but no lookup was given"))?;
            match lookup(entity, &verified.uid)? {
                None => token_user(verified),
                Some(row) => {
                    let mut persona = serde_json::Map::new();
                    persona.insert("id".to_string(), json!(verified.uid));
                    for (key, value) in row.as_object().into_iter().flatten() {
                        if key != "id" {
                            persona.insert(key.clone(), value.clone());
                        }
                    }
                    persona
                }
            }
        }
    };
    Ok(json_to_value(&Json::Object(user)))
}

// ----- a configured sign-in --------------------------------------------------------------------------

/// A row finder: the row of `entity` with `id`, when it exists.
pub type RowFinder = Arc<dyn Fn(&str, &str) -> Result<Option<Json>, String> + Send + Sync>;

/// A configured sign-in: who verifies a bearer, where `@user` comes from, and the row finder the API keys and a
/// directory identity read through.
#[derive(Clone)]
pub struct SignIn {
    pub verifier: Arc<dyn TokenVerifier>,
    pub identity: IdentitySource,
    pub find: RowFinder,
}

/// An API key row (`std-api-keys`' `ApiKeyRecord`: `id` is the scrypt hash of the key, `owner` who it acts as, `revoked`,
/// `expiresAt` in epoch ms with 0 for never) read from `entity` by the hash the bearer hashes to. The row's remaining fields
/// (label, whatever the minting program added) travel as the verified user's claims.
pub fn api_key_lookup(find: RowFinder, entity: &str) -> KeyLookup {
    let entity = entity.to_string();
    Arc::new(move |hash: &str| {
        let Some(row) = find(&entity, hash)? else { return Ok(None) };
        let field = |name: &str| row.get(name).cloned().unwrap_or(Json::Null);
        let principal = field("owner").as_str().filter(|o| !o.is_empty()).map(str::to_string);
        let Some(principal) = principal else { return Err(format!("the API key row {hash} names no owner")) };
        let mut claims = row.as_object().cloned().unwrap_or_default();
        for key in ["id", "owner", "revoked", "expiresAt"] {
            claims.remove(key);
        }
        Ok(Some(ApiKeyRecord {
            principal,
            revoked: field("revoked").as_bool().unwrap_or(false),
            expires_at: field("expiresAt").as_f64().filter(|at| *at > 0.0).map(|at| at as i64),
            claims: Json::Object(claims),
        }))
    })
}

/// The program's sign-in as the environment declares it (`AUTH_PROVIDER`, `FIREBASE_PROJECT_ID`, `OIDC_*`, and
/// `ALMADAR_API_KEY_SALT` with `ALMADAR_API_KEY_ENTITY` for API keys). Under `NODE_ENV=production` the declaration is
/// required and any problem in it is an error; elsewhere none declared is `None` (an open server), and a declaration with
/// problems only warns. The keys are fetched through `feed_host` (see [`verifier_from_env`]) and kept in `cache`.
pub fn sign_in_from_env(
    env: &AuthEnv,
    schema: &OrbitalSchema,
    feed_host: &dyn Fn(&[String]) -> HostAnswer,
    cache: Arc<dyn SecretSource>,
    clock: Clock,
    find: RowFinder,
) -> Result<Option<SignIn>, String> {
    let production = env.get("NODE_ENV").is_some_and(|v| v == "production");
    let declared = ["AUTH_PROVIDER", "FIREBASE_PROJECT_ID", "OIDC_ISSUER_URL"].iter().any(|k| env.get(*k).is_some_and(|v| !v.is_empty()));
    if !production && !declared {
        return Ok(None);
    }
    let problems = auth_env_problems(env);
    if production && !problems.is_empty() {
        return Err(format!("refusing to boot in production with a broken sign-in config:\n  {}", problems.join("\n  ")));
    }
    for problem in &problems {
        tracing::warn!(target: orbital_core_base::logging::ns::SERVER_DATA, %problem, "auth config");
    }
    let sign_in = verifier_from_env(env, feed_host, cache, clock.clone())?;
    let verifier: Arc<dyn TokenVerifier> = match env.get("ALMADAR_API_KEY_SALT").filter(|v| !v.is_empty()) {
        Some(salt) => {
            let entity = env.get("ALMADAR_API_KEY_ENTITY").map_or("ApiKeyRecord", String::as_str);
            let keys: Arc<dyn TokenVerifier> = Arc::new(ApiKeyVerifier::new(salt, api_key_lookup(find.clone(), entity), clock)?);
            Arc::new(WithApiKeys { sign_in, keys })
        }
        None => sign_in,
    };
    Ok(Some(SignIn { verifier, identity: identity_source_of(schema), find }))
}

/// The `@user` a request's `Authorization` header resolves to. With `anonymous_ok`, no header is the anonymous viewer;
/// without it, no header is refused. A bearer that does not verify is refused (`Err` is the message to answer).
pub fn viewer_of(
    verifier: &dyn TokenVerifier,
    identity: &IdentitySource,
    tenant: &Tenant,
    authorization: Option<&str>,
    anonymous_ok: bool,
    find: DirectoryLookup<'_>,
) -> Result<Value, String> {
    if anonymous_ok && authorization.is_none() {
        return Ok(anonymous_user());
    }
    match authenticate_bearer(verifier, authorization, tenant) {
        AuthOutcome::Refused { error, .. } => Err(error),
        AuthOutcome::Verified(user) => resolve_viewer(Some(&user), identity, Some(find)).map_err(|error| {
            tracing::warn!(target: orbital_core_base::logging::ns::SERVER_DATA, %error, "auth:viewer");
            "Unauthorized".to_string()
        }),
    }
}

impl SignIn {
    /// [`viewer_of`] for an app with no tenants.
    pub fn viewer(&self, authorization: Option<&str>, anonymous_ok: bool) -> Result<Value, String> {
        let find = |entity: &str, id: &str| (self.find)(entity, id);
        viewer_of(self.verifier.as_ref(), &self.identity, &Tenant::None, authorization, anonymous_ok, &find)
    }
}

// ----- the tick invoker ------------------------------------------------------------------------------

/// The Google issuer whose OIDC tokens Cloud Scheduler signs, and where its keys are published.
pub const GOOGLE_ISSUER: &str = "https://accounts.google.com";
pub const GOOGLE_JWKS_ORIGIN: &str = "https://www.googleapis.com";

/// Who may run a tick pass over HTTP (`POST /api/tick`, the exported Terraform's scheduler): a token from the issuer for
/// the audience, whose verified `email` is the invoker's.
pub struct TickInvoker {
    verifier: Arc<dyn TokenVerifier>,
    email: String,
}

impl TickInvoker {
    pub fn new(verifier: Arc<dyn TokenVerifier>, email: &str) -> Self {
        Self { verifier, email: email.to_string() }
    }

    /// The invoker `ORB_TICK_INVOKER` names, its tokens for `ORB_TICK_AUDIENCE` (the service URL the scheduler calls);
    /// `None` when no invoker is named. An invoker without an audience is an error.
    pub fn from_env(
        env: &AuthEnv,
        feed_host: &dyn Fn(&[String]) -> HostAnswer,
        cache: Arc<dyn SecretSource>,
        clock: Clock,
    ) -> Result<Option<Self>, String> {
        let Some(email) = env_text(env, "ORB_TICK_INVOKER") else { return Ok(None) };
        let audience = env_text(env, "ORB_TICK_AUDIENCE")
            .ok_or("ORB_TICK_INVOKER needs ORB_TICK_AUDIENCE, the service URL the scheduler's token is minted for")?;
        let allowed = vec![GOOGLE_ISSUER.to_string(), GOOGLE_JWKS_ORIGIN.to_string()];
        let feed = Feed::new(feed_host(&allowed), cache, clock.clone(), allowed);
        Ok(Some(Self::new(Arc::new(OidcVerifier::new(GOOGLE_ISSUER, audience, feed, clock)), email)))
    }

    /// Whether `authorization` (the `Authorization` header) is the invoker's.
    pub fn admits(&self, authorization: Option<&str>) -> Result<(), AuthError> {
        let token = authorization
            .and_then(|header| header.strip_prefix(BEARER_PREFIX))
            .ok_or_else(|| AuthError::new("missing", "Authorization header missing or malformed"))?;
        let verified = self.verifier.verify(token, None)?;
        let email_verified = verified.claims.get("email_verified").and_then(Json::as_bool) == Some(true);
        match verified.email.as_deref() {
            Some(email) if email == self.email && email_verified => Ok(()),
            _ => Err(AuthError::new("not-invoker", "the token is not the tick invoker's")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::Credentials;
    use crate::http_service::HttpResponse;
    use orbital_core_base::schema::types::OrbitalSchema;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    fn fixture(name: &str) -> Json {
        let path = format!("{}/tests/fixtures/auth/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture json")
    }

    /// A host that answers GETs from `documents` and counts them; anything else is refused.
    struct Documents {
        by_url: Mutex<HashMap<String, String>>,
        served: Mutex<Vec<String>>,
    }

    impl Documents {
        fn new(pairs: &[(&str, String)]) -> Arc<Self> {
            Arc::new(Self {
                by_url: Mutex::new(pairs.iter().map(|(u, b)| (u.to_string(), b.clone())).collect()),
                served: Mutex::new(Vec::new()),
            })
        }
        fn answer(self: &Arc<Self>) -> HostAnswer {
            let this = self.clone();
            Arc::new(move |request: &IoRequest| {
                let IoRequest::Http { request, .. } = request else { panic!("only http is asked: {request:?}") };
                this.served.lock().unwrap().push(request.url.clone());
                match this.by_url.lock().unwrap().get(&request.url) {
                    Some(body) => IoAnswer::Http(Ok(crate::http_service::HttpResponse {
                        status: 200,
                        headers: vec![("Cache-Control".to_string(), "public, max-age=3600".to_string())],
                        body: body.clone().into_bytes(),
                    })),
                    None => IoAnswer::Http(Ok(HttpResponse { status: 404, headers: vec![], body: b"{}".to_vec() })),
                }
            })
        }
        fn count(&self, url: &str) -> usize {
            self.served.lock().unwrap().iter().filter(|u| *u == url).count()
        }
    }

    fn real_clock() -> Clock {
        Arc::new(|| orbital_core_base::time_util::now_millis())
    }

    fn feed(documents: &Arc<Documents>, clock: Clock) -> Feed {
        let mut allowed: Vec<String> = documents.by_url.lock().unwrap().keys().filter_map(|url| origin_of(url)).collect();
        allowed.sort();
        allowed.dedup();
        Feed::new(documents.answer(), Arc::new(Credentials::default()), clock, allowed)
    }

    fn env_of(pairs: &[(&str, &str)]) -> AuthEnv {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// `verifier_from_env` over `documents`, which also records which allowlist the verifier was given.
    fn from_env(documents: &Arc<Documents>, pairs: &[(&str, &str)]) -> (Result<Arc<dyn TokenVerifier>, String>, Arc<Mutex<Vec<Vec<String>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let answer = documents.answer();
        let feed_host = move |allowed: &[String]| -> HostAnswer {
            log.lock().unwrap().push(allowed.to_vec());
            answer.clone()
        };
        (verifier_from_env(&env_of(pairs), &feed_host, Arc::new(Credentials::default()), real_clock()), seen)
    }

    fn oidc_world() -> (Json, Arc<Documents>) {
        let fx = fixture("tokens.json");
        let o = &fx["oidc"];
        let documents = Documents::new(&[
            (&format!("{}/.well-known/openid-configuration", o["issuer"].as_str().unwrap()), o["discovery"].to_string()),
            (o["jwksUri"].as_str().unwrap(), o["jwks"].to_string()),
        ]);
        (fx, documents)
    }

    fn check(outcome: Result<VerifiedUser, AuthError>, expect: &Json, name: &str) {
        if expect["ok"].as_bool() == Some(true) {
            let user = outcome.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(Some(user.uid.as_str()), expect["uid"].as_str(), "{name}");
            for (field, got) in [("email", &user.email), ("name", &user.name), ("tenant", &user.tenant)] {
                if let Some(want) = expect[field].as_str() {
                    assert_eq!(got.as_deref(), Some(want), "{name}: {field}");
                }
            }
            for (key, want) in expect["claims"].as_object().into_iter().flatten() {
                assert_eq!(&user.claims[key], want, "{name}: claim {key}");
            }
        } else {
            let error = outcome.expect_err(&format!("{name}: accepted"));
            assert_eq!(Some(error.reason.as_str()), expect["reason"].as_str(), "{name}: {error}");
        }
    }

    #[test]
    fn oidc_tokens_over_the_shared_fixture() {
        let (fx, documents) = oidc_world();
        let o = &fx["oidc"];
        let verifier = OidcVerifier::new(o["issuer"].as_str().unwrap(), o["audience"].as_str().unwrap(), feed(&documents, real_clock()), real_clock());
        for case in o["cases"].as_array().unwrap() {
            check(verifier.verify(case["token"].as_str().unwrap(), None), &case["expect"], case["name"].as_str().unwrap());
        }
    }

    #[test]
    fn control_the_issuer_s_documents_are_fetched_once_and_then_kept() {
        let (fx, documents) = oidc_world();
        let o = &fx["oidc"];
        let verifier = OidcVerifier::new(o["issuer"].as_str().unwrap(), o["audience"].as_str().unwrap(), feed(&documents, real_clock()), real_clock());
        let token = o["cases"][0]["token"].as_str().unwrap();
        verifier.verify(token, None).unwrap();
        verifier.verify(token, None).unwrap();
        assert_eq!(documents.count(o["jwksUri"].as_str().unwrap()), 1);
        assert_eq!(documents.served.lock().unwrap().len(), 2, "discovery and keys, once each");
    }

    #[test]
    fn an_oidc_app_has_no_tenants() {
        let (fx, documents) = oidc_world();
        let o = &fx["oidc"];
        let verifier = OidcVerifier::new(o["issuer"].as_str().unwrap(), o["audience"].as_str().unwrap(), feed(&documents, real_clock()), real_clock());
        let error = verifier.verify(o["cases"][0]["token"].as_str().unwrap(), Some("t1")).unwrap_err();
        assert_eq!(error.reason, "tenant");
        assert!(documents.served.lock().unwrap().is_empty());
    }

    fn firebase_world(certs: &Json) -> (Json, Arc<Documents>) {
        let fx = fixture("tokens.json");
        let documents = Documents::new(&[(FIREBASE_CERTS_URL, certs.to_string())]);
        (fx, documents)
    }

    #[test]
    fn firebase_tokens_over_the_shared_fixture() {
        let certs = fixture("tokens.json")["firebase"]["certs"].clone();
        let (fx, documents) = firebase_world(&certs);
        let f = &fx["firebase"];
        let verifier = FirebaseVerifier::new(f["projectId"].as_str().unwrap(), FIREBASE_CERTS_URL, feed(&documents, real_clock()), real_clock());
        for case in f["cases"].as_array().unwrap() {
            check(verifier.verify(case["token"].as_str().unwrap(), case["tenant"].as_str()), &case["expect"], case["name"].as_str().unwrap());
        }
    }

    fn emulator_world() -> (Json, Arc<Documents>) {
        let fx = fixture("tokens.json");
        let documents = Documents::new(&[
            (FIREBASE_CERTS_URL, fx["firebase"]["certs"].to_string()),
            (EMULATOR_LOOKUP, json!({ "users": [{ "localId": "uid-1" }] }).to_string()),
            (EMULATOR_TENANT_LOOKUP, json!({ "users": [{ "localId": "uid-1" }] }).to_string()),
        ]);
        (fx, documents)
    }

    const EMULATOR_LOOKUP: &str = "http://127.0.0.1:9099/identitytoolkit.googleapis.com/v1/projects/test-project/accounts:lookup";
    const EMULATOR_TENANT_LOOKUP: &str = "http://127.0.0.1:9099/identitytoolkit.googleapis.com/v1/projects/test-project/tenants/t1/accounts:lookup";

    #[test]
    fn the_emulator_s_unsigned_tokens_over_the_shared_fixture() {
        for on in [true, false] {
            let (fx, documents) = emulator_world();
            let f = &fx["firebase"];
            let verifier = FirebaseVerifier::new("test-project", FIREBASE_CERTS_URL, feed(&documents, real_clock()), real_clock())
                .with_emulator(on.then_some("127.0.0.1:9099"));
            for case in f["emulatorCases"].as_array().unwrap() {
                if case["emulator"].as_bool() != Some(on) {
                    continue;
                }
                check(verifier.verify(case["token"].as_str().unwrap(), case["tenant"].as_str()), &case["expect"], case["name"].as_str().unwrap());
            }
        }
    }

    #[test]
    fn control_an_unsigned_token_is_refused_when_no_emulator_is_named() {
        let (fx, documents) = emulator_world();
        let verifier = FirebaseVerifier::new("test-project", FIREBASE_CERTS_URL, feed(&documents, real_clock()), real_clock());
        let unsigned = fx["firebase"]["emulatorCases"][0]["token"].as_str().unwrap();
        assert_eq!(verifier.verify(unsigned, None).unwrap_err().reason, "algorithm");
        assert_eq!(documents.count(EMULATOR_LOOKUP), 0, "no emulator is asked about");
    }

    #[test]
    fn the_emulator_must_know_the_user_and_not_have_disabled_them() {
        let (fx, documents) = emulator_world();
        let unsigned = fx["firebase"]["emulatorCases"][0]["token"].as_str().unwrap();
        let verifier = FirebaseVerifier::new("test-project", FIREBASE_CERTS_URL, feed(&documents, real_clock()), real_clock())
            .with_emulator(Some("127.0.0.1:9099"));
        assert!(verifier.verify(unsigned, None).is_ok());
        documents.by_url.lock().unwrap().insert(EMULATOR_LOOKUP.to_string(), json!({}).to_string());
        assert_eq!(verifier.verify(unsigned, None).unwrap_err().reason, "user-not-found");
        documents.by_url.lock().unwrap().insert(EMULATOR_LOOKUP.to_string(), json!({ "users": [{ "localId": "uid-1", "disabled": true }] }).to_string());
        assert_eq!(verifier.verify(unsigned, None).unwrap_err().reason, "user-disabled");
        documents.by_url.lock().unwrap().remove(EMULATOR_LOOKUP);
        assert_eq!(verifier.verify(unsigned, None).unwrap_err().reason, "emulator-unavailable");
    }

    #[test]
    fn a_key_id_the_cache_lacks_refetches_once_after_the_cooldown_and_picks_up_the_rotated_key() {
        let fx = fixture("tokens.json");
        let f = &fx["firebase"];
        let token = f["cases"][0]["token"].as_str().unwrap();
        let documents = Documents::new(&[(FIREBASE_CERTS_URL, json!({}).to_string())]);
        let now = Arc::new(AtomicI64::new(orbital_core_base::time_util::now_millis()));
        let clock: Clock = { let now = now.clone(); Arc::new(move || now.load(Ordering::SeqCst)) };
        let verifier = FirebaseVerifier::new(f["projectId"].as_str().unwrap(), FIREBASE_CERTS_URL, feed(&documents, clock.clone()), clock);
        assert_eq!(verifier.verify(token, None).unwrap_err().reason, "unknown-key");
        let fetched = documents.count(FIREBASE_CERTS_URL);
        assert_eq!(fetched, 1, "a set fetched just now is not fetched again for an unknown kid");
        documents.by_url.lock().unwrap().insert(FIREBASE_CERTS_URL.to_string(), f["certs"].to_string());
        assert_eq!(verifier.verify(token, None).unwrap_err().reason, "unknown-key", "inside the cooldown the keys are not refetched");
        assert_eq!(documents.count(FIREBASE_CERTS_URL), fetched);
        now.fetch_add(REFETCH_COOLDOWN_MS + 1, Ordering::SeqCst);
        assert!(verifier.verify(token, None).is_ok(), "after the cooldown the rotated key is fetched");
        assert_eq!(documents.count(FIREBASE_CERTS_URL), fetched + 1);
    }

    #[test]
    fn control_an_unreachable_key_document_is_a_refusal_and_not_a_pass() {
        let fx = fixture("tokens.json");
        let documents = Documents::new(&[]);
        let unreachable = Feed::new(documents.answer(), Arc::new(Credentials::default()), real_clock(), vec!["https://www.googleapis.com".to_string()]);
        let verifier = FirebaseVerifier::new("test-project", FIREBASE_CERTS_URL, unreachable, real_clock());
        let error = verifier.verify(fx["firebase"]["cases"][0]["token"].as_str().unwrap(), None).unwrap_err();
        assert_eq!(error.reason, "keys-unavailable");
    }

    #[test]
    fn api_keys_over_the_shared_fixture() {
        let fx = fixture("tokens.json");
        let k = &fx["apiKeys"];
        let records: Vec<Json> = k["records"].as_array().unwrap().clone();
        let lookup: KeyLookup = Arc::new(move |hash| {
            Ok(records.iter().find(|r| r["keyHash"] == hash).map(|r| ApiKeyRecord {
                principal: r["principal"].as_str().unwrap().to_string(),
                revoked: r["revoked"].as_bool().unwrap(),
                expires_at: r["expiresAt"].as_i64(),
                claims: r["claims"].clone(),
            }))
        });
        let now = k["now"].as_i64().unwrap();
        let verifier = ApiKeyVerifier::new(k["salt"].as_str().unwrap(), lookup, Arc::new(move || now)).unwrap();
        for case in k["cases"].as_array().unwrap() {
            check(verifier.verify(case["token"].as_str().unwrap(), None), &case["expect"], case["name"].as_str().unwrap());
        }
        assert!(ApiKeyVerifier::new("not hex", Arc::new(|_| Ok(None)), Arc::new(|| 0)).is_err());
    }

    #[test]
    fn the_hash_is_the_one_node_scrypt_made_for_the_minted_keys() {
        assert_eq!(
            hash_api_key("sk_live_0e9477cf4605be3c22f072fbb33a598877b51ec0a097d964bf06694de0fc497e", "00112233445566778899").unwrap(),
            "1dac766300b5dba31ab53a18ef01ad07e57fe36303f03b3b31418e894f8d8a4a"
        );
    }

    #[test]
    fn a_bearer_is_routed_to_the_key_verifier_by_its_prefix_and_refusals_are_401() {
        let signed_in: Arc<dyn TokenVerifier> = Arc::new(Stub(Provider::Firebase, "id-token"));
        let keys: Arc<dyn TokenVerifier> = Arc::new(Stub(Provider::ApiKey, "sk_live_x"));
        let routed = WithApiKeys { sign_in: signed_in, keys };
        let ok = |header: &str, tenant: &Tenant| authenticate_bearer(&routed, Some(header), tenant);
        assert!(matches!(ok("Bearer id-token", &Tenant::None), AuthOutcome::Verified(u) if u.provider == Provider::Firebase));
        assert!(matches!(ok("Bearer sk_live_x", &Tenant::None), AuthOutcome::Verified(u) if u.provider == Provider::ApiKey));
        let refused = |outcome: AuthOutcome| match outcome {
            AuthOutcome::Refused { status, error } => (status, error),
            other => panic!("{other:?}"),
        };
        assert_eq!(refused(ok("Bearer wrong", &Tenant::None)), (401, "Unauthorized".to_string()));
        assert_eq!(refused(ok("Bearer id-token", &Tenant::NoSignIn)), (401, "This app has no sign-in tenant".to_string()));
        assert_eq!(refused(authenticate_bearer(&routed, None, &Tenant::None)), (401, "Authorization header missing or malformed".to_string()));
        assert_eq!(refused(ok("Basic abc", &Tenant::None)).1, "Authorization header missing or malformed");
    }

    struct Stub(Provider, &'static str);

    impl TokenVerifier for Stub {
        fn provider(&self) -> Provider {
            self.0
        }
        fn verify(&self, token: &str, _: Option<&str>) -> Result<VerifiedUser, AuthError> {
            if token == self.1 {
                Ok(VerifiedUser { uid: "u".to_string(), provider: self.0, email: None, name: None, tenant: None, claims: json!({}) })
            } else {
                Err(AuthError::new("signature", "no"))
            }
        }
    }

    #[test]
    fn env_problems_over_the_shared_cases() {
        let fx = fixture("env-problems.json");
        for case in fx["cases"].as_array().unwrap() {
            let env: AuthEnv = case["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
            let want: Vec<String> = case["expect"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            assert_eq!(auth_env_problems(&env), want, "{}", case["name"]);
        }
        let none: AuthEnv = HashMap::new();
        assert_eq!(auth_env_problems(&none), vec!["AUTH_PROVIDER=firebase needs FIREBASE_PROJECT_ID".to_string()]);
    }

    #[test]
    fn the_boot_refuses_a_provider_with_a_missing_setting() {
        let documents = Documents::new(&[]);
        let build = |pairs: &[(&str, &str)]| {
            let env: AuthEnv = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            verifier_from_env(&env, &|_: &[String]| documents.answer(), Arc::new(Credentials::default()), real_clock()).err()
        };
        assert!(build(&[("AUTH_PROVIDER", "oidc"), ("OIDC_AUDIENCE", "a")]).unwrap().contains("OIDC_ISSUER_URL"));
        assert!(build(&[]).unwrap().contains("FIREBASE_PROJECT_ID"));
        assert!(build(&[("AUTH_PROVIDER", "saml")]).is_some());
        assert!(build(&[("FIREBASE_PROJECT_ID", "p")]).is_none());
    }

    fn schema_of(entities: &Json) -> OrbitalSchema {
        let orbitals: Vec<Json> = entities
            .as_array()
            .unwrap()
            .iter()
            .map(|e| json!({ "name": format!("{}Orbital", e["name"].as_str().unwrap()), "entity": e, "traits": [], "pages": [] }))
            .collect();
        serde_json::from_value(json!({ "name": "app", "orbitals": orbitals })).unwrap()
    }

    #[test]
    fn identity_sources_over_the_shared_cases() {
        for case in fixture("viewer-cases.json")["sources"].as_array().unwrap() {
            let got = match identity_source_of(&schema_of(&case["entities"])) {
                IdentitySource::None => json!({ "kind": "none" }),
                IdentitySource::Claims(fields) => json!({ "kind": "claims", "fields": fields }),
                IdentitySource::Directory(entity) => json!({ "kind": "directory", "entity": entity }),
            };
            assert_eq!(got, case["expect"], "{}", case["name"]);
        }
    }

    #[test]
    fn viewers_over_the_shared_cases() {
        let fx = fixture("viewer-cases.json");
        let directory = fx["directory"].clone();
        let lookup = |entity: &str, id: &str| -> Result<Option<Json>, String> {
            Ok(directory[entity].as_array().and_then(|rows| rows.iter().find(|r| r["id"] == id)).cloned())
        };
        for case in fx["viewers"].as_array().unwrap() {
            let verified = match &case["verified"] {
                Json::Bool(false) => None,
                patch => {
                    let mut user = fx["verified"].clone();
                    for (key, value) in patch.as_object().into_iter().flatten() {
                        user[key] = value.clone();
                    }
                    Some(VerifiedUser {
                        uid: user["uid"].as_str().unwrap().to_string(),
                        provider: Provider::Firebase,
                        email: user["email"].as_str().map(str::to_string),
                        name: user["name"].as_str().map(str::to_string),
                        tenant: None,
                        claims: user["claims"].clone(),
                    })
                }
            };
            let source = match case["source"]["kind"].as_str().unwrap() {
                "none" => IdentitySource::None,
                "claims" => IdentitySource::Claims(case["source"]["fields"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect()),
                _ => IdentitySource::Directory(case["source"]["entity"].as_str().unwrap().to_string()),
            };
            let with_lookup: Option<DirectoryLookup<'_>> = if case["noLookup"] == true { None } else { Some(&lookup) };
            let result = resolve_viewer(verified.as_ref(), &source, with_lookup);
            if case["expect"]["error"].is_string() {
                assert!(result.unwrap_err().contains("no lookup"), "{}", case["name"]);
            } else {
                let got: Json = serde_json::to_value(result.unwrap()).unwrap();
                assert_eq!(got, case["expect"], "{}", case["name"]);
            }
        }
    }

    struct Fixed;

    impl TokenVerifier for Fixed {
        fn provider(&self) -> Provider {
            Provider::Oidc
        }

        fn verify(&self, token: &str, _: Option<&str>) -> Result<VerifiedUser, AuthError> {
            match token {
                "good" => Ok(VerifiedUser { uid: "u1".into(), provider: Provider::Oidc, email: None, name: None, tenant: None, claims: json!({ "sub": "u1" }) }),
                _ => Err(AuthError::new("signature", "bad signature")),
            }
        }
    }

    fn viewer(authorization: Option<&str>, anonymous_ok: bool) -> Result<Value, String> {
        let find = |_: &str, _: &str| Ok(None);
        viewer_of(&Fixed, &IdentitySource::None, &Tenant::None, authorization, anonymous_ok, &find)
    }

    #[test]
    fn a_missing_bearer_is_the_anonymous_viewer_where_that_is_allowed_and_refused_where_it_is_not() {
        assert_eq!(viewer(None, true), Ok(anonymous_user()));
        assert!(viewer(None, false).unwrap_err().contains("missing"));
    }

    #[test]
    fn a_verified_bearer_is_that_viewer_and_an_invalid_one_is_refused() {
        let user = viewer(Some("Bearer good"), true).unwrap();
        assert_eq!(user.get_path("id").and_then(|id| id.as_str().map(str::to_string)), Some("u1".to_string()));
        assert_eq!(viewer(Some("Bearer forged"), true), Err("Unauthorized".to_string()), "control: anonymous_ok does not excuse a bad token");
        assert!(viewer(Some("good"), true).is_err(), "control: a header that is not a Bearer is refused");
    }

    #[test]
    fn the_firebase_verifier_fetches_googles_certificates_with_no_program_egress_in_sight() {
        let fx = fixture("tokens.json");
        let documents = Documents::new(&[(FIREBASE_CERTS_URL, fx["firebase"]["certs"].to_string())]);
        let (verifier, allowed) = from_env(&documents, &[("FIREBASE_PROJECT_ID", "test-project")]);
        let verifier = verifier.unwrap();
        assert_eq!(*allowed.lock().unwrap(), vec![vec!["https://www.googleapis.com".to_string()]], "the origin is the provider's, nothing else");
        let case = fx["firebase"]["cases"].as_array().unwrap().iter().find(|c| c["name"] == "project token").unwrap();
        let user = verifier.verify(case["token"].as_str().unwrap(), None).unwrap();
        assert_eq!(user.uid, "uid-1");
        assert_eq!(documents.count(FIREBASE_CERTS_URL), 1);
    }

    #[test]
    fn a_discovery_document_cannot_send_the_verifier_to_another_origin() {
        let (fx, _) = oidc_world();
        let o = &fx["oidc"];
        let elsewhere = "https://keys.elsewhere.test/jwks";
        let discovery = json!({ "issuer": o["issuer"], "jwks_uri": elsewhere }).to_string();
        let documents = Documents::new(&[
            (&format!("{}/.well-known/openid-configuration", o["issuer"].as_str().unwrap()), discovery),
            (elsewhere, o["jwks"].to_string()),
            (o["jwksUri"].as_str().unwrap(), o["jwks"].to_string()),
        ]);
        let _ = fx;
        let base = [("AUTH_PROVIDER", "oidc"), ("OIDC_ISSUER_URL", o["issuer"].as_str().unwrap()), ("OIDC_AUDIENCE", o["audience"].as_str().unwrap())];
        let token = o["cases"][0]["token"].as_str().unwrap();
        let (verifier, allowed) = from_env(&documents, &base);
        let refused = verifier.unwrap().verify(token, None).unwrap_err();
        assert_eq!(refused.reason, "origin-not-allowed", "{refused}");
        assert_eq!(documents.count(elsewhere), 0, "the other origin was never asked");
        assert_eq!(*allowed.lock().unwrap(), vec![vec!["https://issuer.test".to_string()]]);

        let mut named = base.to_vec();
        named.push(("OIDC_JWKS_ORIGIN", "https://keys.elsewhere.test"));
        let (verifier, _) = from_env(&documents, &named);
        assert_eq!(verifier.unwrap().verify(token, None).unwrap().uid, "auth0|ada", "control: an origin the config names is allowed");
        assert_eq!(documents.count(elsewhere), 1);
    }

    #[test]
    fn control_a_discovery_document_that_names_the_issuers_own_origin_verifies() {
        let (fx, documents) = oidc_world();
        let o = &fx["oidc"];
        let env = [("AUTH_PROVIDER", "oidc"), ("OIDC_ISSUER_URL", o["issuer"].as_str().unwrap()), ("OIDC_AUDIENCE", o["audience"].as_str().unwrap())];
        let (verifier, _) = from_env(&documents, &env);
        assert_eq!(verifier.unwrap().verify(o["cases"][0]["token"].as_str().unwrap(), None).unwrap().uid, "auth0|ada");
    }

    #[test]
    fn origins_are_read_the_way_the_transport_reads_them() {
        assert_eq!(origin_of("https://WWW.Example.com/a?b#c").as_deref(), Some("https://www.example.com"));
        assert_eq!(origin_of("http://127.0.0.1:9000/certs").as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(origin_of("https://allowed.test@evil.test/x"), None, "credentials in the authority hide the real host");
        assert_eq!(origin_of("ftp://x/y"), None);
        assert_eq!(origin_of("not a url"), None);
    }

    struct Scripted(Json);

    impl TokenVerifier for Scripted {
        fn provider(&self) -> Provider {
            Provider::Oidc
        }
        fn verify(&self, token: &str, _: Option<&str>) -> Result<VerifiedUser, AuthError> {
            if token != "good" {
                return Err(AuthError::new("invalid", "bad signature"));
            }
            Ok(VerifiedUser {
                uid: "sa".into(),
                provider: Provider::Oidc,
                email: self.0["email"].as_str().map(str::to_string),
                name: None,
                tenant: None,
                claims: self.0.clone(),
            })
        }
    }

    fn invoker(claims: Json) -> TickInvoker {
        TickInvoker::new(Arc::new(Scripted(claims)), "app-sa@p.iam.gserviceaccount.com")
    }

    #[test]
    fn the_tick_invoker_admits_only_its_own_verified_email() {
        let own = serde_json::json!({ "email": "app-sa@p.iam.gserviceaccount.com", "email_verified": true });
        assert!(invoker(own.clone()).admits(Some("Bearer good")).is_ok());
        assert_eq!(invoker(own.clone()).admits(Some("Bearer bad")).unwrap_err().reason, "invalid", "control: a bad signature");
        assert_eq!(invoker(own).admits(None).unwrap_err().reason, "missing");
        let other = serde_json::json!({ "email": "someone@p.iam.gserviceaccount.com", "email_verified": true });
        assert_eq!(invoker(other).admits(Some("Bearer good")).unwrap_err().reason, "not-invoker");
        let unverified = serde_json::json!({ "email": "app-sa@p.iam.gserviceaccount.com", "email_verified": false });
        assert_eq!(invoker(unverified).admits(Some("Bearer good")).unwrap_err().reason, "not-invoker");
    }

    #[test]
    fn edge_an_invoker_without_an_audience_is_refused_and_none_named_is_none() {
        let host = |_: &[String]| -> HostAnswer { Arc::new(|_| IoAnswer::Random(Ok(Vec::new()))) };
        let cache: Arc<dyn SecretSource> = Arc::new(crate::credentials::Credentials::from_env(Default::default()));
        let clock: Clock = Arc::new(|| 0);
        let env = |pairs: &[(&str, &str)]| -> AuthEnv { pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
        assert!(TickInvoker::from_env(&env(&[]), &host, cache.clone(), clock.clone()).unwrap().is_none());
        assert!(TickInvoker::from_env(&env(&[("ORB_TICK_INVOKER", "a@b")]), &host, cache.clone(), clock.clone()).is_err());
        assert!(TickInvoker::from_env(&env(&[("ORB_TICK_INVOKER", "a@b"), ("ORB_TICK_AUDIENCE", "https://app.run.app")]), &host, cache, clock)
            .unwrap()
            .is_some());
    }
}
