//! Effect Executor
//!
//! Environment-aware effect execution with routing logic.

use super::response::ServerEffectCode;
use super::runs_on::{runs_on, RunsOn};
use super::types::{
    Effect, EffectResult, FetchDeliver, LogLevel, PersistOperation, RuntimeEnvironment,
};
use crate::evaluator::ValueMap;
use crate::evaluator::{BindingContext, Evaluator, Value};
use crate::events::{Event, SharedEventBus};
use crate::runtime::entity_access::{
    check_mutation_access, missing_required_fields, scope_fetched_rows, EntityAccessTable,
};
use crate::runtime::row_query::{
    compare_rows, compile_predicate, default_row_order, PredicateRole, RowDirection, RowOrder, RowPredicate,
    RowSetRef, RowTotalMode, RowWriteBatch, RowWriteFailureMode, RowWriteOp, RowWriteOutcome,
};
use crate::schema::SExpression;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// A persist update/delete that resolved no row key.
const NO_ROW_KEY: &str = "persist update/delete resolved no row key — the id was neither \
     supplied as the operation's id argument nor present on the row \
     being written. Bind it before the write, e.g. \
     `(set @entity.id ?row.id)` on the transition that selects the row.";

/// Handler for persist operations
pub trait PersistenceHandler: Send + Sync {
    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String>;
    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String>;
    /// The row, or `None` when no such row exists; an `Err` is a failure, not a miss. The default
    /// reads the row and takes `Ok(Null)` as absent; a store that tracks absence overrides it.
    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        match self.read(entity_type, id)? {
            Value::Null => Ok(None),
            row => Ok(Some(row)),
        }
    }
    fn update(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String>;
    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String>;
    /// List all entities of a type (for fetch without ID)
    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String>;
    /// One conditional write: the row is tested against the op's guard (on its pre-image) and written in one
    /// step. `None` = this store cannot, and nothing was written; the caller takes the read-then-write path.
    fn write_where(&self, _op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        None
    }
    /// Several conditional writes at once, `atomic` or `stop` as the batch says. `None` = this store cannot, and
    /// nothing was written.
    fn write_batch(&self, _batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        None
    }
}

/// Handler for fetch operations (server-only)
///
/// This trait handles the `fetch` effect which queries entities from persistence.
/// It supports fetching all entities, by ID, with filters, and with pagination.
///
/// Reference: Orbital_Pattern.orb uses fetch in INIT transitions like:
/// - `["fetch", "User"]` - fetch all users
/// - `["fetch", "User", {"id": "@payload.row.id"}]` - fetch by ID
pub trait FetchHandler: Send + Sync {
    /// Fetch entities from persistence
    ///
    /// # Arguments
    /// * `entity_type` - The entity type to fetch (e.g., "User", "Connection")
    /// * `id` - Optional specific entity ID
    /// * `filter` - Optional filter expression (S-expression format)
    /// * `limit` - Optional limit for pagination
    /// * `offset` - Optional offset for pagination
    /// * `include` - Optional list of relation fields to include
    /// * `ctx` - Binding context for resolving @-references in filter
    ///
    /// # Returns
    /// The page of rows plus the count before offset/limit, or error
    #[allow(clippy::too_many_arguments)] // fetch query params (filter/limit/offset/include)
    fn fetch(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        limit: Option<usize>,
        offset: Option<usize>,
        include: Option<&Vec<String>>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String>;

    /// `fetch` with an `orderBy`: the page is the window of the matches ordered by `order` (ties by `id`
    /// ascending; with no `order`, by `createdAt` then `id`, EXEC-FETCH-1). The default fetches every match and orders them here; a handler that can order where the
    /// data lives overrides it.
    #[allow(clippy::too_many_arguments)]
    fn fetch_ordered(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        order: Option<&RowOrder>,
        limit: Option<usize>,
        offset: Option<usize>,
        include: Option<&Vec<String>>,
        total: RowTotalMode,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        let _ = total;
        let default_order = default_row_order();
        match (order, id) {
            (None, Some(_)) => self.fetch(entity_type, id, filter, limit, offset, include, ctx),
            (order, _) => self
                .fetch(entity_type, id, filter, None, None, include, ctx)
                .map(|page| FetchPage {
                    total: page.total,
                    rows: page_ordered_rows(
                        page.rows,
                        order.unwrap_or(&default_order),
                        offset,
                        limit,
                    ),
                }),
        }
    }

    /// A collection fetch delivered by reference (`deliver: ids`): the window's ids, the count of every match,
    /// and the query that re-counts it at resolve time. A handler that cannot express the fetch as a row query
    /// refuses, since a reference without its query could not re-count `total` (Q6).
    #[allow(clippy::too_many_arguments)]
    fn fetch_row_set(
        &self,
        entity_type: &str,
        _filter: Option<&Value>,
        _order: Option<&RowOrder>,
        _limit: Option<usize>,
        _offset: Option<usize>,
        _total: RowTotalMode,
        _ctx: &BindingContext,
    ) -> Result<RowSetRef, String> {
        Err(format!(
            "{entity_type}: this fetch handler cannot deliver rows by reference"
        ))
    }
}

/// A fetch's rows after `@read`, `filter:`, offset and limit, with `total` counted
/// before offset/limit — the success payload's `totalCount`.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchPage {
    pub rows: Vec<Value>,
    pub total: usize,
}

/// Who a `call-service` runs for: the dispatch's `@user` (`{ principal: id, role }`, as the JS runtime
/// passes it). `None` when nobody is signed in or the viewer has no id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceCaller {
    pub principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl ServiceCaller {
    pub fn of_user(user: &Value) -> Option<Self> {
        let Value::Object(fields) = user else {
            return None;
        };
        let Some(Value::String(principal)) = fields.get("id") else {
            return None;
        };
        let role = match fields.get("role") {
            Some(Value::String(role)) => Some(role.clone()),
            _ => None,
        };
        Some(Self {
            principal: principal.clone(),
            role,
        })
    }
}

/// Handler for service calls
pub trait ServiceHandler: Send + Sync {
    /// Run one call. `key` is the call's declared `key:` — the name `cancel`
    /// aborts it by; `None` for an unkeyed call.
    fn call(
        &self,
        service: &str,
        method: &str,
        args: &Value,
        key: Option<&str>,
        caller: Option<&ServiceCaller>,
    ) -> Result<Value, String>;

    /// Abort the in-flight call started with `key`. A call that already
    /// finished (or never started) is not an error.
    fn cancel(&self, key: &str) -> Result<(), String>;
}

/// A substrate operator's failure: the message, plus any structured issues (a program's
/// validator issues for `program/eval`) carried on the failure event as `errors`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubstrateFailure {
    pub message: String,
    pub errors: Vec<Value>,
}

/// Host for substrate operators (`program/*`, `behavior/*`, `llm/*`, …): the effects the
/// installed operator table routes to a host (`ir::is_substrate_operator`). The JS twin is
/// `@almadar/integrations/program` `runProgramEffect` behind `EffectExecutor.ts`.
pub trait SubstrateHandler: Send + Sync {
    /// Run one op with its positional arguments (the trailing `{ emit }` already split off).
    fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure>;
}

/// Handler for UI rendering (client-only)
pub trait RenderHandler: Send + Sync {
    fn render(&self, slot: &str, content: &Value) -> Result<(), String>;
    fn clear(&self, slot: &str) -> Result<(), String>;
}

/// Handler for navigation (client-only)
pub trait NavigationHandler: Send + Sync {
    fn navigate(&self, path: &str, params: Option<&ValueMap>) -> Result<(), String>;

    /// Pop the navigation stack and return to the previous entry. Hosts
    /// without a stack (headless verify harnesses) keep the default noop-ish
    /// error, which the executor reports as a noop rather than a failure.
    fn navigate_back(&self) -> Result<(), String> {
        Err("no navigation stack".to_string())
    }
}

/// Handler for game entities (client/game only)
pub trait GameEntityHandler: Send + Sync {
    fn spawn(&self, entity_type: &str, props: &Value) -> Result<String, String>;
    fn despawn(&self, entity_id: &str) -> Result<(), String>;
}

/// Handler for streaming fetch operations (server-only).
///
/// Receives chunks via the callback; fires `on_message` per chunk and
/// `success` on completion. The kernel calls this synchronously in the
/// Rust path (orbital-verify); the real async stream runs in the JS runtime.
pub trait StreamFetchHandler: Send + Sync {
    fn fetch_stream(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        on_chunk: &dyn Fn(Value),
    ) -> Result<Value, String>;
}

/// Delegate for effects that need to be sent to another environment
pub trait EffectDelegate: Send + Sync {
    fn delegate(&self, effect: &Effect, target: RuntimeEnvironment) -> Result<(), String>;

    /// Escape hatch for recovering a concrete delegate type after
    /// `KernelConfig`/`EffectExecutor` erase it to `Arc<dyn EffectDelegate>` —
    /// e.g. `RuntimeKernel::dispatch_with_server_leg` downcasting to
    /// `runtime::server_leg::ServerLegCollector` to drain what it collected
    /// during one dispatch. `None` by default; a delegate opts in by
    /// overriding it with `Some(self)`.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

/// In-memory persistence handler for testing
#[derive(Debug, Default)]
pub struct InMemoryPersistence {
    /// Rows per entity in insertion order, as the JS store's `Map`.
    data: parking_lot::RwLock<HashMap<String, indexmap::IndexMap<String, Value>>>,
    next_id: std::sync::atomic::AtomicU64,
    entity_policies: Option<Arc<RwLock<EntityAccessTable>>>,
}

impl InMemoryPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_entity_policies(mut self, table: Arc<RwLock<EntityAccessTable>>) -> Self {
        self.entity_policies = Some(table);
        self
    }

    fn generate_id(&self) -> String {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("id-{}", id)
    }
}

impl PersistenceHandler for InMemoryPersistence {
    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let mut store = self.data.write();
        let collection = store.entry(entity_type.to_string()).or_default();

        let id = if let Some(obj) = data.as_object() {
            obj.get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
        .unwrap_or_else(|| self.generate_id());

        let mut record = data.clone();
        if let Some(obj) = record.as_object_mut() {
            obj.insert("id".to_string(), Value::String(id.clone()));
        }

        collection.insert(id.clone(), record.clone());
        Ok(record)
    }

    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String> {
        let store = self.data.read();
        store
            .get(entity_type)
            .and_then(|c| c.get(id))
            .cloned()
            .ok_or_else(|| format!("Entity not found: {}:{}", entity_type, id))
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        Ok(self
            .data
            .read()
            .get(entity_type)
            .and_then(|c| c.get(id))
            .cloned())
    }

    fn update(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let mut store = self.data.write();
        let collection = store
            .get_mut(entity_type)
            .ok_or_else(|| format!("Collection not found: {}", entity_type))?;

        let existing = collection
            .get_mut(id)
            .ok_or_else(|| format!("Entity not found: {}:{}", entity_type, id))?;

        // Merge data
        if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), data.as_object()) {
            for (k, v) in new_obj {
                existing_obj.insert(k.clone(), v.clone());
            }
        }

        Ok(existing.clone())
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let mut store = self.data.write();
        let collection = store
            .get_mut(entity_type)
            .ok_or_else(|| format!("Collection not found: {}", entity_type))?;

        collection
            .shift_remove(id)
            .ok_or_else(|| format!("Entity not found: {}:{}", entity_type, id))?;

        Ok(())
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String> {
        let store = self.data.read();
        Ok(store
            .get(entity_type)
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default())
    }
}

impl FetchHandler for InMemoryPersistence {
    fn fetch(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        limit: Option<usize>,
        offset: Option<usize>,
        _include: Option<&Vec<String>>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        let mut entities: Vec<Value> = {
            let store = self.data.read();
            let collection = store.get(entity_type);
            match id {
                Some(id) => collection
                    .and_then(|c| c.get(id))
                    .cloned()
                    .into_iter()
                    .collect(),
                None => collection
                    .map(|c| c.values().cloned().collect())
                    .unwrap_or_default(),
            }
        };
        let policy = self
            .entity_policies
            .as_ref()
            .and_then(|table| table.read().get(entity_type).and_then(|p| p.read.clone()));
        let total = scope_fetched_rows(
            &mut entities,
            policy.as_ref(),
            filter,
            limit,
            offset,
            ctx,
            entity_type,
        );
        Ok(FetchPage {
            rows: entities,
            total,
        })
    }
}

/// Interpolate `@entity`/`@entity.*` string leaves in a fetch filter against
/// the ISSUING trait's context; the row-scoped `["object/get", "@entity", …]`
/// form is preserved verbatim (mirrors JS `interpolateFilterTraitRefs`).
fn interpolate_filter_trait_refs(value: &Value, ctx: &BindingContext) -> Value {
    match value {
        Value::String(s) => {
            if s == "@entity" || s.starts_with("@entity.") {
                ctx.resolve_binding(s).unwrap_or(Value::Null)
            } else if s.starts_with("@config.") {
                // A knob whose value IS an `@entity` binding (a list-detail pair's `initialFilterValue`)
                // reads the issuing trait's entity, not the candidate row the filter runs over.
                match ctx.resolve_binding(s) {
                    Some(Value::String(inner))
                        if inner == "@entity" || inner.starts_with("@entity.") =>
                    {
                        ctx.resolve_binding(&inner).unwrap_or(Value::Null)
                    }
                    _ => value.clone(),
                }
            } else {
                value.clone()
            }
        }
        Value::Array(arr) => {
            if arr.len() >= 2
                && arr[0] == Value::String("object/get".to_string())
                && arr[1] == Value::String("@entity".to_string())
            {
                return value.clone();
            }
            Value::Array(
                arr.iter()
                    .map(|v| interpolate_filter_trait_refs(v, ctx))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

/// Effect executor with environment-aware routing
pub struct EffectExecutor {
    /// Current runtime environment
    environment: RuntimeEnvironment,

    /// Event bus for emit effects
    event_bus: Option<SharedEventBus>,

    /// Persistence handler
    persistence: Option<Arc<dyn PersistenceHandler>>,

    /// Fetch handler (server-only)
    fetch_handler: Option<Arc<dyn FetchHandler>>,

    /// Service handler
    service_handler: Option<Arc<dyn ServiceHandler>>,
    substrate_handler: Option<Arc<dyn SubstrateHandler>>,

    /// Render handler (client-only)
    render_handler: Option<Arc<dyn RenderHandler>>,

    /// Navigation handler (client-only)
    navigation_handler: Option<Arc<dyn NavigationHandler>>,

    /// Game entity handler (client/game only)
    game_handler: Option<Arc<dyn GameEntityHandler>>,

    /// Streaming fetch handler (server-only)
    stream_fetch_handler: Option<Arc<dyn StreamFetchHandler>>,

    /// Delegate for cross-environment effects
    delegate: Option<Arc<dyn EffectDelegate>>,

    /// Schema-derived `@create`/`@update`/`@delete` directives, keyed by
    /// entity name. Checked in `do_persist` before every mutation — the
    /// un-bypassable twin of the read-side policy `FetchHandler`s apply via
    /// `apply_read_access`. `None` when no kernel has wired a schema yet.
    entity_policies: Option<Arc<RwLock<EntityAccessTable>>>,
}

impl Default for EffectExecutor {
    fn default() -> Self {
        Self::new(RuntimeEnvironment::Server)
    }
}

impl EffectExecutor {
    /// Create a new effect executor for the given environment
    pub fn new(environment: RuntimeEnvironment) -> Self {
        Self {
            environment,
            event_bus: None,
            persistence: None,
            fetch_handler: None,
            stream_fetch_handler: None,
            service_handler: None,
            substrate_handler: None,
            render_handler: None,
            navigation_handler: None,
            game_handler: None,
            delegate: None,
            entity_policies: None,
        }
    }

    /// Create a server-side executor
    pub fn server() -> Self {
        Self::new(RuntimeEnvironment::Server)
    }

    /// Create a client-side executor
    pub fn client() -> Self {
        Self::new(RuntimeEnvironment::Client)
    }

    /// Set the event bus
    pub fn with_event_bus(mut self, bus: SharedEventBus) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Set the persistence handler
    pub fn with_persistence(mut self, handler: Arc<dyn PersistenceHandler>) -> Self {
        self.persistence = Some(handler);
        self
    }

    /// Set the fetch handler
    pub fn with_fetch_handler(mut self, handler: Arc<dyn FetchHandler>) -> Self {
        self.fetch_handler = Some(handler);
        self
    }

    /// Set the streaming fetch handler
    pub fn with_stream_fetch_handler(mut self, handler: Arc<dyn StreamFetchHandler>) -> Self {
        self.stream_fetch_handler = Some(handler);
        self
    }

    /// Set the service handler
    pub fn with_substrate_handler(mut self, handler: Arc<dyn SubstrateHandler>) -> Self {
        self.substrate_handler = Some(handler);
        self
    }

    pub fn with_service_handler(mut self, handler: Arc<dyn ServiceHandler>) -> Self {
        self.service_handler = Some(handler);
        self
    }

    /// Set the render handler
    pub fn with_render_handler(mut self, handler: Arc<dyn RenderHandler>) -> Self {
        self.render_handler = Some(handler);
        self
    }

    /// Set the navigation handler
    pub fn with_navigation_handler(mut self, handler: Arc<dyn NavigationHandler>) -> Self {
        self.navigation_handler = Some(handler);
        self
    }

    /// Set the game entity handler
    pub fn with_game_handler(mut self, handler: Arc<dyn GameEntityHandler>) -> Self {
        self.game_handler = Some(handler);
        self
    }

    /// Set the delegate for cross-environment effects
    pub fn with_delegate(mut self, delegate: Arc<dyn EffectDelegate>) -> Self {
        self.delegate = Some(delegate);
        self
    }

    /// Set the schema-derived entity access policy table, shared with the
    /// kernel's `FetchHandler` so both the read and write sides check the
    /// same declared directives.
    pub fn with_entity_policies(mut self, table: Arc<RwLock<EntityAccessTable>>) -> Self {
        self.entity_policies = Some(table);
        self
    }

    /// Get the current environment
    pub fn environment(&self) -> RuntimeEnvironment {
        self.environment
    }

    /// The configured cross-environment delegate, if any. `RuntimeKernel::
    /// dispatch_with_server_leg` downcasts this (via `EffectDelegate::as_any`)
    /// to the concrete `ServerLegCollector` every client-role kernel installs.
    pub fn delegate(&self) -> Option<&Arc<dyn EffectDelegate>> {
        self.delegate.as_ref()
    }

    /// Execute an effect
    pub fn execute(&self, effect: &Effect, ctx: &BindingContext) -> EffectResult {
        if self.environment.is_client() && runs_on(effect.operator()) == RunsOn::Server {
            return self.delegate_to_server(effect.clone());
        }
        match effect {
            Effect::Emit { event, payload } => self.do_emit(event, payload),
            Effect::Set { path, value } => self.do_set(path, value, ctx),
            Effect::Persist {
                operation,
                entity_type,
                data,
                id,
                emit: _,
            } => self.do_persist(*operation, entity_type, data, id.as_deref(), ctx),
            Effect::PersistBatch { ops, emit: _ } => self.do_persist_batch(ops, ctx),
            Effect::Fetch {
                entity_type,
                id,
                filter,
                limit,
                offset,
                order_by,
                include,
                deliver,
                total,
                emit: _,
            } => self.do_fetch(
                entity_type,
                id.as_ref(),
                filter.as_ref(),
                limit.as_ref(),
                offset.as_ref(),
                order_by.as_ref(),
                include.as_ref(),
                *deliver,
                total.unwrap_or(RowTotalMode::Exact),
                ctx,
            ),
            Effect::CallService {
                service,
                method,
                args,
                key,
                ..
            } => self.do_call_service(service, method, args, key.as_deref(), ctx),
            Effect::Http { .. } => {
                EffectResult::noop(effect.clone(), "http is run by the kernel, which owns its host I/O")
            }
            Effect::CancelCall { key } => self.do_cancel_call(key),
            Effect::RandomBytes { .. } | Effect::CredentialStatus { .. } | Effect::CredentialForget { .. } => {
                EffectResult::noop(effect.clone(), "host effects are run by the kernel, which owns its host I/O")
            }
            Effect::RenderUi { slot, content } => self.do_render_ui(slot, content),
            Effect::Navigate {
                path,
                params,
                options,
            } => self.do_navigate(path, params.as_ref(), options.clone()),
            Effect::NavigateBack => self.do_navigate_back(),
            Effect::Spawn { entity_type, props } => self.do_spawn(entity_type, props),
            Effect::Despawn { entity_id } => self.do_despawn(entity_id),
            Effect::Log {
                level,
                message,
                data,
            } => self.do_log(*level, message, data.as_ref()),
            Effect::Custom { name, args, .. } => self.do_custom(name, args),
        }
    }

    /// Execute multiple effects from S-expressions
    pub fn execute_all(&self, effects: &[SExpression], ctx: &BindingContext) -> Vec<EffectResult> {
        effects
            .iter()
            .filter_map(super::types::parse_effect)
            .map(|effect| self.execute(&effect, ctx))
            .collect()
    }

    // ========================================================================
    // Effect Handlers
    // ========================================================================

    /// A `runsOn: server` effect on a client host: hand it to the delegate, or report it skipped.
    fn delegate_to_server(&self, effect: Effect) -> EffectResult {
        if let Some(delegate) = &self.delegate {
            return match delegate.delegate(&effect, RuntimeEnvironment::Server) {
                Ok(()) => EffectResult::delegated(effect, RuntimeEnvironment::Server),
                Err(e) => EffectResult::failed(effect, &e),
            };
        }
        let reason = format!("{} is server-only", effect.operator());
        EffectResult::noop(effect, &reason)
    }

    fn do_emit(&self, event: &str, payload: &Value) -> EffectResult {
        let effect = Effect::Emit {
            event: event.to_string(),
            payload: payload.clone(),
        };

        // Emit works in both environments
        if let Some(bus) = &self.event_bus {
            bus.emit(Event::new(event, payload.clone()));
            debug!(event = event, "Emitted event");
            EffectResult::executed(effect)
        } else {
            EffectResult::noop(effect, "no event bus configured")
        }
    }

    fn do_set(&self, path: &str, value: &Value, _ctx: &BindingContext) -> EffectResult {
        let effect = Effect::Set {
            path: path.to_string(),
            value: value.clone(),
        };

        // Set works in both environments (modifies context)
        // In practice, the state machine manager handles this
        debug!(path = path, "Set effect (handled by state machine)");
        EffectResult::executed(effect)
    }

    /// Resolve bindings in a value recursively.
    ///
    /// Returns a borrowed reference when no `@`-binding is present in the subtree,
    /// so binding-free payloads (the common case) are not deep-cloned.
    /// Resolve an entity id that may be an `@`-binding (e.g. `@payload.row.id`).
    /// Resolve an effect's `id` argument to a row id.
    ///
    /// An `@`-binding that does not resolve is an **error**, not the sigil. This
    /// used to `return raw.to_string()`, so an unresolved `@payload.id` became
    /// the lookup key itself and surfaced as `Entity not found in cache:
    /// Patient:@payload.id` — a missing-row error for what is really a binding
    /// bug, in the one place that should fail loud
    /// (`C-RESOLVE-ID-FALLS-THROUGH-TO-THE-RAW-SIGIL`).
    fn resolve_id(&self, id: Option<&str>, ctx: &BindingContext) -> Result<String, String> {
        let raw = id.unwrap_or("");
        if raw.starts_with('@') {
            let Some(v) = ctx.resolve_binding(raw) else {
                return Err(format!("unresolved binding '{raw}' used as an entity id"));
            };
            if let Some(s) = v.as_str() {
                return Ok(s.to_string());
            }
            if let Some(n) = v.as_number() {
                return Ok(if n.fract() == 0.0 {
                    (n as i64).to_string()
                } else {
                    n.to_string()
                });
            }
            return Err(format!(
                "binding '{raw}' used as an entity id resolved to {v:?}, which is not a string or number"
            ));
        }
        Ok(raw.to_string())
    }

    /// A fetch `id` expression, evaluated so it can never silently widen to a collection fetch.
    fn resolve_fetch_id(
        &self,
        id: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<Option<String>, String> {
        match id {
            None => Ok(None),
            Some(Value::String(raw)) => self.resolve_id(Some(raw), ctx).map(Some),
            Some(expr) => {
                let sexpr: SExpression = expr.clone().into();
                match Evaluator::new().evaluate(&sexpr, ctx).map_err(|e| e.to_string())? {
                    Value::String(s) => Ok(Some(s)),
                    Value::Number(n) if n.fract() == 0.0 => Ok(Some((n as i64).to_string())),
                    other => Err(format!(
                        "fetch id expression resolved to {other:?}, which is not a string or integer"
                    )),
                }
            }
        }
    }

    /// A fetch `limit`/`offset`, evaluated at execution; only a positive number applies (as `effect-stage.ts`).
    fn resolve_page_option(
        &self,
        value: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<Option<usize>, String> {
        let Some(value) = value else { return Ok(None) };
        let sexpr: SExpression = value.clone().into();
        match Evaluator::new()
            .evaluate(&sexpr, ctx)
            .map_err(|e| e.to_string())?
        {
            Value::Number(n) if n > 0.0 => Ok(Some(n as usize)),
            _ => Ok(None),
        }
    }

    /// A fetch's `orderBy` (`"field"` or `"field:asc|desc"`, an expression) as a [`RowOrder`].
    fn resolve_order_spec(
        &self,
        value: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<Option<RowOrder>, String> {
        let Some(value) = value else { return Ok(None) };
        let sexpr: SExpression = value.clone().into();
        match Evaluator::new()
            .evaluate(&sexpr, ctx)
            .map_err(|e| e.to_string())?
        {
            Value::String(spec) if !spec.is_empty() => {
                let (field, direction) = spec.split_once(':').unwrap_or((spec.as_str(), "asc"));
                Ok(Some(RowOrder {
                    field: field.to_string(),
                    direction: if direction == "desc" {
                        RowDirection::Desc
                    } else {
                        RowDirection::Asc
                    },
                }))
            }
            _ => Ok(None),
        }
    }

    /// Resolve every `@`-binding in a value — the canonical walker lives on
    /// `BindingContext::resolve_value`; this is a thin forwarder for in-executor callers.
    fn resolve_value_bindings<'v>(
        &self,
        value: &'v Value,
        ctx: &BindingContext,
    ) -> std::borrow::Cow<'v, Value> {
        ctx.resolve_value(value)
    }

    /// Resolve a `persist` effect's `data` argument for real, not just
    /// sigil-substitute it. `data` came from `parse_persist`, which converts
    /// the authored `SExpression` straight to `Value` — a CALL expression
    /// like `(object/merge ?data {field: @user.id})` survives that
    /// conversion as a literal `["object/merge", ...]` array, and
    /// `resolve_value_bindings`/`ctx.resolve_value` only walks the tree
    /// substituting leaf `@`-sigils; it never dispatches a call, so the
    /// merge itself silently never ran (C-PERSIST-DATA-CALL-EXPR-NEVER-
    /// EVALUATED — invisible until an access-policy check read a field back
    /// out of the "merged" row and found the raw call array instead).
    /// Round-tripping through the full evaluator dispatches calls AND
    /// substitutes bindings identically to the old path for plain literal
    /// data, so this is a strict superset, not a behavior change for the
    /// common case. Evaluation failure falls back to the old plain
    /// substitution rather than hard-failing the persist.
    fn resolve_persist_data<'v>(
        &self,
        data: &'v Value,
        ctx: &BindingContext,
    ) -> std::borrow::Cow<'v, Value> {
        let expr: SExpression = data.clone().into();
        match Evaluator::new().evaluate(&expr, ctx) {
            Ok(evaluated) => std::borrow::Cow::Owned(evaluated),
            Err(err) => {
                warn!(error = %err, "persist data failed to evaluate; falling back to plain binding substitution");
                self.resolve_value_bindings(data, ctx)
            }
        }
    }

    /// Resolve the target row id for `update` — the effect's own `id` arg
    /// when the author supplied one (`(persist update Entity id data)`), else
    /// the already-resolved data's own `id` field (`(persist update Entity
    /// data)` — "the data carries its own id", the dominant corpus idiom).
    /// Mirrors the interpreted path's identical fallback
    /// (`OrbitalServerRuntime.ts`'s persist handler: `data?.id || entityId`).
    fn resolve_update_id(
        &self,
        id: Option<&str>,
        resolved_data: &Value,
        ctx: &BindingContext,
    ) -> Result<String, String> {
        let from_id_arg = self.resolve_id(id, ctx)?;
        if !from_id_arg.is_empty() {
            return Ok(from_id_arg);
        }
        let from_data = resolved_data
            .as_object()
            .and_then(|o| o.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if from_data.is_empty() {
            // FATAL. Re-measured and gated clean — a persist update/delete with
            // no resolvable row key now fails the write instead of silently
            // no-opping it.
            const NO_ROW_KEY_IS_FATAL: bool = true;
            tracing::warn!(
                "persist update/delete resolved NO ROW KEY — this write will not land. \
                 Bind it before the write, e.g. `(set @entity.id ?row.id)` on the \
                 transition that selects the row."
            );
            if !NO_ROW_KEY_IS_FATAL {
                return Ok(String::new());
            }
            // A write with NO row key is a failed write, not a write to the row
            // keyed by "". Returning `Ok(String::new())` sent an empty key
            // through to `persistence.update`, and on the JS path the same
            // condition skipped the update entirely while still reporting
            // `success: true` — two paths, two different wrong answers, both
            // silent. `ORB_BINDING_PERSIST_ROW_ID_NEVER_SET` is the compile-time
            // half of this; this is the runtime half.
            return Err(NO_ROW_KEY.to_string());
        }
        Ok(from_data.to_string())
    }

    fn do_persist(
        &self,
        operation: PersistOperation,
        entity_type: &str,
        data: &Value,
        id: Option<&str>,
        ctx: &BindingContext,
    ) -> EffectResult {
        let effect = Effect::Persist {
            operation,
            entity_type: entity_type.to_string(),
            data: data.clone(),
            id: id.map(|s| s.to_string()),
            emit: None,
        };

        if self.persistence.is_some() {
            // Resolve bindings in data before persisting
            let resolved_data = self.resolve_persist_data(data, ctx);
            self.persist_resolved(
                operation,
                entity_type,
                resolved_data.as_ref(),
                id,
                ctx,
                effect,
            )
        } else {
            EffectResult::noop(effect, "no persistence handler configured")
        }
    }

    /// The row a persist update/delete addresses. An absent row is a failed write coded
    /// `not-found`; a store failure is a plain failure.
    fn existing_row(
        persistence: &dyn PersistenceHandler,
        entity_type: &str,
        entity_id: &str,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        match persistence.find(entity_type, entity_id)? {
            Some(row) => Ok(row),
            None => {
                *code = Some(ServerEffectCode::NotFound);
                Err(format!("{entity_type} {entity_id} not found"))
            }
        }
    }

    /// The access directive, required columns and `@intrinsic` fields that govern one persist.
    fn persist_scope(&self, entity_type: &str, operation: PersistOperation) -> PersistScope {
        let Some(table) = self.entity_policies.as_ref() else {
            return PersistScope::default();
        };
        let table = table.read();
        let Some(policies) = table.get(entity_type) else {
            return PersistScope::default();
        };
        PersistScope {
            policy: match operation {
                PersistOperation::Create | PersistOperation::Upsert => policies.create.clone(),
                PersistOperation::Update => policies.update.clone(),
                PersistOperation::Delete => policies.delete.clone(),
                PersistOperation::Read => None,
            },
            required_fields: policies.required_create_fields.clone(),
            declared_required: policies.required_fields.clone(),
            intrinsic: policies.intrinsic_fields.clone(),
        }
    }

    /// `@intrinsic` fields are the trait's frame, never a column: a write leaves them out.
    fn without_intrinsic(resolved_data: &Value, intrinsic: &[String]) -> Value {
        match resolved_data {
            Value::Object(fields) if fields.keys().any(|k| intrinsic.contains(k)) => Value::Object(
                fields
                    .iter()
                    .filter(|(k, _)| !intrinsic.contains(k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// The checks of a `persist create` that read only the incoming data: required columns, then the
    /// `@create` directive.
    fn check_create(
        entity_type: &str,
        data: &Value,
        scope: &PersistScope,
        ctx: &BindingContext,
    ) -> Result<(), String> {
        let missing = missing_required_fields(&scope.required_fields, data);
        if !missing.is_empty() {
            // Distinct wording from `check_mutation_access`'s denial message
            // (`is_access_denied_error` matches on " denied: the declared
            // access policy for ") so this is never mis-bucketed as one.
            return Err(format!(
                "persist create {entity_type}: required field(s) {} missing",
                missing.join(", ")
            ));
        }
        check_mutation_access(
            PersistOperation::Create,
            data,
            scope.policy.as_ref(),
            ctx,
            entity_type,
        )
    }

    /// What one write outcome means as a persist result. A guarded write must carry the pre-image it was
    /// tested on, and the guard must hold on it: a host that ignored the guard fails loudly here.
    fn outcome_value(
        outcome: RowWriteOutcome,
        operation: PersistOperation,
        entity_type: &str,
        entity_id: &str,
        guard: Option<&RowPredicate>,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        if let Some(guard) = guard {
            if let RowWriteOutcome::Updated { before, .. } | RowWriteOutcome::Deleted { before } =
                &outcome
            {
                if !before.as_ref().is_some_and(|b| guard.matches(b)) {
                    error!(
                        entity_type = entity_type,
                        id = entity_id,
                        "the host applied a guarded write its guard rejects, or returned no pre-image"
                    );
                    return Err(format!(
                        "{entity_type} {entity_id}: the host applied a write whose guard does not hold on the row it replaced"
                    ));
                }
            }
        }
        match (outcome, operation) {
            (RowWriteOutcome::Created { row }, PersistOperation::Create | PersistOperation::Upsert)
            | (RowWriteOutcome::Updated { row, .. }, PersistOperation::Update | PersistOperation::Upsert) => Ok(row),
            (RowWriteOutcome::DuplicateId, _) => {
                *code = Some(ServerEffectCode::DuplicateId);
                Err(format!("{entity_type} {entity_id} already exists"))
            }
            (RowWriteOutcome::Deleted { .. }, PersistOperation::Delete) => Ok(Value::Null),
            (RowWriteOutcome::NotFound, _) => {
                *code = Some(ServerEffectCode::NotFound);
                Err(format!("{entity_type} {entity_id} not found"))
            }
            (RowWriteOutcome::Refused, _) => Err(format!(
                "@{operation:?} denied: the declared access policy for `{entity_type}` rejected this row"
            )),
            (RowWriteOutcome::Failed { message }, _) => Err(message),
            (RowWriteOutcome::RolledBack, _) => Err(format!(
                "{entity_type}: the write was rolled back with its batch"
            )),
            (other, _) => Err(format!(
                "{entity_type}: the host answered a {operation:?} with {other:?}"
            )),
        }
    }

    /// The guard of an `@update`/`@delete` directive as a predicate; `None` when no directive is declared.
    /// `Err` = the directive is not expressible or does not specialise: the caller takes the read path, which
    /// words the denial.
    fn write_guard(
        policy: Option<&Value>,
        ctx: &BindingContext,
        declared_required: &[String],
    ) -> Result<Option<RowPredicate>, ()> {
        match policy {
            None => Ok(None),
            Some(policy) => compile_predicate(policy, ctx, PredicateRole::Filter, &|f| {
                f == "id" || declared_required.iter().any(|r| r == f)
            })
            .map(Some)
            .map_err(|_| ()),
        }
    }

    /// One `persist` the store may run as a conditional write: `None` = take the read-then-write path.
    #[allow(clippy::too_many_arguments)]
    fn pushed_write(
        &self,
        persistence: &dyn PersistenceHandler,
        operation: PersistOperation,
        entity_type: &str,
        entity_id: &str,
        data: &Value,
        policy: Option<&Value>,
        declared_required: &[String],
        ctx: &BindingContext,
        code: &mut Option<ServerEffectCode>,
    ) -> Option<Result<Value, String>> {
        let guard = Self::write_guard(policy, ctx, declared_required).ok()?;
        let op = match operation {
            PersistOperation::Create | PersistOperation::Upsert | PersistOperation::Read => {
                return None
            }
            PersistOperation::Update => RowWriteOp::Update {
                entity_type: entity_type.to_string(),
                id: entity_id.to_string(),
                data: data.clone(),
                guard: guard.clone(),
            },
            PersistOperation::Delete => RowWriteOp::Delete {
                entity_type: entity_type.to_string(),
                id: entity_id.to_string(),
                guard: guard.clone(),
            },
        };
        let outcome = match persistence.write_where(&op)? {
            Ok(outcome) => outcome,
            Err(e) => return Some(Err(e)),
        };
        Some(Self::outcome_value(
            outcome,
            operation,
            entity_type,
            entity_id,
            guard.as_ref(),
            code,
        ))
    }

    /// The string id an incoming row names, when it names one.
    fn row_id_of(data: &Value) -> Option<&str> {
        data.as_object()
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }

    /// A `persist create` that is not pushed down: a row naming a taken id fails `duplicate-id` (EXEC-STEP-8);
    /// a row naming none is minted an id by the store.
    fn create_row(
        &self,
        persistence: &dyn PersistenceHandler,
        entity_type: &str,
        data: &Value,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        if let Some(id) = Self::row_id_of(data) {
            let op = RowWriteOp::Create {
                entity_type: entity_type.to_string(),
                data: data.clone(),
            };
            if let Some(written) = persistence.write_where(&op) {
                return written.and_then(|outcome| {
                    Self::outcome_value(
                        outcome,
                        PersistOperation::Create,
                        entity_type,
                        id,
                        None,
                        code,
                    )
                });
            }
            if persistence.find(entity_type, id)?.is_some() {
                *code = Some(ServerEffectCode::DuplicateId);
                return Err(format!("{entity_type} {id} already exists"));
            }
        }
        persistence.create(entity_type, data)
    }

    /// A `persist upsert` that is not pushed as part of a run: the `@update` directive (when declared) is
    /// tested on the row it replaces, then the store creates or replaces the row by id (EXEC-STEP-8).
    fn upsert_row(
        &self,
        persistence: &dyn PersistenceHandler,
        entity_type: &str,
        data: &Value,
        ctx: &BindingContext,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        let Some(id) = Self::row_id_of(data) else {
            *code = Some(ServerEffectCode::NoRowKey);
            return Err(format!("persist upsert {entity_type}: the row names no id"));
        };
        let update_policy = self
            .persist_scope(entity_type, PersistOperation::Update)
            .policy;
        let existing = if update_policy.is_some() {
            persistence.find(entity_type, id)?
        } else {
            None
        };
        if let Some(existing) = &existing {
            check_mutation_access(
                PersistOperation::Update,
                existing,
                update_policy.as_ref(),
                ctx,
                entity_type,
            )?;
        }
        let op = RowWriteOp::Upsert {
            entity_type: entity_type.to_string(),
            data: data.clone(),
        };
        if let Some(written) = persistence.write_where(&op) {
            return written.and_then(|outcome| {
                Self::outcome_value(outcome, PersistOperation::Upsert, entity_type, id, None, code)
            });
        }
        let existing = match existing {
            Some(row) => Some(row),
            None => persistence.find(entity_type, id)?,
        };
        let Some(existing) = existing else {
            return persistence.create(entity_type, data);
        };
        let mut row = data.as_object().cloned().unwrap_or_default();
        if let Some(created_at) = existing.as_object().and_then(|o| o.get("createdAt")) {
            row.insert("createdAt".to_string(), created_at.clone());
        }
        persistence.delete(entity_type, id)?;
        persistence.create(entity_type, &Value::Object(row))
    }

    /// One persist whose `data` is already evaluated: the access-policy gate,
    /// required-column check and store write shared by the single forms and each
    /// op of a `persist batch`.
    fn persist_resolved(
        &self,
        operation: PersistOperation,
        entity_type: &str,
        resolved_data: &Value,
        id: Option<&str>,
        ctx: &BindingContext,
        effect: Effect,
    ) -> EffectResult {
        if let Some(persistence) = &self.persistence {
            let scope = self.persist_scope(entity_type, operation);
            let data = Self::without_intrinsic(resolved_data, &scope.intrinsic);
            let resolved_data = &data;
            let policy = scope.policy.as_ref();

            let mut code: Option<ServerEffectCode> = None;
            let result = match operation {
                PersistOperation::Create => {
                    match Self::check_create(entity_type, resolved_data, &scope, ctx) {
                        Err(denial) => Err(denial),
                        Ok(()) => self.create_row(
                            persistence.as_ref(),
                            entity_type,
                            resolved_data,
                            &mut code,
                        ),
                    }
                }
                PersistOperation::Upsert => {
                    match Self::check_create(entity_type, resolved_data, &scope, ctx) {
                        Err(denial) => Err(denial),
                        Ok(()) => self.upsert_row(
                            persistence.as_ref(),
                            entity_type,
                            resolved_data,
                            ctx,
                            &mut code,
                        ),
                    }
                }
                PersistOperation::Read => match self.resolve_id(id, ctx) {
                    Err(e) => Err(e),
                    Ok(entity_id) => persistence.read(entity_type, entity_id.as_str()),
                },
                PersistOperation::Update => match self.resolve_update_id(id, resolved_data, ctx) {
                    Err(e) => {
                        code = Some(ServerEffectCode::NoRowKey);
                        Err(e)
                    }
                    Ok(entity_id) => match self.pushed_write(
                        persistence.as_ref(),
                        operation,
                        entity_type,
                        &entity_id,
                        resolved_data,
                        policy,
                        &scope.declared_required,
                        ctx,
                        &mut code,
                    ) {
                        Some(pushed) => pushed,
                        None => Self::existing_row(
                            persistence.as_ref(),
                            entity_type,
                            &entity_id,
                            &mut code,
                        )
                        .and_then(|existing| {
                            if policy.is_some() {
                                check_mutation_access(
                                    operation,
                                    &existing,
                                    policy,
                                    ctx,
                                    entity_type,
                                )?;
                            }
                            persistence.update(entity_type, &entity_id, resolved_data)
                        }),
                    },
                },
                PersistOperation::Delete => match self.resolve_id(id, ctx) {
                    Err(e) => {
                        code = Some(ServerEffectCode::NoRowKey);
                        Err(e)
                    }
                    Ok(entity_id) if entity_id.is_empty() => {
                        code = Some(ServerEffectCode::NoRowKey);
                        Err(NO_ROW_KEY.to_string())
                    }
                    Ok(entity_id) => match self.pushed_write(
                        persistence.as_ref(),
                        operation,
                        entity_type,
                        &entity_id,
                        resolved_data,
                        policy,
                        &scope.declared_required,
                        ctx,
                        &mut code,
                    ) {
                        Some(pushed) => pushed,
                        None => Self::existing_row(
                            persistence.as_ref(),
                            entity_type,
                            &entity_id,
                            &mut code,
                        )
                        .and_then(|existing| {
                            if policy.is_some() {
                                check_mutation_access(
                                    operation,
                                    &existing,
                                    policy,
                                    ctx,
                                    entity_type,
                                )?;
                            }
                            persistence
                                .delete(entity_type, &entity_id)
                                .map(|()| Value::Null)
                        }),
                    },
                },
            };

            match result {
                Ok(value) => {
                    info!(entity_type = entity_type, operation = ?operation, "Persist effect executed");
                    EffectResult::executed_with_value(effect, value)
                }
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Persist effect failed");
                    let failed = EffectResult::failed(effect, &e);
                    match code {
                        Some(code) => failed.with_code(code),
                        None => failed,
                    }
                }
            }
        } else {
            EffectResult::noop(effect, "no persistence handler configured")
        }
    }

    /// Decide how one evaluated persist is sent to the store: as a conditional write (`Push`), refused here
    /// before any I/O (`Refused`, the failed result), or through the read-then-write path (`Legacy`).
    fn prepare_push(
        &self,
        operation: PersistOperation,
        entity_type: &str,
        resolved_data: &Value,
        id: Option<&str>,
        ctx: &BindingContext,
        effect: Effect,
    ) -> PersistPrep {
        if self.persistence.is_none() {
            return PersistPrep::Legacy;
        }
        let scope = self.persist_scope(entity_type, operation);
        let data = Self::without_intrinsic(resolved_data, &scope.intrinsic);
        let refuse = |effect: Effect, e: String, code: Option<ServerEffectCode>| {
            let failed = EffectResult::failed(effect, &e);
            PersistPrep::Refused(match code {
                Some(code) => failed.with_code(code),
                None => failed,
            })
        };
        let (op, row_id, guard) = match operation {
            PersistOperation::Read => return PersistPrep::Legacy,
            PersistOperation::Create | PersistOperation::Upsert => {
                let Some(row_id) = Self::row_id_of(&data).map(str::to_string) else {
                    return PersistPrep::Legacy;
                };
                if operation == PersistOperation::Upsert
                    && self
                        .persist_scope(entity_type, PersistOperation::Update)
                        .policy
                        .is_some()
                {
                    return PersistPrep::Legacy;
                }
                if let Err(denial) = Self::check_create(entity_type, &data, &scope, ctx) {
                    return refuse(effect, denial, None);
                }
                let op = if operation == PersistOperation::Create {
                    RowWriteOp::Create {
                        entity_type: entity_type.to_string(),
                        data: data.clone(),
                    }
                } else {
                    RowWriteOp::Upsert {
                        entity_type: entity_type.to_string(),
                        data: data.clone(),
                    }
                };
                (op, row_id, None)
            }
            PersistOperation::Update | PersistOperation::Delete => {
                let row_id = if operation == PersistOperation::Update {
                    self.resolve_update_id(id, &data, ctx)
                } else {
                    self.resolve_id(id, ctx).and_then(|row_id| {
                        if row_id.is_empty() {
                            Err(NO_ROW_KEY.to_string())
                        } else {
                            Ok(row_id)
                        }
                    })
                };
                let row_id = match row_id {
                    Ok(row_id) => row_id,
                    Err(e) => return refuse(effect, e, Some(ServerEffectCode::NoRowKey)),
                };
                let Ok(guard) =
                    Self::write_guard(scope.policy.as_ref(), ctx, &scope.declared_required)
                else {
                    return PersistPrep::Legacy;
                };
                let op = if operation == PersistOperation::Update {
                    RowWriteOp::Update {
                        entity_type: entity_type.to_string(),
                        id: row_id.clone(),
                        data: data.clone(),
                        guard: guard.clone(),
                    }
                } else {
                    RowWriteOp::Delete {
                        entity_type: entity_type.to_string(),
                        id: row_id.clone(),
                        guard: guard.clone(),
                    }
                };
                (op, row_id, guard)
            }
        };
        PersistPrep::Push(Box::new(PreparedWrite {
            effect,
            operation,
            entity_type: entity_type.to_string(),
            id: id.map(str::to_string),
            resolved_data: resolved_data.clone(),
            row_id,
            op,
            guard,
        }))
    }

    /// The persist result of the outcome the store answered for a prepared write.
    fn finish_push(prepared: &PreparedWrite, outcome: RowWriteOutcome) -> EffectResult {
        let mut code: Option<ServerEffectCode> = None;
        match Self::outcome_value(
            outcome,
            prepared.operation,
            &prepared.entity_type,
            &prepared.row_id,
            prepared.guard.as_ref(),
            &mut code,
        ) {
            Ok(value) => {
                info!(entity_type = %prepared.entity_type, operation = ?prepared.operation, "Persist effect executed");
                EffectResult::executed_with_value(prepared.effect.clone(), value)
            }
            Err(e) => {
                error!(entity_type = %prepared.entity_type, error = %e, "Persist effect failed");
                let failed = EffectResult::failed(prepared.effect.clone(), &e);
                match code {
                    Some(code) => failed.with_code(code),
                    None => failed,
                }
            }
        }
    }

    /// Run consecutive top-level `persist` effects of one transition as a write run. Plain persists the
    /// store can run as conditional writes go out as ONE `stop` batch; a persist refused here before any I/O,
    /// or one the store cannot push, takes its own path in place. A persist failure is a handled outcome (EXEC-STEP-7),
    /// so after a failure the rest of the run is sent again as a new request: the effects run exactly as they
    /// would one at a time. Results come back in effect order.
    pub fn execute_persist_run(
        &self,
        effects: &[Effect],
        ctx: &BindingContext,
    ) -> Vec<EffectResult> {
        if self.environment.is_client() || self.persistence.is_none() {
            return effects.iter().map(|e| self.execute(e, ctx)).collect();
        }
        let mut out: Vec<Option<EffectResult>> = vec![None; effects.len()];
        let mut pending: Vec<(usize, PreparedWrite)> = Vec::new();
        for (k, effect) in effects.iter().enumerate() {
            let Effect::Persist {
                operation,
                entity_type,
                data,
                id,
                ..
            } = effect
            else {
                self.flush_writes(&mut pending, &mut out, ctx);
                out[k] = Some(self.execute(effect, ctx));
                continue;
            };
            let resolved = self.resolve_persist_data(data, ctx);
            let mut result_effect = effect.clone();
            if let Effect::Persist { emit, .. } = &mut result_effect {
                *emit = None;
            }
            match self.prepare_push(
                *operation,
                entity_type,
                resolved.as_ref(),
                id.as_deref(),
                ctx,
                result_effect.clone(),
            ) {
                PersistPrep::Push(prepared) => pending.push((k, *prepared)),
                PersistPrep::Refused(result) => out[k] = Some(result),
                PersistPrep::Legacy => {
                    self.flush_writes(&mut pending, &mut out, ctx);
                    out[k] = Some(self.persist_resolved(
                        *operation,
                        entity_type,
                        resolved.as_ref(),
                        id.as_deref(),
                        ctx,
                        result_effect,
                    ));
                }
            }
        }
        self.flush_writes(&mut pending, &mut out, ctx);
        out.into_iter()
            .zip(effects)
            .map(|(result, effect)| {
                result.unwrap_or_else(|| {
                    EffectResult::failed(effect.clone(), "the persist was never run")
                })
            })
            .collect()
    }

    /// Send the pending prepared writes; see [`Self::execute_persist_run`].
    fn flush_writes(
        &self,
        pending: &mut Vec<(usize, PreparedWrite)>,
        out: &mut [Option<EffectResult>],
        ctx: &BindingContext,
    ) {
        let Some(persistence) = &self.persistence else {
            return;
        };
        while !pending.is_empty() {
            let batch = RowWriteBatch {
                ops: pending.iter().map(|(_, p)| p.op.clone()).collect(),
                on_failure: RowWriteFailureMode::Stop,
            };
            match persistence.write_batch(&batch) {
                None => {
                    for (k, p) in pending.drain(..) {
                        out[k] = Some(self.persist_resolved(
                            p.operation,
                            &p.entity_type,
                            &p.resolved_data,
                            p.id.as_deref(),
                            ctx,
                            p.effect.clone(),
                        ));
                    }
                }
                Some(Err(e)) => {
                    for (k, p) in pending.drain(..) {
                        error!(entity_type = %p.entity_type, error = %e, "Persist effect failed");
                        out[k] = Some(EffectResult::failed(p.effect, &e));
                    }
                }
                Some(Ok(outcomes)) if outcomes.is_empty() => {
                    for (k, p) in pending.drain(..) {
                        let e = "the host answered a write run with no outcome";
                        out[k] = Some(EffectResult::failed(p.effect, e));
                    }
                }
                Some(Ok(outcomes)) => {
                    let attempted = outcomes.len().min(pending.len());
                    for ((k, p), outcome) in pending.drain(..attempted).zip(outcomes) {
                        out[k] = Some(Self::finish_push(&p, outcome));
                    }
                }
            }
        }
    }

    /// `persist batch`: evaluate `ops` to an array, check every op's shape and local gates (required column,
    /// `@create`, row key) before anything is written, then run the ops in order. A store that runs conditional
    /// batches applies them ALL or NONE; on a failure it rolls the earlier ops back. A store that cannot takes
    /// each op through [`Self::persist_resolved`] and stops at the first failure with earlier ops written.
    /// Success returns `{ operations, completedCount, totalCount }`, the shape
    /// `emit_contract::PERSIST_BATCH_FIELDS` declares.
    fn do_persist_batch(&self, ops: &Value, ctx: &BindingContext) -> EffectResult {
        let effect = Effect::PersistBatch {
            ops: ops.clone(),
            emit: None,
        };
        let Some(persistence) = self.persistence.clone() else {
            return EffectResult::noop(effect, "no persistence handler configured");
        };
        let expr: SExpression = ops.clone().into();
        let evaluated = match Evaluator::new().evaluate(&expr, ctx) {
            Ok(v) => v,
            Err(err) => {
                return EffectResult::failed(
                    effect,
                    &format!("persist batch operations failed to evaluate: {err}"),
                )
            }
        };
        let Value::Array(items) = evaluated else {
            return EffectResult::failed(
                effect,
                "persist batch operations must evaluate to an array",
            );
        };
        if items.is_empty() {
            return EffectResult::failed(effect, "Batch requires a non-empty operations array");
        }
        let total = items.len();
        let mut parsed: Vec<BatchOp> = Vec::with_capacity(total);
        for item in &items {
            let tuple = match item {
                Value::Array(t) => t.as_slice(),
                other => {
                    return EffectResult::failed(
                        effect,
                        &format!("Invalid batch operation format: {other:?}"),
                    )
                }
            };
            let action = tuple.first().and_then(Value::as_str).unwrap_or("");
            let entity = tuple.get(1).and_then(Value::as_str).unwrap_or("");
            let arity = match action {
                "create" | "delete" => 3,
                "update" => 4,
                _ => {
                    return EffectResult::failed(
                        effect,
                        &format!("Unknown batch operation action: {action}"),
                    )
                }
            };
            if tuple.len() != arity || entity.is_empty() {
                return EffectResult::failed(
                    effect,
                    &format!("Invalid batch operation format: {item:?}"),
                );
            }
            let (operation, row_id, data): (PersistOperation, Option<String>, Value) = match action
            {
                "create" => (PersistOperation::Create, None, tuple[2].clone()),
                "update" => (
                    PersistOperation::Update,
                    tuple[2].as_str().map(str::to_string),
                    tuple[3].clone(),
                ),
                _ => (
                    PersistOperation::Delete,
                    tuple[2].as_str().map(str::to_string),
                    Value::Null,
                ),
            };
            parsed.push(BatchOp {
                action: action.to_string(),
                entity: entity.to_string(),
                operation,
                row_id,
                data,
            });
        }

        let op_effect = |b: &BatchOp| Effect::Persist {
            operation: b.operation,
            entity_type: b.entity.clone(),
            data: b.data.clone(),
            id: b.row_id.clone(),
            emit: None,
        };
        let batch_failure = |b: &BatchOp, detail: &str| {
            EffectResult::failed(
                effect.clone(),
                &format!(
                    "Batch operation [{}, {}] failed: {detail}",
                    b.action, b.entity
                ),
            )
        };

        let mut prepared: Vec<PreparedWrite> = Vec::with_capacity(total);
        let mut pushable = true;
        for b in parsed.iter().filter(|_| pushable) {
            match self.prepare_push(
                b.operation,
                &b.entity,
                &b.data,
                b.row_id.as_deref(),
                ctx,
                op_effect(b),
            ) {
                PersistPrep::Push(p) => prepared.push(*p),
                PersistPrep::Refused(refused) => {
                    let detail = refused.error.unwrap_or_else(|| "not executed".to_string());
                    return batch_failure(b, &detail);
                }
                PersistPrep::Legacy => {
                    pushable = false;
                    break;
                }
            }
        }

        let mut completed: Vec<Value> = Vec::with_capacity(total);
        if pushable {
            let batch = RowWriteBatch {
                ops: prepared.iter().map(|p| p.op.clone()).collect(),
                on_failure: RowWriteFailureMode::Atomic,
            };
            match persistence.write_batch(&batch) {
                Some(Err(e)) => {
                    return EffectResult::failed(effect, &format!("persist batch failed: {e}"))
                }
                Some(Ok(outcomes)) => {
                    let mut seen = 0;
                    for ((b, p), outcome) in parsed.iter().zip(&prepared).zip(outcomes) {
                        seen += 1;
                        if outcome == RowWriteOutcome::RolledBack {
                            continue;
                        }
                        let result = Self::finish_push(p, outcome);
                        if !result.is_executed() {
                            let detail = result.error.unwrap_or_else(|| "not executed".to_string());
                            return batch_failure(b, &detail);
                        }
                        completed.push(batch_entry(b, &result));
                    }
                    if seen < total {
                        return EffectResult::failed(
                            effect,
                            "persist batch failed: the host answered fewer outcomes than operations",
                        );
                    }
                    return batch_summary(effect, completed, total);
                }
                None => {}
            }
        }

        // The store cannot run the batch atomically: each applied op keeps its pre-image so a later failure can
        // compensate. Not isolation: a concurrent reader may see the intermediate rows (D-A22).
        let mut applied: Vec<(usize, Option<Value>, Option<String>)> = Vec::new();
        for (k, b) in parsed.iter().enumerate() {
            let pre = match (b.operation, b.row_id.as_deref()) {
                (PersistOperation::Update | PersistOperation::Delete, Some(id)) => {
                    persistence.find(&b.entity, id).ok().flatten()
                }
                _ => None,
            };
            let result = self.persist_resolved(
                b.operation,
                &b.entity,
                &b.data,
                b.row_id.as_deref(),
                ctx,
                op_effect(b),
            );
            if !result.is_executed() {
                let detail = result.error.unwrap_or_else(|| "not executed".to_string());
                if !applied.is_empty() {
                    warn!(
                        applied = applied.len(),
                        "kernel.write.batch-compensated: restoring the pre-images of the ops applied before the failure"
                    );
                    for (done, pre, created) in applied.iter().rev() {
                        let op = &parsed[*done];
                        let restored = match (op.operation, pre, created) {
                            (PersistOperation::Create, _, Some(id)) => {
                                persistence.delete(&op.entity, id)
                            }
                            (PersistOperation::Update, Some(row), _) => {
                                let mut restore = row.as_object().cloned().unwrap_or_default();
                                if let Some(data) = op.data.as_object() {
                                    for key in data.keys() {
                                        restore.entry(key.clone()).or_insert(Value::Null);
                                    }
                                }
                                persistence
                                    .update(
                                        &op.entity,
                                        op.row_id.as_deref().unwrap_or(""),
                                        &Value::Object(restore),
                                    )
                                    .map(|_| ())
                            }
                            (PersistOperation::Delete, Some(row), _) => {
                                persistence.create(&op.entity, row).map(|_| ())
                            }
                            _ => Ok(()),
                        };
                        if let Err(e) = restored {
                            error!(entity_type = %op.entity, error = %e, "persist batch compensation failed");
                        }
                    }
                }
                return batch_failure(b, &detail);
            }
            let created = match (&result.return_value, b.operation) {
                (Some(Value::Object(row)), PersistOperation::Create) => {
                    row.get("id").and_then(Value::as_str).map(str::to_string)
                }
                _ => None,
            };
            applied.push((k, pre, created));
            completed.push(batch_entry(b, &result));
        }
        batch_summary(effect, completed, total)
    }

    #[allow(clippy::too_many_arguments)] // mirrors FetchHandler::fetch query params
    fn do_fetch(
        &self,
        entity_type: &str,
        id: Option<&Value>,
        filter: Option<&Value>,
        limit: Option<&Value>,
        offset: Option<&Value>,
        order_by: Option<&Value>,
        include: Option<&Vec<String>>,
        deliver: FetchDeliver,
        total_mode: RowTotalMode,
        ctx: &BindingContext,
    ) -> EffectResult {
        let effect = Effect::Fetch {
            entity_type: entity_type.to_string(),
            id: id.cloned(),
            filter: filter.cloned(),
            limit: limit.cloned(),
            offset: offset.cloned(),
            order_by: order_by.cloned(),
            include: include.cloned(),
            deliver,
            total: (total_mode == RowTotalMode::None).then_some(RowTotalMode::None),
            emit: None,
        };
        let counted = |total: usize| (total_mode == RowTotalMode::Exact).then_some(total);

        if let Some(fetch_handler) = &self.fetch_handler {
            let resolved = self.resolve_fetch_id(id, ctx).and_then(|id| {
                Ok((
                    id,
                    self.resolve_page_option(limit, ctx)?,
                    self.resolve_page_option(offset, ctx)?,
                ))
            });
            let order = match self.resolve_order_spec(order_by, ctx) {
                Ok(order) => order,
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    return EffectResult::failed(effect, &e);
                }
            };
            let (resolved_id, limit, offset) = match resolved {
                Ok(v) => v,
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    return EffectResult::failed(effect, &e);
                }
            };
            let interpolated_filter = filter.map(|f| interpolate_filter_trait_refs(f, ctx));
            if deliver == FetchDeliver::Ids {
                if resolved_id.is_some() || include.is_some_and(|i| !i.is_empty()) {
                    let e = format!(
                        "{entity_type}: `deliver: ids` applies to a collection fetch with no `id` or `include`"
                    );
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    return EffectResult::failed(effect, &e);
                }
                return match fetch_handler.fetch_row_set(
                    entity_type,
                    interpolated_filter.as_ref(),
                    order.as_ref(),
                    limit,
                    offset,
                    total_mode,
                    ctx,
                ) {
                    Ok(reference) => {
                        info!(
                            entity_type = entity_type,
                            count = reference.ids.len(),
                            total = reference.total,
                            "Fetch effect delivered by reference"
                        );
                        EffectResult::executed_row_set(effect, &reference, counted(reference.total))
                    }
                    Err(e) => {
                        error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                        EffectResult::failed(effect, &e)
                    }
                };
            }
            let fetched = fetch_handler.fetch_ordered(
                entity_type,
                resolved_id.as_deref(),
                interpolated_filter.as_ref(),
                order.as_ref(),
                limit,
                offset,
                include,
                total_mode,
                ctx,
            );
            match fetched {
                // A by-id miss (absent or `@read`-refused alike) is a failure, as `EffectExecutor.ts`.
                Ok(page) if page.rows.is_empty() && resolved_id.is_some() => {
                    let e = format!(
                        "{entity_type} {} not found",
                        resolved_id.as_deref().unwrap_or_default()
                    );
                    EffectResult::failed(effect, &e)
                }
                Ok(FetchPage { rows, total }) => {
                    info!(
                        entity_type = entity_type,
                        count = rows.len(),
                        total,
                        "Fetch effect executed"
                    );
                    EffectResult::executed_fetch(effect, Value::Array(rows), entity_type, counted(total))
                }
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    EffectResult::failed(effect, &e)
                }
            }
        } else {
            EffectResult::noop(effect, "no fetch handler configured")
        }
    }

    fn do_call_service(
        &self,
        service: &str,
        method: &str,
        args: &Value,
        key: Option<&str>,
        ctx: &BindingContext,
    ) -> EffectResult {
        let effect = Effect::CallService {
            service: service.to_string(),
            method: method.to_string(),
            args: args.clone(),
            key: key.map(str::to_string),
            emit: None,
        };

        if let Some(handler) = &self.service_handler {
            match handler.call(
                service,
                method,
                args,
                key,
                ServiceCaller::of_user(&ctx.user).as_ref(),
            ) {
                Ok(value) => EffectResult::executed_with_value(effect, value),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no service handler configured")
        }
    }

    fn do_cancel_call(&self, key: &str) -> EffectResult {
        let effect = Effect::CancelCall {
            key: key.to_string(),
        };

        if let Some(handler) = &self.service_handler {
            match handler.cancel(key) {
                Ok(()) => EffectResult::executed(effect),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no service handler configured")
        }
    }

    fn do_render_ui(&self, slot: &str, content: &Value) -> EffectResult {
        let effect = Effect::RenderUi {
            slot: slot.to_string(),
            content: content.clone(),
        };

        // Render is client-only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "render-ui is client-only");
        }

        if let Some(handler) = &self.render_handler {
            match handler.render(slot, content) {
                Ok(()) => EffectResult::executed(effect),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no render handler configured")
        }
    }

    fn do_navigate(
        &self,
        path: &str,
        params: Option<&ValueMap>,
        options: Option<super::client_effect::NavigateOptions>,
    ) -> EffectResult {
        let effect = Effect::Navigate {
            path: path.to_string(),
            params: params.cloned(),
            options,
        };

        // Navigate is client-only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "navigate is client-only");
        }

        if let Some(handler) = &self.navigation_handler {
            match handler.navigate(path, params) {
                Ok(()) => EffectResult::executed(effect),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no navigation handler configured")
        }
    }

    fn do_navigate_back(&self) -> EffectResult {
        let effect = Effect::NavigateBack;

        // Navigate-back is client-only, like navigate
        if self.environment.is_server() {
            return EffectResult::noop(effect, "navigate-back is client-only");
        }

        if let Some(handler) = &self.navigation_handler {
            match handler.navigate_back() {
                Ok(()) => EffectResult::executed(effect),
                // A host without a stack is a noop, not a failure — headless
                // harnesses still count the transition as fired.
                Err(e) => EffectResult::noop(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no navigation handler configured")
        }
    }

    fn do_spawn(&self, entity_type: &str, props: &Value) -> EffectResult {
        let effect = Effect::Spawn {
            entity_type: entity_type.to_string(),
            props: props.clone(),
        };

        // Spawn is client/game only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "spawn is client-only");
        }

        if let Some(handler) = &self.game_handler {
            match handler.spawn(entity_type, props) {
                Ok(id) => EffectResult::executed_with_value(effect, Value::String(id)),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no game handler configured")
        }
    }

    fn do_despawn(&self, entity_id: &str) -> EffectResult {
        let effect = Effect::Despawn {
            entity_id: entity_id.to_string(),
        };

        // Despawn is client/game only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "despawn is client-only");
        }

        if let Some(handler) = &self.game_handler {
            match handler.despawn(entity_id) {
                Ok(()) => EffectResult::executed(effect),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no game handler configured")
        }
    }

    fn do_log(&self, level: LogLevel, message: &str, data: Option<&Value>) -> EffectResult {
        let effect = Effect::Log {
            level,
            message: message.to_string(),
            data: data.cloned(),
        };

        // Log works in both environments
        match level {
            LogLevel::Debug => debug!(message = message, data = ?data, "Log effect"),
            LogLevel::Info => info!(message = message, data = ?data, "Log effect"),
            LogLevel::Warn => warn!(message = message, data = ?data, "Log effect"),
            LogLevel::Error => error!(message = message, data = ?data, "Log effect"),
        }

        EffectResult::executed(effect)
    }

    fn do_custom(&self, name: &str, args: &[Value]) -> EffectResult {
        let effect = Effect::Custom {
            name: name.to_string(),
            args: args.to_vec(),
            emit: None,
        };

        if crate::ir::is_substrate_operator(name) {
            if let Some(handler) = &self.substrate_handler {
                return match handler.run(name, args) {
                    Ok(value) => EffectResult::executed_with_value(effect, value),
                    Err(failure) => {
                        let mut result = EffectResult::failed(effect, &failure.message);
                        if !failure.errors.is_empty() {
                            result.return_value = Some(Value::Array(failure.errors));
                        }
                        result
                    }
                };
            }
            if self.environment.is_server() {
                return EffectResult::failed(
                    effect,
                    &format!("no substrate host configured for {name}"),
                );
            }
            return EffectResult::noop(effect, "substrate operators run on the server leg");
        }

        debug!(name = name, args = ?args, "Custom effect (not handled)");
        EffectResult::noop(effect, "custom effects require specific handlers")
    }
}

impl std::fmt::Debug for EffectExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectExecutor")
            .field("environment", &self.environment)
            .field("has_event_bus", &self.event_bus.is_some())
            .field("has_persistence", &self.persistence.is_some())
            .field("has_fetch_handler", &self.fetch_handler.is_some())
            .finish()
    }
}

/// The access directive, required columns and `@intrinsic` fields that govern one persist.
#[derive(Default)]
struct PersistScope {
    policy: Option<Value>,
    required_fields: Vec<String>,
    declared_required: Vec<String>,
    intrinsic: Vec<String>,
}

/// One evaluated persist, ready to be sent to the store as a conditional write.
struct PreparedWrite {
    effect: Effect,
    operation: PersistOperation,
    entity_type: String,
    id: Option<String>,
    resolved_data: Value,
    row_id: String,
    op: RowWriteOp,
    guard: Option<RowPredicate>,
}

/// How one evaluated persist reaches the store.
enum PersistPrep {
    Push(Box<PreparedWrite>),
    Refused(EffectResult),
    Legacy,
}

/// One parsed `persist batch` op tuple.
struct BatchOp {
    action: String,
    entity: String,
    operation: PersistOperation,
    row_id: Option<String>,
    data: Value,
}

/// The `operations` entry a finished batch op contributes.
fn batch_entry(b: &BatchOp, result: &EffectResult) -> Value {
    let mut entry: ValueMap = match (&result.return_value, b.operation) {
        (
            Some(Value::Object(row)),
            PersistOperation::Create | PersistOperation::Upsert | PersistOperation::Update,
        ) => row.clone(),
        (_, PersistOperation::Create | PersistOperation::Upsert | PersistOperation::Update) => {
            b.data.as_object().cloned().unwrap_or_default()
        }
        _ => ValueMap::new(),
    };
    entry.insert("action".to_string(), Value::String(b.action.clone()));
    entry.insert("entityType".to_string(), Value::String(b.entity.clone()));
    if let Some(id) = &b.row_id {
        entry.insert("id".to_string(), Value::String(id.clone()));
    }
    if b.operation == PersistOperation::Delete {
        entry.insert("deleted".to_string(), Value::Bool(true));
    }
    Value::Object(entry)
}

fn batch_summary(effect: Effect, completed: Vec<Value>, total: usize) -> EffectResult {
    let mut summary = ValueMap::new();
    summary.insert(
        "completedCount".to_string(),
        Value::Number(completed.len() as f64),
    );
    summary.insert("totalCount".to_string(), Value::Number(total as f64));
    summary.insert("operations".to_string(), Value::Array(completed));
    EffectResult::executed_with_value(effect, Value::Object(summary))
}

/// Order rows by `order` (missing values last in both directions, ties by `id` ascending, as the compiled
/// path and `@almadar/db`), then cut the page with the same `> 0` offset/limit rule.
pub(crate) fn page_ordered_rows(
    mut rows: Vec<Value>,
    order: &RowOrder,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Vec<Value> {
    rows.sort_by(|a, b| compare_rows(a, b, order));
    let start = offset.unwrap_or(0).min(rows.len());
    let rows = rows.split_off(start);
    match limit {
        Some(n) => rows.into_iter().take(n).collect(),
        None => rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::ValueMap;
    use crate::events::shared_bus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct RecordingService {
        calls: parking_lot::Mutex<Vec<(String, Option<String>)>>,
        cancelled: parking_lot::Mutex<Vec<String>>,
    }

    impl ServiceHandler for RecordingService {
        fn call(
            &self,
            service: &str,
            method: &str,
            _args: &Value,
            key: Option<&str>,
            _caller: Option<&ServiceCaller>,
        ) -> Result<Value, String> {
            self.calls
                .lock()
                .push((format!("{service}.{method}"), key.map(str::to_string)));
            Ok(Value::Null)
        }

        fn cancel(&self, key: &str) -> Result<(), String> {
            self.cancelled.lock().push(key.to_string());
            Ok(())
        }
    }

    #[test]
    fn keyed_call_service_hands_its_key_to_the_service_handler() {
        let svc = Arc::new(RecordingService::default());
        let executor = EffectExecutor::server().with_service_handler(svc.clone());
        let effect = Effect::CallService {
            service: "llm".into(),
            method: "classifyMany".into(),
            args: Value::Null,
            key: Some("batch-1".into()),
            emit: None,
        };
        assert!(executor
            .execute(&effect, &BindingContext::new())
            .is_executed());
        assert_eq!(
            *svc.calls.lock(),
            vec![("llm.classifyMany".to_string(), Some("batch-1".to_string()))]
        );
    }

    #[test]
    fn cancel_call_asks_the_service_handler_to_abort_that_key() {
        let svc = Arc::new(RecordingService::default());
        let executor = EffectExecutor::server().with_service_handler(svc.clone());
        let effect = Effect::CancelCall {
            key: "batch-1".into(),
        };
        assert!(executor
            .execute(&effect, &BindingContext::new())
            .is_executed());
        assert_eq!(*svc.cancelled.lock(), vec!["batch-1".to_string()]);
    }

    #[test]
    fn cancel_call_without_a_service_handler_is_a_noop_on_a_server() {
        let executor = EffectExecutor::server();
        let effect = Effect::CancelCall { key: "k".into() };
        assert!(executor.execute(&effect, &BindingContext::new()).is_noop());
    }

    #[test]
    fn a_client_delegates_cancel_call_instead_of_running_it() {
        let svc = Arc::new(RecordingService::default());
        let executor = EffectExecutor::client().with_service_handler(svc.clone());
        let effect = Effect::CancelCall { key: "k".into() };
        let result = executor.execute(&effect, &BindingContext::new());
        assert!(svc.cancelled.lock().is_empty());
        assert!(!result.is_executed(), "{result:?}");
    }

    #[test]
    fn test_emit_effect() {
        let bus = shared_bus();
        let executor = EffectExecutor::server().with_event_bus(bus.clone());

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        bus.on("TEST_EVENT", move |_| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        let effect = Effect::Emit {
            event: "TEST_EVENT".to_string(),
            payload: Value::Null,
        };

        let result = executor.execute(&effect, &BindingContext::new());
        assert!(result.is_executed());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_persist_server_only() {
        let persistence = Arc::new(InMemoryPersistence::new());
        let server_executor = EffectExecutor::server().with_persistence(persistence.clone());
        let client_executor = EffectExecutor::client();

        let effect = Effect::Persist {
            operation: PersistOperation::Create,
            entity_type: "User".to_string(),
            data: Value::Object({
                let mut m = ValueMap::new();
                m.insert("name".to_string(), Value::String("John".to_string()));
                m
            }),
            id: None,
            emit: None,
        };

        // Server executes
        let server_result = server_executor.execute(&effect, &BindingContext::new());
        assert!(server_result.is_executed());

        // Client no-ops
        let client_result = client_executor.execute(&effect, &BindingContext::new());
        assert!(client_result.is_noop());
    }

    fn row_of(pairs: &[(&str, &str)]) -> Value {
        Value::Object({
            let mut m = ValueMap::new();
            for (k, v) in pairs {
                m.insert((*k).to_string(), Value::String((*v).to_string()));
            }
            m
        })
    }

    fn persist_effect(operation: PersistOperation, data: Value) -> Effect {
        Effect::Persist {
            operation,
            entity_type: "Mix".to_string(),
            data,
            id: None,
            emit: None,
        }
    }

    /// EXEC-STEP-8: a create on a taken id fails `duplicate-id` and changes nothing, on a store with no
    /// conditional write; a create naming no id is minted one (control).
    #[test]
    fn test_persist_create_on_a_taken_id_fails_duplicate_id() {
        let persistence = Arc::new(InMemoryPersistence::new());
        let executor = EffectExecutor::server().with_persistence(persistence.clone());
        let ctx = BindingContext::new();
        let first = persist_effect(
            PersistOperation::Create,
            row_of(&[("id", "mix"), ("title", "one")]),
        );
        assert!(executor.execute(&first, &ctx).is_executed());
        let again = persist_effect(
            PersistOperation::Create,
            row_of(&[("id", "mix"), ("title", "two")]),
        );
        let result = executor.execute(&again, &ctx);
        assert!(result.is_failed());
        assert_eq!(result.code, Some(ServerEffectCode::DuplicateId));
        let stored = persistence.read("Mix", "mix").unwrap();
        assert_eq!(stored.as_object().unwrap()["title"], Value::String("one".into()));
        let minted = persist_effect(PersistOperation::Create, row_of(&[("title", "free")]));
        assert!(executor.execute(&minted, &ctx).is_executed());
        assert!(executor.execute(&minted, &ctx).is_executed());
    }

    /// EXEC-STEP-8: an upsert creates the row, then replaces it whole keeping the stored `createdAt`.
    #[test]
    fn test_persist_upsert_creates_then_replaces_the_whole_row() {
        let persistence = Arc::new(InMemoryPersistence::new());
        let executor = EffectExecutor::server().with_persistence(persistence.clone());
        let ctx = BindingContext::new();
        let created = persist_effect(
            PersistOperation::Upsert,
            row_of(&[("id", "mix"), ("title", "one"), ("tone", "warm"), ("createdAt", "2026-01-01")]),
        );
        assert!(executor.execute(&created, &ctx).is_executed());
        let replaced = persist_effect(
            PersistOperation::Upsert,
            row_of(&[("id", "mix"), ("title", "two"), ("createdAt", "2030-01-01")]),
        );
        let result = executor.execute(&replaced, &ctx);
        assert!(result.is_executed(), "{result:?}");
        let stored = persistence.read("Mix", "mix").unwrap();
        let fields = stored.as_object().unwrap();
        assert_eq!(fields["title"], Value::String("two".into()));
        assert!(!fields.contains_key("tone"), "replaced whole: {fields:?}");
        assert_eq!(fields["createdAt"], Value::String("2026-01-01".into()));
        let keyless = persist_effect(PersistOperation::Upsert, row_of(&[("title", "x")]));
        let failed = executor.execute(&keyless, &ctx);
        assert!(failed.is_failed());
        assert_eq!(failed.code, Some(ServerEffectCode::NoRowKey), "control: an upsert needs an id");
    }

    /// C-PERSIST-UPDATE-TRAILING-EMIT-MISPARSED, end to end: `(persist update
    /// Entity ?data {emit:{...}})` — no separate id arg, the data carries its
    /// own `id` field, and a trailing `{emit:...}` options object rides
    /// along. Before the fix, `parse_persist` misread the emit config as
    /// `data` and the real data token as `id`; the update landed on a
    /// nonexistent row (or silently no-op'd). This proves the real row is
    /// found and updated.
    #[test]
    fn test_persist_update_data_carries_its_own_id_with_trailing_emit_options() {
        let persistence = Arc::new(InMemoryPersistence::new());
        let created = persistence
            .create(
                "Ticket",
                &Value::Object({
                    let mut m = ValueMap::new();
                    m.insert("subject".to_string(), Value::String("original".to_string()));
                    m
                }),
            )
            .unwrap();
        let ticket_id = created
            .as_object()
            .unwrap()
            .get("id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();

        let executor = EffectExecutor::server().with_persistence(persistence.clone());

        // Simulates the compiled effect array `["persist", "update", "Ticket",
        // "@payload.data", {"emit": {...}}]` after `parse_persist` — id is
        // None (no separate id arg), data carries its own `id` field.
        let effect = Effect::Persist {
            operation: PersistOperation::Update,
            entity_type: "Ticket".to_string(),
            data: Value::Object({
                let mut m = ValueMap::new();
                m.insert("id".to_string(), Value::String(ticket_id.clone()));
                m.insert("subject".to_string(), Value::String("edited".to_string()));
                m
            }),
            id: None,
            emit: None,
        };

        let result = executor.execute(&effect, &BindingContext::new());
        assert!(
            result.is_executed(),
            "update must find the real row, not fail: {result:?}"
        );

        let updated = persistence.read("Ticket", &ticket_id).unwrap();
        assert_eq!(
            updated
                .as_object()
                .unwrap()
                .get("subject")
                .unwrap()
                .as_str(),
            Some("edited"),
            "the real row must have been updated, not a nonexistent placeholder id"
        );
    }

    /// C-PERSIST-DATA-CALL-EXPR-NEVER-EVALUATED: `(persist create Entity
    /// (object/merge ?data {field: @user.id}) {...})` — the DOMINANT
    /// server-side-enrichment idiom (`TicketReplyPersistor`, both
    /// `TicketPersistor`/`ChatMessagePersistor` DO_CREATE arms). `data` here
    /// is a CALL expression, not a plain object — `parse_persist` converts it
    /// straight from `SExpression` to `Value` (a 3-element array literally
    /// `["object/merge", "@payload.data", {field: "@user.id"}]`), and
    /// `do_persist` only ran it through `resolve_value_bindings`, which
    /// substitutes leaf `@`-sigils but never DISPATCHES a call — so the
    /// merge itself never ran and the stored row was the raw call array,
    /// not an object. Invisible until something read a field back out of
    /// it (an access-policy check is the first thing that ever did).
    /// Reproduced via the REAL parser (`parse_effect`), not a hand-built
    /// `Effect::Persist{data: Value::Object(...)}` — those bypass the bug
    /// entirely since they skip the call-expression shape.
    #[test]
    fn test_persist_create_data_call_expression_is_evaluated_not_left_literal() {
        let persistence = Arc::new(InMemoryPersistence::new());
        let executor = EffectExecutor::server().with_persistence(persistence.clone());

        let raw = SExpression::Array(vec![
            SExpression::String("persist".to_string()),
            SExpression::String("create".to_string()),
            SExpression::String("ChatMessage".to_string()),
            SExpression::Array(vec![
                SExpression::String("object/merge".to_string()),
                SExpression::String("@payload.data".to_string()),
                SExpression::Object({
                    let mut m = crate::schema::SObject::new();
                    m.insert(
                        "sender".to_string(),
                        SExpression::String("@user.id".to_string()),
                    );
                    m
                }),
            ]),
        ]);
        let effect =
            crate::effects::parse_effect(&raw).expect("parse_persist must recognize this shape");

        let mut payload_data = ValueMap::new();
        payload_data.insert("content".to_string(), Value::String("hello".to_string()));
        let mut payload = ValueMap::new();
        payload.insert("data".to_string(), Value::Object(payload_data));
        let mut user = ValueMap::new();
        user.insert("id".to_string(), Value::String("user-42".to_string()));

        let ctx = crate::evaluator::BindingContextBuilder::new()
            .payload(Value::Object(payload))
            .user(Value::Object(user))
            .build();

        let result = executor.execute(&effect, &ctx);
        assert!(result.is_executed(), "create must execute: {result:?}");

        let all = persistence.list("ChatMessage").unwrap();
        assert_eq!(all.len(), 1, "exactly one row must have been created");
        let row = all[0]
            .as_object()
            .expect("the stored row must be a real object, not the raw call array");
        assert_eq!(
            row.get("content").and_then(|v| v.as_str()),
            Some("hello"),
            "the ?data payload's own fields must survive the merge"
        );
        assert_eq!(
            row.get("sender").and_then(|v| v.as_str()),
            Some("user-42"),
            "object/merge's stamped field must actually be evaluated, not left as a literal call array"
        );
    }

    /// B4-V3 (Rust twin): the JS runtime's persist handler used to fall
    /// back to the submitted data when the store denied a write, so a
    /// denied delete logged `persist:success` and fired the declared
    /// success event for a row the store never touched. `do_persist` here
    /// already routes a `check_mutation_access` denial through the SAME
    /// `Err` arm as any other persist failure — `EffectResult::failed`, not
    /// `executed_with_value` — so there is no row for a caller to
    /// mistakenly treat as written. This pins that the kernel never had
    /// the JS-side bug: a denied delete/update is `EffectOutcome::Failed`
    /// with no `return_value`, never `Executed`.
    #[test]
    fn test_persist_denied_delete_is_failed_not_executed() {
        let persistence = Arc::new(InMemoryPersistence::new());
        persistence
            .create(
                "Note",
                &Value::Object({
                    let mut m = ValueMap::new();
                    m.insert("id".to_string(), Value::String("note-1".to_string()));
                    m.insert(
                        "authorId".to_string(),
                        Value::String("author-1".to_string()),
                    );
                    m
                }),
            )
            .unwrap();

        let mut policies = EntityAccessTable::new();
        policies.insert(
            "Note".to_string(),
            crate::runtime::entity_access::EntityAccessPolicies {
                delete: Some(Value::Array(vec![
                    Value::String("=".to_string()),
                    Value::String("@entity.authorId".to_string()),
                    Value::String("@user.id".to_string()),
                ])),
                ..Default::default()
            },
        );

        let executor = EffectExecutor::server()
            .with_persistence(persistence.clone())
            .with_entity_policies(Arc::new(RwLock::new(policies)));

        // The viewer is NOT the row's author — the declared `@delete`
        // policy must reject this row.
        let viewer = Value::Object({
            let mut m = ValueMap::new();
            m.insert("id".to_string(), Value::String("someone-else".to_string()));
            m
        });
        let ctx = crate::evaluator::BindingContextBuilder::new()
            .user(viewer)
            .build();

        let effect = Effect::Persist {
            operation: PersistOperation::Delete,
            entity_type: "Note".to_string(),
            data: Value::Null,
            id: Some("note-1".to_string()),
            emit: None,
        };

        let result = executor.execute(&effect, &ctx);
        assert!(
            result.is_failed(),
            "a denied delete must be Failed, never Executed: {result:?}"
        );
        assert!(result.return_value.is_none());
        assert!(result
            .error
            .as_deref()
            .is_some_and(|e| crate::runtime::entity_access::is_access_denied_error(e)));

        // The row must genuinely survive — the denial is real, not cosmetic.
        assert!(persistence.read("Note", "note-1").is_ok());
    }

    #[test]
    fn test_render_ui_client_only() {
        let server_executor = EffectExecutor::server();
        let client_executor = EffectExecutor::client();

        let effect = Effect::RenderUi {
            slot: "main".to_string(),
            content: Value::Object(ValueMap::new()),
        };

        // Server no-ops
        let server_result = server_executor.execute(&effect, &BindingContext::new());
        assert!(server_result.is_noop());

        // Client would execute if handler configured, otherwise no-ops
        let client_result = client_executor.execute(&effect, &BindingContext::new());
        assert!(client_result.is_noop()); // No handler configured
    }

    #[test]
    fn test_navigate_client_only() {
        let server_executor = EffectExecutor::server();

        let effect = Effect::Navigate {
            path: "/users".to_string(),
            params: None,
            options: None,
        };

        let result = server_executor.execute(&effect, &BindingContext::new());
        assert!(result.is_noop());
    }

    #[test]
    fn test_log_effect() {
        let executor = EffectExecutor::server();

        let effect = Effect::Log {
            level: LogLevel::Info,
            message: "Test message".to_string(),
            data: None,
        };

        let result = executor.execute(&effect, &BindingContext::new());
        assert!(result.is_executed());
    }

    #[test]
    fn test_in_memory_persistence() {
        let persistence = InMemoryPersistence::new();

        // Create
        let data = Value::Object({
            let mut m = ValueMap::new();
            m.insert("name".to_string(), Value::String("John".to_string()));
            m
        });
        let created = persistence.create("User", &data).unwrap();
        let id = created
            .as_object()
            .unwrap()
            .get("id")
            .unwrap()
            .as_str()
            .unwrap();

        // Read
        let read = persistence.read("User", id).unwrap();
        assert_eq!(
            read.as_object().unwrap().get("name").unwrap(),
            &Value::String("John".to_string())
        );

        // Update
        let update_data = Value::Object({
            let mut m = ValueMap::new();
            m.insert("name".to_string(), Value::String("Jane".to_string()));
            m
        });
        let updated = persistence.update("User", id, &update_data).unwrap();
        assert_eq!(
            updated.as_object().unwrap().get("name").unwrap(),
            &Value::String("Jane".to_string())
        );

        // Delete
        persistence.delete("User", id).unwrap();
        assert!(persistence.read("User", id).is_err());
    }

    #[test]
    fn test_execute_all_from_sexpr() {
        let bus = shared_bus();
        let executor = EffectExecutor::server().with_event_bus(bus.clone());

        let effects: Vec<SExpression> = vec![
            serde_json::from_str(r#"["emit", "EVENT_1", null]"#).unwrap(),
            serde_json::from_str(r#"["log", "info", "Test message"]"#).unwrap(),
        ];

        let ctx = BindingContext::new();
        let results = executor.execute_all(&effects, &ctx);

        assert_eq!(results.len(), 2);
        assert!(results[0].is_executed());
        assert!(results[1].is_executed());
    }

    #[test]
    fn test_custom_effect() {
        let executor = EffectExecutor::server();

        let effect = Effect::Custom {
            name: "my-custom-effect".to_string(),
            args: vec![Value::Number(42.0)],
            emit: None,
        };

        let result = executor.execute(&effect, &BindingContext::new());
        assert!(result.is_noop()); // Custom effects are not handled
    }

    #[test]
    fn test_fetch_effect_server() {
        let persistence = Arc::new(InMemoryPersistence::new());

        // Create some test data
        persistence
            .create(
                "User",
                &Value::Object({
                    let mut m = ValueMap::new();
                    m.insert("id".to_string(), Value::String("user-1".to_string()));
                    m.insert("name".to_string(), Value::String("John".to_string()));
                    m
                }),
            )
            .unwrap();
        persistence
            .create(
                "User",
                &Value::Object({
                    let mut m = ValueMap::new();
                    m.insert("id".to_string(), Value::String("user-2".to_string()));
                    m.insert("name".to_string(), Value::String("Jane".to_string()));
                    m
                }),
            )
            .unwrap();

        let executor = EffectExecutor::server()
            .with_persistence(persistence.clone())
            .with_fetch_handler(persistence);

        // Test fetch all
        let effect = Effect::Fetch {
            entity_type: "User".to_string(),
            id: None,
            filter: None,
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            emit: None,
        };

        let result = executor.execute(&effect, &BindingContext::new());
        assert!(result.is_executed());

        // Verify entity_type is tracked
        assert_eq!(result.entity_type, Some("User".to_string()));

        let value = result.return_value.unwrap();
        let arr = value.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_fetch_effect_by_id() {
        let persistence = Arc::new(InMemoryPersistence::new());

        persistence
            .create(
                "User",
                &Value::Object({
                    let mut m = ValueMap::new();
                    m.insert("id".to_string(), Value::String("user-123".to_string()));
                    m.insert("name".to_string(), Value::String("John".to_string()));
                    m
                }),
            )
            .unwrap();

        let executor = EffectExecutor::server()
            .with_persistence(persistence.clone())
            .with_fetch_handler(persistence);

        // Test fetch by ID
        let effect = Effect::Fetch {
            entity_type: "User".to_string(),
            id: Some(Value::String("user-123".to_string())),
            filter: None,
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            emit: None,
        };

        let result = executor.execute(&effect, &BindingContext::new());
        assert!(result.is_executed());
        let value = result.return_value.unwrap();
        let arr = value.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].as_object().unwrap().get("name").unwrap(),
            &Value::String("John".to_string())
        );
    }

    #[test]
    fn test_fetch_effect_client_noop() {
        let executor = EffectExecutor::client();

        let effect = Effect::Fetch {
            entity_type: "User".to_string(),
            id: None,
            filter: None,
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            emit: None,
        };

        let result = executor.execute(&effect, &BindingContext::new());
        assert!(result.is_noop()); // Fetch is server-only
    }

    #[test]
    fn test_fetch_filter_resolves_frame_ref_against_issuing_context() {
        struct RowScopedFetchHandler;
        impl FetchHandler for RowScopedFetchHandler {
            fn fetch(
                &self,
                entity_type: &str,
                _id: Option<&str>,
                filter: Option<&Value>,
                _limit: Option<usize>,
                _offset: Option<usize>,
                _include: Option<&Vec<String>>,
                ctx: &BindingContext,
            ) -> Result<FetchPage, String> {
                let mut rows = vec![
                    Value::Object({
                        let mut m = ValueMap::new();
                        m.insert("id".to_string(), Value::String("r1".to_string()));
                        m.insert("channel".to_string(), Value::String("general".to_string()));
                        m
                    }),
                    Value::Object({
                        let mut m = ValueMap::new();
                        m.insert("id".to_string(), Value::String("r2".to_string()));
                        m.insert("channel".to_string(), Value::String("random".to_string()));
                        m
                    }),
                ];
                crate::runtime::entity_access::apply_read_access(
                    &mut rows,
                    None,
                    filter,
                    ctx,
                    entity_type,
                );
                Ok(FetchPage {
                    total: rows.len(),
                    rows,
                })
            }
        }

        // ["=", ["object/get", "@entity", "channel"], "@entity.activeChannel"] —
        // row-scoped LHS stays verbatim, issuing-frame RHS resolves before the
        // per-row evaluation. Previously both sides re-scoped to the row and
        // this self-comparison never matched.
        let filter = Value::Array(vec![
            Value::String("=".to_string()),
            Value::Array(vec![
                Value::String("object/get".to_string()),
                Value::String("@entity".to_string()),
                Value::String("channel".to_string()),
            ]),
            Value::String("@entity.activeChannel".to_string()),
        ]);

        let issuing_entity = Value::Object({
            let mut m = ValueMap::new();
            m.insert(
                "activeChannel".to_string(),
                Value::String("general".to_string()),
            );
            m
        });
        let ctx = crate::evaluator::BindingContextBuilder::new()
            .entity(issuing_entity)
            .build();

        let executor = EffectExecutor::server().with_fetch_handler(Arc::new(RowScopedFetchHandler));
        let effect = Effect::Fetch {
            entity_type: "Message".to_string(),
            id: None,
            filter: Some(filter),
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            emit: None,
        };

        let result = executor.execute(&effect, &ctx);
        assert!(result.is_executed());
        let arr = result.return_value.unwrap().as_array().unwrap().clone();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].get_path("id"),
            Some(&Value::String("r1".to_string()))
        );
    }

    #[test]
    fn test_fetch_policy_predicate_stays_row_scoped() {
        // A declared @read policy with a bare-string leaf (`@entity.ownerId`)
        // must keep evaluating against each ROW — the frame-ref rewrite in
        // `interpolate_filter_trait_refs` only ever touches the call-site
        // `filter:` passed into `do_fetch`, never the policy predicate a
        // FetchHandler applies internally.
        struct PolicyScopedFetchHandler;
        impl FetchHandler for PolicyScopedFetchHandler {
            fn fetch(
                &self,
                entity_type: &str,
                _id: Option<&str>,
                filter: Option<&Value>,
                _limit: Option<usize>,
                _offset: Option<usize>,
                _include: Option<&Vec<String>>,
                ctx: &BindingContext,
            ) -> Result<FetchPage, String> {
                let mut rows = vec![
                    Value::Object({
                        let mut m = ValueMap::new();
                        m.insert("id".to_string(), Value::String("r1".to_string()));
                        m.insert("ownerId".to_string(), Value::String("user-1".to_string()));
                        m
                    }),
                    Value::Object({
                        let mut m = ValueMap::new();
                        m.insert("id".to_string(), Value::String("r2".to_string()));
                        m.insert("ownerId".to_string(), Value::String("user-2".to_string()));
                        m
                    }),
                ];
                let policy = Value::Array(vec![
                    Value::String("=".to_string()),
                    Value::String("@entity.ownerId".to_string()),
                    Value::String("@user.id".to_string()),
                ]);
                crate::runtime::entity_access::apply_read_access(
                    &mut rows,
                    Some(&policy),
                    filter,
                    ctx,
                    entity_type,
                );
                Ok(FetchPage {
                    total: rows.len(),
                    rows,
                })
            }
        }

        let viewer = Value::Object({
            let mut m = ValueMap::new();
            m.insert("id".to_string(), Value::String("user-1".to_string()));
            m
        });
        let ctx = crate::evaluator::BindingContextBuilder::new()
            .user(viewer)
            .build();

        let executor =
            EffectExecutor::server().with_fetch_handler(Arc::new(PolicyScopedFetchHandler));
        let effect = Effect::Fetch {
            entity_type: "Ticket".to_string(),
            id: None,
            filter: None,
            limit: None,
            offset: None,
            order_by: None,
            include: None,
            deliver: FetchDeliver::Rows,
            total: None,
            emit: None,
        };

        let result = executor.execute(&effect, &ctx);
        assert!(result.is_executed());
        let arr = result.return_value.unwrap().as_array().unwrap().clone();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].get_path("id"),
            Some(&Value::String("r1".to_string()))
        );
    }

    mod in_memory_read_access {
        use super::*;
        use crate::runtime::entity_access::EntityAccessPolicies;
        use serde_json::json;

        fn v(j: serde_json::Value) -> Value {
            serde_json::from_value(j).unwrap()
        }

        fn store(
            rows: &[(&str, &str, &str)],
            read: Option<serde_json::Value>,
        ) -> Arc<InMemoryPersistence> {
            let mut table: EntityAccessTable = HashMap::new();
            table.insert(
                "Note".to_string(),
                EntityAccessPolicies {
                    read: read.map(v),
                    ..Default::default()
                },
            );
            let persistence =
                InMemoryPersistence::new().with_entity_policies(Arc::new(RwLock::new(table)));
            for (id, owner, status) in rows {
                persistence
                    .create(
                        "Note",
                        &v(json!({ "id": id, "ownerId": owner, "status": status })),
                    )
                    .unwrap();
            }
            Arc::new(persistence)
        }

        fn owner_only() -> Option<serde_json::Value> {
            Some(json!([
                "=",
                ["object/get", "@entity", "ownerId"],
                "@user.id"
            ]))
        }

        fn viewer(user: Option<serde_json::Value>) -> BindingContext {
            match user {
                Some(u) => crate::evaluator::BindingContextBuilder::new()
                    .user(v(u))
                    .build(),
                None => BindingContext::new(),
            }
        }

        fn run(
            handler: Arc<InMemoryPersistence>,
            id: Option<&str>,
            filter: Option<serde_json::Value>,
            limit: Option<usize>,
            offset: Option<usize>,
            ctx: &BindingContext,
        ) -> EffectResult {
            let executor = EffectExecutor::server().with_fetch_handler(handler);
            let effect = Effect::Fetch {
                entity_type: "Note".to_string(),
                id: id.map(|s| Value::String(s.to_string())),
                filter: filter.map(v),
                limit: limit.map(|n| Value::Number(n as f64)),
                offset: offset.map(|n| Value::Number(n as f64)),
                order_by: None,
                include: None,
                deliver: FetchDeliver::Rows,
                total: None,
                emit: None,
            };
            executor.execute(&effect, ctx)
        }

        fn fetch(
            handler: Arc<InMemoryPersistence>,
            id: Option<&str>,
            filter: Option<serde_json::Value>,
            limit: Option<usize>,
            offset: Option<usize>,
            ctx: &BindingContext,
        ) -> Vec<(String, String)> {
            let result = run(handler, id, filter, limit, offset, ctx);
            assert!(
                result.is_executed(),
                "fetch must execute, got {:?}",
                result.outcome
            );
            let rows = result
                .return_value
                .map(|r| serde_json::to_value(r).unwrap());
            let mut out: Vec<(String, String)> = rows
                .and_then(|r| r.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .map(|r| {
                    (
                        r["id"].as_str().unwrap().to_string(),
                        r["ownerId"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            out.sort();
            out
        }

        fn ids(rows: &[(String, String)]) -> Vec<&str> {
            rows.iter().map(|(id, _)| id.as_str()).collect()
        }

        const ROWS: &[(&str, &str, &str)] = &[
            ("a1", "alice", "open"),
            ("a2", "alice", "closed"),
            ("b1", "bob", "open"),
        ];

        #[test]
        fn collection_is_scoped_to_the_viewers_rows() {
            let rows = fetch(
                store(ROWS, owner_only()),
                None,
                None,
                None,
                None,
                &viewer(Some(json!({ "id": "alice" }))),
            );
            assert_eq!(ids(&rows), vec!["a1", "a2"]);
        }

        #[test]
        fn anonymous_viewer_sees_nothing_under_an_owner_policy() {
            assert!(fetch(
                store(ROWS, owner_only()),
                None,
                None,
                None,
                None,
                &viewer(None)
            )
            .is_empty());
        }

        #[test]
        fn by_id_of_an_unreadable_row_fails_as_not_found() {
            let alice = viewer(Some(json!({ "id": "alice" })));
            let r = run(
                store(ROWS, owner_only()),
                Some("b1"),
                None,
                None,
                None,
                &alice,
            );
            assert!(!r.is_executed());
            assert_eq!(r.error.as_deref(), Some("Note b1 not found"));
        }

        #[test]
        fn by_id_of_a_readable_row_is_served() {
            let rows = fetch(
                store(ROWS, owner_only()),
                Some("a1"),
                None,
                None,
                None,
                &viewer(Some(json!({ "id": "alice" }))),
            );
            assert_eq!(ids(&rows), vec!["a1"]);
        }

        #[test]
        fn by_id_of_a_missing_row_fails_exactly_like_a_refused_one() {
            let alice = viewer(Some(json!({ "id": "alice" })));
            let missing = run(
                store(ROWS, owner_only()),
                Some("zz"),
                None,
                None,
                None,
                &alice,
            );
            assert!(!missing.is_executed());
            assert_eq!(missing.error.as_deref(), Some("Note zz not found"));
            let refused = run(
                store(ROWS, owner_only()),
                Some("b1"),
                None,
                None,
                None,
                &alice,
            );
            assert_eq!(
                refused.error.as_deref().map(|e| e.replace("b1", "zz")),
                missing.error,
                "a refused row must be indistinguishable from a missing one"
            );
        }

        #[test]
        fn call_site_filter_narrows_within_the_policy() {
            let filter = json!(["=", ["object/get", "@entity", "status"], "open"]);
            let rows = fetch(
                store(ROWS, owner_only()),
                None,
                Some(filter),
                None,
                None,
                &viewer(Some(json!({ "id": "alice" }))),
            );
            assert_eq!(ids(&rows), vec!["a1"]);
        }

        #[test]
        fn filter_cannot_widen_past_the_policy() {
            let filter = json!(["=", ["object/get", "@entity", "ownerId"], "bob"]);
            let rows = fetch(
                store(ROWS, owner_only()),
                None,
                Some(filter),
                None,
                None,
                &viewer(Some(json!({ "id": "alice" }))),
            );
            assert!(rows.is_empty());
        }

        #[test]
        fn pagination_applies_after_the_policy() {
            let many: Vec<(String, String)> = (0..6)
                .map(|i| {
                    (
                        format!("r{i}"),
                        if i % 2 == 0 {
                            "alice".into()
                        } else {
                            "bob".into()
                        },
                    )
                })
                .collect();
            let rows_ref: Vec<(&str, &str, &str)> = many
                .iter()
                .map(|(id, o)| (id.as_str(), o.as_str(), "open"))
                .collect();
            let alice = viewer(Some(json!({ "id": "alice" })));
            let page = fetch(
                store(&rows_ref, owner_only()),
                None,
                None,
                Some(2),
                Some(1),
                &alice,
            );
            assert_eq!(page.len(), 2);
            assert!(page.iter().all(|(_, owner)| owner == "alice"));
            let past_end = fetch(
                store(&rows_ref, owner_only()),
                None,
                None,
                None,
                Some(3),
                &alice,
            );
            assert!(past_end.is_empty());
        }

        #[test]
        fn no_read_policy_is_allow_all() {
            let rows = fetch(
                store(ROWS, None),
                None,
                None,
                None,
                None,
                &viewer(Some(json!({ "id": "alice" }))),
            );
            assert_eq!(ids(&rows), vec!["a1", "a2", "b1"]);
        }

        #[test]
        fn no_policy_table_is_allow_all() {
            let persistence = Arc::new(InMemoryPersistence::new());
            for (id, owner, status) in ROWS {
                persistence
                    .create(
                        "Note",
                        &v(json!({ "id": id, "ownerId": owner, "status": status })),
                    )
                    .unwrap();
            }
            let rows = fetch(persistence, None, None, None, None, &viewer(None));
            assert_eq!(ids(&rows), vec!["a1", "a2", "b1"]);
        }

        #[test]
        fn role_or_owner_policy_admits_both_kinds_of_viewer() {
            let policy = Some(json!([
                "or",
                ["=", "@user.role", "admin"],
                ["=", ["object/get", "@entity", "ownerId"], "@user.id"]
            ]));
            let admin = fetch(
                store(ROWS, policy.clone()),
                None,
                None,
                None,
                None,
                &viewer(Some(json!({ "id": "carol", "role": "admin" }))),
            );
            assert_eq!(ids(&admin), vec!["a1", "a2", "b1"]);
            let bob = fetch(
                store(ROWS, policy),
                None,
                None,
                None,
                None,
                &viewer(Some(json!({ "id": "bob", "role": "member" }))),
            );
            assert_eq!(ids(&bob), vec!["b1"]);
        }
    }

    mod fetch_option_evaluation {
        use super::*;
        use serde_json::json;

        fn v(j: serde_json::Value) -> Value {
            serde_json::from_value(j).unwrap()
        }

        fn run(
            options: serde_json::Value,
            payload: serde_json::Value,
            entity: serde_json::Value,
        ) -> EffectResult {
            let persistence = Arc::new(InMemoryPersistence::new());
            for i in 0..5 {
                persistence
                    .create("Row", &v(json!({ "id": format!("r{i}"), "n": i })))
                    .unwrap();
            }
            let executor = EffectExecutor::server().with_fetch_handler(persistence);
            let raw: SExpression =
                serde_json::from_value(json!(["fetch", "Row", options])).unwrap();
            let effect = crate::effects::parse_effect(&raw).expect("fetch parses");
            let mut ctx = BindingContext::new();
            ctx.payload = Arc::new(v(payload));
            ctx.set_entity(v(entity));
            executor.execute(&effect, &ctx)
        }

        fn count(result: &EffectResult) -> usize {
            assert!(
                result.is_executed(),
                "fetch must execute: {:?}",
                result.error
            );
            result
                .return_value
                .as_ref()
                .and_then(|r| r.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        }

        #[test]
        fn literal_limit_applies() {
            assert_eq!(count(&run(json!({ "limit": 2 }), json!({}), json!({}))), 2);
        }

        #[test]
        fn payload_bound_limit_and_offset_apply() {
            let r = run(
                json!({ "limit": "@payload.limit", "offset": "@payload.offset" }),
                json!({ "limit": 2, "offset": 4 }),
                json!({}),
            );
            assert_eq!(count(&r), 1);
        }

        #[test]
        fn expression_limit_is_evaluated() {
            let opts = json!({ "limit": ["or", "@payload.limit", 3] });
            assert_eq!(count(&run(opts.clone(), json!({}), json!({}))), 3);
            assert_eq!(count(&run(opts, json!({ "limit": 1 }), json!({}))), 1);
        }

        #[test]
        fn non_positive_or_non_numeric_limit_means_no_limit() {
            for limit in [json!(0), json!(-1), json!("abc"), json!("@payload.missing")] {
                assert_eq!(
                    count(&run(json!({ "limit": limit }), json!({}), json!({}))),
                    5
                );
            }
        }

        fn ids(result: &EffectResult) -> Vec<String> {
            result
                .return_value
                .as_ref()
                .and_then(|r| r.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| match row.get_path("id") {
                            Some(Value::String(id)) => Some(id.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        }

        #[test]
        fn order_by_field_and_direction_applies_before_paging() {
            assert_eq!(
                ids(&run(json!({ "orderBy": "n:desc" }), json!({}), json!({}))),
                ["r4", "r3", "r2", "r1", "r0"]
            );
            assert_eq!(
                ids(&run(
                    json!({ "orderBy": "n:desc", "limit": 2 }),
                    json!({}),
                    json!({})
                )),
                ["r4", "r3"]
            );
        }

        #[test]
        fn a_bare_order_field_is_ascending_and_a_payload_spec_is_evaluated() {
            assert_eq!(
                ids(&run(
                    json!({ "orderBy": "@payload.order", "limit": 2 }),
                    json!({ "order": "n:desc" }),
                    json!({})
                )),
                ["r4", "r3"]
            );
            assert_eq!(
                ids(&run(
                    json!({ "orderBy": "n", "offset": 3 }),
                    json!({}),
                    json!({})
                )),
                ["r3", "r4"]
            );
        }

        #[test]
        fn without_order_by_rows_lacking_created_at_follow_id_order() {
            assert_eq!(
                ids(&run(json!({ "limit": 2 }), json!({}), json!({}))),
                ["r0", "r1"]
            );
        }

        /// EXEC-FETCH-1: an unordered fetch is `createdAt` ascending, a row without one last, ties by id; the
        /// insertion order of the store is not the answer. Twin: `almadar-runtime/test/fetch-default-order.test.ts`.
        #[test]
        fn without_order_by_the_default_order_is_created_at_then_id() {
            let persistence = Arc::new(InMemoryPersistence::new());
            for (id, created) in [
                ("a", Some("2026-03-02T00:00:00.000Z")),
                ("b", None),
                ("c", Some("2026-03-01T00:00:00.000Z")),
                ("d", Some("2026-03-02T00:00:00.000Z")),
            ] {
                let mut row = json!({ "id": id });
                if let Some(created) = created {
                    row["createdAt"] = json!(created);
                }
                persistence.create("Row", &v(row)).unwrap();
            }
            let executor = EffectExecutor::server().with_fetch_handler(persistence);
            let fetch = |options: serde_json::Value| {
                let raw: SExpression =
                    serde_json::from_value(json!(["fetch", "Row", options])).unwrap();
                let effect = crate::effects::parse_effect(&raw).expect("fetch parses");
                let mut ctx = BindingContext::new();
                ctx.payload = Arc::new(v(json!({})));
                ctx.set_entity(v(json!({})));
                ids(&executor.execute(&effect, &ctx))
            };
            assert_eq!(fetch(json!({})), ["c", "a", "d", "b"]);
            assert_eq!(fetch(json!({ "offset": 1, "limit": 2 })), ["a", "d"]);
            // control: an authored order replaces the default
            assert_eq!(fetch(json!({ "orderBy": "id:desc" })), ["d", "c", "b", "a"]);
        }

        #[test]
        fn offset_past_the_end_is_empty() {
            assert_eq!(count(&run(json!({ "offset": 9 }), json!({}), json!({}))), 0);
        }

        #[test]
        fn expression_id_fetches_one_row_not_the_collection() {
            let r = run(
                json!({ "id": ["object/get", "@payload", "rowId"] }),
                json!({ "rowId": "r3" }),
                json!({}),
            );
            assert_eq!(count(&r), 1);
            let row = r.return_value.unwrap().as_array().unwrap()[0].clone();
            assert_eq!(row.get_path("id"), Some(&Value::String("r3".to_string())));
        }

        #[test]
        fn expression_id_resolving_to_a_non_id_fails_loudly() {
            let r = run(
                json!({ "id": ["object/get", "@payload", "rowId"] }),
                json!({ "rowId": true }),
                json!({}),
            );
            assert!(!r.is_executed());
            assert!(r.error.unwrap().contains("not a string or integer"));
        }

        #[test]
        fn by_id_miss_fails_instead_of_succeeding_empty() {
            let r = run(
                json!({ "id": "@payload.rowId" }),
                json!({ "rowId": "zz" }),
                json!({}),
            );
            assert!(!r.is_executed());
            assert_eq!(r.error.as_deref(), Some("Row zz not found"));
        }

        #[test]
        fn unresolved_id_binding_still_fails_loudly() {
            let r = run(json!({ "id": "@payload.nope" }), json!({}), json!({}));
            assert!(!r.is_executed());
        }
    }
}
