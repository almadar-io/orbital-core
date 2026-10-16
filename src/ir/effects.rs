//! OIR Effect Definitions
//!
//! Effects are actions that modify state or interact with the outside world.

use serde::{Deserialize, Serialize};

use super::{OirBinding, OirExpr, OirPattern};
use crate::runtime::http_request::HttpMethod;
use crate::runtime::os::OsOp;

/// Configuration for the `emit:` option on async/reactive data operators.
///
/// Each key maps to a bus event that fires when the effect reaches that
/// lifecycle point. The set of allowed keys depends on the operator shape
/// (see validator). Unknown keys are rejected at compile time.
///
/// Payload convention:
/// - `success`    → the effect's result (fetched entity, persisted record, etc.)
/// - `failure`    → `{ error: string, code?: string }`
/// - `on_change`  → the new data (for `ref` reactive updates)
/// - `on_message` → the incoming message (for `os/watch-*` streams)
/// - `cancelled`  → `{ key: string }` (for a keyed `call-service` aborted by `cancel-call`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OirEmitConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_change: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<String>,
}

impl OirEmitConfig {
    /// True if no emit hooks are configured.
    pub fn is_empty(&self) -> bool {
        self.success.is_none()
            && self.failure.is_none()
            && self.on_change.is_none()
            && self.on_message.is_none()
            && self.cancelled.is_none()
    }

    /// Iterate over all set event names in a stable order
    /// (success, failure, on_change, on_message, cancelled).
    pub fn event_names(&self) -> impl Iterator<Item = &str> {
        [
            self.success.as_deref(),
            self.failure.as_deref(),
            self.on_change.as_deref(),
            self.on_message.as_deref(),
            self.cancelled.as_deref(),
        ]
        .into_iter()
        .flatten()
    }
}

/// Effect definitions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum OirEffect {
    /// Set a value
    Set {
        target: OirBinding,
        value: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Set a value with a dynamic path (computed at runtime)
    SetDynamic {
        target: OirExpr,
        value: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Emit an event
    Emit {
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<OirExpr>,
    },

    /// Fetch data from database
    Fetch {
        entity_type: String,
        /// Fetch a single entity by ID (generates getById)
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<OirExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<OirExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        order_by: Option<OirExpr>,
        /// Pagination offset. Expression-typed so dynamic forms like
        /// `(* (- @payload.page 1) @config.pageSize)` survive lowering.
        #[serde(skip_serializing_if = "Option::is_none")]
        offset: Option<OirExpr>,
        /// Pagination limit. Expression-typed for the same reason as
        /// `offset` — schema authors use `@config.pageSize` etc.
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<OirExpr>,
        /// Relation fields to populate via populateRelations()
        #[serde(skip_serializing_if = "Option::is_none")]
        include: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Persist data to database
    Persist {
        operation: OirPersistOp,
        entity_type: String,
        data: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// `["persist", "batch", <ops>, { emit }?]`: one effect writing many rows.
    /// `ops` evaluates to an array of `["create"|"update"|"delete", …]` tuples;
    /// `entities` is the statically-readable set the tuples name (empty when
    /// `ops` is an opaque expression).
    PersistBatch {
        ops: OirExpr,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    // ── Resource operators (ref/deref/swap/watch/atomic) ──
    /// Ref: reactive subscription to entity data.
    /// Server: initial DB query (same as Fetch). Client: subscribes to EntityStore.
    Ref {
        entity_type: String,
        /// Subscribe to a single entity by ID
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<OirExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<OirExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        order_by: Option<OirExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<OirExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        include: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Deref: snapshot read from entity store (pure, no subscription).
    /// Returns current value. Usable in guards and expressions.
    Deref {
        entity_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<OirExpr>,
    },

    /// Swap: atomic read-modify-write with CAS retry.
    /// Server: CAS loop (read, transform, write WHERE version=N, retry on conflict).
    Swap {
        entity_type: String,
        id: OirExpr,
        transform: OirExpr,
    },

    /// Watch: register side effects to run when entity data changes.
    /// Client-only. Callback fires on EntityStore advance.
    Watch {
        entity_type: String,
        effects: Vec<OirExpr>,
    },

    /// Atomic: transaction grouping (all-or-nothing).
    /// Server: DB transaction. Client: advances EntityStore once for whole group.
    Atomic { effects: Vec<OirExpr> },

    /// Navigate to a path
    Navigate {
        path: OirExpr,
        #[serde(skip_serializing_if = "Option::is_none")]
        params: Option<OirExpr>,
        /// Label for the navigation-stack entry the client pushes for the
        /// target page (e.g. a record title known at push time). Absent →
        /// the target page's declared/derived label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        crumb: Option<OirExpr>,
    },

    /// Pop the orbital-scoped navigation stack and navigate to the previous
    /// entry. Client-only; the stack is client-session state seeded from
    /// declared page-path prefixes on cold load.
    NavigateBack,

    /// Render UI pattern to a slot
    RenderUi { slot: String, pattern: OirPattern },

    /// Call external service
    CallService {
        service: String,
        method: String,
        args: OirExpr,
        /// String-typed expression naming this call so `cancel-call` can abort it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// One HTTP request (`runtime::http_request`). `origin` is a literal or a `url` knob, `request`
    /// carries `query`/`headers`/one body/`auth`/`timeoutMs`/`responseAs`/`paginate`/`stream`.
    /// Server-only, keyed and routed like `call-service`.
    Http {
        method: HttpMethod,
        origin: OirExpr,
        path: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Abort the in-flight keyed `call-service` or `http` started with this key. A
    /// no-op when none is in flight. Server-only, like `call-service`.
    CancelCall { key: OirExpr },

    /// `bytes` of journaled host CSPRNG output; success carries `{ value }` (base64url, no padding).
    /// Server-only.
    RandomBytes {
        bytes: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// One `os/<op>` operation (`docs/Almadar_OS_Operators.md`): the host answers it, keyed and routed like
    /// `http`; a streaming op delivers its events to `emit.on_message`. Server-only.
    Os {
        op: OsOp,
        args: Vec<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Whether the host holds the credential named by the `secret` knob `credential`; success
    /// carries `{ connected }`. Server-only.
    CredentialStatus {
        credential: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Drop the credential named by the `secret` knob `credential`. Server-only.
    CredentialForget {
        credential: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Start an OAuth authorization for `client`; success carries `{ authUrl, state }`. Server-only.
    OAuthAuthorize {
        client: OirExpr,
        options: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// The signed-in user's connection to the provider word `provider`. Server-only.
    OAuthConnectionStatus {
        provider: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Drop the signed-in user's connection to the provider word `provider`. Server-only.
    OAuthConnectionForget {
        provider: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Spawn a game entity
    Spawn { entity_type: String, props: OirExpr },

    /// Despawn a game entity
    Despawn { entity_id: OirExpr },

    /// Log a message
    Log {
        level: OirLogLevel,
        message: OirExpr,
    },

    /// Custom effect
    Custom {
        name: String,
        args: crate::ir::OirFields,
        /// Success/failure event dispatch for substrate ops (llm/*, workspace/*,
        /// …), mirroring `Fetch`/`Persist`/`CallService`. The effect fires
        /// `getSubstrateService().invoke(name, args)` and dispatches the
        /// success/failure event on completion — the `.lolo` async model
        /// (C-EMIT-SUBSTRATE-OPS-MARKER). `None` for non-substrate Custom
        /// effects and legacy fire-and-forget substrate calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    // OS trigger effects (server-side only)
    /// Watch files matching glob pattern
    OsWatchFiles {
        glob: OirExpr,
        #[serde(skip_serializing_if = "Option::is_none")]
        options: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Monitor a process by name
    OsWatchProcess {
        name: OirExpr,
        #[serde(skip_serializing_if = "Option::is_none")]
        subcommand: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Monitor a port for open/close
    OsWatchPort {
        port: OirExpr,
        #[serde(skip_serializing_if = "Option::is_none")]
        protocol: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Intercept HTTP responses matching pattern
    OsWatchHttp {
        url_pattern: OirExpr,
        #[serde(skip_serializing_if = "Option::is_none")]
        method: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Register a cron schedule
    OsWatchCron {
        expression: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Register an OS signal handler
    OsWatchSignal {
        signal: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Watch an environment variable for changes
    OsWatchEnv {
        variable: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Configure debounce for an event type
    OsDebounce { ms: OirExpr, event_type: OirExpr },

    /// Stream an LLM/HTTP response: fires on_message per chunk, success on completion,
    /// failure on error. Server-only (mirrors Fetch's field shape, adds on_message emit).
    FetchStream {
        entity_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<OirExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Send an orbital event to the server over WebSocket. Client-only.
    /// Produces the wire message { type: 'ORBITAL_EVENT', payload: { orbital, event, payload, entityId? } }
    /// which `setupEventBroadcast` decodes → server bus → `processOrbitalEvent`.
    SendServer {
        event: OirExpr,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<OirExpr>,
    },

    // Browser device effects (client host path)
    //
    // User-initiated, async device APIs. Client-only; resolve via `emit` with
    // the uniform `{ result }` payload on success and `{ error }` on failure,
    // mirroring `CallService`/`Fetch`. Never executed server-side: the
    // classifier returns `ServerAction::ReturnToClient` and the JS runtime
    // delegates to the real `window`/`navigator` APIs.
    /// Open the OS file picker. Success `result` = `{ files: [{name,size,type,lastModified}] }`.
    BrowserOpenFilePicker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Read text from the system clipboard. Success `result` = `{ text }`.
    BrowserClipboardRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Write text to the system clipboard. Success `result` = `{ text }` (echo).
    BrowserClipboardWrite {
        text: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Read the current device position. Success `result` = `{ latitude, longitude, accuracy }`.
    BrowserGeolocationCurrent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Read the requested page facts, in list order. Success `result` is `PAGE_FACTS_SHAPE`:
    /// `{ available, hostname, referrer, screenWidth }`; a fact that was not requested keeps its default.
    BrowserPageFacts {
        fields: Vec<PageFactField>,
        /// String-typed expression echoed as `key` in the success and failure payloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// POST `request.text` (the request object) to the absolute `url` as `text/plain` through the browser's beacon queue,
    /// falling back to one `keepalive` fetch. Built and egress-checked by the one http driver.
    BrowserBeacon {
        url: OirExpr,
        request: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Subscribe this browser to Web Push (registers the shared service worker,
    /// fetches the host's VAPID public key, calls PushManager.subscribe).
    /// Success `result` = `{ endpoint, p256dh, auth }` — flat strings, ready to
    /// persist and to hand back to the `push` service as
    /// `subscription: { endpoint, keys: { p256dh, auth } }`.
    BrowserPushSubscribe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },
}

/// One page fact `browser/page-facts` can read. The closed set; list order is read order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageFactField {
    Hostname,
    Referrer,
    ScreenWidth,
}

impl PageFactField {
    /// Every fact, in the spelling a program writes it.
    pub const ALL: [PageFactField; 3] = [Self::Hostname, Self::Referrer, Self::ScreenWidth];

    /// The bare word a program writes (`hostname`, `referrer`, `screenWidth`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hostname => "hostname",
            Self::Referrer => "referrer",
            Self::ScreenWidth => "screenWidth",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == word)
    }
}

/// The canonical set of substrate-operator prefixes. A substrate operator is
/// a `Custom` effect (e.g. `llm/embed`, `memory/put`) that is routed to the
/// substrate service instead of being compiled as a plain call — see
/// `OirEffect::Custom` and C-EMIT-SUBSTRATE-OPS-MARKER.
///
/// This is the single source of truth: the compiler (`resolve.rs`), the TS
/// shell (`operator_names.rs`), and the Python shell (`trait_generator.rs`)
/// all delegate here instead of keeping private in-sync copies.
const SUBSTRATE_OPERATOR_PREFIXES: &[&str] = &[
    "llm/",
    "memory/",
    "trace/",
    "session/",
    "workspace/",
];


/// Whether `name` is a substrate operator (one of the `SUBSTRATE_OPERATOR_PREFIXES`
/// prefixes, or an effect the installed operator table routes to the program host). The canonical predicate —
/// shells and the compiler must call this rather than re-deriving the set.
pub fn is_substrate_operator(name: &str) -> bool {
    SUBSTRATE_OPERATOR_PREFIXES
        .iter()
        .any(|p| name.starts_with(p))
        || crate::language::registry::is_program_host_effect(name)
}

impl OirEffect {
    // Constructor methods for common effects

    /// Create a set effect
    pub fn set(target: OirBinding, value: OirExpr) -> Self {
        Self::Set {
            target,
            value,
            emit: None,
        }
    }

    /// Create a set effect with a dynamic path (computed at runtime)
    pub fn set_dynamic(target: OirExpr, value: OirExpr) -> Self {
        Self::SetDynamic {
            target,
            value,
            emit: None,
        }
    }

    /// Create an emit effect
    pub fn emit(event: impl Into<String>) -> Self {
        Self::Emit {
            event: event.into(),
            payload: None,
        }
    }

    /// Create an emit effect with payload
    pub fn emit_with_payload(event: impl Into<String>, payload: OirExpr) -> Self {
        Self::Emit {
            event: event.into(),
            payload: Some(payload),
        }
    }

    /// Create a fetch effect (list all)
    pub fn fetch(entity_type: impl Into<String>) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: None,
            filter: None,
            order_by: None,
            offset: None,
            limit: None,
            include: None,
            emit: None,
        }
    }

    /// Create a fetch effect by ID (single entity)
    pub fn fetch_by_id(entity_type: impl Into<String>, id: OirExpr) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: Some(id),
            filter: None,
            order_by: None,
            offset: None,
            limit: None,
            include: None,
            emit: None,
        }
    }

    /// Create a fetch effect with filter
    pub fn fetch_with_filter(entity_type: impl Into<String>, filter: OirExpr) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: None,
            filter: Some(filter),
            order_by: None,
            offset: None,
            limit: None,
            include: None,
            emit: None,
        }
    }

    /// Create a fetch effect with filter, sort, and limit
    pub fn fetch_with_options(
        entity_type: impl Into<String>,
        filter: Option<OirExpr>,
        order_by: Option<OirExpr>,
        limit: Option<OirExpr>,
    ) -> Self {
        Self::Fetch {
            entity_type: entity_type.into(),
            id: None,
            filter,
            order_by,
            offset: None,
            limit,
            include: None,
            emit: None,
        }
    }

    // ── Resource operator constructors ──

    /// Create a ref effect (reactive entity subscription)
    pub fn ref_entity(entity_type: impl Into<String>) -> Self {
        Self::Ref {
            entity_type: entity_type.into(),
            id: None,
            filter: None,
            order_by: None,
            limit: None,
            include: None,
            emit: None,
        }
    }

    /// Create a deref effect (snapshot read)
    pub fn deref_entity(entity_type: impl Into<String>) -> Self {
        Self::Deref {
            entity_type: entity_type.into(),
            id: None,
        }
    }

    /// Create a deref effect with ID (single-record read)
    pub fn deref_by_id(entity_type: impl Into<String>, id: OirExpr) -> Self {
        Self::Deref {
            entity_type: entity_type.into(),
            id: Some(id),
        }
    }

    /// Create a swap effect (atomic CAS)
    pub fn swap(entity_type: impl Into<String>, id: OirExpr, transform: OirExpr) -> Self {
        Self::Swap {
            entity_type: entity_type.into(),
            id,
            transform,
        }
    }

    /// Create a watch effect (react to entity changes)
    pub fn watch(entity_type: impl Into<String>, effects: Vec<OirExpr>) -> Self {
        Self::Watch {
            entity_type: entity_type.into(),
            effects,
        }
    }

    /// Create an atomic effect (transaction group)
    pub fn atomic(effects: Vec<OirExpr>) -> Self {
        Self::Atomic { effects }
    }

    /// Create a persist batch effect
    pub fn persist_batch(ops: OirExpr, entities: Vec<String>) -> Self {
        Self::PersistBatch {
            ops,
            entities,
            emit: None,
        }
    }

    /// Create a persist create effect
    pub fn create(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Create,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist upsert effect (create-or-replace by id)
    pub fn upsert(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Upsert,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist update effect
    pub fn update(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Update,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist delete effect
    pub fn delete(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Delete,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist read effect
    pub fn read(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Read,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist list effect
    pub fn list(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::List,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a persist query effect
    pub fn query(entity_type: impl Into<String>, data: OirExpr) -> Self {
        Self::Persist {
            operation: OirPersistOp::Query,
            entity_type: entity_type.into(),
            data,
            emit: None,
        }
    }

    /// Create a navigate effect
    pub fn navigate(path: OirExpr) -> Self {
        Self::Navigate {
            path,
            params: None,
            crumb: None,
        }
    }

    /// Create a navigate-back effect
    pub fn navigate_back() -> Self {
        Self::NavigateBack
    }

    /// Create a render-ui effect
    pub fn render_ui(slot: impl Into<String>, pattern: OirPattern) -> Self {
        Self::RenderUi {
            slot: slot.into(),
            pattern,
        }
    }

    /// Create a log effect
    pub fn log(level: OirLogLevel, message: OirExpr) -> Self {
        Self::Log { level, message }
    }

    /// Create a spawn effect
    pub fn spawn(entity_type: impl Into<String>, props: OirExpr) -> Self {
        Self::Spawn {
            entity_type: entity_type.into(),
            props,
        }
    }

    /// Create a despawn effect
    pub fn despawn(entity_id: OirExpr) -> Self {
        Self::Despawn { entity_id }
    }

    /// Create a call-service effect
    pub fn call_service(
        service: impl Into<String>,
        method: impl Into<String>,
        args: OirExpr,
    ) -> Self {
        Self::CallService {
            service: service.into(),
            method: method.into(),
            args,
            key: None,
            emit: None,
        }
    }

    /// Create an http effect
    pub fn http(method: HttpMethod, origin: OirExpr, path: OirExpr) -> Self {
        Self::Http {
            method,
            origin,
            path,
            request: None,
            key: None,
            emit: None,
        }
    }

    /// Create a cancel-call effect
    pub fn cancel_call(key: OirExpr) -> Self {
        Self::CancelCall { key }
    }

    /// Create a random-bytes effect
    pub fn random_bytes(bytes: OirExpr, emit: Option<OirEmitConfig>) -> Self {
        Self::RandomBytes { bytes, key: None, emit }
    }

    /// Create an `os/<op>` effect
    pub fn os(op: OsOp, args: Vec<OirExpr>, key: Option<OirExpr>, emit: Option<OirEmitConfig>) -> Self {
        Self::Os { op, args, key, emit }
    }

    /// Create a credential-status effect
    pub fn credential_status(credential: OirExpr, emit: Option<OirEmitConfig>) -> Self {
        Self::CredentialStatus { credential, emit }
    }

    /// Create a credential-forget effect
    pub fn credential_forget(credential: OirExpr, emit: Option<OirEmitConfig>) -> Self {
        Self::CredentialForget { credential, emit }
    }

    /// Get the effect kind as a string
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Set { .. } => "set",
            Self::SetDynamic { .. } => "set-dynamic",
            Self::Emit { .. } => "emit",
            Self::Fetch { .. } => "fetch",
            Self::Persist { .. } | Self::PersistBatch { .. } => "persist",
            Self::Ref { .. } => "ref",
            Self::Deref { .. } => "deref",
            Self::Swap { .. } => "swap",
            Self::Watch { .. } => "watch",
            Self::Atomic { .. } => "atomic",
            Self::Navigate { .. } => "navigate",
            Self::NavigateBack => "navigate-back",
            Self::RenderUi { .. } => "render-ui",
            Self::CallService { .. } => "call-service",
            Self::Http { .. } => "http",
            Self::CancelCall { .. } => "cancel-call",
            Self::RandomBytes { .. } => "random-bytes",
            Self::Os { op, .. } => op.name(),
            Self::CredentialStatus { .. } => "credential-status",
            Self::CredentialForget { .. } => "credential-forget",
            Self::OAuthAuthorize { .. } => "oauth-authorize",
            Self::OAuthConnectionStatus { .. } => "oauth-connection-status",
            Self::OAuthConnectionForget { .. } => "oauth-connection-forget",
            Self::Spawn { .. } => "spawn",
            Self::Despawn { .. } => "despawn",
            Self::Log { .. } => "log",
            Self::Custom { .. } => "custom",
            Self::OsWatchFiles { .. } => "os/watch-files",
            Self::OsWatchProcess { .. } => "os/watch-process",
            Self::OsWatchPort { .. } => "os/watch-port",
            Self::OsWatchHttp { .. } => "os/watch-http",
            Self::OsWatchCron { .. } => "os/watch-cron",
            Self::OsWatchSignal { .. } => "os/watch-signal",
            Self::OsWatchEnv { .. } => "os/watch-env",
            Self::OsDebounce { .. } => "os/debounce",
            Self::FetchStream { .. } => "fetch-stream",
            Self::SendServer { .. } => "send-server",
            Self::BrowserOpenFilePicker { .. } => "browser/open-file-picker",
            Self::BrowserClipboardRead { .. } => "browser/clipboard-read",
            Self::BrowserClipboardWrite { .. } => "browser/clipboard-write",
            Self::BrowserGeolocationCurrent { .. } => "browser/geolocation-current",
            Self::BrowserPushSubscribe { .. } => "browser/push-subscribe",
            Self::BrowserPageFacts { .. } => "browser/page-facts",
            Self::BrowserBeacon { .. } => "browser/beacon",
        }
    }

    /// Whether a client host runs this effect (its `runsOn` is not `server`).
    pub fn is_client_effect(&self) -> bool {
        crate::effects::effect_runs_on(self) != crate::effects::RunsOn::Server
    }

    /// Whether a server runs this effect (its `runsOn` is not `client`).
    pub fn is_server_effect(&self) -> bool {
        crate::effects::effect_runs_on(self) != crate::effects::RunsOn::Client
    }

    /// Collect all bindings used in this effect
    pub(crate) fn collect_bindings_recursive<'a>(&'a self, bindings: &mut Vec<&'a OirBinding>) {
        match self {
            Self::Set { target, value, .. } => {
                bindings.push(target);
                value
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::SetDynamic { target, value, .. } => {
                // Dynamic target is an expression, collect its bindings
                target
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                value
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Emit { payload, .. } => {
                if let Some(p) = payload {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Fetch { id, filter, .. } => {
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(f) = filter {
                    f.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Persist { data, .. } => {
                data.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::PersistBatch { ops, .. } => {
                ops.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Ref { id, filter, .. } => {
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(f) = filter {
                    f.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Deref { id, .. } => {
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Swap { id, transform, .. } => {
                id.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                transform
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Watch { effects, .. } => {
                for eff in effects {
                    eff.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Atomic { effects } => {
                for eff in effects {
                    eff.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Navigate {
                path,
                params,
                crumb,
            } => {
                path.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(p) = params {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(c) = crumb {
                    c.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::NavigateBack => {}
            Self::CallService { args, key, .. } => {
                args.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(k) = key {
                    k.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::Http {
                origin,
                path,
                request,
                key,
                ..
            } => {
                for expr in [origin, path].into_iter().chain(request).chain(key) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::CancelCall { key } => {
                key.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Os { args, key, .. } => {
                for expr in args.iter().chain(key) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OAuthAuthorize { client, options, key, .. } => {
                for expr in [Some(client), Some(options), key.as_ref()].into_iter().flatten() {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::RandomBytes { bytes, key, .. } => {
                for expr in std::iter::once(bytes).chain(key.as_ref()) {
                    expr.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::CredentialStatus {
                credential: expr, ..
            }
            | Self::CredentialForget {
                credential: expr, ..
            }
            | Self::OAuthConnectionStatus {
                provider: expr, ..
            }
            | Self::OAuthConnectionForget {
                provider: expr, ..
            } => {
                expr.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Spawn { props, .. } => {
                props
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Despawn { entity_id } => {
                entity_id
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Log { message, .. } => {
                message
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::Custom { args, .. } => {
                for arg in args.values() {
                    arg.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::RenderUi { .. } => {}
            Self::OsWatchFiles { glob, options, .. } => {
                glob.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(o) = options {
                    o.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchProcess {
                name, subcommand, ..
            } => {
                name.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(s) = subcommand {
                    s.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchPort { port, protocol, .. } => {
                port.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(p) = protocol {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchHttp {
                url_pattern,
                method,
                ..
            } => {
                url_pattern
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(m) = method {
                    m.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::OsWatchCron { expression, .. } => {
                expression
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::OsWatchSignal { signal, .. } => {
                signal
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::OsWatchEnv { variable, .. } => {
                variable
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::OsDebounce { ms, event_type } => {
                ms.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                event_type
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::FetchStream { id, filter, .. } => {
                if let Some(id_expr) = id {
                    id_expr
                        .collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
                if let Some(f) = filter {
                    f.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::SendServer { event, payload } => {
                event
                    .collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
                if let Some(p) = payload {
                    p.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserOpenFilePicker { options, .. } => {
                if let Some(o) = options {
                    o.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserClipboardRead { .. } => {}
            Self::BrowserPushSubscribe { .. } => {}
            Self::BrowserPageFacts { key, .. } => {
                if let Some(k) = key {
                    k.collect_bindings().into_iter().for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserBeacon { url, request, key, .. } => {
                for e in [Some(url), Some(request), key.as_ref()].into_iter().flatten() {
                    e.collect_bindings().into_iter().for_each(|b| bindings.push(b));
                }
            }
            Self::BrowserClipboardWrite { text, .. } => {
                text.collect_bindings()
                    .into_iter()
                    .for_each(|b| bindings.push(b));
            }
            Self::BrowserGeolocationCurrent { options, .. } => {
                if let Some(o) = options {
                    o.collect_bindings()
                        .into_iter()
                        .for_each(|b| bindings.push(b));
                }
            }
        }
    }

    /// Every child expression this effect carries — the one enumeration of
    /// expr-bearing fields, used by `OirExpr::calls_function` to descend
    /// through nested effects. `RenderUi` carries `OirValue` config (render
    /// pipeline), deliberately not part of this surface. Exhaustive match:
    /// a new variant is compiler-forced to declare its expressions here.
    pub fn child_exprs(&self) -> Vec<&OirExpr> {
        let mut out: Vec<&OirExpr> = Vec::new();
        fn push_opt<'a>(out: &mut Vec<&'a OirExpr>, e: &'a Option<OirExpr>) {
            if let Some(e) = e.as_ref() {
                out.push(e);
            }
        }
        match self {
            Self::Set { value, .. } => out.push(value),
            Self::SetDynamic { target, value, .. } => {
                out.push(target);
                out.push(value);
            }
            Self::Emit { payload, .. } => push_opt(&mut out, payload),
            Self::Fetch {
                id,
                filter,
                order_by,
                offset,
                limit,
                ..
            } => {
                push_opt(&mut out, id);
                push_opt(&mut out, filter);
                push_opt(&mut out, order_by);
                push_opt(&mut out, offset);
                push_opt(&mut out, limit);
            }
            Self::Persist { data, .. } => out.push(data),
            Self::PersistBatch { ops, .. } => out.push(ops),
            Self::Ref {
                id,
                filter,
                order_by,
                ..
            } => {
                push_opt(&mut out, id);
                push_opt(&mut out, filter);
                push_opt(&mut out, order_by);
            }
            Self::Deref { id, .. } => push_opt(&mut out, id),
            Self::Swap { id, transform, .. } => {
                out.push(id);
                out.push(transform);
            }
            Self::Watch { effects, .. } | Self::Atomic { effects } => out.extend(effects.iter()),
            Self::Navigate {
                path,
                params,
                crumb,
            } => {
                out.push(path);
                push_opt(&mut out, params);
                push_opt(&mut out, crumb);
            }
            Self::NavigateBack => {}
            Self::RenderUi { .. } => {}
            Self::CallService { args, key, .. } => {
                out.push(args);
                push_opt(&mut out, key);
            }
            Self::Http {
                origin,
                path,
                request,
                key,
                ..
            } => {
                out.push(origin);
                out.push(path);
                push_opt(&mut out, request);
                push_opt(&mut out, key);
            }
            Self::CancelCall { key } => out.push(key),
            Self::Os { args, key, .. } => {
                out.extend(args.iter());
                push_opt(&mut out, key);
            }
            Self::RandomBytes { bytes, key, .. } => {
                out.push(bytes);
                push_opt(&mut out, key);
            }
            Self::CredentialStatus {
                credential: expr, ..
            }
            | Self::CredentialForget {
                credential: expr, ..
            }
            | Self::OAuthConnectionStatus {
                provider: expr, ..
            }
            | Self::OAuthConnectionForget {
                provider: expr, ..
            } => out.push(expr),
            Self::OAuthAuthorize { client, options, key, .. } => {
                out.push(client);
                out.push(options);
                push_opt(&mut out, key);
            }
            Self::Spawn { props, .. } => out.push(props),
            Self::Despawn { entity_id } => out.push(entity_id),
            Self::Log { message, .. } => out.push(message),
            Self::Custom { args, .. } => out.extend(args.values()),
            Self::OsWatchFiles { glob, options, .. } => {
                out.push(glob);
                push_opt(&mut out, options);
            }
            Self::OsWatchProcess {
                name, subcommand, ..
            } => {
                out.push(name);
                push_opt(&mut out, subcommand);
            }
            Self::OsWatchPort { port, protocol, .. } => {
                out.push(port);
                push_opt(&mut out, protocol);
            }
            Self::OsWatchHttp {
                url_pattern,
                method,
                ..
            } => {
                out.push(url_pattern);
                push_opt(&mut out, method);
            }
            Self::OsWatchCron { expression, .. } => out.push(expression),
            Self::OsWatchSignal { signal, .. } => out.push(signal),
            Self::OsWatchEnv { variable, .. } => out.push(variable),
            Self::OsDebounce { ms, event_type } => {
                out.push(ms);
                out.push(event_type);
            }
            Self::FetchStream { id, filter, .. } => {
                push_opt(&mut out, id);
                push_opt(&mut out, filter);
            }
            Self::SendServer { event, payload } => {
                out.push(event);
                push_opt(&mut out, payload);
            }
            Self::BrowserOpenFilePicker { options, .. } => push_opt(&mut out, options),
            Self::BrowserClipboardRead { .. } => {}
            Self::BrowserClipboardWrite { text, .. } => out.push(text),
            Self::BrowserGeolocationCurrent { options, .. } => push_opt(&mut out, options),
            Self::BrowserPushSubscribe { .. } => {}
            Self::BrowserPageFacts { key, .. } => push_opt(&mut out, key),
            Self::BrowserBeacon { url, request, key, .. } => {
                out.push(url);
                out.push(request);
                push_opt(&mut out, key);
            }
        }
        out
    }
}

/// Persist operation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OirPersistOp {
    Create,
    Upsert,
    Read,
    Update,
    Delete,
    List,
    Query,
}

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum OirLogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Execution environment for effects
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OirExecutionEnv {
    Client,
    Server,
}

/// Effect execution classification
#[derive(Debug, Clone)]
pub struct EffectClassification {
    pub effect: OirEffect,
    pub client_action: ClientAction,
    pub server_action: ServerAction,
}

/// What the client should do with an effect
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAction {
    /// Execute the effect locally
    Execute,
    /// Ignore (server handles it)
    Ignore,
}

/// What the server should do with an effect
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAction {
    /// Execute the effect on server
    Execute,
    /// Return to client for execution
    ReturnToClient,
}

impl OirEffect {
    /// Classify this effect for client/server execution
    pub fn classify(&self) -> EffectClassification {
        let site = crate::effects::effect_runs_on(self);
        let client_action = if site == crate::effects::RunsOn::Server {
            ClientAction::Ignore
        } else {
            ClientAction::Execute
        };
        let server_action = if site == crate::effects::RunsOn::Client {
            ServerAction::ReturnToClient
        } else {
            ServerAction::Execute
        };

        EffectClassification {
            effect: self.clone(),
            client_action,
            server_action,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_host_effects_are_substrate_operators_through_the_installed_table() {
        assert!(is_substrate_operator("behavior/resolve"));
        assert!(is_substrate_operator("program/eval"));
        assert!(
            is_substrate_operator("llm/generate"),
            "a prefixed substrate op"
        );
        assert!(
            !is_substrate_operator("behavior/apply"),
            "control: a pure behavior operator"
        );
        assert!(!is_substrate_operator("persist"), "control: a core effect");
    }

    #[test]
    fn test_effect_kind() {
        assert_eq!(OirEffect::emit("test").kind(), "emit");
        assert_eq!(OirEffect::fetch("Task").kind(), "fetch");
        assert_eq!(
            OirEffect::render_ui("main", super::super::OirPattern::new("entity-table")).kind(),
            "render-ui"
        );
    }

    #[test]
    fn test_effect_classification() {
        let fetch = OirEffect::fetch("Task");
        let class = fetch.classify();
        assert_eq!(class.client_action, ClientAction::Ignore);
        assert_eq!(class.server_action, ServerAction::Execute);

        let render = OirEffect::render_ui("main", super::super::OirPattern::new("entity-table"));
        let class = render.classify();
        assert_eq!(class.client_action, ClientAction::Execute);
        assert_eq!(class.server_action, ServerAction::ReturnToClient);
    }

    #[test]
    fn test_is_client_server_effect() {
        let fetch = OirEffect::fetch("Task");
        assert!(!fetch.is_client_effect());
        assert!(fetch.is_server_effect());

        let navigate = OirEffect::navigate(OirExpr::string("/home"));
        assert!(navigate.is_client_effect());
        assert!(!navigate.is_server_effect());

        let emit = OirEffect::emit("test");
        assert!(emit.is_client_effect());
        assert!(emit.is_server_effect());
    }

    #[test]
    fn emit_config_roundtrip_success_failure() {
        let cfg = OirEmitConfig {
            success: Some("LOADED".into()),
            failure: Some("LOAD_FAILED".into()),
            on_change: None,
            on_message: None,
            cancelled: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        // camelCase + skips None fields
        assert_eq!(json, r#"{"success":"LOADED","failure":"LOAD_FAILED"}"#);

        let round: OirEmitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(round, cfg);
    }

    #[test]
    fn emit_config_roundtrip_watch_shape() {
        let cfg = OirEmitConfig {
            success: None,
            failure: Some("SUBSCRIBE_FAILED".into()),
            on_change: None,
            on_message: Some("PUSH_RECEIVED".into()),
            cancelled: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""failure":"SUBSCRIBE_FAILED""#));
        assert!(json.contains(r#""onMessage":"PUSH_RECEIVED""#));
        let round: OirEmitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(round, cfg);
    }

    #[test]
    fn emit_config_default_is_empty() {
        let cfg = OirEmitConfig::default();
        assert!(cfg.is_empty());
        assert_eq!(serde_json::to_string(&cfg).unwrap(), "{}");
    }

    #[test]
    fn emit_config_event_names_iter_is_stable() {
        let cfg = OirEmitConfig {
            success: Some("A".into()),
            failure: Some("B".into()),
            on_change: None,
            on_message: Some("C".into()),
            cancelled: Some("D".into()),
        };
        let names: Vec<&str> = cfg.event_names().collect();
        assert_eq!(names, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn fetch_with_emit_serializes_field() {
        let cfg = OirEmitConfig {
            success: Some("PATIENT_LOADED".into()),
            failure: Some("PATIENT_LOAD_FAILED".into()),
            ..Default::default()
        };
        let eff = OirEffect::Fetch {
            entity_type: "Patient".into(),
            id: None,
            filter: None,
            order_by: None,
            limit: None,
            offset: None,
            include: None,
            emit: Some(cfg.clone()),
        };
        let json = serde_json::to_value(&eff).unwrap();
        assert_eq!(
            json["emit"],
            serde_json::json!({
                "success": "PATIENT_LOADED",
                "failure": "PATIENT_LOAD_FAILED"
            })
        );

        // Round-trip
        let round: OirEffect = serde_json::from_value(json).unwrap();
        if let OirEffect::Fetch { emit, .. } = round {
            assert_eq!(emit, Some(cfg));
        } else {
            panic!("expected Fetch");
        }
    }

    #[test]
    fn fetch_without_emit_skips_field() {
        let eff = OirEffect::fetch("Patient");
        let json = serde_json::to_value(&eff).unwrap();
        assert!(
            json.get("emit").is_none(),
            "emit field should be skipped when None, got: {}",
            json
        );
    }

    #[test]
    fn legacy_fetch_json_without_emit_deserializes() {
        // Schemas authored before this field existed must still load.
        let json = serde_json::json!({
            "kind": "fetch",
            "entity_type": "Patient",
        });
        let eff: OirEffect = serde_json::from_value(json).unwrap();
        if let OirEffect::Fetch { emit, .. } = eff {
            assert!(emit.is_none());
        } else {
            panic!("expected Fetch");
        }
    }

    #[test]
    fn emit_config_cancelled_roundtrips_and_counts_as_set() {
        let cfg = OirEmitConfig {
            cancelled: Some("CLASSIFY_CANCELLED".into()),
            ..Default::default()
        };
        assert!(!cfg.is_empty());
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(json, r#"{"cancelled":"CLASSIFY_CANCELLED"}"#);
        let round: OirEmitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(round, cfg);
    }

    #[test]
    fn keyed_call_service_roundtrips_and_exposes_key_binding() {
        let eff = OirEffect::CallService {
            service: "llm".into(),
            method: "classifyMany".into(),
            args: OirExpr::object(crate::ir::OirFields::new()),
            key: Some(OirExpr::string("batch-1")),
            emit: Some(OirEmitConfig {
                cancelled: Some("CANCELLED".into()),
                ..Default::default()
            }),
        };
        let json = serde_json::to_value(&eff).unwrap();
        assert!(json.get("key").is_some());
        assert_eq!(json["emit"]["cancelled"], "CANCELLED");
        let round: OirEffect = serde_json::from_value(json).unwrap();
        assert_eq!(round, eff);
        assert_eq!(eff.child_exprs().len(), 2);
    }

    #[test]
    fn unkeyed_call_service_skips_key_field() {
        let eff = OirEffect::call_service("llm", "generate", OirExpr::object(crate::ir::OirFields::new()));
        let json = serde_json::to_value(&eff).unwrap();
        assert!(json.get("key").is_none(), "{json}");
        assert_eq!(eff.child_exprs().len(), 1);
    }

    #[test]
    fn cancel_call_is_a_server_effect_with_a_key_child() {
        let eff = OirEffect::cancel_call(OirExpr::string("batch-1"));
        assert_eq!(eff.kind(), "cancel-call");
        assert!(eff.is_server_effect());
        assert!(!eff.is_client_effect());
        assert_eq!(eff.child_exprs().len(), 1);
        let round: OirEffect = serde_json::from_value(serde_json::to_value(&eff).unwrap()).unwrap();
        assert_eq!(round, eff);
    }
}
