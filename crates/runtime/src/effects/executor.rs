//! Effect Executor
//!
//! Environment-aware effect execution with routing logic.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::bus_event::ServerEffectCode;
use orbital_core_base::runs_on::{runs_on, RunsOn};
use orbital_core_base::effect_types::{call_key_text, Effect, EffectResult, FetchDeliver, LogLevel, PersistOperation, RuntimeEnvironment};
use orbital_core_base::value::ValueMap;
use orbital_core_eval::evaluator::context::BindingContext;
use orbital_core_eval::evaluator::Evaluator;
use orbital_core_base::value::Value;
use orbital_core_base::events::types::Event;
use orbital_core_base::events::bus::SharedEventBus;
use crate::entity_access::{apply_derived, check_mutation_access, missing_required_fields, scope_fetched_rows, EntityAccessTable};
use crate::row_query::{compare_rows, compile_predicate, default_row_order, lower_object_filter, PredicateRole, RowWriteBatch, RowWriteFailureMode, RowWriteOp, RowWriteOutcome};
use orbital_core_base::row_set::{nearest_rows, RowDirection, RowNearest, RowOrder, RowPredicate, RowSetRef, RowTotalMode};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::sync::RwLock;
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
        nearest: Option<&RowNearest>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        let _ = total;
        if let Some(nearest) = nearest {
            let page = self.fetch(entity_type, id, filter, None, None, include, ctx)?;
            let rows = nearest_rows(page.rows.into_iter().map(Arc::new).collect(), nearest)?;
            let rows: Vec<Value> = rows.iter().map(|row| (**row).clone()).collect();
            return Ok(FetchPage { total: rows.len(), rows });
        }
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
        _nearest: Option<&RowNearest>,
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
/// `runProgramEffect` in `@almadar/runtime`'s program host (`host/program`).
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
    /// Where `cancel-call` goes when no service handler is set: the host, which may hold a keyed substrate or `os/`
    /// call under the key (EXEC-QUEUE-10/13).
    call_canceller: Option<Arc<dyn ServiceHandler>>,
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

mod in_memory;
mod builder;
mod run;
mod bindings;
mod persist;
mod persist_batch;
mod fetch;
mod persist_types;
use in_memory::*;
use persist_types::*;
pub use in_memory::InMemoryPersistence;
pub(crate) use persist_types::page_ordered_rows;

#[cfg(test)]
mod tests;
