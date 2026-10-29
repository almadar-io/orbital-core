//! What a dispatch asks the host and how the host answers: requests, routes, call sites and answers.

use super::*;

/// Names one request of one dispatch; an answer carries the id of the request it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub(super) u64);

impl RequestId {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

/// The I/O the kernel needs the host to do. The kernel applies row policy, filters and paging itself
/// around these, so a host answers plain storage reads and writes. `Debug` shows no secret value
/// (`SecretSet.value`; an `HttpRequest` redacts its own credentials).
#[derive(Clone, PartialEq)]
pub enum IoRequest {
    ReadRow {
        entity_type: String,
        id: String,
    },
    ListRows {
        entity_type: String,
    },
    /// A query the host runs where the data lives: only the page (or its ids) crosses. Sent only to a host
    /// that declared capabilities covering `query`.
    QueryRows {
        entity_type: String,
        query: RowQuery,
        project: RowProjection,
    },
    /// Conditional writes the host runs in one step: `batch.on_failure` is `atomic` (a `persist batch`) or
    /// `stop` (a run of plain persists). Sent only to a host that declared write capabilities covering it.
    WriteRows {
        batch: RowWriteBatch,
    },
    /// Rows the kernel decided, committed together only if every one still equals the version read
    /// (EXEC-STORE-5). Sent only to a host that declared `commitRows`.
    CommitRows {
        rows: Vec<RowCommit>,
    },
    CreateRow {
        entity_type: String,
        data: Value,
    },
    WriteRow {
        entity_type: String,
        id: String,
        data: Value,
    },
    DeleteRow {
        entity_type: String,
        id: String,
    },
    CallService {
        service: String,
        method: String,
        args: Value,
        key: Option<String>,
        caller: Option<ServiceCaller>,
        /// Where the call is made from; `None` for a call outside a transition's effects.
        site: Option<CallSite>,
    },
    CancelCall {
        key: String,
    },
    /// One substrate or program op. A keyed op with a declared route sets `key` and `site`: the host may then answer
    /// [`IoAnswer::ServiceDeferred`] and deliver its outcome later as a dispatch of its own (R-a).
    Substrate {
        op: String,
        args: Vec<Value>,
        key: Option<String>,
        site: Option<CallSite>,
    },
    /// One request of the `http` effect.
    Http {
        request: HttpRequest,
        /// The effect's key, so `cancel-call` can abort the request in flight.
        key: Option<String>,
        /// How long the host waits before sending (a retry's backoff); `None` sends now.
        delay_ms: Option<u64>,
        site: Option<CallSite>,
    },
    /// A request of the `http` effect whose reply is a stream: the host dispatches each event to `site`'s
    /// `onMessage` route as it arrives, then answers [`IoAnswer::Http`] with the final reply, body empty.
    HttpStream {
        request: HttpRequest,
        key: Option<String>,
        framing: StreamFraming,
        site: Option<CallSite>,
        /// In-process continuation; the wire contract never carries held credentials.
        continuation: Option<Box<crate::http_request::HttpCallDriver>>,
    },
    /// The built request of a `browser/beacon`: the host queues it or sends one keepalive fetch, and answers how.
    HttpBeacon {
        request: HttpRequest,
        key: Option<String>,
        site: Option<CallSite>,
    },
    /// The page facts a `browser/page-facts` asks for, in read order.
    PageFacts {
        fields: Vec<PageFactField>,
    },
    /// A `browser/prompt`: the on-device model answers with JSON text satisfying the JSON Schema text `schema`.
    /// `key` lets `cancel-call` abort the ask in flight.
    Prompt {
        text: String,
        schema: String,
        key: Option<String>,
    },
    /// A `browser/embed`: the on-device embedder answers one vector per text, in order.
    Embed {
        texts: Vec<String>,
        key: Option<String>,
    },
    /// One raw ask of a `browser/tab-*` effect: the host performs a tab or page operation and answers its JSON
    /// reply. `key` lets `cancel-call` abort the ask in flight.
    Tab {
        ask: TabAsk,
        key: Option<String>,
    },
    /// A credential by name. `principal` is the dispatch's `@user` id for a user-scoped credential
    /// (set by the kernel, never by the program); `None` is the app-wide credential.
    Secret {
        name: String,
        principal: Option<String>,
    },
    /// Keep a derived secret (an OAuth token) in the host's store.
    SecretSet {
        name: String,
        value: String,
        ttl_ms: u64,
        principal: Option<String>,
    },
    /// Replace the credential `name` the host holds with `value`, durably (a rotated refresh token).
    SecretRotate {
        name: String,
        value: String,
        principal: Option<String>,
    },
    /// `bytes` of CSPRNG output, journaled so a replay sees the same bytes.
    Random {
        bytes: u32,
    },
    /// The dispatch's wall clock in epoch milliseconds, journaled so a replay sees the same instant.
    Clock,
    /// One `os/<op>` effect. A streaming op sets `site`: the host dispatches each event to its route, then
    /// answers [`IoAnswer::Os`] with the final reply. `key` lets `cancel-call` abort the ask in flight.
    Os {
        op: OsOp,
        args: Vec<Value>,
        key: Option<String>,
        site: Option<CallSite>,
    },
    /// Whether the host holds a value for the credential `name` (for `principal` when set).
    CredentialStatus {
        name: String,
        principal: Option<String>,
    },
    /// Drop the credential `name` the host holds (for `principal` when set).
    CredentialForget {
        name: String,
        principal: Option<String>,
    },
    /// The credential store's state: whether it can encrypt (a master key is present) and the service and name of every
    /// stored row, never a value. Answered `None` by a host that holds no store.
    CredentialStoreList,
    /// The stored value of credential `name`, from the store only (no environment fallthrough).
    CredentialStoreRead {
        name: String,
    },
    /// Encrypt `value` into the store as credential `name` of `service`, replacing any row of that name.
    CredentialStoreWrite {
        service: String,
        name: String,
        value: String,
    },
    /// Delete the stored row of credential `name`; whether one existed.
    CredentialStoreRemove {
        name: String,
    },
    /// Re-encrypt every stored row under the current master key.
    CredentialStoreRotate,
    /// The host's integrations mode flag; answered `None` by a host that keeps none.
    CredentialModeRead,
    /// Switch the host's integrations mode flag to `mock` or `real`; the host may refuse.
    CredentialModeWrite {
        mode: String,
    },
    /// What an OAuth flow asks the host to persist (`principal` is set by the kernel, never by the program).
    OAuth {
        ask: OAuthAsk,
        principal: Option<String>,
    },
    /// An `emit` whose declared listener runs in another deployment unit (EXEC-QUEUE-9). `site` is the emitting
    /// trait's call site; `key` lets `cancel-call` withdraw the ask.
    Deliver {
        envelope: Envelope,
        key: Option<String>,
        site: CallSite,
    },
}

/// The events a `call-service` call site routes its outcome and live messages to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallRoutes {
    pub success: Option<String>,
    pub failure: Option<String>,
    pub cancelled: Option<String>,
    pub on_message: Option<String>,
}

/// The call site of a `call-service`: the trait whose transition runs it (and that trait's orbital),
/// the routes it declares, and whether the step is a follow-on of the dispatch (a listener's arm, or
/// the calling trait's own next step) rather than the request's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub orbital: Option<String>,
    pub trait_name: String,
    pub follow_on: bool,
    /// The frame key the call was made on ([`crate::process_result::RUNTIME_SINGLETON`] for an id-less dispatch): a
    /// deferred outcome is dispatched to it. A host names it to a viewer through [`crate::process_result::public_entity_id`].
    pub entity_id: String,
    pub emit: CallRoutes,
}

/// The event and payload `outcome` fires on `site`'s routes.
pub fn substrate_completion_event(site: &CallSite, key: &str, outcome: Result<Value, Value>) -> Option<(String, Value)> {
    let (event, payload) = match outcome {
        Ok(payload) => (site.emit.success.clone()?, payload),
        Err(payload) => (site.emit.failure.clone()?, payload),
    };
    let payload = match payload {
        Value::Object(mut fields) => {
            fields.insert("key".into(), Value::string(key.to_string()));
            Value::Object(fields)
        }
        other => other,
    };
    Some((event, payload))
}

pub fn completion_event(
    site: &CallSite,
    key: &str,
    outcome: Result<Value, OsError>,
) -> Option<(String, Value)> {
    let key = Value::string(key.to_string());
    match outcome {
        Ok(result) => {
            let mut fields = match result {
                Value::Object(fields) => fields,
                other => ValueMap::from_iter([("value".into(), other)]).into(),
            };
            fields.insert("key".into(), key);
            Some((site.emit.success.clone()?, Value::Object(fields)))
        }
        Err(error) => Some((
            site.emit.failure.clone()?,
            Value::object(ValueMap::from_iter([
                ("error".into(), Value::String(error.message.into())),
                (
                    "kind".into(),
                    Value::String(error.kind.as_str().to_string().into()),
                ),
                ("key".into(), key),
            ])),
        )),
    }
}

/// How a host settled a `call-service` other than by a result or an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallVerdict {
    /// The call was aborted by `cancel-call`: the cancelled route fires with `{ key }`.
    Cancelled,
    /// The host started the call and runs its outcome later as its own dispatch: nothing fires now.
    Deferred,
}

/// A host's answer to one [`IoRequest`]; the variant must be the one the request calls for. `Debug` shows no
/// secret value (`Secret` carries one).
#[derive(Clone, PartialEq)]
pub enum IoAnswer {
    Row(Result<Value, String>),
    Rows(Result<Vec<Arc<Value>>, String>),
    Page(Result<RowPage, String>),
    IdPage(Result<RowIdPage, String>),
    /// The host cannot run the query it was sent; nothing was read. The kernel lists the table itself.
    QueryUnsupported(String),
    Wrote(Result<Vec<RowWriteOutcome>, String>),
    /// The host cannot run the writes it was sent; nothing was written. The kernel takes the one-row path.
    WriteUnsupported(String),
    /// `true` when every row was committed; `false` on a conflict, when nothing was.
    Committed(Result<bool, String>),
    Deleted(Result<(), String>),
    Service(Result<Value, String>),
    /// The call was aborted by `cancel-call` while it ran.
    ServiceCancelled,
    /// The call was started and its outcome will arrive as a dispatch of its own.
    ServiceDeferred,
    Cancelled(Result<(), String>),
    Substrate(Result<Value, SubstrateFailure>),
    Http(Result<HttpResponse, TransportError>),
    HttpBeacon(Result<BeaconOutcome, TransportError>),
    PageFacts(Result<PageFacts, String>),
    Prompt(Result<String, ModelFailure>),
    Embed(Result<Vec<Vec<f64>>, ModelFailure>),
    Tab(Result<Value, TabFailure>),
    Secret(Result<Option<String>, String>),
    SecretSet(Result<(), String>),
    SecretRotate(Result<(), String>),
    Random(Result<Vec<u8>, String>),
    Clock(Result<i64, String>),
    Os(Result<Value, OsError>),
    CredentialStatus(Result<crate::credential_admin::CredentialStanding, String>),
    CredentialForget(Result<(), String>),
    CredentialStoreList(Result<Option<StoreListing>, String>),
    CredentialStoreRead(Result<Option<String>, String>),
    CredentialStoreWrite(Result<(), String>),
    CredentialStoreRemove(Result<bool, String>),
    CredentialStoreRotate(Result<RotateCounts, String>),
    CredentialModeRead(Result<Option<String>, String>),
    CredentialModeWrite(Result<String, String>),
    OAuth(Result<OAuthReply, String>),
    Deliver(Result<Accepted, DeliverRefusal>),
}

/// Why the on-device model or embedder gave no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFailureKind {
    /// The host wires no model or embedder.
    Unconfigured,
    /// The host has one that cannot answer now (still downloading, not supported on this device).
    Unavailable,
    /// The model or embedder answered with an error, or with an answer outside the contract.
    Failed,
}

impl ModelFailureKind {
    /// The word a failure payload carries as `kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// A `browser/prompt` or `browser/embed` the host could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFailure {
    pub kind: ModelFailureKind,
    pub message: String,
}

impl ModelFailure {
    pub fn new(kind: ModelFailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

/// What the page offered: `available` is false when there is no page; the other facts are as read.
#[derive(Debug, Clone, PartialEq)]
pub struct PageFacts {
    pub available: bool,
    pub hostname: String,
    pub referrer: String,
    pub screen_width: Option<f64>,
}

pub(super) struct Redacted;

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl std::fmt::Debug for IoRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadRow { entity_type, id } => f
                .debug_struct("ReadRow")
                .field("entity_type", entity_type)
                .field("id", id)
                .finish(),
            Self::ListRows { entity_type } => f.debug_struct("ListRows").field("entity_type", entity_type).finish(),
            Self::QueryRows {
                entity_type,
                query,
                project,
            } => f
                .debug_struct("QueryRows")
                .field("entity_type", entity_type)
                .field("query", query)
                .field("project", project)
                .finish(),
            Self::WriteRows { batch } => f.debug_struct("WriteRows").field("batch", batch).finish(),
            Self::CommitRows { rows } => f.debug_struct("CommitRows").field("rows", rows).finish(),
            Self::CreateRow { entity_type, data } => f
                .debug_struct("CreateRow")
                .field("entity_type", entity_type)
                .field("data", data)
                .finish(),
            Self::WriteRow { entity_type, id, data } => f
                .debug_struct("WriteRow")
                .field("entity_type", entity_type)
                .field("id", id)
                .field("data", data)
                .finish(),
            Self::DeleteRow { entity_type, id } => f
                .debug_struct("DeleteRow")
                .field("entity_type", entity_type)
                .field("id", id)
                .finish(),
            Self::CallService {
                service,
                method,
                args,
                key,
                caller,
                site,
            } => f
                .debug_struct("CallService")
                .field("service", service)
                .field("method", method)
                .field("args", args)
                .field("key", key)
                .field("caller", caller)
                .field("site", site)
                .finish(),
            Self::CancelCall { key } => f.debug_struct("CancelCall").field("key", key).finish(),
            Self::Substrate { op, args, key, site } => f
                .debug_struct("Substrate")
                .field("op", op)
                .field("args", args)
                .field("key", key)
                .field("site", site)
                .finish(),
            Self::Http {
                request,
                key,
                delay_ms,
                site,
            } => f
                .debug_struct("Http")
                .field("request", request)
                .field("key", key)
                .field("delay_ms", delay_ms)
                .field("site", site)
                .finish(),
            Self::HttpStream {
                request,
                key,
                framing,
                site,
                ..
            } => f
                .debug_struct("HttpStream")
                .field("request", request)
                .field("key", key)
                .field("framing", framing)
                .field("site", site)
                .finish(),
            Self::HttpBeacon { request, key, site } => f
                .debug_struct("HttpBeacon")
                .field("request", request)
                .field("key", key)
                .field("site", site)
                .finish(),
            Self::PageFacts { fields } => f.debug_struct("PageFacts").field("fields", fields).finish(),
            Self::Prompt { text, schema, key } => f
                .debug_struct("Prompt")
                .field("text", text)
                .field("schema", schema)
                .field("key", key)
                .finish(),
            Self::Embed { texts, key } => f.debug_struct("Embed").field("texts", texts).field("key", key).finish(),
            Self::Tab { ask, key } => f.debug_struct("Tab").field("ask", ask).field("key", key).finish(),
            Self::Secret { name, principal } => f
                .debug_struct("Secret")
                .field("name", name)
                .field("principal", principal)
                .finish(),
            Self::SecretSet {
                name,
                ttl_ms,
                principal,
                ..
            } => f
                .debug_struct("SecretSet")
                .field("name", name)
                .field("value", &Redacted)
                .field("ttl_ms", ttl_ms)
                .field("principal", principal)
                .finish(),
            Self::SecretRotate { name, principal, .. } => f
                .debug_struct("SecretRotate")
                .field("name", name)
                .field("value", &Redacted)
                .field("principal", principal)
                .finish(),
            Self::Random { bytes } => f.debug_struct("Random").field("bytes", bytes).finish(),
            Self::Clock => f.write_str("Clock"),
            Self::Os { op, args, key, site } => f
                .debug_struct("Os")
                .field("op", op)
                .field("args", args)
                .field("key", key)
                .field("site", site)
                .finish(),
            Self::CredentialStatus { name, principal } => f
                .debug_struct("CredentialStatus")
                .field("name", name)
                .field("principal", principal)
                .finish(),
            Self::CredentialForget { name, principal } => f
                .debug_struct("CredentialForget")
                .field("name", name)
                .field("principal", principal)
                .finish(),
            Self::CredentialStoreList => f.write_str("CredentialStoreList"),
            Self::CredentialStoreRead { name } => f.debug_struct("CredentialStoreRead").field("name", name).finish(),
            Self::CredentialStoreWrite { service, name, .. } => f
                .debug_struct("CredentialStoreWrite")
                .field("service", service)
                .field("name", name)
                .field("value", &Redacted)
                .finish(),
            Self::CredentialStoreRemove { name } => f.debug_struct("CredentialStoreRemove").field("name", name).finish(),
            Self::CredentialStoreRotate => f.write_str("CredentialStoreRotate"),
            Self::CredentialModeRead => f.write_str("CredentialModeRead"),
            Self::CredentialModeWrite { mode } => f.debug_struct("CredentialModeWrite").field("mode", mode).finish(),
            Self::OAuth { ask, principal } => f
                .debug_struct("OAuth")
                .field("ask", ask)
                .field("principal", principal)
                .finish(),
            Self::Deliver { envelope, key, site } => f
                .debug_struct("Deliver")
                .field("envelope", envelope)
                .field("key", key)
                .field("site", site)
                .finish(),
        }
    }
}

impl std::fmt::Debug for IoAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Row(r) => f.debug_tuple("Row").field(r).finish(),
            Self::Rows(r) => f.debug_tuple("Rows").field(r).finish(),
            Self::Page(r) => f.debug_tuple("Page").field(r).finish(),
            Self::IdPage(r) => f.debug_tuple("IdPage").field(r).finish(),
            Self::QueryUnsupported(m) => f.debug_tuple("QueryUnsupported").field(m).finish(),
            Self::Wrote(r) => f.debug_tuple("Wrote").field(r).finish(),
            Self::WriteUnsupported(m) => f.debug_tuple("WriteUnsupported").field(m).finish(),
            Self::Committed(r) => f.debug_tuple("Committed").field(r).finish(),
            Self::Deleted(r) => f.debug_tuple("Deleted").field(r).finish(),
            Self::Service(r) => f.debug_tuple("Service").field(r).finish(),
            Self::ServiceCancelled => f.write_str("ServiceCancelled"),
            Self::ServiceDeferred => f.write_str("ServiceDeferred"),
            Self::Cancelled(r) => f.debug_tuple("Cancelled").field(r).finish(),
            Self::Substrate(r) => f.debug_tuple("Substrate").field(r).finish(),
            Self::Http(r) => f.debug_tuple("Http").field(r).finish(),
            Self::HttpBeacon(r) => f.debug_tuple("HttpBeacon").field(r).finish(),
            Self::PageFacts(r) => f.debug_tuple("PageFacts").field(r).finish(),
            Self::Prompt(r) => f.debug_tuple("Prompt").field(r).finish(),
            Self::Embed(r) => f.debug_tuple("Embed").field(r).finish(),
            Self::Tab(r) => f.debug_tuple("Tab").field(r).finish(),
            Self::Secret(r) => {
                let shown: Result<Option<Redacted>, &String> = match r {
                    Ok(value) => Ok(value.as_ref().map(|_| Redacted)),
                    Err(message) => Err(message),
                };
                f.debug_tuple("Secret").field(&shown).finish()
            }
            Self::SecretSet(r) => f.debug_tuple("SecretSet").field(r).finish(),
            Self::SecretRotate(r) => f.debug_tuple("SecretRotate").field(r).finish(),
            Self::Random(r) => {
                let shown: Result<Redacted, &String> = match r {
                    Ok(_) => Ok(Redacted),
                    Err(message) => Err(message),
                };
                f.debug_tuple("Random").field(&shown).finish()
            }
            Self::Clock(r) => f.debug_tuple("Clock").field(r).finish(),
            Self::Os(r) => {
                let shown: Result<Redacted, &OsError> = match r {
                    Ok(_) => Ok(Redacted),
                    Err(error) => Err(error),
                };
                f.debug_tuple("Os").field(&shown).finish()
            }
            Self::CredentialStatus(r) => f.debug_tuple("CredentialStatus").field(r).finish(),
            Self::CredentialForget(r) => f.debug_tuple("CredentialForget").field(r).finish(),
            Self::CredentialStoreList(r) => f.debug_tuple("CredentialStoreList").field(r).finish(),
            Self::CredentialStoreRead(r) => {
                let shown: Result<Option<Redacted>, &String> = match r {
                    Ok(value) => Ok(value.as_ref().map(|_| Redacted)),
                    Err(message) => Err(message),
                };
                f.debug_tuple("CredentialStoreRead").field(&shown).finish()
            }
            Self::CredentialStoreWrite(r) => f.debug_tuple("CredentialStoreWrite").field(r).finish(),
            Self::CredentialStoreRemove(r) => f.debug_tuple("CredentialStoreRemove").field(r).finish(),
            Self::CredentialStoreRotate(r) => f.debug_tuple("CredentialStoreRotate").field(r).finish(),
            Self::CredentialModeRead(r) => f.debug_tuple("CredentialModeRead").field(r).finish(),
            Self::CredentialModeWrite(r) => f.debug_tuple("CredentialModeWrite").field(r).finish(),
            Self::OAuth(r) => f.debug_tuple("OAuth").field(r).finish(),
            Self::Deliver(r) => f.debug_tuple("Deliver").field(r).finish(),
        }
    }
}

impl IoAnswer {
    pub(super) fn answers(&self, request: &IoRequest) -> bool {
        if let (IoRequest::Os { op, key, site, .. }, IoAnswer::ServiceDeferred) = (request, self) {
            return key.is_some() && (site.is_some() || !op.defers_completion());
        }
        if let (IoRequest::Http { key, .. } | IoRequest::HttpStream { key, .. }, IoAnswer::ServiceDeferred) = (request, self) {
            return key.is_some();
        }
        if let (IoRequest::Substrate { key, site, .. }, IoAnswer::ServiceDeferred) = (request, self) {
            return key.is_some() && site.is_some();
        }
        matches!(
            (request, self),
            (
                IoRequest::ReadRow { .. }
                    | IoRequest::CreateRow { .. }
                    | IoRequest::WriteRow { .. },
                IoAnswer::Row(_)
            ) | (IoRequest::ListRows { .. }, IoAnswer::Rows(_))
                | (
                    IoRequest::QueryRows {
                        project: RowProjection::Rows,
                        ..
                    },
                    IoAnswer::Page(_) | IoAnswer::QueryUnsupported(_)
                )
                | (
                    IoRequest::QueryRows {
                        project: RowProjection::Ids,
                        ..
                    },
                    IoAnswer::IdPage(_) | IoAnswer::QueryUnsupported(_)
                )
                | (
                    IoRequest::WriteRows { .. },
                    IoAnswer::Wrote(_) | IoAnswer::WriteUnsupported(_)
                )
                | (IoRequest::CommitRows { .. }, IoAnswer::Committed(_))
                | (IoRequest::DeleteRow { .. }, IoAnswer::Deleted(_))
                | (
                    IoRequest::CallService { .. },
                    IoAnswer::Service(_) | IoAnswer::ServiceCancelled | IoAnswer::ServiceDeferred
                )
                | (IoRequest::CancelCall { .. }, IoAnswer::Cancelled(_))
                | (IoRequest::Substrate { .. }, IoAnswer::Substrate(_))
                | (
                    IoRequest::Http { .. } | IoRequest::HttpStream { .. },
                    IoAnswer::Http(_) | IoAnswer::ServiceCancelled
                )
                | (
                    IoRequest::HttpBeacon { .. },
                    IoAnswer::HttpBeacon(_) | IoAnswer::ServiceCancelled
                )
                | (IoRequest::PageFacts { .. }, IoAnswer::PageFacts(_))
                | (IoRequest::Prompt { .. }, IoAnswer::Prompt(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::Embed { .. }, IoAnswer::Embed(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::Tab { .. }, IoAnswer::Tab(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::Secret { .. }, IoAnswer::Secret(_))
                | (IoRequest::SecretSet { .. }, IoAnswer::SecretSet(_))
                | (IoRequest::SecretRotate { .. }, IoAnswer::SecretRotate(_))
                | (IoRequest::Random { .. }, IoAnswer::Random(_))
                | (IoRequest::Clock, IoAnswer::Clock(_))
                | (IoRequest::Os { .. }, IoAnswer::Os(_) | IoAnswer::ServiceCancelled)
                | (IoRequest::CredentialStatus { .. }, IoAnswer::CredentialStatus(_))
                | (IoRequest::CredentialForget { .. }, IoAnswer::CredentialForget(_))
                | (IoRequest::CredentialStoreList, IoAnswer::CredentialStoreList(_))
                | (IoRequest::CredentialStoreRead { .. }, IoAnswer::CredentialStoreRead(_))
                | (IoRequest::CredentialStoreWrite { .. }, IoAnswer::CredentialStoreWrite(_))
                | (IoRequest::CredentialStoreRemove { .. }, IoAnswer::CredentialStoreRemove(_))
                | (IoRequest::CredentialStoreRotate, IoAnswer::CredentialStoreRotate(_))
                | (IoRequest::CredentialModeRead, IoAnswer::CredentialModeRead(_))
                | (IoRequest::CredentialModeWrite { .. }, IoAnswer::CredentialModeWrite(_))
                | (IoRequest::OAuth { .. }, IoAnswer::OAuth(_))
                | (IoRequest::Deliver { .. }, IoAnswer::Deliver(_))
        )
    }
}
