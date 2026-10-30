//! OIR Effect Definitions
//!
//! Effects are actions that modify state or interact with the outside world.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::ir::expr::{OirBinding, OirExpr};
use crate::ir::types::OirPattern;
use crate::os::OsOp;

/// Configuration for the `emit:` option on async/reactive data operators.
///
/// Each key maps to a bus event that fires when the effect reaches that
/// lifecycle point. The set of allowed keys depends on the operator shape
/// (see validator). Unknown keys are rejected at compile time.
///
/// Payload convention:
/// - `success`    → the effect's result (fetched entity, persisted record, etc.)
/// - `failure`    → `{ error: string, code?: string }`
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
    pub on_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<String>,
}

impl OirEmitConfig {
    /// True if no emit hooks are configured.
    pub fn is_empty(&self) -> bool {
        self.success.is_none()
            && self.failure.is_none()
            && self.on_message.is_none()
            && self.cancelled.is_none()
    }

    /// Iterate over all set event names in a stable order
    /// (success, failure, on_message, cancelled).
    pub fn event_names(&self) -> impl Iterator<Item = &str> {
        [
            self.success.as_deref(),
            self.failure.as_deref(),
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
        /// `{ field, vector, k }` as one object expression: the nearest `k` rows by cosine similarity (EXEC-FETCH-2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nearest: Option<OirExpr>,
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

    /// One credential administration effect (`crate::credential_admin`); `request` is the closed parameter object (for
    /// the request object). Server-only.
    CredentialAdmin {
        op: crate::credential_admin::CredentialAdminOp,
        request: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// The kernel's verdict on one access policy; `request` is `{ entity, action, row, viewer }`, success carries
    /// `{ allowed }`. Server-only.
    AccessCheck {
        request: OirExpr,
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
        args: crate::ir::expr::OirFields,
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
    /// `{ available, hostname, referrer, screenWidth, href }`; a fact that was not requested keeps its default.
    BrowserPageFacts {
        fields: Vec<PageFactField>,
        /// String-typed expression echoed as `key` in the success and failure payloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Ask the host's on-device model for a reply to `text` that satisfies the JSON Schema text `schema`. Success
    /// `result` is `PROMPT_SHAPE`: `{ reply, key }`; failure is `MODEL_FAILURE_SHAPE`: `{ error, kind, key }`.
    BrowserPrompt {
        text: OirExpr,
        schema: OirExpr,
        /// String-typed expression echoed as `key`, and the name `cancel-call` aborts the ask by.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// Ask the host's on-device embedder for one vector per text. Success `result` is `EMBED_SHAPE`:
    /// `{ embeddings, key }`; failure is `MODEL_FAILURE_SHAPE`.
    BrowserEmbed {
        texts: OirExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<OirExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<OirEmitConfig>,
    },

    /// One `browser/tab-<op>` operation on the browser's tabs. `request` is the op's request object; the kernel
    /// validates it when the effect runs and shapes the result, the host performs only the raw tab and page asks.
    BrowserTab {
        op: crate::tab::TabOp,
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
    Href,
}

impl PageFactField {
    /// Every fact, in the spelling a program writes it.
    pub const ALL: [PageFactField; 4] = [Self::Hostname, Self::Referrer, Self::ScreenWidth, Self::Href];

    /// The bare word a program writes (`hostname`, `referrer`, `screenWidth`, `href`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hostname => "hostname",
            Self::Referrer => "referrer",
            Self::ScreenWidth => "screenWidth",
            Self::Href => "href",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == word)
    }
}

/// Whether `name` is a substrate operator: one the installed operator table routes to the substrate
/// (`host: "substrate"`: llm, memory, trace, session, workspace) or to the program host (`host: "program"`). Such an
/// operator is a `Custom` effect (e.g. `llm/embed`, `memory/store`) routed to its service instead of being compiled
/// as a plain call — see `OirEffect::Custom` and C-EMIT-SUBSTRATE-OPS-MARKER. The canonical predicate — shells and
/// the compiler must call this rather than re-deriving the set.
pub fn is_substrate_operator(name: &str) -> bool {
    crate::language::registry::try_get_operator(name)
        .is_some_and(|operator| matches!(operator.host.as_deref(), Some("program" | "substrate")))
}

impl OirEffect {
    // Constructor methods for common effects

    /// Get the effect kind as a string
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Set { .. } => "set",
            Self::SetDynamic { .. } => "set-dynamic",
            Self::Emit { .. } => "emit",
            Self::Fetch { .. } => "fetch",
            Self::Persist { .. } | Self::PersistBatch { .. } => "persist",
            Self::Navigate { .. } => "navigate",
            Self::NavigateBack => "navigate-back",
            Self::RenderUi { .. } => "render-ui",
            Self::CallService { .. } => "call-service",
            Self::CancelCall { .. } => "cancel-call",
            Self::RandomBytes { .. } => "random-bytes",
            Self::Os { op, .. } => op.name(),
            Self::CredentialStatus { .. } => "credential-status",
            Self::CredentialForget { .. } => "credential-forget",
            Self::CredentialAdmin { op, .. } => op.name(),
            Self::AccessCheck { .. } => "access-check",
            Self::Spawn { .. } => "spawn",
            Self::Despawn { .. } => "despawn",
            Self::Log { .. } => "log",
            Self::Custom { .. } => "custom",
            Self::OsWatchFiles { .. } => "os/watch-files",
            Self::OsWatchProcess { .. } => "os/watch-process",
            Self::OsWatchPort { .. } => "os/watch-port",
            Self::OsWatchCron { .. } => "os/watch-cron",
            Self::OsWatchSignal { .. } => "os/watch-signal",
            Self::OsWatchEnv { .. } => "os/watch-env",
            Self::FetchStream { .. } => "fetch-stream",
            Self::SendServer { .. } => "send-server",
            Self::BrowserOpenFilePicker { .. } => "browser/open-file-picker",
            Self::BrowserClipboardRead { .. } => "browser/clipboard-read",
            Self::BrowserClipboardWrite { .. } => "browser/clipboard-write",
            Self::BrowserGeolocationCurrent { .. } => "browser/geolocation-current",
            Self::BrowserPushSubscribe { .. } => "browser/push-subscribe",
            Self::BrowserPageFacts { .. } => "browser/page-facts",
            Self::BrowserPrompt { .. } => "browser/prompt",
            Self::BrowserEmbed { .. } => "browser/embed",
            Self::BrowserTab { op, .. } => op.name(),
        }
    }

    /// Whether a client host runs this effect (its `runsOn` is not `server`).
    pub fn is_client_effect(&self) -> bool {
        crate::runs_on::effect_runs_on(self) != crate::runs_on::RunsOn::Server
    }

    /// Whether a server runs this effect (its `runsOn` is not `client`).
    pub fn is_server_effect(&self) -> bool {
        crate::runs_on::effect_runs_on(self) != crate::runs_on::RunsOn::Client
    }
}

mod classification;
mod constructors;
mod walk;
pub use classification::{
    ClientAction, EffectClassification, OirExecutionEnv, OirLogLevel, OirPersistOp, ServerAction,
};

#[cfg(test)]
mod tests;
