//! Effect Types
//!
//! Core types for effect execution.

use crate::evaluator::Value;
use crate::schema::SExpression;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}

impl FetchEmit {
    /// True when neither a success nor a failure event is declared (nothing to emit).
    pub fn is_empty(&self) -> bool {
        self.success.is_none() && self.failure.is_none()
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
    PersistBatch {
        ops: Value,
        emit: Option<FetchEmit>,
    },

    /// Call an external service
    CallService {
        service: String,
        method: String,
        args: Value,
        /// Evaluated `key:` option naming the call for `cancel-call`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },

    /// Abort the in-flight keyed call-service started with `key`
    CancelCall { key: String },

    /// Render UI (client-only)
    RenderUi { slot: String, content: Value },

    /// Navigate to a route (client-only)
    Navigate {
        path: String,
        params: Option<HashMap<String, Value>>,
        /// `{ crumb?: string }` — the optional third `navigate` argument
        /// (`(navigate path params { crumb: ... })`). Mirrors the wire's
        /// `NavigateOptions` (`super::client_effect`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<super::client_effect::NavigateOptions>,
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
        /// Completion events fired after the fetch resolves (`success` carries the rows
        /// as `{ data: [...] }`; `failure` fires on error). Drives `loading` → `loaded`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },

    /// Generic/unknown effect (for extensibility)
    Custom { name: String, args: Vec<Value> },
}

/// All client-targeted effect wire kinds, in `kebab-case`. Every client shell MUST handle
/// each; the shell effect-coverage gates iterate this. Kept in sync with [`Effect::client_kind`]
/// by `effects::tests::client_kind_matches_kinds`.
pub const CLIENT_EFFECT_KINDS: &[&str] =
    &["render-ui", "navigate", "navigate-back", "emit"];

impl Effect {
    /// Registry name of the operator this effect came from.
    pub fn operator(&self) -> &str {
        match self {
            Effect::Emit { .. } => "emit",
            Effect::Set { .. } => "set",
            Effect::Persist { .. } | Effect::PersistBatch { .. } => "persist",
            Effect::CallService { .. } => "call-service",
            Effect::CancelCall { .. } => "cancel-call",
            Effect::RenderUi { .. } => "render-ui",
            Effect::Navigate { .. } => "navigate",
            Effect::NavigateBack => "navigate-back",
            Effect::Spawn { .. } => "spawn",
            Effect::Despawn { .. } => "despawn",
            Effect::Log { .. } => "log",
            Effect::Fetch { .. } => "fetch",
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
        }
    }

    /// Create a successful fetch execution with entity type tracking
    ///
    /// This is used for Fetch effects to track which entity type was fetched.
    pub fn executed_fetch(
        effect: Effect,
        entities: Value,
        entity_type: &str,
        total: usize,
    ) -> Self {
        Self {
            effect,
            outcome: EffectOutcome::Executed,
            return_value: Some(entities),
            error: None,
            entity_type: Some(entity_type.to_string()),
            total_count: Some(total),
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
        }
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

    match op {
        "emit" => parse_emit(args),
        "set" => parse_set(args),
        "persist" => parse_persist(args),
        "fetch" => parse_fetch(args),
        "call-service" => parse_call_service(args),
        "cancel-call" => parse_cancel_call(args),
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
        _ => Some(Effect::Custom {
            name: op.to_string(),
            args: args.iter().map(|e| Value::from(e.clone())).collect(),
        }),
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
///   `workspace/*`, `integration/*`) and the named reflection effects
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
        || op.starts_with("integration/")
        || crate::ir::is_substrate_operator(op)
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
            | "cancel-call"
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
fn strip_trailing_emit_options(args: &[SExpression]) -> &[SExpression] {
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
    obj.as_object()?.get("emit").and_then(|v| v.as_object()).and_then(|e| {
        let success = e
            .get("success")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let failure = e
            .get("failure")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let fe = FetchEmit { success, failure };
        (!fe.is_empty()).then_some(fe)
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
    let (id, filter, limit, offset, order_by, include, emit) = if let Some(config) = args.get(1) {
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
            let emit = parse_emit_config(&config_value);
            (id, filter, limit, offset, order_by, include, emit)
        } else {
            (None, None, None, None, None, None, None)
        }
    } else {
        (None, None, None, None, None, None, None)
    };

    Some(Effect::Fetch {
        entity_type,
        id,
        filter,
        limit,
        offset,
        order_by,
        include,
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
            .and_then(|k| k.as_str())
            .map(str::to_string),
        _ => None,
    };
    Some(Effect::CallService {
        service,
        method,
        args: call_args,
        key,
    })
}

fn parse_cancel_call(args: &[SExpression]) -> Option<Effect> {
    let key = args.first()?.as_str()?.to_string();
    Some(Effect::CancelCall { key })
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
            let crumb = obj.get("crumb").and_then(|v| v.as_str()).map(|s| s.to_string());
            Some(super::client_effect::NavigateOptions { crumb })
        } else {
            None
        }
    });
    Some(Effect::Navigate { path, params, options })
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
                emit: None,
            }
            .client_kind(),
            None
        );
    }

    #[test]
    fn parse_call_service_reads_the_key_option() {
        let expr: SExpression = serde_json::from_str(
            r#"["call-service", "llm", "classifyMany", {"items": []}, {"key": "batch-1", "emit": {"cancelled": "C"}}]"#,
        )
        .unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CallService { key, .. } => assert_eq!(key.as_deref(), Some("batch-1")),
            other => panic!("expected CallService, got {other:?}"),
        }
    }

    #[test]
    fn parse_call_service_without_options_has_no_key() {
        let expr: SExpression =
            serde_json::from_str(r#"["call-service", "llm", "generate", {}]"#).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CallService { key, .. } => assert_eq!(key, None),
            other => panic!("expected CallService, got {other:?}"),
        }
    }

    #[test]
    fn parse_cancel_call() {
        let expr: SExpression = serde_json::from_str(r#"["cancel-call", "batch-1"]"#).unwrap();
        match parse_effect(&expr).unwrap() {
            Effect::CancelCall { key } => assert_eq!(key, "batch-1"),
            other => panic!("expected CancelCall, got {other:?}"),
        }
        assert!(is_known_effect_name("cancel-call"));
        assert_eq!(
            Effect::CancelCall { key: "k".into() }.operator(),
            "cancel-call"
        );
    }

    #[test]
    fn parse_cancel_call_without_a_key_is_not_an_effect() {
        let expr: SExpression = serde_json::from_str(r#"["cancel-call"]"#).unwrap();
        assert!(parse_effect(&expr).is_none());
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
            Effect::Navigate { path, params, options } => {
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
            Effect::Custom { name, args } => {
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
