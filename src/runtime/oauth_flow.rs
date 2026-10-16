//! The step machine of an `oauth-client` auth: one operation (discovery, code, connect, refresh, revoke,
//! userinfo) as a sequence of host asks (what to persist) and HTTP exchanges (what to send). The pure response
//! checks are `crate::oauth`; this module only orders them. Sans-I/O: the driver feeds it each answer.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::evaluator::{Value, ValueMap};
use crate::oauth::ask::{MockTokenState, OAuthAsk, OAuthConnectionRow, OAuthFailureReason, OAuthProvider, OAuthReply, OAuthTokenGrant, PendingTake};
use crate::oauth::client::{OAuthClientDecl, OAuthInputs, OAuthMode, OAuthOp, OAuthServerDecl};
use crate::oauth::{OidcError, OidcProcessor, Pending, Tokens};
use crate::runtime::http_request::{origin_allowed, HttpRefusal};
use crate::runtime::http_service::{HttpRequest, HttpResponse, TransportError};

/// Private values a flow holds between asks; never serialized, never printed.
#[derive(Clone, PartialEq, Default)]
pub struct OAuthHeld {
    pub pending: Option<Pending>,
    pub tokens: Option<Tokens>,
    pub response: Option<HttpResponse>,
    /// Mock mode: the subject minted for the token pair in flight.
    pub mock_subject: Option<String>,
    /// The credentials of the client a pending grant's provider selected (the `clients` form of `op: code`).
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

impl OAuthHeld {
    pub fn is_empty(&self) -> bool {
        self.pending.is_none()
            && self.tokens.is_none()
            && self.response.is_none()
            && self.mock_subject.is_none()
            && self.client_id.is_none()
            && self.client_secret.is_none()
    }
}

/// How long a mock access token lives, and the `expiresIn` it reports.
const MOCK_TOKEN_LIFETIME_S: f64 = 3600.0;
/// A mock authorization never expires (the mock backend keeps its states for the life of the process).
pub const MOCK_PENDING_TTL_MS: u64 = i64::MAX as u64;

/// What a flow needs to run: the call's declaration and the resolved client credentials.
pub struct OAuthEnv<'a> {
    pub op: OAuthOp,
    /// The client the call runs as; with `clients`, the one the pending grant's provider selected.
    pub client: &'a OAuthClientDecl,
    /// The declared clients of an `op: code` that routes by the grant's provider; empty for a single `client`.
    pub clients: &'a [OAuthClientDecl],
    pub inputs: &'a OAuthInputs,
    pub client_id: &'a str,
    pub client_secret: &'a str,
    pub egress: &'a [String],
    /// The literal URL a `discovery` op was authored against.
    pub authored_url: &'a str,
}

pub enum FlowAsk {
    OAuth(OAuthAsk),
    Http(HttpRequest),
    Random { bytes: u32 },
    Secret { name: String, scope: crate::runtime::http_request::SecretScope },
}

pub enum FlowAnswer {
    OAuth(Result<OAuthReply, String>),
    Http(Result<HttpResponse, TransportError>),
    Random(Result<Vec<u8>, String>),
    Secret(Result<Option<String>, String>),
}

pub enum FlowFailure {
    Reason(OAuthFailureReason),
    Refused(HttpRefusal),
    Transport(TransportError),
}

pub enum FlowStep {
    Ask(FlowAsk),
    Done(Result<Value, FlowFailure>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Stage {
    Start,
    Avail,
    Take,
    MetaGet,
    MetaHttp,
    MetaPut,
    Lookup,
    TokenHttp,
    TokenClock,
    RevokeHttp,
    InfoHttp,
    InfoClock,
    Remember,
    Save,
    Forget,
    MockAccess,
    MockRefreshToken,
    MockSub,
    MockClock,
    MockPut,
    MockNewAccess,
    MockRefreshClock,
    MockRefreshed,
    MockRevoked,
    MockChecked,
    MockLookup,
    SelectId,
    SelectSecret,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlow {
    stage: Stage,
    #[serde(default)]
    principal: Option<String>,
    #[serde(default)]
    metadata: Option<Map<String, Json>>,
    #[serde(default)]
    expected_subject: Option<String>,
    /// The `clients` entry the pending grant's provider selected.
    #[serde(default)]
    selected: Option<usize>,
    /// Mock mode: the provider that issued the token being checked.
    #[serde(default)]
    mock_provider: Option<OAuthProvider>,
}

fn reason(reason: OAuthFailureReason) -> FlowStep {
    FlowStep::Done(Err(FlowFailure::Reason(reason)))
}

fn refused(message: impl Into<String>) -> FlowStep {
    FlowStep::Done(Err(FlowFailure::Refused(HttpRefusal::Invalid(message.into()))))
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    Value::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<ValueMap>())
}

/// A connection row as the payload a program reads.
pub fn row_value(row: &OAuthConnectionRow) -> Value {
    object(vec![
        ("provider", text(row.provider.name())),
        ("connected", Value::Bool(row.connected)),
        ("account", text(&row.account)),
        ("avatarUrl", text(&row.avatar_url)),
        ("scope", Value::Array(row.scope.iter().map(|s| text(s)).collect())),
        ("connectedAt", text(&row.connected_at)),
    ])
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, text)
}

/// The profile the mock backend derives from a provider and a subject.
fn mock_profile(provider: OAuthProvider, sub: &str) -> (String, String, String) {
    let short: String = sub.chars().take(8).collect();
    let name = provider.name();
    (
        format!("user-{short}@{name}.example.com"),
        format!("Mock User ({name})"),
        format!("https://{name}.example.com/avatar/{short}.png"),
    )
}

fn mock_authorization_url(provider: OAuthProvider) -> &'static str {
    match provider {
        OAuthProvider::Google => "https://accounts.google.com/o/oauth2/v2/auth",
        OAuthProvider::Github => "https://github.com/login/oauth/authorize",
        OAuthProvider::Auth0 => "https://auth.example.com/authorize",
    }
}

/// The mock backend's authorization: a provider URL carrying the request, `state` as hex of the drawn bytes, and
/// the pending grant (scopes and subject; no PKCE). It never reads a credential.
pub fn build_mock_authorization(plan: &AuthorizePlan, state_bytes: &[u8]) -> (String, String, Pending) {
    let state = crate::codec::hex(state_bytes);
    let query = [
        ("response_type", "code".to_string()),
        ("scope", plan.options.scopes.join(" ")),
        ("redirect_uri", plan.options.redirect_uri.clone()),
        ("state", state.clone()),
        ("client_id", "mock-client-id".to_string()),
    ]
    .iter()
    .map(|(k, v)| format!("{}={}", crate::codec::form_urlencode_value(k), crate::codec::form_urlencode_value(v)))
    .collect::<Vec<_>>()
    .join("&");
    let pending = Pending {
        provider: plan.client.tag.name().to_string(),
        redirect_uri: plan.options.redirect_uri.clone(),
        verifier: String::new(),
        subject: plan.options.subject.clone(),
        scopes: plan.options.scopes.clone(),
    };
    (format!("{}?{query}", mock_authorization_url(plan.client.tag)), state, pending)
}

/// The static metadata of a client that names its endpoints.
fn static_metadata(server: &OAuthServerDecl) -> Option<Map<String, Json>> {
    let OAuthServerDecl::Endpoints { authorization, token, revocation, userinfo } = server else {
        return None;
    };
    let mut meta = Map::new();
    let issuer = url::Url::parse(authorization)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default();
    meta.insert("issuer".into(), Json::String(issuer));
    meta.insert("authorization_endpoint".into(), Json::String(authorization.clone()));
    meta.insert("token_endpoint".into(), Json::String(token.clone()));
    if let Some(url) = revocation {
        meta.insert("revocation_endpoint".into(), Json::String(url.clone()));
    }
    if let Some(url) = userinfo {
        meta.insert("userinfo_endpoint".into(), Json::String(url.clone()));
    }
    Some(meta)
}

/// The server metadata an `oauth-authorize` can use without a network leg: the declared endpoints, or the
/// metadata a `discovery` op cached through the host.
pub fn declared_metadata(server: &OAuthServerDecl) -> Option<Map<String, Json>> {
    static_metadata(server)
}

pub fn processor_for(client: &OAuthClientDecl, client_id: &str, client_secret: &str) -> OidcProcessor {
    OidcProcessor::new(client.tag.name(), client_id, client_secret).with_client_auth(client.client_auth)
}

/// The options of an `oauth-authorize`, parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeOptions {
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub subject: Option<String>,
    pub params: Vec<(String, String)>,
}

/// What an `oauth-authorize` needs before it asks the host anything: the client and the options, parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizePlan {
    pub client: OAuthClientDecl,
    pub options: AuthorizeOptions,
}

const AUTHORIZE_KEYS: [&str; 4] = ["redirectUri", "scope", "subject", "params"];

fn authorize_fault(message: impl std::fmt::Display) -> String {
    format!("oauth-authorize: {message}")
}

/// Parse the client object and options of an `oauth-authorize`; the message is the effect's failure.
pub fn plan_authorize(client: &Value, options: &Value) -> Result<AuthorizePlan, String> {
    use crate::runtime::http_request::parse_oauth_client;
    let client = parse_oauth_client(client).map_err(|e| authorize_fault(e.message()))?;
    let Value::Object(options) = options else {
        return Err(authorize_fault("the options must be an object"));
    };
    if let Some(key) = options.keys().find(|key| !AUTHORIZE_KEYS.contains(&key.as_str())) {
        return Err(authorize_fault(format!("unknown option `{key}`")));
    }
    let Some(Value::String(redirect_uri)) = options.get("redirectUri") else {
        return Err(authorize_fault("redirectUri must be a string"));
    };
    let bad_scope = || authorize_fault("scope must be a string or a list of strings");
    let scopes = match options.get("scope") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(scope)) => scope.split_whitespace().map(str::to_string).collect(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|item| match item {
                Value::String(scope) => Ok(scope.clone()),
                _ => Err(bad_scope()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(bad_scope()),
    };
    let subject = match options.get("subject") {
        None | Some(Value::Null) => None,
        Some(Value::String(subject)) => Some(subject.clone()),
        Some(_) => return Err(authorize_fault("subject must be a string")),
    };
    let params = match options.get("params") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Object(params)) => params
            .iter()
            .map(|(name, value)| match value {
                Value::String(text) => Ok((name.clone(), text.clone())),
                Value::Number(n) => Ok((name.clone(), crate::str_utils::js_number_string(*n))),
                Value::Bool(b) => Ok((name.clone(), b.to_string())),
                _ => Err(authorize_fault("params must be strings, numbers or booleans")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(authorize_fault("params must be an object")),
    };
    Ok(AuthorizePlan {
        client,
        options: AuthorizeOptions { redirect_uri: redirect_uri.clone(), scopes, subject, params },
    })
}

/// The authorization URL, the state and the pending grant of an `oauth-authorize`, from the metadata and the two
/// journaled random draws. A failure is the flow's closed reason, or a refusal message for a bad caller input.
pub fn build_authorization(
    plan: &AuthorizePlan,
    client_id: &str,
    meta: &Map<String, Json>,
    state_bytes: Vec<u8>,
    verifier_bytes: Vec<u8>,
) -> Result<(String, String, Pending), FlowFailure> {
    let draws = std::cell::RefCell::new(vec![verifier_bytes, state_bytes]);
    let processor = processor_for(&plan.client, client_id, "");
    let extra: Vec<(&str, &str)> = plan.options.params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    processor
        .authorize(
            meta,
            &plan.options.redirect_uri,
            &plan.options.scopes,
            plan.options.subject.as_deref(),
            &extra,
            |_| draws.borrow_mut().pop().unwrap_or_default(),
        )
        .map_err(|error| match error.kind.reason() {
            Some(reason) => FlowFailure::Reason(reason),
            None => FlowFailure::Refused(HttpRefusal::Invalid(authorize_fault(error.message))),
        })
}

impl OAuthFlow {
    pub fn new(principal: Option<String>) -> Self {
        Self { stage: Stage::Start, principal, metadata: None, expected_subject: None, selected: None, mock_provider: None }
    }

    /// The `clients` entry selected so far; the driver builds the env from it.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The processor of the client the call runs as; credentials a taken grant just selected are read from `held`.
    fn processor(&self, env: &OAuthEnv<'_>, held: &OAuthHeld) -> OidcProcessor {
        processor_for(
            env.client,
            held.client_id.as_deref().unwrap_or(env.client_id),
            held.client_secret.as_deref().unwrap_or(env.client_secret),
        )
    }

    fn ask(&mut self, stage: Stage, ask: OAuthAsk) -> FlowStep {
        self.stage = stage;
        FlowStep::Ask(FlowAsk::OAuth(ask))
    }

    fn send(&mut self, env: &OAuthEnv<'_>, stage: Stage, request: HttpRequest) -> FlowStep {
        if !origin_allowed(&request.url, env.egress) {
            self.stage = Stage::Finished;
            return FlowStep::Done(Err(FlowFailure::Refused(HttpRefusal::EgressDenied(request.url))));
        }
        self.stage = stage;
        FlowStep::Ask(FlowAsk::Http(request))
    }

    fn finish(&mut self, step: FlowStep) -> FlowStep {
        if matches!(step, FlowStep::Done(_)) {
            self.stage = Stage::Finished;
        }
        step
    }

    fn check_error(&mut self, error: OidcError) -> FlowStep {
        let step = match error.kind.reason() {
            Some(found) => reason(found),
            None => refused(error.message),
        };
        self.finish(step)
    }

    pub fn start(&mut self, env: &OAuthEnv<'_>, held: &mut OAuthHeld) -> FlowStep {
        let mock = env.client.mode == OAuthMode::Mock;
        let step = match env.op {
            OAuthOp::Discovery if mock => match &env.client.server {
                OAuthServerDecl::Issuer(issuer) => FlowStep::Done(Ok(object(vec![("issuer", text(issuer))]))),
                OAuthServerDecl::Endpoints { .. } => refused("oauth-client op discovery needs an issuer, not endpoints"),
            },
            OAuthOp::Refresh if mock => {
                self.stage = Stage::MockNewAccess;
                FlowStep::Ask(FlowAsk::Random { bytes: 32 })
            }
            OAuthOp::Revoke if mock => match &env.inputs.token {
                Some(token) => self.ask(Stage::MockRevoked, OAuthAsk::MockTokenRevoke { token: token.clone() }),
                None => refused("oauth-client op revoke needs a token"),
            },
            OAuthOp::Userinfo if mock => match &env.inputs.access_token {
                Some(token) => self.ask(Stage::MockChecked, OAuthAsk::MockTokenCheck { access_token: token.clone() }),
                None => refused("oauth-client op userinfo needs an accessToken"),
            },
            OAuthOp::Code => match &env.inputs.state {
                Some(state) => self.ask(Stage::Take, OAuthAsk::PendingTake { state: state.clone() }),
                None => refused("oauth-client op code needs a state"),
            },
            OAuthOp::Connect => {
                if self.principal.as_deref().is_none_or(str::is_empty) {
                    reason(OAuthFailureReason::NotSignedIn)
                } else {
                    self.ask(Stage::Avail, OAuthAsk::ConnectionAvailable)
                }
            }
            _ => self.begin_meta(env, held),
        };
        self.finish(step)
    }

    fn begin_meta(&mut self, env: &OAuthEnv<'_>, held: &mut OAuthHeld) -> FlowStep {
        match &env.client.server {
            OAuthServerDecl::Endpoints { .. } => {
                if env.op == OAuthOp::Discovery {
                    return refused("oauth-client op discovery needs an issuer, not endpoints");
                }
                self.metadata = static_metadata(&env.client.server);
                self.after_meta(env, held)
            }
            OAuthServerDecl::Issuer(issuer) => {
                self.ask(Stage::MetaGet, OAuthAsk::MetadataGet { issuer: issuer.clone() })
            }
        }
    }

    fn after_meta(&mut self, env: &OAuthEnv<'_>, held: &mut OAuthHeld) -> FlowStep {
        let Some(meta) = self.metadata.clone() else {
            return reason(OAuthFailureReason::MetadataInvalid);
        };
        let processor = self.processor(env, held);
        let input = |value: &Option<String>, what: &str| value.clone().ok_or_else(|| refused(format!("oauth-client op {} needs {what}", env.op.name())));
        match env.op {
            OAuthOp::Discovery => {
                let OAuthServerDecl::Issuer(issuer) = &env.client.server else {
                    return refused("oauth-client op discovery needs an issuer");
                };
                FlowStep::Done(Ok(object(vec![("issuer", text(issuer))])))
            }
            OAuthOp::Code | OAuthOp::Connect => {
                let (Some(pending), Some(code), Some(state)) =
                    (held.pending.clone(), env.inputs.code.clone(), env.inputs.state.clone())
                else {
                    return refused("oauth-client op needs a code and a state");
                };
                if let Err(error) = processor.check_grant_redirect(&pending) {
                    return self.check_error(error);
                }
                match processor.exchange_request(&meta, &pending, &code, &state) {
                    Ok(request) => self.send(env, Stage::TokenHttp, request),
                    Err(error) => self.check_error(error),
                }
            }
            OAuthOp::Refresh => {
                let token = match input(&env.inputs.refresh_token, "a refreshToken") {
                    Ok(token) => token,
                    Err(step) => return step,
                };
                match processor.refresh_request(&meta, &token) {
                    Ok(request) => self.send(env, Stage::TokenHttp, request),
                    Err(error) => self.check_error(error),
                }
            }
            OAuthOp::Revoke => {
                let token = match input(&env.inputs.token, "a token") {
                    Ok(token) => token,
                    Err(step) => return step,
                };
                match processor.revoke_request(&meta, &token) {
                    Ok(request) => self.send(env, Stage::RevokeHttp, request),
                    Err(error) => self.check_error(error),
                }
            }
            OAuthOp::Userinfo => {
                let token = match input(&env.inputs.access_token, "an accessToken") {
                    Ok(token) => token,
                    Err(step) => return step,
                };
                self.ask(Stage::Lookup, OAuthAsk::SubjectLookup { access_token: token })
            }
        }
    }

    pub fn resume(&mut self, env: &OAuthEnv<'_>, held: &mut OAuthHeld, answer: FlowAnswer) -> FlowStep {
        let step = self.step(env, held, answer);
        self.finish(step)
    }

    fn step(&mut self, env: &OAuthEnv<'_>, held: &mut OAuthHeld, answer: FlowAnswer) -> FlowStep {
        let processor = self.processor(env, held);
        match (self.stage, answer) {
            (Stage::Finished | Stage::Start, _) => refused("unexpected answer for the oauth call"),
            (_, FlowAnswer::OAuth(Err(_)) | FlowAnswer::Random(Err(_)) | FlowAnswer::Secret(Err(_))) => reason(OAuthFailureReason::StoreUnavailable),
            (Stage::MockAccess, FlowAnswer::Random(Ok(bytes))) => {
                held.tokens = Some(Tokens {
                    access_token: crate::codec::hex(&bytes),
                    refresh_token: None,
                    expires_in: Some(MOCK_TOKEN_LIFETIME_S),
                    scope: held.pending.as_ref().map(|p| p.scopes.clone()).unwrap_or_default(),
                    id_token_subject: None,
                });
                self.stage = Stage::MockRefreshToken;
                FlowStep::Ask(FlowAsk::Random { bytes: 32 })
            }
            (Stage::MockRefreshToken, FlowAnswer::Random(Ok(bytes))) => {
                if let Some(tokens) = held.tokens.as_mut() {
                    tokens.refresh_token = Some(crate::codec::hex(&bytes));
                }
                self.stage = Stage::MockSub;
                FlowStep::Ask(FlowAsk::Random { bytes: 8 })
            }
            (Stage::MockSub, FlowAnswer::Random(Ok(bytes))) => {
                held.mock_subject = Some(crate::codec::hex(&bytes));
                self.ask(Stage::MockClock, OAuthAsk::Clock)
            }
            (Stage::MockClock, FlowAnswer::OAuth(Ok(OAuthReply::Millis { value: now }))) => {
                let (Some(tokens), Some(sub)) = (held.tokens.as_mut(), held.mock_subject.clone()) else {
                    return refused("unexpected answer for the oauth call");
                };
                tokens.id_token_subject = Some(sub);
                let (Some(refresh), access) = (tokens.refresh_token.clone(), tokens.access_token.clone()) else {
                    return refused("unexpected answer for the oauth call");
                };
                let Some(provider) = held.pending.as_ref().and_then(|p| OAuthProvider::parse(&p.provider)) else {
                    return reason(OAuthFailureReason::ProviderMismatch);
                };
                let put = OAuthAsk::MockTokenPut {
                    access_token: access,
                    refresh_token: refresh,
                    expires_at_ms: now.saturating_add((MOCK_TOKEN_LIFETIME_S as i64) * 1000),
                    provider,
                };
                self.ask(Stage::MockPut, put)
            }
            (Stage::MockPut, FlowAnswer::OAuth(Ok(OAuthReply::Done))) => {
                let (Some(tokens), Some(sub)) = (held.tokens.as_ref(), held.mock_subject.take()) else {
                    return refused("unexpected answer for the oauth call");
                };
                let remember = OAuthAsk::SubjectRemember { access_token: tokens.access_token.clone(), subject: sub };
                self.ask(Stage::Remember, remember)
            }
            (Stage::MockNewAccess, FlowAnswer::Random(Ok(bytes))) => {
                held.tokens = Some(Tokens {
                    access_token: crate::codec::hex(&bytes),
                    refresh_token: None,
                    expires_in: Some(MOCK_TOKEN_LIFETIME_S),
                    scope: Vec::new(),
                    id_token_subject: None,
                });
                self.ask(Stage::MockRefreshClock, OAuthAsk::Clock)
            }
            (Stage::MockRefreshClock, FlowAnswer::OAuth(Ok(OAuthReply::Millis { value: now }))) => {
                let (Some(tokens), Some(refresh)) = (held.tokens.as_ref(), env.inputs.refresh_token.clone()) else {
                    return refused("unexpected answer for the oauth call");
                };
                let ask = OAuthAsk::MockTokenRefresh {
                    refresh_token: refresh,
                    new_access_token: tokens.access_token.clone(),
                    expires_at_ms: now.saturating_add((MOCK_TOKEN_LIFETIME_S as i64) * 1000),
                };
                self.ask(Stage::MockRefreshed, ask)
            }
            (Stage::MockRefreshed, FlowAnswer::OAuth(Ok(OAuthReply::Flag { value }))) => {
                if value {
                    self.after_tokens(env, held)
                } else {
                    held.tokens = None;
                    reason(OAuthFailureReason::TokenRejected)
                }
            }
            (Stage::MockRevoked, FlowAnswer::OAuth(Ok(OAuthReply::Flag { value }))) => {
                FlowStep::Done(Ok(object(vec![("revoked", Value::Bool(value))])))
            }
            (Stage::MockChecked, FlowAnswer::OAuth(Ok(OAuthReply::MockToken { state, provider }))) => match (state, &env.inputs.access_token) {
                (MockTokenState::Valid, Some(token)) => {
                    self.mock_provider = provider;
                    self.ask(Stage::MockLookup, OAuthAsk::SubjectLookup { access_token: token.clone() })
                }
                (MockTokenState::Valid, None) => refused("oauth-client op userinfo needs an accessToken"),
                (MockTokenState::Missing | MockTokenState::Expired, _) => reason(OAuthFailureReason::TokenRejected),
            },
            (Stage::MockLookup, FlowAnswer::OAuth(Ok(OAuthReply::Subject { subject }))) => match subject {
                Some(sub) => {
                    let Some(provider) = self.mock_provider else {
                        return reason(OAuthFailureReason::UserinfoMismatch);
                    };
                    let (email, name, picture) = mock_profile(provider, &sub);
                    FlowStep::Done(Ok(object(vec![("sub", text(&sub)), ("email", text(&email)), ("name", text(&name)), ("picture", text(&picture))])))
                }
                None => reason(OAuthFailureReason::UserinfoMismatch),
            },
            (Stage::Avail, FlowAnswer::OAuth(Ok(OAuthReply::Flag { value: true }))) => {
                match &env.inputs.state {
                    Some(state) => self.ask(Stage::Take, OAuthAsk::PendingTake { state: state.clone() }),
                    None => refused("oauth-client op connect needs a state"),
                }
            }
            (Stage::Avail, FlowAnswer::OAuth(Ok(OAuthReply::Flag { value: false }))) => {
                reason(OAuthFailureReason::StoreUnavailable)
            }
            (Stage::Take, FlowAnswer::OAuth(Ok(OAuthReply::Taken { taken }))) => match taken {
                PendingTake::Missing => reason(OAuthFailureReason::StateUnknown),
                PendingTake::Expired => reason(OAuthFailureReason::StateExpired),
                PendingTake::Grant { grant } => {
                    let expected = if env.op == OAuthOp::Connect { self.principal.as_deref() } else { env.inputs.subject.as_deref() };
                    if grant.subject.as_deref() != expected {
                        return reason(OAuthFailureReason::SubjectMismatch);
                    }
                    if env.op == OAuthOp::Code && !env.clients.is_empty() {
                        let Some(index) = env.clients.iter().position(|c| c.tag.name() == grant.provider) else {
                            return reason(OAuthFailureReason::ProviderMismatch);
                        };
                        self.selected = Some(index);
                        let chosen = &env.clients[index];
                        held.pending = Some(grant);
                        if chosen.mode == OAuthMode::Mock {
                            self.stage = Stage::MockAccess;
                            return FlowStep::Ask(FlowAsk::Random { bytes: 32 });
                        }
                        self.stage = Stage::SelectId;
                        return FlowStep::Ask(FlowAsk::Secret { name: chosen.client_id.name.clone(), scope: chosen.client_id.scope });
                    }
                    let wanted = if env.op == OAuthOp::Connect { env.inputs.provider } else { Some(env.client.tag) };
                    if wanted.map(|p| p.name()) != Some(grant.provider.as_str()) {
                        return reason(OAuthFailureReason::ProviderMismatch);
                    }
                    held.pending = Some(grant);
                    if env.client.mode == OAuthMode::Mock {
                        self.stage = Stage::MockAccess;
                        return FlowStep::Ask(FlowAsk::Random { bytes: 32 });
                    }
                    self.begin_meta(env, held)
                }
            },
            (Stage::SelectId, FlowAnswer::Secret(Ok(Some(id)))) => {
                held.client_id = Some(id);
                match env.client.client_secret.as_ref() {
                    Some(slot) => {
                        self.stage = Stage::SelectSecret;
                        FlowStep::Ask(FlowAsk::Secret { name: slot.name.clone(), scope: slot.scope })
                    }
                    None => self.begin_meta(env, held),
                }
            }
            (Stage::SelectSecret, FlowAnswer::Secret(Ok(Some(secret)))) => {
                held.client_secret = Some(secret);
                self.begin_meta(env, held)
            }
            (Stage::SelectId | Stage::SelectSecret, FlowAnswer::Secret(Ok(None))) => reason(OAuthFailureReason::UnconfiguredClient),
            (Stage::MetaGet, FlowAnswer::OAuth(Ok(OAuthReply::Metadata { metadata }))) => {
                let OAuthServerDecl::Issuer(issuer) = &env.client.server else {
                    return reason(OAuthFailureReason::MetadataInvalid);
                };
                match metadata {
                    Some(found) => {
                        self.metadata = Some(found);
                        self.after_meta(env, held)
                    }
                    None => match processor.discovery_request(issuer) {
                        Ok(request) => {
                            if env.op == OAuthOp::Discovery && request.url != env.authored_url {
                                return refused("a discovery call is sent to the issuer's /.well-known/openid-configuration");
                            }
                            self.send(env, Stage::MetaHttp, request)
                        }
                        Err(error) => self.check_error(error),
                    },
                }
            }
            (Stage::MetaHttp, FlowAnswer::Http(Err(error))) => FlowStep::Done(Err(FlowFailure::Transport(error))),
            (Stage::MetaHttp, FlowAnswer::Http(Ok(response))) => {
                let OAuthServerDecl::Issuer(issuer) = &env.client.server else {
                    return reason(OAuthFailureReason::MetadataInvalid);
                };
                match processor.process_discovery(issuer, &response) {
                    Ok(meta) => {
                        self.metadata = Some(meta.clone());
                        self.ask(Stage::MetaPut, OAuthAsk::MetadataPut { issuer: issuer.clone(), metadata: meta })
                    }
                    Err(error) => self.check_error(error),
                }
            }
            (Stage::MetaPut, FlowAnswer::OAuth(Ok(OAuthReply::Done))) => self.after_meta(env, held),
            (Stage::Lookup, FlowAnswer::OAuth(Ok(OAuthReply::Subject { subject }))) => {
                self.expected_subject = subject;
                let (Some(meta), Some(token)) = (self.metadata.clone(), env.inputs.access_token.clone()) else {
                    return reason(OAuthFailureReason::MetadataInvalid);
                };
                match processor.userinfo_request(&meta, &token) {
                    Ok(request) => self.send(env, Stage::InfoHttp, request),
                    Err(error) => self.check_error(error),
                }
            }
            (Stage::TokenHttp | Stage::RevokeHttp | Stage::InfoHttp, FlowAnswer::Http(Err(error))) => {
                FlowStep::Done(Err(FlowFailure::Transport(error)))
            }
            (Stage::TokenHttp, FlowAnswer::Http(Ok(response))) => {
                held.response = Some(response);
                self.ask(Stage::TokenClock, OAuthAsk::Clock)
            }
            (Stage::InfoHttp, FlowAnswer::Http(Ok(response))) => {
                held.response = Some(response);
                self.ask(Stage::InfoClock, OAuthAsk::Clock)
            }
            (Stage::RevokeHttp, FlowAnswer::Http(Ok(response))) => match processor.process_revoke(&response) {
                Ok(()) => match &env.inputs.token {
                    Some(token) => self.ask(Stage::Forget, OAuthAsk::SubjectForget { access_token: token.clone() }),
                    None => refused("oauth-client op revoke needs a token"),
                },
                Err(error) => self.check_error(error),
            },
            (Stage::TokenClock, FlowAnswer::OAuth(Ok(OAuthReply::Millis { value: now }))) => {
                let (Some(meta), Some(response)) = (self.metadata.clone(), held.response.take()) else {
                    return refused("unexpected answer for the oauth call");
                };
                let code_grant = matches!(env.op, OAuthOp::Code | OAuthOp::Connect);
                match processor.process_tokens(&meta, &response, now, code_grant) {
                    Ok(tokens) => {
                        let subject = tokens.id_token_subject.clone();
                        let access = tokens.access_token.clone();
                        held.tokens = Some(tokens);
                        match subject {
                            Some(sub) => self.ask(Stage::Remember, OAuthAsk::SubjectRemember { access_token: access, subject: sub }),
                            None => self.after_tokens(env, held),
                        }
                    }
                    Err(error) => self.check_error(error),
                }
            }
            (Stage::Remember, FlowAnswer::OAuth(Ok(OAuthReply::Done))) => self.after_tokens(env, held),
            (Stage::Save, FlowAnswer::OAuth(Ok(OAuthReply::Row { row }))) => FlowStep::Done(Ok(row_value(&row))),
            (Stage::Forget, FlowAnswer::OAuth(Ok(OAuthReply::Done))) => {
                FlowStep::Done(Ok(object(vec![("revoked", Value::Bool(true))])))
            }
            (Stage::InfoClock, FlowAnswer::OAuth(Ok(OAuthReply::Millis { value: now }))) => {
                let (Some(meta), Some(response)) = (self.metadata.clone(), held.response.take()) else {
                    return refused("unexpected answer for the oauth call");
                };
                match processor.process_userinfo(&meta, &response, self.expected_subject.as_deref(), now) {
                    Ok(info) => {
                        let field = |name: &str| text(info.get(name).and_then(Json::as_str).unwrap_or_default());
                        FlowStep::Done(Ok(object(vec![
                            ("sub", field("sub")),
                            ("email", field("email")),
                            ("name", field("name")),
                            ("picture", field("picture")),
                        ])))
                    }
                    Err(error) => self.check_error(error),
                }
            }
            _ => refused("unexpected answer for the oauth call"),
        }
    }

    fn after_tokens(&mut self, env: &OAuthEnv<'_>, held: &mut OAuthHeld) -> FlowStep {
        let (Some(tokens), pending) = (held.tokens.take(), held.pending.take()) else {
            return refused("unexpected answer for the oauth call");
        };
        let expires_in = tokens.expires_in.unwrap_or(3600.0);
        let subject = pending.as_ref().and_then(|p| p.subject.clone());
        match env.op {
            OAuthOp::Refresh => FlowStep::Done(Ok(object(vec![
                ("accessToken", text(&tokens.access_token)),
                ("expiresIn", Value::Number(expires_in)),
            ]))),
            OAuthOp::Code => FlowStep::Done(Ok(object(vec![
                ("accessToken", text(&tokens.access_token)),
                ("refreshToken", opt_text(tokens.refresh_token.as_deref())),
                ("expiresIn", Value::Number(expires_in)),
                ("tokenType", text("bearer")),
                ("scope", Value::Array(tokens.scope.iter().map(|s| text(s)).collect())),
                ("subject", opt_text(subject.as_deref())),
            ]))),
            OAuthOp::Connect => {
                let Some(provider) = pending.as_ref().and_then(|p| crate::oauth::ask::OAuthProvider::parse(&p.provider)) else {
                    return reason(OAuthFailureReason::ProviderMismatch);
                };
                let grant = OAuthTokenGrant {
                    access_token: tokens.access_token,
                    refresh_token: tokens.refresh_token,
                    expires_in,
                    token_type: "bearer".to_string(),
                    scope: Some(tokens.scope),
                    subject,
                };
                self.ask(Stage::Save, OAuthAsk::ConnectionSave { provider, grant })
            }
            _ => refused("unexpected answer for the oauth call"),
        }
    }
}

impl std::fmt::Debug for OAuthHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthHeld")
            .field("pending", &self.pending.is_some())
            .field("tokens", &self.tokens.is_some())
            .field("response", &self.response.is_some())
            .finish()
    }
}
