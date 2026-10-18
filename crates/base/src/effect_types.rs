//! Effect Types
//!
//! Core types for effect execution.

use crate::value::Value;
use crate::value::ValueMap;
use crate::error::EvalError;
use crate::http_wire::HttpMethod;
use crate::os::{accepts_arity, split_envelope, OsOp};
use crate::schema::sexpr::SExpression;
use serde::{Deserialize, Serialize};

pub fn call_key_text(value: &Value) -> Result<&str, EvalError> {
    match value {
        Value::String(text) if !text.is_empty() => Ok(text),
        _ => Err(EvalError::InvalidArgument(format!(
            "call key must evaluate to a non-empty string, got {}",
            value.to_string_value()
        ))),
    }
}

/// Runtime environment for effect execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum RuntimeEnvironment {
    /// Server-side environment (Node.js, Rust server)
    #[default]
    Server,
    /// Client-side environment (Browser, Bevy)
    Client,
}

impl RuntimeEnvironment {
    /// Check if this is the server environment
    pub fn is_server(&self) -> bool {
        matches!(self, RuntimeEnvironment::Server)
    }

    /// Check if this is the client environment
    pub fn is_client(&self) -> bool {
        matches!(self, RuntimeEnvironment::Client)
    }
}

/// Completion events declared on a `fetch` (or `persist`) effect's options
/// object (`["fetch", X, { "emit": { "success": "...", "failure": "..." } }]`).
///
/// After the fetch resolves, the runtime fires `success` (carrying the fetched rows
/// as `{ "data": [...] }`, so a `@payload.data`-bound table renders) or `failure` on
/// error. This is how a data-loading trait advances from its `loading` state to its
/// `loaded` state — without it, the table never renders (RS-G16). `persist` reuses
/// the same shape for its own `emit.failure` (denial / required-column-missing).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FetchEmit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// A keyed `call-service` aborted by `cancel-call`: fires with `{ key }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<String>,
    /// A running `call-service`'s live messages; delivered by the host, never in-band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_message: Option<String>,
}

impl FetchEmit {
    /// True when no completion event is declared (nothing to emit when the effect finishes).
    pub fn is_empty(&self) -> bool {
        self.success.is_none() && self.failure.is_none() && self.cancelled.is_none()
    }
}

/// What a `fetch` hands its success event: the rows, or a [`RowSetRef`](crate::runtime::row_query::RowSetRef)
/// to them (`deliver: ids`, Almadar_Embedded_Kernel_Speed_Design.md section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchDeliver {
    #[default]
    Rows,
    Ids,
}

impl FetchDeliver {
    fn is_rows(&self) -> bool {
        *self == FetchDeliver::Rows
    }
}

/// Typed effect variants
///
/// Effects are the side-effects produced by state machine transitions.
/// The executor determines how to handle each effect based on the runtime environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Effect {
    /// Emit an event to the event bus
    Emit { event: String, payload: Value },

    /// Set a value in the binding context
    Set { path: String, value: Value },

    /// Persist data to storage
    Persist {
        operation: PersistOperation,
        entity_type: String,
        data: Value,
        id: Option<String>,
        /// Declared `emit.success`/`emit.failure` — only `failure` is routed
        /// today (denial / required-column-missing); see `kernel.rs`'s
        /// `persist_failure_emit`.
        emit: Option<FetchEmit>,
    },

    /// `["persist", "batch", <ops>, { emit }?]` — many rows in one effect. `ops`
    /// is evaluated at execution time to an array of `["create"|"update"|"delete", …]`.
    PersistBatch { ops: Value, emit: Option<FetchEmit> },

    /// Call an external service
    CallService {
        service: String,
        method: String,
        args: Value,
        /// The `key:` expression, resolved to a string before dispatch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        /// Completion events: `success` carries the service's result, `failure` `{ error }`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// One HTTP request. `request` is resolved except `paginate.next`/`collect`, which stay
    /// S-expressions the kernel evaluates per page against `@response`.
    Http {
        method: HttpMethod,
        origin: Value,
        path: Value,
        request: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// One redis command. `command` is the list of text items; `options` holds `auth`, `user` and `db`, checked when
    /// the call runs.
    Redis {
        origin: Value,
        command: Value,
        options: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// One read-only SQL statement on Postgres. `params` is the list of bind values; `options` holds `auth`, `user`,
    /// `database`, `tls` and `timeoutMs`, checked when the call runs.
    Postgres {
        origin: Value,
        sql: Value,
        params: Value,
        options: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Abort the in-flight keyed call-service or http started with `key`: one key, or a list of keys
    /// cancelled in turn (a binding or operator call until the kernel resolves it).
    CancelCall { key: Value },

    /// `n` bytes of host CSPRNG output, journaled; success carries `{ value: base64url, no padding }`.
    RandomBytes {
        bytes: Value,
        /// The `key:` expression, resolved to a non-empty string; echoed as `key` in the success and failure payloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// One `os/<op>` operation (`docs/Almadar_OS_Operators.md`): the host answers it, keyed and routed
    /// like `http`; a streaming op delivers its events to `emit.on_message`.
    Os {
        op: OsOp,
        args: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Whether the host holds the credential named by `credential` (for the dispatch's `@user`
    /// when the knob is user-scoped); success carries `{ connected: boolean }`.
    CredentialStatus {
        credential: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Drop the credential named by `credential` the host holds.
    CredentialForget {
        credential: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// One credential administration effect (`crate::credential_admin`): the kernel decides, the host answers only
    /// the raw store and mode-flag asks. Server-only.
    CredentialAdmin {
        op: crate::credential_admin::CredentialAdminOp,
        request: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Start an OAuth authorization: success carries `{ authUrl, state }`.
    OAuthAuthorize {
        client: Value,
        options: Value,
        /// Names the call: evaluated to a non-empty string and echoed in the success and failure payloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// The signed-in user's connection to `provider`; success carries the connection row.
    OAuthConnectionStatus {
        provider: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Drop the signed-in user's connection to `provider`.
    OAuthConnectionForget {
        provider: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Render UI (client-only)
    RenderUi { slot: String, content: Value },

    /// Navigate to a route (client-only)
    Navigate {
        path: String,
        params: Option<ValueMap>,
        /// `{ crumb?: string }` — the optional third `navigate` argument
        /// (`(navigate path params { crumb: ... })`). Mirrors the wire's
        /// `NavigateOptions` (`super::client_effect`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<crate::client_effect::NavigateOptions>,
    },

    /// Pop the navigation stack and return to the previous entry (client-only)
    NavigateBack,

    /// Spawn a game entity (client/game only)
    Spawn { entity_type: String, props: Value },

    /// Despawn a game entity (client/game only)
    Despawn { entity_id: String },

    /// Log a message
    Log {
        level: LogLevel,
        message: String,
        data: Option<Value>,
    },

    /// Fetch entities from persistence (server-only)
    ///
    /// This effect queries the persistence layer and returns entity data.
    /// Used to load data for UI rendering.
    ///
    /// Examples from Orbital_Pattern.orb:
    /// - `["fetch", "User"]` - fetch all users
    /// - `["fetch", "User", {"id": "@payload.row.id"}]` - fetch single user by ID
    /// - `["fetch", "User", {"filter": ["=", "@entity.status", "active"]}]` - fetch with filter
    /// - `["fetch", "Inspection", {"include": ["company", "inspector"]}]` - fetch with related entities
    Fetch {
        /// The entity type to fetch (e.g., "User", "Connection")
        entity_type: String,
        /// Optional entity ID to fetch a single entity (an expression, evaluated at execution)
        id: Option<Value>,
        /// Optional filter expression (S-expression format)
        filter: Option<Value>,
        /// Optional limit for pagination (an expression, evaluated at execution)
        limit: Option<Value>,
        /// Optional offset for pagination (an expression, evaluated at execution)
        offset: Option<Value>,
        /// `"field"` or `"field:asc|desc"` (an expression, evaluated at execution), applied
        /// after the filter and before offset/limit; missing values sort last.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order_by: Option<Value>,
        /// Optional list of relation fields to include (e.g., ["company", "inspector"])
        /// These must reference relation fields on the entity
        include: Option<Vec<String>>,
        /// `rows` (absent) delivers the rows in `data`; `ids` delivers a row-set reference.
        #[serde(default, skip_serializing_if = "FetchDeliver::is_rows")]
        deliver: FetchDeliver,
        /// `none` (ruled Q12): the store skips counting every match; the success payload carries no `totalCount`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<crate::row_set::RowTotalMode>,
        /// `{ field, vector, k }` (an object of expressions, evaluated at execution): the nearest `k` rows by cosine
        /// similarity among those `filter` admits (EXEC-FETCH-2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nearest: Option<Value>,
        /// Completion events fired after the fetch resolves (`success` carries the rows
        /// as `{ data: [...] }`; `failure` fires on error). Drives `loading` → `loaded`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Read the requested page facts through the host (client-site). `fields` is the list of bare words
    /// (`hostname`, `referrer`, `screenWidth`), checked when the effect runs.
    BrowserPageFacts {
        fields: Value,
        /// The `key:` expression, resolved to a non-empty string; echoed as `key` in the success and failure payloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// POST `request.text` to the absolute `url` through the host's beacon port (client-site). Built and
    /// egress-checked by the one http driver.
    BrowserBeacon {
        url: Value,
        request: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Ask the host's on-device model for a reply to `text` that satisfies the JSON Schema text `schema`
    /// (client-site, keyed and cancellable like `call-service`).
    BrowserPrompt {
        text: Value,
        schema: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Ask the host's on-device embedder for one vector per text (client-site, keyed and cancellable).
    BrowserEmbed {
        texts: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// One `browser/tab-<op>` operation on the browser's tabs (client-site, keyed and cancellable). `request` is the
    /// op's request object, checked when the effect runs; the kernel shapes every result and the host performs only
    /// the raw tab and page asks.
    BrowserTab {
        op: crate::tab::TabOp,
        request: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Generic/unknown effect (for extensibility). A substrate op's trailing `{ emit }`
    /// object is split into `emit`: `success` carries `{ result }`, `failure` `{ error }`.
    Custom {
        name: String,
        args: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },
}

/// All client-targeted effect wire kinds, in `kebab-case`. Every client shell MUST handle
/// each; the shell effect-coverage gates iterate this. Kept in sync with [`Effect::client_kind`]
/// by `effects::tests::client_kind_matches_kinds`.
pub const CLIENT_EFFECT_KINDS: &[&str] = &["render-ui", "navigate", "navigate-back", "emit"];

impl Effect {
    /// What an `http` effect reaches, `"<METHOD> <origin><path>"`: the one spelling every verifier and host reads.
    pub fn http_target(&self) -> Option<String> {
        match self {
            Effect::Http { method, origin, path, .. } => {
                Some(format!("{} {}{}", method.as_str(), origin.to_string_value(), path.to_string_value()))
            }
            _ => None,
        }
    }

    /// Registry name of the operator this effect came from.
    pub fn operator(&self) -> &str {
        match self {
            Effect::Emit { .. } => "emit",
            Effect::Set { .. } => "set",
            Effect::Persist { .. } | Effect::PersistBatch { .. } => "persist",
            Effect::CallService { .. } => "call-service",
            Effect::Http { .. } => "http",
            Effect::Redis { .. } => "redis",
            Effect::Postgres { .. } => "postgres",
            Effect::CancelCall { .. } => "cancel-call",
            Effect::RandomBytes { .. } => "random-bytes",
            Effect::Os { op, .. } => op.name(),
            Effect::CredentialStatus { .. } => "credential-status",
            Effect::CredentialForget { .. } => "credential-forget",
            Effect::CredentialAdmin { op, .. } => op.name(),
            Effect::OAuthAuthorize { .. } => "oauth-authorize",
            Effect::OAuthConnectionStatus { .. } => "oauth-connection-status",
            Effect::OAuthConnectionForget { .. } => "oauth-connection-forget",
            Effect::RenderUi { .. } => "render-ui",
            Effect::Navigate { .. } => "navigate",
            Effect::NavigateBack => "navigate-back",
            Effect::Spawn { .. } => "spawn",
            Effect::Despawn { .. } => "despawn",
            Effect::Log { .. } => "log",
            Effect::Fetch { .. } => "fetch",
            Effect::BrowserPageFacts { .. } => "browser/page-facts",
            Effect::BrowserBeacon { .. } => "browser/beacon",
            Effect::BrowserPrompt { .. } => "browser/prompt",
            Effect::BrowserEmbed { .. } => "browser/embed",
            Effect::BrowserTab { op, .. } => op.name(),
            Effect::Custom { name, .. } => name,
        }
    }

    /// The `kebab-case` wire kind iff this effect is client-targeted, else `None`. THE one
    /// place that defines the client/server split — `EventResponse::from_process_result`
    /// selects client effects with this, the server route encodes the wire tag with it, and
    /// the shells are gated against [`CLIENT_EFFECT_KINDS`].
    pub fn client_kind(&self) -> Option<&'static str> {
        match self {
            Effect::RenderUi { .. } => Some("render-ui"),
            Effect::Navigate { .. } => Some("navigate"),
            Effect::NavigateBack => Some("navigate-back"),
            Effect::Emit { .. } => Some("emit"),
            _ => None,
        }
    }
}

/// Persistence operation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersistOperation {
    Create,
    Upsert,
    Read,
    Update,
    Delete,
}

impl PersistOperation {
    /// Parse from string
    #[allow(clippy::should_implement_trait)] // returns Option, not the Result FromStr requires
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "create" => Some(PersistOperation::Create),
            "upsert" => Some(PersistOperation::Upsert),
            "read" => Some(PersistOperation::Read),
            "update" => Some(PersistOperation::Update),
            "delete" => Some(PersistOperation::Delete),
            _ => None,
        }
    }
}

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Result of executing an effect
#[derive(Debug, Clone)]
pub struct EffectResult {
    /// The effect that was processed
    pub effect: Effect,

    /// How the effect was handled
    pub outcome: EffectOutcome,

    /// Return value (if any)
    pub return_value: Option<Value>,

    /// Error message (if failed)
    pub error: Option<String>,

    /// Entity type (for fetch effects, used to track which entity type was fetched)
    ///
    /// This is populated when executing Fetch effects, allowing the caller
    /// to easily identify which entity type the returned data belongs to.
    pub entity_type: Option<String>,

    /// A fetch's row count before offset/limit (the success payload's `totalCount`).
    pub total_count: Option<usize>,

    /// This `emit` is a `persist` effect's `emit.success` envelope: the one emit a live-push host also
    /// fans out to every other connected client.
    pub persist_success: bool,

    /// This `emit` is the reply or failure an effect's declared `emit` route fires on completion (`http`, `os`,
    /// `persist`, `fetch`, `random-bytes`, `credential-*`, `oauth-*`, a keyed `call-service`, ...): it carries that
    /// completion's identity, so the dispatch loop never collapses it into an earlier identical delivery
    /// (EXEC-QUEUE-4).
    pub completion: bool,

    /// Why a failed persist resolved no target row (`ServerEffectResult.code`).
    pub code: Option<crate::bus_event::ServerEffectCode>,

    /// For an `emit`: its cross-unit deliveries that were refused (EXEC-QUEUE-9); each one is a `DELIVERY_REJECTED`.
    pub refused: Vec<crate::envelope::DeliveryRefused>,

    /// For a failed `persist batch`: the operation at fault and how many ran before it.
    pub batch_failure: Option<BatchFailure>,
}

/// Where a `persist batch` stopped: the position of the failing operation in the batch and the number of
/// operations that completed before it (a store that runs the batch atomically has rolled those back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchFailure {
    pub index: usize,
    pub completed_count: usize,
}

impl EffectResult {
    /// Create a successful execution result
    pub fn executed(effect: Effect) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: None,
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a successful execution with return value
    pub fn executed_with_value(effect: Effect, value: Value) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: Some(value),
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a successful fetch execution with entity type tracking
    ///
    /// This is used for Fetch effects to track which entity type was fetched.
    pub fn executed_fetch(
        effect: Effect,
        entities: Value,
        entity_type: &str,
        total: Option<usize>,
    ) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: Some(entities),
            error: None,
            entity_type: Some(entity_type.to_string()),
            total_count: total,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// A fetch delivered by reference (`deliver: ids`): the success payload's `data` is the reference's
    /// render-prop leaf, `totalCount` its count.
    pub fn executed_row_set(
        effect: Effect,
        reference: &crate::row_set::RowSetRef,
        total: Option<usize>,
    ) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: Some(crate::row_set::row_set_marker(reference)),
            error: None,
            entity_type: Some(reference.entity_type.clone()),
            total_count: total,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a no-op result (effect skipped for this environment)
    pub fn noop(effect: Effect, reason: &str) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::NoOp(reason.to_string()),
            return_value: None,
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Create a delegated result (effect sent to another environment)
    pub fn delegated(effect: Effect, target: RuntimeEnvironment) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Delegated(target),
            return_value: None,
            error: None,
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// This result, marked as a `persist` effect's success envelope.
    /// Mark this `emit` as an effect's completion delivery.
    pub fn as_completion(mut self) -> Self {
        self.completion = true;
        self
    }

    pub fn as_persist_success(mut self) -> Self {
        self.persist_success = true;
        self
    }

    /// The declared `emit.failure` event an `http` effect's failure was routed to; `None` for a success, an
    /// effect that is not `http`, or a call that declares none.
    pub fn routed_failure(&self) -> Option<String> {
        match (&self.effect, &self.outcome) {
            (
                Effect::Http { emit: Some(emit), .. }
                | Effect::Redis { emit: Some(emit), .. }
                | Effect::Postgres { emit: Some(emit), .. },
                EffectOutcome::Failed,
            ) => {
                emit.failure.clone()
            }
            _ => None,
        }
    }

    /// This failed `persist batch`, with the operation that failed.
    pub fn with_batch_failure(mut self, index: usize, completed_count: usize) -> Self {
        self.batch_failure = Some(BatchFailure { index, completed_count });
        self
    }

    /// This failed result, with the coded reason its persist resolved no target row.
    pub fn with_code(mut self, code: crate::bus_event::ServerEffectCode) -> Self {
        self.code = Some(code);
        self
    }

    /// Create a failed result
    pub fn failed(effect: Effect, error: &str) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Failed,
            return_value: None,
            error: Some(error.to_string()),
            entity_type: None,
            total_count: None,
            persist_success: false,
            completion: false,
            code: None,
            refused: Vec::new(),
            batch_failure: None,
        }
    }

    /// Check if the effect was executed
    pub fn is_executed(&self) -> bool {
        matches!(self.outcome, EffectOutcome::Executed)
    }

    /// Check if the effect was a no-op
    pub fn is_noop(&self) -> bool {
        matches!(self.outcome, EffectOutcome::NoOp(_))
    }

    /// Check if the effect was delegated
    pub fn is_delegated(&self) -> bool {
        matches!(self.outcome, EffectOutcome::Delegated(_))
    }

    /// Check if the effect failed
    pub fn is_failed(&self) -> bool {
        matches!(self.outcome, EffectOutcome::Failed)
    }
}

/// How an effect was handled
#[derive(Debug, Clone)]
pub enum EffectOutcome {
    /// Effect was executed in this environment
    Executed,

    /// Effect was skipped (not applicable to this environment)
    NoOp(String),

    /// Effect was delegated to another environment
    Delegated(RuntimeEnvironment),

    /// Effect execution failed
    Failed,
}

/// Parse an effect from an S-expression
///
/// Supports formats like:
/// - `["emit", "EVENT_NAME", { payload }]`
/// - `["persist", "create", "Entity", data]`
/// - `["render-ui", "slot", { content }]`
/// - `["navigate", "/path"]`
/// - `["set", "@path", value]`
pub fn parse_effect(expr: &SExpression) -> Option<Effect> {
    let arr = expr.as_array()?;
    if arr.is_empty() {
        return None;
    }

    let op = arr[0].as_str()?;
    let args = &arr[1..];

    if let Some(os_op) = OsOp::from_name(op) {
        return parse_os(os_op, args);
    }

    if let Some(tab_op) = crate::tab::TabOp::from_name(op) {
        return parse_tab(tab_op, args);
    }

    match op {
        "emit" => parse_emit(args),
        "set" => parse_set(args),
        "persist" => parse_persist(args),
        "fetch" => parse_fetch(args),
        "call-service" => parse_call_service(args),
        "http" => parse_http(args),
        "redis" => parse_redis(args),
        "postgres" => parse_postgres(args),
        "cancel-call" => parse_cancel_call(args),
        "random-bytes" => parse_random_bytes(args),
        "credential-status" => parse_credential(args, true),
        "credential-forget" => parse_credential(args, false),
        admin if crate::credential_admin::CredentialAdminOp::from_name(admin).is_some() => {
            parse_credential_admin(admin, args)
        }
        "oauth-authorize" => parse_oauth_authorize(args),
        "oauth-connection-status" => parse_oauth_connection(args, true),
        "oauth-connection-forget" => parse_oauth_connection(args, false),
        "browser/page-facts" => parse_page_facts(args),
        "browser/beacon" => parse_beacon(args),
        "browser/prompt" => parse_prompt(args),
        "browser/embed" => parse_embed(args),
        "render-ui" => parse_render_ui(args),
        "navigate" => parse_navigate(args),
        "navigate-back" => Some(Effect::NavigateBack),
        "spawn" => parse_spawn(args),
        "despawn" => parse_despawn(args),
        "log" => parse_log(args),
        // `notify` is `.lolo`-only sugar for `render-ui toast` (owner ruling
        // 2026-09-11), desugared away at lowering; the `.orb` IR never
        // carries this kind, so a stale tuple falls through to `Custom`
        // below rather than a dedicated variant — never crashes the parser.
        _ => {
            let mut values: Vec<Value> = args.iter().map(|e| Value::from(e.clone())).collect();
            let emit = values.last().and_then(parse_emit_config);
            if emit.is_some() {
                values.pop();
            }
            Some(Effect::Custom {
                name: op.to_string(),
                args: values,
                emit,
            })
        }
    }
}

/// Whether a string is recognized as an effect operator name.
///
/// Used by callers (notably `orbital_compiler::phases::resolve`) that need
/// to disambiguate a Lisp-style call form from a string-array literal in
/// JSON `.orb` payloads — the only signal at the JSON level is the head
/// string. Recognises:
/// - The explicit effect names matched in `parse_effect` above (emit, set,
///   persist, fetch, call-service, render-ui, navigate, spawn, despawn,
///   log, notify).
/// - Namespaced effect ops parsed via `parse_effect`'s `_` arm into
///   `Effect::Custom`: `agent/*`, `os/*`, and the substrate operator
///   families (`llm/*`, `memory/*`, `trace/*`, `session/*`,
///   `workspace/*`) and the named reflection effects
///   (`behavior/catalog|describe|source`, `program/read|print|eval`).
///   These don't have explicit arms above but ARE legitimate effect calls;
///   treat them as operators. `browser/*` device ops are also recognized
///   here (they lower to dedicated `OirEffect` variants in the compiler).
///
/// Keep in sync with `parse_effect`'s explicit arms above and with
/// `resolve_sexpr_call`'s substrate routing in orbital-compiler.
pub fn is_known_effect_name(op: &str) -> bool {
    if op.starts_with("agent/")
        || op.starts_with("os/")
        || op.starts_with("browser/")
        || op.starts_with("llm/")
        || op.starts_with("memory/")
        || op.starts_with("trace/")
        || op.starts_with("session/")
        || op.starts_with("workspace/")
        || crate::ir::effects::is_substrate_operator(op)
        || crate::credential_admin::CredentialAdminOp::from_name(op).is_some()
    {
        return true;
    }
    matches!(
        op,
        "emit"
            | "set"
            | "persist"
            | "fetch"
            | "call-service"
            | "http"
            | "redis"
            | "postgres"
            | "cancel-call"
            | "random-bytes"
            | "credential-status"
            | "credential-forget"
            | "oauth-authorize"
            | "oauth-connection-status"
            | "oauth-connection-forget"
            | "render-ui"
            | "navigate"
            | "navigate-back"
            | "spawn"
            | "despawn"
            | "log"
            | "notify"
    )
}

fn parse_emit(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let event = args[0].as_str()?.to_string();
    let payload = args
        .get(1)
        .map(|e| Value::from(e.clone()))
        .unwrap_or(Value::Null);
    Some(Effect::Emit { event, payload })
}

fn parse_set(args: &[SExpression]) -> Option<Effect> {
    if args.len() < 2 {
        return None;
    }
    let path = args[0].as_str()?.to_string();
    let value = Value::from(args[1].clone());
    Some(Effect::Set { path, value })
}

/// A trailing `{ emit: {...} }` config object is options, never positional
/// id/data — mirrors the compiler validator's own `extract_emit_object_ref`
/// check (`orbital-compiler/src/phases/validation/effect/emit.rs`), which
/// treats a trailing object with an `emit` key as options-only for exactly
/// this reason. `parse_persist` must agree, or `(persist update Entity ?data
/// {emit:{...}})` — the dominant corpus idiom for "update with no separate
/// id, emit routes elsewhere" — misreads the emit config as `data` and the
/// literal data token as `id` (C-PERSIST-UPDATE-TRAILING-EMIT-MISPARSED).
pub fn strip_trailing_emit_options(args: &[SExpression]) -> &[SExpression] {
    match args.last().and_then(|a| a.as_object()) {
        Some(obj) if obj.contains_key("emit") => &args[..args.len() - 1],
        _ => args,
    }
}

/// Parse a `{ "emit": { "success": "...", "failure": "..." } }` options
/// object into a `FetchEmit`. Shared by `parse_fetch` and `parse_persist` —
/// one shape, one reader. `None` when `obj` carries no `emit` key or neither
/// event is declared.
fn parse_emit_config(obj: &Value) -> Option<FetchEmit> {
    obj.as_object()?
        .get("emit")
        .and_then(|v| v.as_object())
        .and_then(|e| {
            let success = e
                .get("success")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let failure = e
                .get("failure")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let route = |snake: &str, camel: &str| {
                e.get(snake)
                    .or_else(|| e.get(camel))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            };
            let fe = FetchEmit {
                success,
                failure,
                cancelled: route("cancelled", "cancelled"),
                on_message: route("on_message", "onMessage"),
            };
            (!fe.is_empty() || fe.on_message.is_some()).then_some(fe)
        })
}

fn parse_persist(args: &[SExpression]) -> Option<Effect> {
    if args.first().and_then(|a| a.as_str()) == Some("batch") {
        let ops = Value::from(args.get(1)?.clone());
        let emit = args
            .get(2)
            .and_then(|a| parse_emit_config(&Value::from(a.clone())));
        return Some(Effect::PersistBatch { ops, emit });
    }
    if args.len() < 3 {
        return None;
    }
    let op_str = args[0].as_str()?;
    let operation = PersistOperation::from_str(op_str)?;
    let entity_type = args[1].as_str()?.to_string();

    let emit = args
        .last()
        .and_then(|a| parse_emit_config(&Value::from(a.clone())));

    // `delete` carries only an id (`[delete, Type, id]`); `update` with 4
    // (non-options) args is id + data, with 3 the data carries its own id;
    // `create` takes data. A trailing options object never counts toward
    // this arity — see `strip_trailing_emit_options`.
    let positional = strip_trailing_emit_options(args);
    let (id, data) = match operation {
        PersistOperation::Delete => (
            positional
                .get(2)
                .and_then(|a| a.as_str())
                .map(|s| s.to_string()),
            Value::Null,
        ),
        PersistOperation::Update if positional.len() >= 4 => (
            positional[2].as_str().map(|s| s.to_string()),
            Value::from(positional[3].clone()),
        ),
        _ => (None, Value::from(positional.get(2)?.clone())),
    };

    Some(Effect::Persist {
        operation,
        entity_type,
        data,
        id,
        emit,
    })
}

/// Parse fetch effect from S-expression
///
/// Supports formats from Orbital_Pattern.orb:
/// - `["fetch", "User"]` - fetch all entities
/// - `["fetch", "User", {"id": "@payload.row.id"}]` - fetch by ID
/// - `["fetch", "User", {"filter": [...], "limit": 10}]` - fetch with options
/// - `["fetch", "Inspection", {"include": ["company"]}]` - fetch with related entities
fn parse_fetch(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }

    let entity_type = args[0].as_str()?.to_string();

    // Parse optional config object
    let (id, filter, limit, offset, order_by, include, deliver, total, nearest, emit) = if let Some(config) = args.get(1) {
        let config_value = Value::from(config.clone());
        if let Some(obj) = config_value.as_object() {
            let option = |key: &str| obj.get(key).filter(|v| !v.is_null()).cloned();
            let id = option("id");
            let filter = obj.get("filter").cloned();
            let limit = option("limit");
            let offset = option("offset");
            let order_by = option("orderBy");
            let include = obj.get("include").and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                        .collect()
                })
            });
            let deliver = match obj.get("deliver").and_then(Value::as_str) {
                Some("ids") => FetchDeliver::Ids,
                _ => FetchDeliver::Rows,
            };
            let total = match obj.get("total").and_then(Value::as_str) {
                Some("none") => Some(crate::row_set::RowTotalMode::None),
                _ => None,
            };
            let nearest = option("nearest");
            let emit = parse_emit_config(&config_value);
            (id, filter, limit, offset, order_by, include, deliver, total, nearest, emit)
        } else {
            (None, None, None, None, None, None, FetchDeliver::Rows, None, None, None)
        }
    } else {
        (None, None, None, None, None, None, FetchDeliver::Rows, None, None, None)
    };

    Some(Effect::Fetch {
        entity_type,
        id,
        filter,
        limit,
        offset,
        order_by,
        include,
        deliver,
        total,
        nearest,
        emit,
    })
}

fn parse_call_service(args: &[SExpression]) -> Option<Effect> {
    if args.len() < 2 {
        return None;
    }
    let service = args[0].as_str()?.to_string();
    let method = args[1].as_str()?.to_string();
    let call_args = args
        .get(2)
        .map(|e| Value::from(e.clone()))
        .unwrap_or(Value::Null);
    let key = match args.get(3) {
        Some(SExpression::Object(options)) => options
            .get("key")
            .map(|key| Value::from(key.clone())),
        _ => None,
    };
    let emit = args
        .get(3)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    Some(Effect::CallService {
        service,
        method,
        args: call_args,
        key,
        emit,
    })
}

/// `["http", METHOD, origin, path, request?, { key?, emit? }?]`.
fn parse_http(args: &[SExpression]) -> Option<Effect> {
    if !(3..=5).contains(&args.len()) {
        return None;
    }
    let method: HttpMethod =
        serde_json::from_value(serde_json::Value::String(args[0].as_str()?.to_string())).ok()?;
    let origin = Value::from(args[1].clone());
    let path = Value::from(args[2].clone());
    let request = args
        .get(3)
        .map(|e| Value::from(e.clone()))
        .unwrap_or_else(|| Value::Object(ValueMap::new()));
    let options = args.get(4).map(|e| Value::from(e.clone()));
    let key = options
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = options.as_ref().and_then(parse_emit_config);
    Some(Effect::Http {
        method,
        origin,
        path,
        request,
        key,
        emit,
    })
}

/// `["redis", origin, command, { auth?, user?, db?, key?, emit? }?, { key?, emit? }?]`: the trailing objects
/// share one namespace, `key` and `emit` going to the call and every other key to the connection `options`.
fn parse_redis(args: &[SExpression]) -> Option<Effect> {
    if !(2..=4).contains(&args.len()) {
        return None;
    }
    let mut options = ValueMap::new();
    let mut key = None;
    let mut routes = ValueMap::new();
    for trailing in &args[2..] {
        let SExpression::Object(fields) = trailing else {
            return None;
        };
        for (name, value) in fields {
            let value = Value::from(value.clone());
            match name.as_str() {
                "key" => key = Some(value),
                "emit" => {
                    routes.insert("emit".to_string(), value);
                }
                _ => {
                    options.insert(name.clone(), value);
                }
            }
        }
    }
    Some(Effect::Redis {
        origin: Value::from(args[0].clone()),
        command: Value::from(args[1].clone()),
        options: Value::Object(options),
        key,
        emit: parse_emit_config(&Value::Object(routes)),
    })
}

/// `["postgres", origin, sql, params, { auth?, user?, database?, tls?, timeoutMs?, key?, emit? }?, { key?, emit? }?]`:
/// the trailing objects share one namespace, `key` and `emit` going to the call and every other key to the connection
/// `options`.
fn parse_postgres(args: &[SExpression]) -> Option<Effect> {
    if !(3..=5).contains(&args.len()) {
        return None;
    }
    let mut options = ValueMap::new();
    let mut key = None;
    let mut routes = ValueMap::new();
    for trailing in &args[3..] {
        let SExpression::Object(fields) = trailing else {
            return None;
        };
        for (name, value) in fields {
            let value = Value::from(value.clone());
            match name.as_str() {
                "key" => key = Some(value),
                "emit" => {
                    routes.insert("emit".to_string(), value);
                }
                _ => {
                    options.insert(name.clone(), value);
                }
            }
        }
    }
    Some(Effect::Postgres {
        origin: Value::from(args[0].clone()),
        sql: Value::from(args[1].clone()),
        params: Value::from(args[2].clone()),
        options: Value::Object(options),
        key,
        emit: parse_emit_config(&Value::Object(routes)),
    })
}

/// `["browser/page-facts", [field, …], { emit? }?]`.
fn parse_page_facts(args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let options = args.get(1).map(|e| Value::from(e.clone()));
    let key = options
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = options.as_ref().and_then(parse_emit_config);
    Some(Effect::BrowserPageFacts {
        fields: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// The `key` and `emit` of a trailing options object.
fn keyed_options(options: Option<&SExpression>) -> (Option<Value>, Option<FetchEmit>) {
    let options = options.map(|e| Value::from(e.clone()));
    let key = options
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = options.as_ref().and_then(parse_emit_config);
    (key, emit)
}

/// `["browser/prompt", text, schemaText, { key?, emit? }?]`.
fn parse_prompt(args: &[SExpression]) -> Option<Effect> {
    if !(2..=3).contains(&args.len()) {
        return None;
    }
    let (key, emit) = keyed_options(args.get(2));
    Some(Effect::BrowserPrompt {
        text: Value::from(args[0].clone()),
        schema: Value::from(args[1].clone()),
        key,
        emit,
    })
}

/// `["browser/embed", [text, …], { key?, emit? }?]`.
fn parse_embed(args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let (key, emit) = keyed_options(args.get(1));
    Some(Effect::BrowserEmbed {
        texts: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// `["browser/beacon", url, { text, key? }, { key?, emit? }?]`; `key` may ride either object.
fn parse_beacon(args: &[SExpression]) -> Option<Effect> {
    if !(2..=3).contains(&args.len()) {
        return None;
    }
    let mut request = Value::from(args[1].clone());
    let mut key = None;
    if let Value::Object(map) = &mut request {
        key = map.shift_remove("key");
    }
    let options = args.get(2).map(|e| Value::from(e.clone()));
    if let Some(k) = options.as_ref().and_then(|o| o.as_object()).and_then(|o| o.get("key")) {
        key = Some(k.clone());
    }
    let emit = options.as_ref().and_then(parse_emit_config);
    Some(Effect::BrowserBeacon {
        url: Value::from(args[0].clone()),
        request,
        key,
        emit,
    })
}

/// `["browser/tab-<op>", request, { key?, emit? }?]`.
fn parse_tab(op: crate::tab::TabOp, args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let (key, emit) = keyed_options(args.get(1));
    Some(Effect::BrowserTab {
        op,
        request: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// `["os/<op>", args…, { key?, emit? }?]`; the envelope is told from the op's own options object by
/// [`split_envelope`], and the positional count must be one the op takes.
fn parse_os(op: OsOp, args: &[SExpression]) -> Option<Effect> {
    let (positional, envelope) = split_envelope(op, args);
    if !accepts_arity(op, positional.len()) {
        return None;
    }
    let options = envelope.map(|fields| Value::from(SExpression::Object(fields.clone())));
    let key = options
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = options.as_ref().and_then(parse_emit_config);
    Some(Effect::Os {
        op,
        args: positional.iter().map(|e| Value::from(e.clone())).collect(),
        key,
        emit,
    })
}

/// `["random-bytes", n, { key?, emit? }?]`.
fn parse_random_bytes(args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let trailing = args.get(1).map(|options| Value::from(options.clone()));
    let key = trailing.as_ref().and_then(|o| o.as_object()).and_then(|o| o.get("key")).cloned();
    let emit = trailing.as_ref().and_then(parse_emit_config);
    Some(Effect::RandomBytes {
        bytes: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// `["credential-status" | "credential-forget", credential, { emit? }?]`.
fn parse_credential(args: &[SExpression], status: bool) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let credential = Value::from(args[0].clone());
    let emit = args
        .get(1)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    Some(if status {
        Effect::CredentialStatus { credential, emit }
    } else {
        Effect::CredentialForget { credential, emit }
    })
}

/// `["credential-<verb>", request, { emit? }?]`.
fn parse_credential_admin(head: &str, args: &[SExpression]) -> Option<Effect> {
    let op = crate::credential_admin::CredentialAdminOp::from_name(head)?;
    let first = usize::from(op.takes_request());
    if !(first..=first + 1).contains(&args.len()) {
        return None;
    }
    let emit = args
        .get(first)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    let request = if op.takes_request() { Value::from(args[0].clone()) } else { Value::Null };
    Some(Effect::CredentialAdmin { op, request, emit })
}

/// `["oauth-authorize", client, options, { key?, emit? }?]`.
fn parse_oauth_authorize(args: &[SExpression]) -> Option<Effect> {
    if !(2..=3).contains(&args.len()) {
        return None;
    }
    let trailing = args.get(2).map(|options| Value::from(options.clone()));
    let key = trailing.as_ref().and_then(|o| o.as_object()).and_then(|o| o.get("key")).cloned();
    let emit = trailing.as_ref().and_then(parse_emit_config);
    Some(Effect::OAuthAuthorize {
        client: Value::from(args[0].clone()),
        options: Value::from(args[1].clone()),
        key,
        emit,
    })
}

/// `["oauth-connection-status" | "oauth-connection-forget", provider, { emit? }?]`.
fn parse_oauth_connection(args: &[SExpression], status: bool) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let provider = Value::from(args[0].clone());
    let emit = args
        .get(1)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    Some(if status {
        Effect::OAuthConnectionStatus { provider, emit }
    } else {
        Effect::OAuthConnectionForget { provider, emit }
    })
}

fn parse_cancel_call(args: &[SExpression]) -> Option<Effect> {
    let key = Value::from(args.first()?.clone());
    matches!(key, Value::String(_) | Value::Array(_)).then_some(Effect::CancelCall { key })
}

fn parse_render_ui(args: &[SExpression]) -> Option<Effect> {
    if args.len() < 2 {
        return None;
    }
    let slot = args[0].as_str()?.to_string();
    let content = Value::from(args[1].clone());
    Some(Effect::RenderUi { slot, content })
}

fn parse_navigate(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let path = args[0].as_str()?.to_string();
    let params = args.get(1).and_then(|e| {
        if let Value::Object(obj) = Value::from(e.clone()) {
            Some(obj)
        } else {
            None
        }
    });
    let options = args.get(2).and_then(|e| {
        if let Value::Object(obj) = Value::from(e.clone()) {
            let crumb = obj
                .get("crumb")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(crate::client_effect::NavigateOptions { crumb })
        } else {
            None
        }
    });
    Some(Effect::Navigate {
        path,
        params,
        options,
    })
}

fn parse_spawn(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let entity_type = args[0].as_str()?.to_string();
    let props = args
        .get(1)
        .map(|e| Value::from(e.clone()))
        .unwrap_or(Value::Null);
    Some(Effect::Spawn { entity_type, props })
}

fn parse_despawn(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let entity_id = args[0].as_str()?.to_string();
    Some(Effect::Despawn { entity_id })
}

fn parse_log(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }

    // Check if first arg is a level
    let (level, message, data) = if let Some(level_str) = args[0].as_str() {
        match level_str {
            "debug" | "info" | "warn" | "error" => {
                let level = match level_str {
                    "debug" => LogLevel::Debug,
                    "info" => LogLevel::Info,
                    "warn" => LogLevel::Warn,
                    "error" => LogLevel::Error,
                    _ => LogLevel::Info,
                };
                let message = args
                    .get(1)
                    .and_then(|e| e.as_str())
                    .unwrap_or("")
                    .to_string();
                let data = args.get(2).map(|e| Value::from(e.clone()));
                (level, message, data)
            }
            _ => {
                // First arg is the message
                (
                    LogLevel::Info,
                    level_str.to_string(),
                    args.get(1).map(|e| Value::from(e.clone())),
                )
            }
        }
    } else {
        return None;
    };

    Some(Effect::Log {
        level,
        message,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_emit_effect() {
        let expr: SExpression =
            serde_json::from_str(r#"["emit", "USER_REGISTERED", {"userId": "123"}]"#).unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Emit { event, payload } => {
                assert_eq!(event, "USER_REGISTERED");
                assert!(payload.as_object().is_some());
            }
            _ => panic!("Expected Emit effect"),
        }
    }

    #[test]
    fn the_oauth_effects_parse_with_their_arity_and_reject_the_rest() {
        let parse = |src: &str| parse_effect(&serde_json::from_str::<SExpression>(src).unwrap());
        assert!(matches!(
            parse(r#"["oauth-authorize", {"tag":"google"}, {"redirectUri":"u"}, {"emit":{"success":"OK"}}]"#),
            Some(Effect::OAuthAuthorize { emit: Some(_), .. })
        ));
        assert!(matches!(parse(r#"["oauth-authorize", {}, {}]"#), Some(Effect::OAuthAuthorize { emit: None, .. })));
        assert!(matches!(parse(r#"["oauth-connection-status", "google"]"#), Some(Effect::OAuthConnectionStatus { .. })));
        assert!(matches!(
            parse(r#"["oauth-connection-forget", "github", {"emit":{"success":"OK"}}]"#),
            Some(Effect::OAuthConnectionForget { emit: Some(_), .. })
        ));
        for src in [r#"["oauth-authorize", {}]"#, r#"["oauth-authorize", {}, {}, {}, {}]"#, r#"["oauth-connection-status"]"#, r#"["oauth-connection-forget", "a", {}, {}]"#] {
            assert!(parse(src).is_none(), "{src}");
        }
        for head in ["oauth-authorize", "oauth-connection-status", "oauth-connection-forget"] {
            assert!(is_known_effect_name(head), "{head}");
        }
    }

    #[test]
    fn client_kind_matches_kinds() {
        // Every client-effect variant reports a kind that is listed in CLIENT_EFFECT_KINDS...
        let client_variants = [
            Effect::RenderUi {
                slot: "main".into(),
                content: Value::Null,
            },
            Effect::Navigate {
                path: "/".into(),
                params: None,
                options: None,
            },
            Effect::NavigateBack,
            Effect::Emit {
                event: "E".into(),
                payload: Value::Null,
            },
        ];
        let reported: Vec<&str> = client_variants
            .iter()
            .map(|e| e.client_kind().expect("client variant has a kind"))
            .collect();
        for kind in &reported {
            assert!(
                CLIENT_EFFECT_KINDS.contains(kind),
                "{kind} missing from CLIENT_EFFECT_KINDS"
            );
        }
        // ...and CLIENT_EFFECT_KINDS lists exactly those, nothing stale.
        assert_eq!(reported, CLIENT_EFFECT_KINDS);

        // A server effect is not client-targeted.
        assert_eq!(
            Effect::Fetch {
                entity_type: "User".into(),
                id: None,
                filter: None,
                limit: None,
                offset: None,
                order_by: None,
                include: None,
                deliver: FetchDeliver::Rows,
                total: None,
                nearest: None,
                emit: None,
            }
            .client_kind(),
            None
        );
    }

    #[test]
    fn parse_call_service_reads_the_key_option() {
        let expr: SExpression = serde_json::from_str(
            r#"["call-service", "test-svc", "run", {"items": []}, {"key": "batch-1", "emit": {"cancelled": "C"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CallService { key, .. } => assert_eq!(key.as_ref().and_then(Value::as_str), Some("batch-1")),
            other => panic!("expected CallService, got {other:?}"),
        }
    }

    #[test]
    fn parse_call_service_without_options_has_no_key() {
        let expr: SExpression =
            serde_json::from_str(r#"["call-service", "test-svc", "run", {}]"#).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CallService { key, .. } => assert_eq!(key, None),
            other => panic!("expected CallService, got {other:?}"),
        }
    }

    #[test]
    fn parse_host_effects_read_their_argument_and_emit() {
        let random: SExpression =
            serde_json::from_str(r#"["random-bytes", 32, {"emit": {"success": "S", "failure": "F"}}]"#).unwrap();
        match parse_effect(&random).unwrap() {
            Effect::RandomBytes { bytes, emit, .. } => {
                assert_eq!(bytes, Value::Number(32.0));
                assert_eq!(emit.and_then(|e| e.success).as_deref(), Some("S"));
            }
            other => panic!("expected RandomBytes, got {other:?}"),
        }
        for (head, status) in [("credential-status", true), ("credential-forget", false)] {
            let expr: SExpression = serde_json::from_str(&format!(r#"["{head}", "tok", {{"emit": {{"success": "S"}}}}]"#)).unwrap();
            let parsed = parse_effect(&expr).unwrap();
            assert_eq!(parsed.operator(), head);
            assert_eq!(matches!(parsed, Effect::CredentialStatus { .. }), status);
            assert!(is_known_effect_name(head));
        }
        assert!(is_known_effect_name("random-bytes"));
        for op in crate::credential_admin::CredentialAdminOp::ALL {
            let expr: SExpression =
                serde_json::from_str(&format!(r#"["{}", {{"service": "s"}}, {{"emit": {{"success": "S", "failure": "F"}}}}]"#, op.name())).unwrap();
            match parse_effect(&expr).unwrap() {
                Effect::CredentialAdmin { op: parsed, request, emit } => {
                    assert_eq!(parsed, op);
                    assert!(request.as_object().is_some_and(|fields| fields.contains_key("service")));
                    assert_eq!(emit.and_then(|e| e.failure).as_deref(), Some("F"));
                }
                other => panic!("expected CredentialAdmin, got {other:?}"),
            }
            assert!(is_known_effect_name(op.name()));
            let bare: SExpression = serde_json::from_str(&format!(r#"["{}"]"#, op.name())).unwrap();
            assert!(parse_effect(&bare).is_none(), "{} needs its request", op.name());
        }
        assert!(!is_known_effect_name("credential-purge"));
    }

    #[test]
    fn parse_redis_reads_the_command_the_connection_options_the_key_and_the_routes() {
        let expr: SExpression = serde_json::from_str(
            r#"["redis", "redis://cache.internal", ["GET", "k"], {"auth": "PW", "db": 2}, {"key": "c1", "emit": {"success": "S", "failure": "F"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Redis { origin, command, options, key, emit } => {
                assert_eq!(origin, Value::String("redis://cache.internal".to_string()));
                assert_eq!(command, Value::Array(vec![Value::String("GET".to_string()), Value::String("k".to_string())]));
                let options = options.as_object().cloned().unwrap();
                assert_eq!(options.get("auth"), Some(&Value::String("PW".to_string())));
                assert_eq!(options.get("db"), Some(&Value::Number(2.0)));
                assert!(options.get("key").is_none() && options.get("emit").is_none());
                assert_eq!(key, Some(Value::String("c1".to_string())));
                let emit = emit.unwrap();
                assert_eq!((emit.success.as_deref(), emit.failure.as_deref()), (Some("S"), Some("F")));
            }
            other => panic!("expected Redis, got {other:?}"),
        }
        assert!(is_known_effect_name("redis"));
    }

    #[test]
    fn parse_redis_accepts_one_trailing_object_that_mixes_options_and_routes_and_none_at_all() {
        let mixed: SExpression =
            serde_json::from_str(r#"["redis", "redis://h", ["PING"], {"db": 1, "emit": {"success": "S"}}]"#).unwrap();
        match parse_effect(&mixed).unwrap() {
            Effect::Redis { options, emit, .. } => {
                assert_eq!(options.as_object().and_then(|o| o.get("db")), Some(&Value::Number(1.0)));
                assert_eq!(emit.and_then(|e| e.success).as_deref(), Some("S"));
            }
            other => panic!("expected Redis, got {other:?}"),
        }
        let bare: SExpression = serde_json::from_str(r#"["redis", "redis://h", ["PING"]]"#).unwrap();
        assert!(matches!(parse_effect(&bare), Some(Effect::Redis { key: None, emit: None, .. })));
        assert_eq!(parse_effect(&bare).unwrap().operator(), "redis");
    }

    #[test]
    fn parse_postgres_reads_sql_params_connection_options_key_and_routes() {
        let expr: SExpression = serde_json::from_str(
            r#"["postgres", "postgres://db.internal:5432", "SELECT $1::int", [1], {"auth": "PW", "user": "app", "database": "main"}, {"key": "q1", "emit": {"success": "S", "failure": "F"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Postgres { origin, sql, params, options, key, emit } => {
                assert_eq!(origin, Value::String("postgres://db.internal:5432".to_string()));
                assert_eq!(sql, Value::String("SELECT $1::int".to_string()));
                assert_eq!(params, Value::Array(vec![Value::Number(1.0)]));
                let options = options.as_object().cloned().unwrap();
                assert_eq!(options.get("database"), Some(&Value::String("main".to_string())));
                assert!(options.get("key").is_none() && options.get("emit").is_none());
                assert_eq!(key, Some(Value::String("q1".to_string())));
                assert_eq!(emit.and_then(|e| e.failure).as_deref(), Some("F"));
            }
            other => panic!("expected Postgres, got {other:?}"),
        }
        assert!(is_known_effect_name("postgres"));
    }

    #[test]
    fn control_parse_postgres_refuses_the_wrong_arity_and_a_non_object_trailer() {
        for src in [r#"["postgres", "postgres://h", "SELECT 1"]"#, r#"["postgres", "postgres://h", "SELECT 1", [], {}, {}, {}]"#, r#"["postgres", "postgres://h", "SELECT 1", [], "x"]"#] {
            let expr: SExpression = serde_json::from_str(src).unwrap();
            assert!(parse_effect(&expr).is_none(), "{src}");
        }
    }

    #[test]
    fn control_parse_redis_refuses_the_wrong_arity_and_a_non_object_trailer() {
        for src in [r#"["redis", "redis://h"]"#, r#"["redis", "redis://h", ["PING"], {}, {}, {}]"#, r#"["redis", "redis://h", ["PING"], "x"]"#] {
            let expr: SExpression = serde_json::from_str(src).unwrap();
            assert!(parse_effect(&expr).is_none(), "{src}");
        }
    }

    #[test]
    fn parse_host_effects_refuse_the_wrong_arity_and_never_fall_to_custom() {
        for src in [r#"["random-bytes"]"#, r#"["random-bytes", 1, {}, {}]"#, r#"["credential-status"]"#, r#"["credential-forget", "a", {}, {}]"#] {
            let expr: SExpression = serde_json::from_str(src).unwrap();
            assert!(parse_effect(&expr).is_none(), "{src}");
        }
    }

    #[test]
    fn parse_os_effects_read_args_key_and_emit_and_every_op_is_known() {
        let expr: SExpression = serde_json::from_str(
            r#"["os/spawn", "git", ["status"], {"cwd": "/r"}, {"key": "p", "emit": {"success": "S", "onMessage": "M"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Os { op, args, key, emit } => {
                assert_eq!(op, OsOp::Spawn);
                assert_eq!(args.len(), 3);
                assert_eq!(key, Some(Value::String("p".to_string())));
                let emit = emit.unwrap();
                assert_eq!((emit.success.as_deref(), emit.on_message.as_deref()), (Some("S"), Some("M")));
            }
            other => panic!("expected Os, got {other:?}"),
        }
        let plain: SExpression = serde_json::from_str(r#"["os/cwd"]"#).unwrap();
        assert!(matches!(parse_effect(&plain), Some(Effect::Os { op: OsOp::Cwd, key: None, emit: None, .. })));
        for op in OsOp::ALL {
            assert!(is_known_effect_name(op.name()));
        }
        assert_eq!(parse_effect(&plain).unwrap().operator(), "os/cwd");
    }

    #[test]
    fn parse_os_effects_refuse_the_wrong_arity_and_the_unknown_ops_still_fall_to_custom() {
        for src in [r#"["os/cwd", "x"]"#, r#"["os/read-file"]"#, r#"["os/read-file", "a", "b", "c"]"#, r#"["os/tcp-connect", "h"]"#] {
            let expr: SExpression = serde_json::from_str(src).unwrap();
            assert!(parse_effect(&expr).is_none(), "{src}");
        }
        let unknown: SExpression = serde_json::from_str(r#"["os/watch-http", "/x"]"#).unwrap();
        assert!(matches!(parse_effect(&unknown), Some(Effect::Custom { .. })));
    }

    #[test]
    fn parse_cancel_call() {
        let expr: SExpression = serde_json::from_str(r#"["cancel-call", "batch-1"]"#).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CancelCall { key } => assert_eq!(key, Value::String("batch-1".into())),
            other => panic!("expected CancelCall, got {other:?}"),
        }
        assert!(is_known_effect_name("cancel-call"));
        assert_eq!(
            Effect::CancelCall { key: "k".into() }.operator(),
            "cancel-call"
        );
    }

    #[test]
    fn parse_cancel_call_takes_a_list_of_keys_and_refuses_other_shapes() {
        let list: SExpression = serde_json::from_str(r#"["cancel-call", "@entity.keys"]"#).unwrap();
        assert!(parse_effect(&list).is_some(), "a binding stays one expression until resolved");
        let literal: SExpression = serde_json::from_str(r#"["cancel-call", ["list", "a", "b"]]"#).unwrap();
        assert!(matches!(
            parse_effect(&literal),
            Some(Effect::CancelCall { key: Value::Array(items) }) if items.len() == 3
        ));
        for refused in [r#"["cancel-call", 3]"#, r#"["cancel-call", {"a": 1}]"#, r#"["cancel-call", null]"#] {
            let expr: SExpression = serde_json::from_str(refused).unwrap();
            assert!(parse_effect(&expr).is_none(), "{refused}");
        }
    }

    #[test]
    fn parse_cancel_call_without_a_key_is_not_an_effect() {
        let expr: SExpression = serde_json::from_str(r#"["cancel-call"]"#).unwrap();
        assert!(parse_effect(&expr).is_none());
    }

    #[test]
    fn parse_browser_prompt_and_embed_carry_their_key_and_routes() {
        let prompt: SExpression = serde_json::from_str(
            r#"["browser/prompt", "@payload.text", "{}", {"key": "batch-1", "emit": {"success": "OK", "failure": "BAD", "cancelled": "STOP"}}]"#,
        )
        .unwrap();
        match parse_effect(&prompt).unwrap() {
            Effect::BrowserPrompt { text, schema, key, emit } => {
                assert_eq!(text, Value::String("@payload.text".into()));
                assert_eq!(schema, Value::String("{}".into()));
                assert_eq!(key, Some(Value::String("batch-1".into())));
                assert_eq!(emit.unwrap().cancelled.as_deref(), Some("STOP"));
            }
            other => panic!("expected BrowserPrompt, got {other:?}"),
        }
        let embed: SExpression = serde_json::from_str(r#"["browser/embed", ["a", "b"]]"#).unwrap();
        match parse_effect(&embed).unwrap() {
            Effect::BrowserEmbed { key, emit, .. } => assert!(key.is_none() && emit.is_none()),
            other => panic!("expected BrowserEmbed, got {other:?}"),
        }
        assert_eq!(parse_effect(&embed).unwrap().operator(), "browser/embed");
    }

    #[test]
    fn parse_browser_tab_effects_carry_their_request_key_and_routes() {
        let query: SExpression = serde_json::from_str(
            r#"["browser/tab-query", {"tab": "?tab", "selector": ".r"}, {"key": "q-1", "emit": {"success": "OK", "failure": "BAD", "cancelled": "STOP"}}]"#,
        )
        .unwrap();
        match parse_effect(&query).unwrap() {
            Effect::BrowserTab { op, request, key, emit } => {
                assert_eq!(op, crate::tab::TabOp::Query);
                assert_eq!(request.as_object().and_then(|o| o.get("selector")), Some(&Value::String(".r".into())));
                assert_eq!(key, Some(Value::String("q-1".into())));
                assert_eq!(emit.unwrap().cancelled.as_deref(), Some("STOP"));
            }
            other => panic!("expected BrowserTab, got {other:?}"),
        }
        for op in crate::tab::TabOp::ALL {
            let src = format!(r#"["{}", {{}}]"#, op.name());
            let expr: SExpression = serde_json::from_str(&src).unwrap();
            assert_eq!(parse_effect(&expr).unwrap().operator(), op.name());
        }
    }

    #[test]
    fn control_browser_tab_effects_refuse_the_wrong_arity() {
        for src in [r#"["browser/tab-list"]"#, r#"["browser/tab-open", {}, {}, {}]"#, r#"["browser/tab-nope", {}]"#] {
            let expr: SExpression = serde_json::from_str(src).unwrap();
            assert!(!matches!(parse_effect(&expr), Some(Effect::BrowserTab { .. })), "{src}");
        }
    }

    #[test]
    fn parse_browser_prompt_and_embed_refuse_the_wrong_arity() {
        for src in [r#"["browser/prompt", "t"]"#, r#"["browser/prompt", "t", "s", {}, {}]"#, r#"["browser/embed"]"#, r#"["browser/embed", [], {}, {}]"#] {
            let expr: SExpression = serde_json::from_str(src).unwrap();
            assert!(parse_effect(&expr).is_none(), "{src}");
        }
    }

    #[test]
    fn test_parse_persist_create() {
        let expr: SExpression =
            serde_json::from_str(r#"["persist", "create", "User", {"name": "John"}]"#).unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Persist {
                operation,
                entity_type,
                ..
            } => {
                assert_eq!(operation, PersistOperation::Create);
                assert_eq!(entity_type, "User");
            }
            _ => panic!("Expected Persist effect"),
        }
    }

    #[test]
    fn test_parse_persist_update() {
        let expr: SExpression =
            serde_json::from_str(r#"["persist", "update", "User", "user-123", {"name": "Jane"}]"#)
                .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Persist {
                operation,
                entity_type,
                id,
                ..
            } => {
                assert_eq!(operation, PersistOperation::Update);
                assert_eq!(entity_type, "User");
                assert_eq!(id, Some("user-123".to_string()));
            }
            _ => panic!("Expected Persist effect"),
        }
    }

    /// C-PERSIST-UPDATE-TRAILING-EMIT-MISPARSED: `(persist update Entity ?data
    /// {emit:{...}})` — no separate id, data carries its own id, emit is a
    /// TRAILING OPTIONS object — must not be read as (id, data).
    #[test]
    fn test_parse_persist_update_with_trailing_emit_options_is_not_misread_as_id_data() {
        let expr: SExpression = serde_json::from_str(
            r#"["persist", "update", "Ticket", "@payload.data", {"emit": {"success": "TICKET_UPDATED"}}]"#,
        )
        .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Persist {
                operation,
                entity_type,
                id,
                data,
                emit,
            } => {
                assert_eq!(operation, PersistOperation::Update);
                assert_eq!(entity_type, "Ticket");
                assert_eq!(
                    id, None,
                    "no separate id was declared — must not be the emit config"
                );
                assert_eq!(
                    data,
                    Value::String("@payload.data".to_string()),
                    "the real data token, not the trailing {{emit}} object"
                );
                assert_eq!(
                    emit.and_then(|e| e.success),
                    Some("TICKET_UPDATED".to_string()),
                    "the trailing {{emit}} object is captured, not just stripped"
                );
            }
            _ => panic!("Expected Persist effect"),
        }
    }

    /// A trailing object WITHOUT an `emit` key is real positional data, not
    /// options — the explicit-id shape (`[.., id, data]`) must keep working.
    #[test]
    fn test_parse_persist_update_explicit_id_with_non_emit_trailing_object_still_works() {
        let expr: SExpression =
            serde_json::from_str(r#"["persist", "update", "User", "user-123", {"name": "Jane"}]"#)
                .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Persist { id, data, .. } => {
                assert_eq!(id, Some("user-123".to_string()));
                let name = data
                    .as_object()
                    .and_then(|o| o.get("name"))
                    .and_then(|v| v.as_str());
                assert_eq!(name, Some("Jane"));
            }
            _ => panic!("Expected Persist effect"),
        }
    }

    /// `(persist delete Entity id {emit:{...}})` — delete's own arm always
    /// reads its id positionally regardless of trailing options; confirm the
    /// stripping helper doesn't disturb it.
    #[test]
    fn test_parse_persist_delete_with_trailing_emit_options() {
        let expr: SExpression = serde_json::from_str(
            r#"["persist", "delete", "Ticket", "@payload.id", {"emit": {"success": "TICKET_DELETED"}}]"#,
        )
        .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Persist { operation, id, .. } => {
                assert_eq!(operation, PersistOperation::Delete);
                assert_eq!(id, Some("@payload.id".to_string()));
            }
            _ => panic!("Expected Persist effect"),
        }
    }

    #[test]
    fn test_parse_render_ui() {
        let expr: SExpression = serde_json::from_str(
            r#"["render-ui", "main", {"type": "page-header", "title": "Users"}]"#,
        )
        .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::RenderUi { slot, content } => {
                assert_eq!(slot, "main");
                assert!(content.as_object().is_some());
            }
            _ => panic!("Expected RenderUi effect"),
        }
    }

    #[test]
    fn test_parse_navigate() {
        let expr: SExpression = serde_json::from_str(r#"["navigate", "/users"]"#).unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Navigate {
                path,
                params,
                options,
            } => {
                assert_eq!(path, "/users");
                assert!(params.is_none());
                assert!(options.is_none());
            }
            _ => panic!("Expected Navigate effect"),
        }
    }

    #[test]
    fn test_parse_set() {
        let expr: SExpression =
            serde_json::from_str(r#"["set", "@entity.status", "active"]"#).unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Set { path, value } => {
                assert_eq!(path, "@entity.status");
                assert_eq!(value, Value::String("active".to_string()));
            }
            _ => panic!("Expected Set effect"),
        }
    }

    #[test]
    fn test_parse_custom_effect() {
        let expr: SExpression = serde_json::from_str(r#"["custom-effect", "arg1", 42]"#).unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Custom { name, args, .. } => {
                assert_eq!(name, "custom-effect");
                assert_eq!(args.len(), 2);
            }
            _ => panic!("Expected Custom effect"),
        }
    }

    #[test]
    fn test_runtime_environment() {
        assert!(RuntimeEnvironment::Server.is_server());
        assert!(!RuntimeEnvironment::Server.is_client());
        assert!(RuntimeEnvironment::Client.is_client());
        assert!(!RuntimeEnvironment::Client.is_server());
    }

    #[test]
    fn test_effect_result() {
        let effect = Effect::Emit {
            event: "TEST".to_string(),
            payload: Value::Null,
        };

        let executed = EffectResult::executed(effect.clone());
        assert!(executed.is_executed());

        let noop = EffectResult::noop(effect.clone(), "client-only");
        assert!(noop.is_noop());

        let delegated = EffectResult::delegated(effect.clone(), RuntimeEnvironment::Server);
        assert!(delegated.is_delegated());

        let failed = EffectResult::failed(effect, "error message");
        assert!(failed.is_failed());
    }

    #[test]
    fn parse_fetch_reads_total_none_and_only_none() {
        let total_of = |config: &str| {
            let expr: SExpression =
                serde_json::from_str(&format!(r#"["fetch", "User", {config}]"#)).unwrap();
            match parse_effect(&expr).unwrap() {
                Effect::Fetch { total, .. } => total,
                other => panic!("not a fetch: {other:?}"),
            }
        };
        assert_eq!(
            total_of(r#"{"total": "none"}"#),
            Some(crate::row_set::RowTotalMode::None)
        );
        assert_eq!(total_of(r#"{"total": "exact"}"#), None);
        assert_eq!(total_of(r#"{"limit": 5}"#), None);
        let expr: SExpression = serde_json::from_str(r#"["fetch", "User"]"#).unwrap();
        let Effect::Fetch { total, .. } = parse_effect(&expr).unwrap() else {
            panic!("not a fetch")
        };
        assert_eq!(total, None);
    }

    #[test]
    fn parse_fetch_reads_the_nearest_option() {
        let nearest_of = |config: &str| {
            let expr: SExpression =
                serde_json::from_str(&format!(r#"["fetch", "Doc", {config}]"#)).unwrap();
            match parse_effect(&expr).unwrap() {
                Effect::Fetch { nearest, .. } => nearest,
                other => panic!("not a fetch: {other:?}"),
            }
        };
        let got = nearest_of(r#"{"nearest": {"field": "embedding", "vector": "@payload.v", "k": 3}}"#)
            .expect("nearest kept");
        let members = got.as_object().expect("an object");
        assert_eq!(members.get("field").and_then(Value::as_str), Some("embedding"));
        assert!(members.get("k").is_some());
        assert!(nearest_of(r#"{"limit": 5}"#).is_none());
        assert!(nearest_of(r#"{"nearest": null}"#).is_none());
    }

    #[test]
    fn test_parse_fetch_all() {
        // ["fetch", "User"] - fetch all users (from Orbital_Pattern.orb INIT transition)
        let expr: SExpression = serde_json::from_str(r#"["fetch", "User"]"#).unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Fetch {
                entity_type,
                id,
                filter,
                limit,
                offset,
                ..
            } => {
                assert_eq!(entity_type, "User");
                assert!(id.is_none());
                assert!(filter.is_none());
                assert!(limit.is_none());
                assert!(offset.is_none());
            }
            _ => panic!("Expected Fetch effect"),
        }
    }

    /// A `call-service` options object carries its completion events beside `key:` — JS
    /// `EffectExecutor.ts` reads `emit` from `args[3]` (drift D20).
    #[test]
    fn a_call_service_options_object_carries_its_completion_events() {
        let expr: SExpression = serde_json::from_str(
            r#"["call-service", "test-svc", "run", {"texts": ["a"]}, {"key": "k1", "emit": {"success": "EMBEDDED", "failure": "EMBED_FAILED"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CallService { key, emit, .. } => {
                assert_eq!(key.as_ref().and_then(Value::as_str), Some("k1"));
                let emit = emit.expect("emit parsed");
                assert_eq!(emit.success.as_deref(), Some("EMBEDDED"));
                assert_eq!(emit.failure.as_deref(), Some("EMBED_FAILED"));
            }
            other => panic!("expected CallService, got {other:?}"),
        }
    }

    #[test]
    fn control_a_call_service_without_options_declares_no_completion() {
        let expr: SExpression =
            serde_json::from_str(r#"["call-service", "test-svc", "run", {}]"#).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CallService { emit, .. } => assert!(emit.is_none()),
            other => panic!("expected CallService, got {other:?}"),
        }
    }

    /// A substrate/program op's trailing `{ emit }` object is its completion config, not a
    /// positional argument — JS `splitSubstrateEmit` (drift D20).
    #[test]
    fn a_substrate_ops_trailing_emit_object_is_split_from_its_arguments() {
        let expr: SExpression = serde_json::from_str(
            r#"["program/eval", {"name": "App", "orbitals": []}, {"final": true}, {"emit": {"success": "EVALUATED", "failure": "EVAL_FAILED"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Custom { name, args, emit } => {
                assert_eq!(name, "program/eval");
                assert_eq!(args.len(), 2, "the emit object is not an argument");
                assert_eq!(
                    emit.expect("emit parsed").success.as_deref(),
                    Some("EVALUATED")
                );
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn edge_a_trailing_object_without_emit_stays_an_argument() {
        let expr: SExpression = serde_json::from_str(
            r#"["program/eval", {"name": "App", "orbitals": []}, {"final": true}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Custom { args, emit, .. } => {
                assert_eq!(args.len(), 2);
                assert!(emit.is_none());
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_fetch_emit() {
        // The fetch-completion `emit` (RS-G16): a data-loading trait declares the
        // events fired when the fetch resolves, advancing it loading → loaded.
        let expr: SExpression = serde_json::from_str(
            r#"["fetch", "InventoryItem", {"limit": 10, "emit": {"success": "BrowseItemLoaded", "failure": "BrowseItemLoadFailed"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::Fetch { limit, emit, .. } => {
                assert_eq!(limit, Some(Value::Number(10.0)));
                let emit = emit.expect("emit parsed");
                assert_eq!(emit.success.as_deref(), Some("BrowseItemLoaded"));
                assert_eq!(emit.failure.as_deref(), Some("BrowseItemLoadFailed"));
            }
            _ => panic!("Expected Fetch effect"),
        }

        // No `emit` key → None (the common case stays unchanged).
        let bare: SExpression = serde_json::from_str(r#"["fetch", "User"]"#).unwrap();
        match parse_effect(&bare).unwrap() {
            Effect::Fetch { emit, .. } => assert!(emit.is_none()),
            _ => panic!("Expected Fetch effect"),
        }
    }

    #[test]
    fn test_parse_fetch_by_id() {
        // ["fetch", "User", {"id": "user-123"}] - fetch single user (from Orbital_Pattern.orb EDIT transition)
        let expr: SExpression =
            serde_json::from_str(r#"["fetch", "User", {"id": "user-123"}]"#).unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Fetch {
                entity_type,
                id,
                filter,
                ..
            } => {
                assert_eq!(entity_type, "User");
                assert_eq!(id, Some(Value::String("user-123".to_string())));
                assert!(filter.is_none());
            }
            _ => panic!("Expected Fetch effect"),
        }
    }

    #[test]
    fn test_parse_fetch_with_filter() {
        // ["fetch", "User", {"filter": ["=", "@entity.status", "active"]}]
        let expr: SExpression = serde_json::from_str(
            r#"["fetch", "User", {"filter": ["=", "@entity.status", "active"]}]"#,
        )
        .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Fetch {
                entity_type,
                id,
                filter,
                ..
            } => {
                assert_eq!(entity_type, "User");
                assert!(id.is_none());
                assert!(filter.is_some());
                // Verify filter is an array
                let filter_value = filter.unwrap();
                assert!(filter_value.as_array().is_some());
            }
            _ => panic!("Expected Fetch effect"),
        }
    }

    #[test]
    fn test_parse_fetch_with_pagination() {
        let expr: SExpression =
            serde_json::from_str(r#"["fetch", "Connection", {"limit": 10, "offset": 20}]"#)
                .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Fetch {
                entity_type,
                limit,
                offset,
                ..
            } => {
                assert_eq!(entity_type, "Connection");
                assert_eq!(limit, Some(Value::Number(10.0)));
                assert_eq!(offset, Some(Value::Number(20.0)));
            }
            _ => panic!("Expected Fetch effect"),
        }
    }

    #[test]
    fn parse_persist_batch_keeps_the_ops_expression_and_emit() {
        let expr: SExpression = serde_json::from_str(
            r#"["persist", "batch", [["create", "User", {"name": "A"}]], {"emit": {"success": "SAVED", "failure": "FAILED"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::PersistBatch { ops, emit } => {
                assert!(matches!(ops, Value::Array(ref a) if a.len() == 1));
                let emit = emit.unwrap();
                assert_eq!(emit.success.as_deref(), Some("SAVED"));
                assert_eq!(emit.failure.as_deref(), Some("FAILED"));
            }
            other => panic!("expected PersistBatch, got {other:?}"),
        }
        assert_eq!(parse_effect(&expr).unwrap().operator(), "persist");
    }

    #[test]
    fn test_parse_fetch_with_include() {
        // ["fetch", "Inspection", {"include": ["company", "inspector"]}]
        let expr: SExpression = serde_json::from_str(
            r#"["fetch", "Inspection", {"include": ["company", "inspector"]}]"#,
        )
        .unwrap();

        let effect = parse_effect(&expr).unwrap();
        match effect {
            Effect::Fetch {
                entity_type,
                include,
                ..
            } => {
                assert_eq!(entity_type, "Inspection");
                assert!(include.is_some());
                let include_vec = include.unwrap();
                assert_eq!(include_vec.len(), 2);
                assert!(include_vec.contains(&"company".to_string()));
                assert!(include_vec.contains(&"inspector".to_string()));
            }
            _ => panic!("Expected Fetch effect"),
        }
    }
}
