//! The `Effect` IR every host executes, and what each variant asks for.

use super::*;

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
        /// `where: (fn row …)` on a single `delete`/`update`: the row guard the store checks in the same
        /// atomic step as the write; a row it rejects is `not-found` (EXEC-STEP-8).
        guard: Option<Value>,
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

    /// The kernel's verdict on one access policy (`crate::runtime::entity_access`): `request` is
    /// `{ entity, action, row, viewer }`; success carries `{ allowed }`. Server-only, answered with no host ask.
    AccessCheck {
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
        /// The options object's `key:` expression; the completion echoes it as `key`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emit: Option<FetchEmit>,
    },
}

/// All client-targeted effect wire kinds, in `kebab-case`. Every client shell MUST handle
/// each; the shell effect-coverage gates iterate this. Kept in sync with [`Effect::client_kind`]
/// by `effects::tests::client_kind_matches_kinds`.
pub const CLIENT_EFFECT_KINDS: &[&str] = &["render-ui", "navigate", "navigate-back", "emit"];

impl Effect {
    /// The effect's `emit` slot; `None` for a kind that declares no completion events.
    pub(super) fn emit_slot(&mut self) -> Option<&mut Option<FetchEmit>> {
        match self {
            Effect::Persist { emit, .. }
            | Effect::PersistBatch { emit, .. }
            | Effect::CallService { emit, .. }
            | Effect::Http { emit, .. }
            | Effect::Redis { emit, .. }
            | Effect::Postgres { emit, .. }
            | Effect::RandomBytes { emit, .. }
            | Effect::Os { emit, .. }
            | Effect::CredentialStatus { emit, .. }
            | Effect::CredentialForget { emit, .. }
            | Effect::CredentialAdmin { emit, .. }
            | Effect::AccessCheck { emit, .. }
            | Effect::OAuthAuthorize { emit, .. }
            | Effect::OAuthConnectionStatus { emit, .. }
            | Effect::OAuthConnectionForget { emit, .. }
            | Effect::Fetch { emit, .. }
            | Effect::BrowserPageFacts { emit, .. }
            | Effect::BrowserBeacon { emit, .. }
            | Effect::BrowserPrompt { emit, .. }
            | Effect::BrowserEmbed { emit, .. }
            | Effect::BrowserTab { emit, .. }
            | Effect::Custom { emit, .. } => Some(emit),
            Effect::Emit { .. }
            | Effect::Set { .. }
            | Effect::CancelCall { .. }
            | Effect::RenderUi { .. }
            | Effect::Navigate { .. }
            | Effect::NavigateBack
            | Effect::Spawn { .. }
            | Effect::Despawn { .. }
            | Effect::Log { .. } => None,
        }
    }

    /// The completion events this effect declares (`emit: { success, failure, … }`); `None` for a kind that takes none.
    pub fn emit_routes(&self) -> Option<&FetchEmit> {
        match self {
            Effect::Persist { emit, .. }
            | Effect::PersistBatch { emit, .. }
            | Effect::CallService { emit, .. }
            | Effect::Http { emit, .. }
            | Effect::Redis { emit, .. }
            | Effect::Postgres { emit, .. }
            | Effect::RandomBytes { emit, .. }
            | Effect::Os { emit, .. }
            | Effect::CredentialStatus { emit, .. }
            | Effect::CredentialForget { emit, .. }
            | Effect::CredentialAdmin { emit, .. }
            | Effect::AccessCheck { emit, .. }
            | Effect::OAuthAuthorize { emit, .. }
            | Effect::OAuthConnectionStatus { emit, .. }
            | Effect::OAuthConnectionForget { emit, .. }
            | Effect::Fetch { emit, .. }
            | Effect::BrowserPageFacts { emit, .. }
            | Effect::BrowserBeacon { emit, .. }
            | Effect::BrowserPrompt { emit, .. }
            | Effect::BrowserEmbed { emit, .. }
            | Effect::BrowserTab { emit, .. }
            | Effect::Custom { emit, .. } => emit.as_ref(),
            Effect::Emit { .. }
            | Effect::Set { .. }
            | Effect::CancelCall { .. }
            | Effect::RenderUi { .. }
            | Effect::Navigate { .. }
            | Effect::NavigateBack
            | Effect::Spawn { .. }
            | Effect::Despawn { .. }
            | Effect::Log { .. } => None,
        }
    }

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
            Effect::AccessCheck { .. } => "access-check",
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
