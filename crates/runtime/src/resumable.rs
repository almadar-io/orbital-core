//! Resumable dispatch: a dispatch that stops at its I/O points and goes on when the host has answered.
//!
//! The kernel never performs I/O itself. Its reads, writes, service calls and substrate operators go
//! through an [`IoPort`]. A blocking host (the Rust server, `orb`, tests) leaves the port passing
//! straight through to its synchronous handlers. An async host starts a dispatch with
//! [`RuntimeKernel::start_dispatch`], gets [`Progress::Await`] at the first I/O the in-flight step has no
//! answer for, and calls [`RuntimeKernel::resume`] with the answers, in any order and as late as it likes.
//! The step that asked is run again from the state the dispatch had before it, now with the answers in
//! hand, so a request is asked for once however often the step is replayed.

use crate::effects::executor::{ServiceCaller, ServiceHandler, SubstrateFailure, SubstrateHandler};
use orbital_core_base::value::{Value, ValueMap};
use crate::credential_admin::{RotateCounts, StoreListing};
use crate::entity_source::EntitySource;
use orbital_core_base::ir::effects::PageFactField;
use orbital_core_base::envelope::{Accepted, DeliverRefusal, DeliverRefusalKind, Envelope};
use orbital_core_base::oauth_ask::{OAuthAsk, OAuthReply};
use crate::http_request;
use crate::http_request::{BeaconOutcome, StreamFraming};
use crate::http_service::{HttpRequest, HttpResponse, SecretSource, Transport, TransportError};
use crate::kernel::dispatch_run::DispatchRun;
use crate::kernel::KernelError;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::tab::{TabAsk, TabFailure, TabFailureKind};
use crate::process_result::ProcessEventResult;
use crate::row_query::{guard_unsupported_reason, query_unsupported_reason, QueryCapabilities, QueryRun, RowIdPage, RowPage, RowProjection, RowWriteBatch, RowWriteFailureMode, RowWriteOp, RowWriteOutcome, ScanControl, WriteCapabilities};
use orbital_core_base::row_set::{RowQuery, RowSetRef};
use parking_lot::{Mutex, MutexGuard, RwLock};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread::ThreadId;

/// What a dispatch produced: one outcome per delivery, in dispatch order.
pub type DispatchOutcomes = Vec<(String, ProcessEventResult)>;

/// Names one request of one dispatch; an answer carries the id of the request it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(u64);

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
    /// The frame the call was made on (`singleton` for an id-less dispatch): a deferred outcome is dispatched to it.
    /// Not carried by [`crate::embed::wire::CallSiteWire`]: a site rebuilt from the wire addresses `singleton`.
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
            fields.insert("key".to_string(), Value::String(key.to_string()));
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
    let key = Value::String(key.to_string());
    match outcome {
        Ok(result) => {
            let mut fields = match result {
                Value::Object(fields) => fields,
                other => ValueMap::from([("value".to_string(), other)]),
            };
            fields.insert("key".to_string(), key);
            Some((site.emit.success.clone()?, Value::Object(fields)))
        }
        Err(error) => Some((
            site.emit.failure.clone()?,
            Value::Object(ValueMap::from([
                ("error".to_string(), Value::String(error.message)),
                (
                    "kind".to_string(),
                    Value::String(error.kind.as_str().to_string()),
                ),
                ("key".to_string(), key),
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

struct Redacted;

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
    fn answers(&self, request: &IoRequest) -> bool {
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

/// A request the dispatch is waiting on.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRequest {
    pub id: RequestId,
    pub request: IoRequest,
}

/// The host's answer to the request `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct IoReply {
    pub id: RequestId,
    pub answer: IoAnswer,
}

/// Where a resumable dispatch stands.
#[expect(clippy::large_enum_variant, reason = "public unboxed layout is the API; pinned by tests/public_layout_contract.rs")]
pub enum Progress<T> {
    Done(T),
    Await {
        continuation: DispatchContinuation,
        requests: Vec<PendingRequest>,
    },
}

impl<T: std::fmt::Debug> std::fmt::Debug for Progress<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Progress::Done(done) => f.debug_tuple("Done").field(done).finish(),
            Progress::Await {
                continuation,
                requests,
            } => f
                .debug_struct("Await")
                .field("continuation", continuation)
                .field("requests", requests)
                .finish(),
        }
    }
}

/// Why a set of answers cannot resume a dispatch.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ResumeError {
    #[error("request {} was not answered", .0.raw())]
    Unanswered(RequestId),
    #[error("request {} is not one this dispatch is waiting on", .0.raw())]
    Unknown(RequestId),
    #[error("request {} was answered twice", .0.raw())]
    Duplicate(RequestId),
    #[error("the answer to request {} is not the kind that request calls for", .0.raw())]
    WrongKind(RequestId),
    #[error("the replayed step asked for {got:?} where the recorded step asked for {recorded:?}")]
    Diverged {
        recorded: Box<IoRequest>,
        got: Box<IoRequest>,
    },
}

/// A failure of a resumable dispatch.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error(transparent)]
    Kernel(#[from] KernelError),
    #[error(transparent)]
    Resume(#[from] ResumeError),
}

/// A dispatch stopped at its I/O: the whole of what [`RuntimeKernel::resume`] needs to go on.
pub struct DispatchContinuation {
    pub(crate) run: DispatchRun,
    pub(crate) journal: Vec<Exchange>,
    pub(crate) pending: Vec<PendingRequest>,
    pub(crate) next_id: u64,
    /// The rows the host has answered for this dispatch, kept across its steps (see [`ReadCache`]).
    pub(crate) reads: Arc<ReadCache>,
    /// The store ask this dispatch is parked on and the driver orbital's dispatch answering it (EXEC-STORE-4).
    pub(crate) store: Option<Box<StoreLeg>>,
    /// The boot seeds still to write before this dispatch's first step; `None` until taken (the seed barrier).
    pub(crate) seeds: Option<std::collections::VecDeque<(String, StoreAsk)>>,
    /// The kernel's dispatch lane, held while this dispatch is parked (EXEC-CONC-1).
    pub(crate) lane: Option<crate::kernel::lane::LaneHold>,
}

impl DispatchContinuation {
    pub fn dispatch_id(&self) -> u64 { self.run.dispatch_id() }

    /// The lane turn this parked dispatch holds (EXEC-CONC-0): the token a call made on its behalf carries.
    pub fn turn(&self) -> Option<crate::kernel::LaneTurn> {
        self.lane.as_ref().map(|hold| hold.turn())
    }
}

/// A parked row ask and the driver dispatch that answers it; the driver's own host asks are this dispatch's.
pub(crate) struct StoreLeg {
    pub(crate) ask: StoreAsk,
    pub(crate) driver: DispatchContinuation,
    /// A boot seed of declared `instances` (its registered key): no step asked it, so its answer is checked,
    /// never journaled.
    pub(crate) seed: Option<String>,
}

/// Accept `replies` into `c`, or into the driver dispatch it is parked on.
pub(crate) fn accept_into(c: &mut DispatchContinuation, replies: Vec<IoReply>) -> Result<(), ResumeError> {
    if let Some(leg) = c.store.as_mut() {
        accept_into(&mut leg.driver, replies)?;
    } else {
        let answered = accept_replies(&c.pending, replies)?;
        c.journal.extend(answered);
    }
    c.pending.clear();
    Ok(())
}

impl std::fmt::Debug for DispatchContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchContinuation")
            .field("pending", &self.pending)
            .field("answered", &self.journal.len())
            .field("store", &self.store.as_ref().map(|leg| &leg.ask))
            .finish()
    }
}

/// A row-set reference resolving through a host-answered store: the whole of what
/// [`RuntimeKernel::resume_resolve_rows`] needs to go on.
pub struct ResolveContinuation {
    pub(crate) reference: RowSetRef,
    pub(crate) viewer: Value,
    pub(crate) journal: Vec<Exchange>,
    pub(crate) pending: Vec<PendingRequest>,
    pub(crate) next_id: u64,
    /// The store ask this resolve is parked on and its driver's dispatch (EXEC-STORE-4).
    pub(crate) store: Option<Box<StoreLeg>>,
    /// The boot seeds still to write before the resolve reads; `None` until taken (the seed barrier).
    pub(crate) seeds: Option<std::collections::VecDeque<(String, StoreAsk)>>,
}

impl std::fmt::Debug for ResolveContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveContinuation")
            .field("entity_type", &self.reference.entity_type)
            .field("pending", &self.pending)
            .field("answered", &self.journal.len())
            .finish()
    }
}

/// Where a resolve stands: the resolved page, or the requests the host answers next.
#[derive(Debug)]
#[expect(clippy::large_enum_variant, reason = "public unboxed layout is the API; pinned by tests/public_layout_contract.rs")]
pub enum ResolveProgress {
    Done(RowPage),
    Await {
        continuation: ResolveContinuation,
        requests: Vec<PendingRequest>,
    },
}

/// One request the in-flight step made and the answer it got.
#[derive(Debug, Clone)]
pub(crate) struct Exchange {
    request: IoRequest,
    answer: IoAnswer,
}

impl Exchange {
    pub(crate) fn new(request: IoRequest, answer: IoAnswer) -> Self {
        Self { request, answer }
    }
}

/// The rows a dispatch has read from the host, by (entity type, id); `None` is a row the host answered as absent. A
/// dispatch asks the host for a row once: its later steps read the answer, until the kernel writes that entity type.
/// A step that stops to await the host keeps none of its own reads or invalidations (its replay makes them again from
/// the journal), so the journal and the cache never both answer the same read.
pub(crate) type ReadCache = HashMap<(String, String), Option<Arc<Value>>>;

/// What the replayed step left behind: the request it could not go on without, or a request that
/// disagrees with the recorded one.
pub(crate) struct StepEnd {
    pub(crate) reads: Arc<ReadCache>,
    pub(crate) journal: Vec<Exchange>,
    pub(crate) pending: Asks,
    /// Row asks on store-bound entities, answered by their driver orbital in-kernel (EXEC-STORE-4).
    pub(crate) store_asks: Vec<StoreAsk>,
    pub(crate) diverged: Option<ResumeError>,
}

/// A row ask on an entity bound to a store-driver orbital: never sent to the host, answered by the driver's
/// keyed reply (`store_driver::answer_for`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoreAsk {
    pub(crate) driver: String,
    pub(crate) key: String,
    pub(crate) request: IoRequest,
}

/// Which journal entries a step has claimed: a prefix counter while answers are claimed in order (no
/// allocation), and the indices claimed ahead of it when a keyed call is answered out of order.
#[derive(Default)]
struct Claimed {
    prefix: usize,
    ahead: Vec<usize>,
}

impl Claimed {
    fn covers(&self, entries: usize) -> bool {
        self.prefix >= entries
    }

    fn has(&self, index: usize) -> bool {
        index < self.prefix || self.ahead.contains(&index)
    }

    fn claim(&mut self, index: usize) {
        if index != self.prefix {
            self.ahead.push(index);
            return;
        }
        self.prefix += 1;
        while let Some(at) = self.ahead.iter().position(|&i| i == self.prefix) {
            self.ahead.swap_remove(at);
            self.prefix += 1;
        }
    }
}

struct StepLedger {
    thread: ThreadId,
    reads: Arc<ReadCache>,
    reads_at_start: Arc<ReadCache>,
    journal: Vec<Exchange>,
    claimed: Claimed,
    pending: Asks,
    store_asks: Vec<StoreAsk>,
    keys: HashSet<String>,
    blocked: bool,
    diverged: Option<ResumeError>,
}

/// The requests one step leaves outstanding: inline for the common single ask, so a step allocates only once
/// it has more than one.
pub(crate) type Asks = smallvec::SmallVec<[IoRequest; 1]>;

/// The entity a row ask addresses (a batch: its first op's).
fn request_entity(request: &IoRequest) -> Option<&str> {
    match request {
        IoRequest::ReadRow { entity_type, .. }
        | IoRequest::ListRows { entity_type }
        | IoRequest::QueryRows { entity_type, .. }
        | IoRequest::CreateRow { entity_type, .. }
        | IoRequest::WriteRow { entity_type, .. }
        | IoRequest::DeleteRow { entity_type, .. } => Some(entity_type),
        IoRequest::WriteRows { batch } => batch.ops.first().map(RowWriteOp::entity_type),
        _ => None,
    }
}

/// Give each of `requests` the next id of its dispatch, in issue order.
pub(crate) fn number_requests(next_id: &mut u64, requests: Asks) -> Vec<PendingRequest> {
    let mut numbered = Vec::with_capacity(requests.len());
    for request in requests {
        let id = RequestId::from_raw(*next_id);
        *next_id += 1;
        numbered.push(PendingRequest { id, request });
    }
    numbered
}

/// The error a handler returns when the step cannot get an answer yet; the step's result is dropped.
pub(crate) const AWAITING_HOST: &str = "awaiting host I/O";

pub(crate) const NO_PAGE_FACTS_HOST: &str = "browser/page-facts: no host answers page facts";
pub(crate) const NO_TAB_HOST: &str = "browser/tab: no host answers browser tabs";

pub(crate) const NO_PROMPT_HOST: &str = "browser/prompt: no on-device model is wired into this host";

pub(crate) const NO_EMBED_HOST: &str = "browser/embed: no on-device embedder is wired into this host";

/// The error a call settled `Cancelled` carries, as JS's `untilAborted` rejects.
const CALL_CANCELLED: &str = "The call was cancelled";

enum Asked {
    Direct,
    Answer(IoAnswer),
    Withheld,
}

/// The one door the kernel's I/O goes through.
pub(crate) struct IoPort {
    source: Arc<dyn EntitySource>,
    service: Option<Arc<dyn ServiceHandler>>,
    substrate: Option<Arc<dyn SubstrateHandler>>,
    ledger: Mutex<Option<StepLedger>>,
    gate: Mutex<()>,
    /// The call site the effect being run belongs to, stamped on the `call-service` it makes.
    site: Mutex<Option<CallSite>>,
    /// How the host settled the last `call-service`, when not by a result or an error.
    verdict: Mutex<Option<CallVerdict>>,
    /// The `key` of the substrate op being run, stamped on its ask with `site`.
    substrate_key: Mutex<Option<String>>,
    /// What the host's query push-down honours, per entity type; an entity not listed is never sent a query.
    query_capabilities: RwLock<HashMap<String, QueryCapabilities>>,
    /// What the host's conditional writes honour, per entity type; an entity not listed is never sent `WriteRows`.
    write_capabilities: RwLock<HashMap<String, WriteCapabilities>>,
    /// Fallback notices already logged, one per (kind, entity, reason).
    noticed: Mutex<HashSet<String>>,
    /// The kernel's configured clock: when set, the time a driver reads (TLS) is this clock's, not a host ask.
    clock: Option<crate::config::DispatchClock>,
    /// `[runtime]` entities live in `memory` and are never a host ask, when the kernel is configured to own them.
    runtime_in_kernel: bool,
    runtime_types: RwLock<HashSet<String>>,
    memory: crate::entity_source::InMemoryEntityCache,
    /// `[store: Driver]` bindings: entity type -> the driver orbital that answers its row asks.
    store_drivers: RwLock<HashMap<String, String>>,
    store_seq: std::sync::atomic::AtomicU64,
}

struct Installed<'a> {
    port: &'a IoPort,
    _gate: MutexGuard<'a, ()>,
}

impl Drop for Installed<'_> {
    fn drop(&mut self) {
        *self.port.ledger.lock() = None;
    }
}

impl IoPort {
    pub(crate) fn new(
        source: Arc<dyn EntitySource>,
        service: Option<Arc<dyn ServiceHandler>>,
        substrate: Option<Arc<dyn SubstrateHandler>>,
    ) -> Self {
        Self {
            source,
            service,
            substrate,
            ledger: Mutex::new(None),
            gate: Mutex::new(()),
            site: Mutex::new(None),
            verdict: Mutex::new(None),
            substrate_key: Mutex::new(None),
            query_capabilities: RwLock::new(HashMap::new()),
            write_capabilities: RwLock::new(HashMap::new()),
            noticed: Mutex::new(HashSet::new()),
            clock: None,
            runtime_in_kernel: false,
            runtime_types: RwLock::new(HashSet::new()),
            memory: crate::entity_source::InMemoryEntityCache::new(),
            store_drivers: RwLock::new(HashMap::new()),
            store_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The create batch that writes `rows` of a store-bound `entity_type` through its driver; `None` when unbound.
    pub(crate) fn store_seed(&self, entity_type: &str, rows: Vec<Value>) -> Option<StoreAsk> {
        let driver = self.store_drivers.read().get(entity_type).cloned()?;
        let ops = rows
            .into_iter()
            .map(|data| RowWriteOp::Create { entity_type: entity_type.to_string(), data })
            .collect();
        Some(StoreAsk {
            driver,
            key: self.next_store_key(),
            request: IoRequest::WriteRows { batch: RowWriteBatch { ops, on_failure: RowWriteFailureMode::Stop } },
        })
    }

    pub(crate) fn next_store_key(&self) -> String {
        format!("store-{}", self.store_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    /// `entity_type` declares `store: <driver>`: its row asks are answered by that driver orbital.
    pub(crate) fn bind_store(&self, entity_type: &str, driver: &str) {
        self.store_drivers.write().insert(entity_type.to_string(), driver.to_string());
    }

    /// A store-bound entity's rows exist only in its driver, reached on the resumable path.
    fn store_off_path(&self, entity_type: &str) -> String {
        let driver = self.store_drivers.read().get(entity_type).cloned().unwrap_or_default();
        format!("entity {entity_type} is bound to store driver {driver}: its rows are answered only on the resumable path (EXEC-STORE-4)")
    }

    fn store_bound(&self, entity_type: &str) -> bool {
        self.store_drivers.read().contains_key(entity_type)
    }

    /// The driver orbital that answers `request`; a batch is answered by one only when every op is bound to it.
    fn store_driver_for(&self, request: &IoRequest) -> Option<String> {
        let drivers = self.store_drivers.read();
        match request {
            IoRequest::ReadRow { entity_type, .. }
            | IoRequest::ListRows { entity_type }
            | IoRequest::QueryRows { entity_type, .. } => drivers.get(entity_type).cloned(),
            IoRequest::WriteRows { batch } => {
                let mut ops = batch.ops.iter().map(|op| drivers.get(op.entity_type()));
                let first = ops.next()??;
                ops.all(|d| d == Some(first)).then(|| first.clone())
            }
            _ => None,
        }
    }

    /// A single delete or update on a store-bound entity: ONE conditional op, so a missing row fails on every
    /// store (EXEC-STEP-8). `None` when the entity is not bound or no step is in flight.
    fn bound_single_write(
        &self,
        entity_type: &str,
        op: impl FnOnce() -> RowWriteOp,
    ) -> Option<Result<RowWriteOutcome, String>> {
        if !self.store_bound(entity_type) {
            return None;
        }
        if self.direct() {
            return Some(Err(self.store_off_path(entity_type)));
        }
        let batch = RowWriteBatch { ops: vec![op()], on_failure: RowWriteFailureMode::Stop };
        Some(match self.ask(|| IoRequest::WriteRows { batch }) {
            Asked::Answer(IoAnswer::Wrote(outcomes)) => {
                self.forget_reads(entity_type);
                outcomes.and_then(|mut o| o.pop().ok_or_else(|| "the store driver answered a write with no outcome".to_string()))
            }
            Asked::Direct | Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        })
    }

    /// Serve the rows of `[runtime]` entities from the kernel's own memory instead of asking the host.
    pub(crate) fn with_runtime_rows(mut self, in_kernel: bool) -> Self {
        self.runtime_in_kernel = in_kernel;
        self
    }

    /// `entity_type` is a `[runtime]` entity: its rows live in the kernel when the kernel owns them.
    pub(crate) fn claim_runtime(&self, entity_type: &str) {
        if self.runtime_in_kernel {
            self.runtime_types.write().insert(entity_type.to_string());
        }
    }

    /// Whether this kernel holds the rows of `entity_type` itself (a claimed `[runtime]` entity).
    pub(crate) fn holds_runtime(&self, entity_type: &str) -> bool {
        self.local(entity_type)
    }

    fn local(&self, entity_type: &str) -> bool {
        self.runtime_in_kernel && self.runtime_types.read().contains(entity_type)
    }

    /// Where the rows of `entity_type` are read and written when no resumable step is in flight.
    fn source_for(&self, entity_type: &str) -> &dyn EntitySource {
        if self.local(entity_type) {
            &self.memory
        } else {
            self.source.as_ref()
        }
    }

    /// The entity a row request addresses, when it is one a kernel-owned `[runtime]` entity answers itself.
    fn local_entity<'r>(&self, request: &'r IoRequest) -> Option<&'r str> {
        let entity = match request {
            IoRequest::ReadRow { entity_type, .. }
            | IoRequest::ListRows { entity_type }
            | IoRequest::QueryRows { entity_type, .. }
            | IoRequest::CreateRow { entity_type, .. }
            | IoRequest::WriteRow { entity_type, .. }
            | IoRequest::DeleteRow { entity_type, .. } => entity_type.as_str(),
            _ => return None,
        };
        self.local(entity).then_some(entity)
    }

    /// This port reads time from `clock`, the kernel's configured dispatch clock (`KernelConfig.clock`).
    pub(crate) fn with_clock(mut self, clock: Option<crate::config::DispatchClock>) -> Self {
        self.clock = clock;
        self
    }

    pub(crate) fn set_query_capabilities(
        &self,
        entity_type: &str,
        capabilities: QueryCapabilities,
    ) {
        self.query_capabilities
            .write()
            .insert(entity_type.to_string(), capabilities);
    }

    pub(crate) fn set_write_capabilities(
        &self,
        entity_type: &str,
        capabilities: WriteCapabilities,
    ) {
        self.write_capabilities
            .write()
            .insert(entity_type.to_string(), capabilities);
    }

    /// What the host declared its query push-down honours for `entity_type`, when it declared anything.
    pub(crate) fn query_capabilities_of(&self, entity_type: &str) -> Option<QueryCapabilities> {
        self.query_capabilities.read().get(entity_type).cloned()
    }

    /// One warn per (kind, entity, reason) per port.
    fn notice(&self, kind: &str, entity_type: &str, reason: &str, detail: &str) {
        if self
            .noticed
            .lock()
            .insert(format!("{kind}\u{1f}{entity_type}\u{1f}{reason}"))
        {
            tracing::warn!(
                target: orbital_core_base::logging::ns::RUNTIME_FETCH,
                entity = %entity_type,
                reason = %reason,
                detail = %detail,
                "{}",
                kind
            );
        }
    }

    /// Whether I/O right now goes straight to the host handlers (no resumable step in flight on this thread).
    fn direct(&self) -> bool {
        match self.ledger.lock().as_ref() {
            None => true,
            Some(ledger) => ledger.thread != std::thread::current().id(),
        }
    }

    /// Stamp `site` on the `call-service` made while it is set.
    pub(crate) fn set_call_site(&self, site: Option<CallSite>) {
        *self.site.lock() = site;
        *self.verdict.lock() = None;
        *self.substrate_key.lock() = None;
    }

    /// Stamp `site` and `key` on the substrate op run while they are set.
    pub(crate) fn set_substrate_call(&self, site: Option<CallSite>, key: Option<String>) {
        *self.site.lock() = site;
        *self.verdict.lock() = None;
        *self.substrate_key.lock() = key;
    }

    /// A keyed substrate op with a declared route: the key and site its ask carries, else neither.
    fn substrate_site(&self) -> (Option<String>, Option<CallSite>) {
        let key = self.substrate_key.lock().clone();
        let site = self.site.lock().clone().filter(|site| {
            site.emit.success.is_some() || site.emit.failure.is_some() || site.emit.on_message.is_some()
        });
        match (key, site) {
            (Some(key), Some(site)) => (Some(key), Some(site)),
            _ => (None, None),
        }
    }

    /// How the host settled the last `call-service`, when not by a result or an error.
    pub(crate) fn take_call_verdict(&self) -> Option<CallVerdict> {
        self.verdict.lock().take()
    }

    pub(crate) fn has_service(&self) -> bool {
        self.service.is_some()
    }

    pub(crate) fn has_substrate(&self) -> bool {
        self.substrate.is_some()
    }

    /// Whether the step in flight on this thread has asked for I/O it has no answer for yet, or
    /// diverged from its record: the step's result is garbage and must be rolled back.
    pub(crate) fn suspended(&self) -> bool {
        self.ledger.lock().as_ref().is_some_and(|l| {
            l.thread == std::thread::current().id()
                && (!l.pending.is_empty() || !l.store_asks.is_empty() || l.diverged.is_some())
        })
    }

    /// Run `step` with `journal` as the answers its I/O is served from. Steps from different threads
    /// take turns; I/O from threads that are not running a step goes straight to the host handlers.
    pub(crate) fn run_step<R>(
        &self,
        journal: Vec<Exchange>,
        step: impl FnOnce() -> R,
    ) -> (R, StepEnd) {
        self.run_step_reading(journal, Arc::default(), step)
    }

    /// [`Self::run_step`] with the rows the dispatch has already read: the step reads them without asking.
    pub(crate) fn run_step_reading<R>(
        &self,
        journal: Vec<Exchange>,
        reads: Arc<ReadCache>,
        step: impl FnOnce() -> R,
    ) -> (R, StepEnd) {
        let gate = self.gate.lock();
        *self.ledger.lock() = Some(StepLedger {
            thread: std::thread::current().id(),
            claimed: Claimed::default(),
            reads_at_start: reads.clone(),
            reads,
            journal,
            pending: Asks::new(),
            store_asks: Vec::new(),
            keys: HashSet::new(),
            blocked: false,
            diverged: None,
        });
        let installed = Installed {
            port: self,
            _gate: gate,
        };
        let result = step();
        let ledger = installed.port.ledger.lock().take();
        drop(installed);
        let end = match ledger {
            Some(l) => StepEnd {
                reads: if l.pending.is_empty() && l.store_asks.is_empty() && l.diverged.is_none() {
                    l.reads
                } else {
                    l.reads_at_start
                },
                journal: l.journal,
                pending: l.pending,
                store_asks: l.store_asks,
                diverged: l.diverged,
            },
            None => StepEnd {
                reads: Arc::default(),
                journal: Vec::new(),
                pending: Asks::new(),
                store_asks: Vec::new(),
                diverged: None,
            },
        };
        (result, end)
    }

    /// The row the dispatch already read from the host, when the step in flight may answer from it.
    fn read_before(&self, entity_type: &str, id: &str) -> Option<Option<Arc<Value>>> {
        let slot = self.ledger.lock();
        let ledger = slot.as_ref()?;
        if ledger.thread != std::thread::current().id() || ledger.blocked || ledger.diverged.is_some() {
            return None;
        }
        ledger.reads.get(&(entity_type.to_string(), id.to_string())).cloned()
    }

    fn remember_read(&self, entity_type: &str, id: &str, row: Option<Arc<Value>>) {
        if self.local(entity_type) {
            return;
        }
        if let Some(ledger) = self.ledger.lock().as_mut() {
            if ledger.thread == std::thread::current().id() {
                Arc::make_mut(&mut ledger.reads).insert((entity_type.to_string(), id.to_string()), row);
            }
        }
    }

    /// The kernel wrote `entity_type`: what the dispatch read of it is no longer what the host holds.
    fn forget_reads(&self, entity_type: &str) {
        if let Some(ledger) = self.ledger.lock().as_mut() {
            if ledger.thread == std::thread::current().id() && ledger.reads.keys().any(|(t, _)| t == entity_type) {
                Arc::make_mut(&mut ledger.reads).retain(|(t, _), _| t != entity_type);
            }
        }
    }

    /// Serve `request` from the journal, or record it. A keyed `call-service` is matched to its recorded
    /// answer by key and, with no answer yet, is recorded as one more pending request while the call
    /// returns the deferred value, so the step goes on and a later keyed call is asked in the same step.
    /// Every other ask is matched to the first unclaimed journal entry and, with no answer yet, is the
    /// step's one pending request; any further ask of that kind is withheld unrecorded.
    fn ask(&self, request: impl FnOnce() -> IoRequest) -> Asked {
        let mut slot = self.ledger.lock();
        let direct = match slot.as_ref() {
            None => true,
            Some(ledger) => ledger.thread != std::thread::current().id(),
        };
        if direct {
            drop(slot);
            return self.direct_ask(request);
        }
        let Some(ledger) = slot.as_mut() else {
            return Asked::Direct;
        };
        if ledger.diverged.is_some() || (ledger.blocked && ledger.claimed.covers(ledger.journal.len())) {
            return Asked::Withheld;
        }
        let request = request();
        let keyed = match &request {
            IoRequest::CallService { key: Some(key), .. } => Some(key.clone()),
            _ => None,
        };
        if let Some(key) = &keyed {
            if !ledger.keys.insert(key.clone()) {
                return Asked::Answer(IoAnswer::Service(Err(format!(
                    "call-service key '{key}' is already used by another call in this step"
                ))));
            }
        }
        let store_driver = self.store_driver_for(&request);
        let claimed = &ledger.claimed;
        let at = ledger.journal.iter().enumerate().position(|(i, recorded)| {
            !claimed.has(i)
                && match &keyed {
                    Some(key) => matches!(
                        &recorded.request,
                        IoRequest::CallService { key: Some(recorded_key), .. } if recorded_key == key
                    ),
                    None => true,
                }
        });
        if let Some(driver) = store_driver.filter(|_| at.is_none() && !ledger.blocked) {
            ledger.store_asks.push(StoreAsk { driver, key: self.next_store_key(), request });
            ledger.blocked = true;
            return Asked::Withheld;
        }
        match at {
            Some(i) => {
                let recorded = &ledger.journal[i];
                if recorded.request == request {
                    let answer = recorded.answer.clone();
                    ledger.claimed.claim(i);
                    Asked::Answer(answer)
                } else {
                    ledger.diverged = Some(ResumeError::Diverged {
                        recorded: Box::new(recorded.request.clone()),
                        got: Box::new(request),
                    });
                    Asked::Withheld
                }
            }
            None if !ledger.blocked && self.local_entity(&request).is_some() => {
                let answer = BlockingHost { source: &self.memory, service: None, substrate: None }.answer(&request);
                ledger.journal.push(Exchange::new(request, answer.clone()));
                let at = ledger.journal.len() - 1;
                ledger.claimed.claim(at);
                Asked::Answer(answer)
            }
            None if keyed.is_some() && !ledger.blocked => {
                ledger.pending.push(request);
                Asked::Answer(IoAnswer::ServiceDeferred)
            }
            None => {
                if ledger.pending.is_empty() && !ledger.blocked {
                    ledger.pending.push(request);
                }
                ledger.blocked = true;
                Asked::Withheld
            }
        }
    }

    /// An ask with no step in flight runs in place, except a row ask on a store-bound entity, which fails.
    fn direct_ask(&self, request: impl FnOnce() -> IoRequest) -> Asked {
        if self.store_drivers.read().is_empty() {
            return Asked::Direct;
        }
        let request = request();
        let Some(entity) = request_entity(&request) else {
            return Asked::Direct;
        };
        if !self.store_bound(entity) {
            return Asked::Direct;
        }
        let message = self.store_off_path(entity);
        match crate::store_driver::failed_answer(&request, message) {
            Some(answer) => Asked::Answer(answer),
            None => Asked::Direct,
        }
    }

    /// Take the keyed `call-service` this step has recorded as pending out of the step, so it is never sent.
    /// `false` when the step has no such pending call (or can no longer trust its own state); the cancel is
    /// then an ask of its own.
    fn withdraw_pending_call(&self, key: &str) -> bool {
        let mut slot = self.ledger.lock();
        let Some(ledger) = slot.as_mut() else {
            return false;
        };
        if ledger.thread != std::thread::current().id()
            || ledger.blocked
            || ledger.diverged.is_some()
        {
            return false;
        }
        let Some(at) = ledger.pending.iter().position(|pending| {
            matches!(pending, IoRequest::CallService { key: Some(pending_key), .. } if pending_key == key)
        }) else {
            return false;
        };
        ledger.pending.remove(at);
        true
    }
}

impl EntitySource for IoPort {
    fn read(&self, entity_type: &str, id: &str) -> Result<Arc<Value>, String> {
        match self.find(entity_type, id)? {
            Some(row) => Ok(row),
            None => Err(format!("Entity not found: {entity_type}:{id}")),
        }
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Arc<Value>>, String> {
        if let Some(read) = self.read_before(entity_type, id) {
            return Ok(read);
        }
        match self.ask(|| IoRequest::ReadRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
        }) {
            Asked::Direct => self.source_for(entity_type).find(entity_type, id),
            Asked::Answer(IoAnswer::Row(Ok(Value::Null))) => {
                self.remember_read(entity_type, id, None);
                Ok(None)
            }
            Asked::Answer(IoAnswer::Row(row)) => row.map(|r| {
                let row = Arc::new(r);
                self.remember_read(entity_type, id, Some(row.clone()));
                Some(row)
            }),
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let update = || RowWriteOp::Update {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
            data: data.clone(),
            guard: None,
        };
        if let Some(outcome) = self.bound_single_write(entity_type, update) {
            return match outcome? {
                RowWriteOutcome::Updated { row, .. } => Ok(row),
                RowWriteOutcome::NotFound => Err(format!("Entity not found: {entity_type}:{id}")),
                other => Err(format!("the store driver answered an update with {other:?}")),
            };
        }
        match self.ask(|| IoRequest::WriteRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
            data: data.clone(),
        }) {
            Asked::Direct => self.source_for(entity_type).write(entity_type, id, data),
            Asked::Answer(IoAnswer::Row(row)) => {
                self.forget_reads(entity_type);
                row
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let create = || RowWriteOp::Create { entity_type: entity_type.to_string(), data: data.clone() };
        if let Some(outcome) = self.bound_single_write(entity_type, create) {
            return match outcome? {
                RowWriteOutcome::Created { row } => Ok(row),
                RowWriteOutcome::DuplicateId => Err(format!("Entity already exists: {entity_type}")),
                other => Err(format!("the store driver answered a create with {other:?}")),
            };
        }
        match self.ask(|| IoRequest::CreateRow {
            entity_type: entity_type.to_string(),
            data: data.clone(),
        }) {
            Asked::Direct => self.source_for(entity_type).create(entity_type, data),
            Asked::Answer(IoAnswer::Row(row)) => {
                self.forget_reads(entity_type);
                row
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let delete = || RowWriteOp::Delete { entity_type: entity_type.to_string(), id: id.to_string(), guard: None };
        if let Some(outcome) = self.bound_single_write(entity_type, delete) {
            return match outcome? {
                RowWriteOutcome::Deleted { .. } => Ok(()),
                RowWriteOutcome::NotFound => Err(format!("Entity not found: {entity_type}:{id}")),
                other => Err(format!("the store driver answered a delete with {other:?}")),
            };
        }
        match self.ask(|| IoRequest::DeleteRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
        }) {
            Asked::Direct => self.source_for(entity_type).delete(entity_type, id),
            Asked::Answer(IoAnswer::Deleted(done)) => {
                self.forget_reads(entity_type);
                done
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn clear(&self) {
        self.source.clear();
        self.memory.clear();
    }

    fn count(&self) -> usize {
        self.source.count() + self.memory.count()
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String> {
        match self.ask(|| IoRequest::ListRows {
            entity_type: entity_type.to_string(),
        }) {
            Asked::Direct => self.source_for(entity_type).list(entity_type),
            Asked::Answer(IoAnswer::Rows(rows)) => rows,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn scan(
        &self,
        entity_type: &str,
        visit: &mut dyn FnMut(&Arc<Value>) -> ScanControl,
    ) -> Result<(), String> {
        if self.direct() {
            if self.store_bound(entity_type) {
                return Err(self.store_off_path(entity_type));
            }
            return self.source_for(entity_type).scan(entity_type, visit);
        }
        for row in self.list(entity_type)? {
            if visit(&row) == ScanControl::Stop {
                break;
            }
        }
        Ok(())
    }

    fn query_page(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowPage, String>> {
        if self.direct() {
            if self.store_bound(entity_type) {
                return Some(Err(self.store_off_path(entity_type)));
            }
            return self.source_for(entity_type).query_page(entity_type, query);
        }
        let capabilities = self.query_capabilities.read().get(entity_type).cloned();
        if let Some(capabilities) = capabilities {
            match query_unsupported_reason(&capabilities, query) {
                Some(reason) => self.notice(
                    "kernel.fetch.pushdown-fallback",
                    entity_type,
                    reason.as_str(),
                    "",
                ),
                None => match self.ask(|| IoRequest::QueryRows {
                    entity_type: entity_type.to_string(),
                    query: query.clone(),
                    project: RowProjection::Rows,
                }) {
                    Asked::Direct => return self.source_for(entity_type).query_page(entity_type, query),
                    Asked::Answer(IoAnswer::Page(page)) => {
                        return Some(page.and_then(|page| {
                            echo_checked(entity_type, query, &page)?;
                            Ok(page)
                        }))
                    }
                    Asked::Answer(IoAnswer::QueryUnsupported(detail)) => self.notice(
                        "kernel.fetch.pushdown-fallback",
                        entity_type,
                        "host-declined",
                        &detail,
                    ),
                    Asked::Answer(_) | Asked::Withheld => {
                        return Some(Err(AWAITING_HOST.to_string()))
                    }
                },
            }
        }
        let mut run = QueryRun::new(query);
        if let Err(e) = self.scan(entity_type, &mut |row| run.visit(row)) {
            return Some(Err(e));
        }
        Some(run.finish())
    }

    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowIdPage, String>> {
        if self.direct() {
            if self.store_bound(entity_type) {
                return Some(Err(self.store_off_path(entity_type)));
            }
            return self.source_for(entity_type).query_ids(entity_type, query);
        }
        let capabilities = self.query_capabilities.read().get(entity_type).cloned();
        if let Some(capabilities) = capabilities {
            let reason = if capabilities.ids_only == Some(true) {
                query_unsupported_reason(&capabilities, query).map(|r| r.as_str())
            } else {
                Some("ids-only")
            };
            match reason {
                Some(reason) => {
                    self.notice("kernel.fetch.pushdown-fallback", entity_type, reason, "")
                }
                None => match self.ask(|| IoRequest::QueryRows {
                    entity_type: entity_type.to_string(),
                    query: query.clone(),
                    project: RowProjection::Ids,
                }) {
                    Asked::Direct => return self.source_for(entity_type).query_ids(entity_type, query),
                    Asked::Answer(IoAnswer::IdPage(page)) => return Some(page),
                    Asked::Answer(IoAnswer::QueryUnsupported(detail)) => self.notice(
                        "kernel.fetch.pushdown-fallback",
                        entity_type,
                        "host-declined",
                        &detail,
                    ),
                    Asked::Answer(_) | Asked::Withheld => {
                        return Some(Err(AWAITING_HOST.to_string()))
                    }
                },
            }
        }
        let mut run = QueryRun::new(query);
        if let Err(e) = self.scan(entity_type, &mut |row| run.visit(row)) {
            return Some(Err(e));
        }
        Some(run.finish_ids())
    }

    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        if let Some(outcome) = self.bound_single_write(op.entity_type(), || op.clone()) {
            return Some(outcome);
        }
        if self.direct() {
            if self.store_bound(op.entity_type()) {
                return Some(Err(self.store_off_path(op.entity_type())));
            }
            return self.source.write_where(op);
        }
        let batch = RowWriteBatch {
            ops: vec![op.clone()],
            on_failure: RowWriteFailureMode::Stop,
        };
        self.push_writes(&batch).map(|sent| {
            sent.and_then(|mut outcomes| {
                outcomes
                    .pop()
                    .ok_or_else(|| "the host answered a write with no outcome".to_string())
            })
        })
    }

    fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        if self.direct() {
            if let Some(op) = batch.ops.iter().find(|op| self.store_bound(op.entity_type())) {
                return Some(Err(self.store_off_path(op.entity_type())));
            }
            return self.source.write_batch(batch);
        }
        self.push_writes(batch)
    }

    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.source.bind_collection(entity_type, collection)
    }
}

impl IoPort {
    /// Send `batch` as one `WriteRows` when the host declared capabilities covering every op; `None` when it
    /// did not (or declined) and nothing was written.
    fn push_writes(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        {
            let declared = self.write_capabilities.read();
            for op in &batch.ops {
                let entity = op.entity_type();
                let capabilities = declared.get(entity)?;
                let reason = if !capabilities.conditional {
                    Some("not-conditional")
                } else if batch.on_failure == RowWriteFailureMode::Atomic
                    && batch.ops.len() > 1
                    && !capabilities.atomic_batch
                {
                    Some("atomic-batch")
                } else if batch.on_failure == RowWriteFailureMode::Stop
                    && batch.ops.len() > 1
                    && !capabilities.stop_runs
                {
                    Some("stop-run")
                } else {
                    op.guard()
                        .and_then(|g| guard_unsupported_reason(capabilities, g))
                        .map(|r| r.as_str())
                };
                if let Some(reason) = reason {
                    self.notice("kernel.write.pushdown-fallback", entity, reason, "");
                    return None;
                }
            }
        }
        match self.ask(|| IoRequest::WriteRows {
            batch: batch.clone(),
        }) {
            Asked::Direct => self.source.write_batch(batch),
            Asked::Answer(IoAnswer::Wrote(outcomes)) => {
                for op in &batch.ops {
                    self.forget_reads(op.entity_type());
                }
                Some(outcomes)
            }
            Asked::Answer(IoAnswer::WriteUnsupported(detail)) => {
                let entity = batch.ops.first().map_or("", |op| op.entity_type());
                self.notice(
                    "kernel.write.pushdown-fallback",
                    entity,
                    "host-declined",
                    &detail,
                );
                None
            }
            Asked::Answer(_) | Asked::Withheld => Some(Err(AWAITING_HOST.to_string())),
        }
    }
}

/// A host's page must hold only rows the query's policy admits: a mis-implemented adapter cannot widen what a
/// viewer sees.
fn echo_checked(entity_type: &str, query: &RowQuery, page: &RowPage) -> Result<(), String> {
    if let Some(policy) = &query.policy {
        if page.rows.iter().any(|row| !policy.matches(row)) {
            tracing::error!(
                target: orbital_core_base::logging::ns::RUNTIME_FETCH,
                entity = %entity_type,
                "the host returned a row its @read policy rejects"
            );
            return Err(format!(
                "fetch {entity_type}: the host returned a row that the declared @read policy rejects"
            ));
        }
    }
    Ok(())
}

impl ServiceHandler for IoPort {
    fn call(
        &self,
        service: &str,
        method: &str,
        args: &Value,
        key: Option<&str>,
        caller: Option<&ServiceCaller>,
    ) -> Result<Value, String> {
        match self.ask(|| IoRequest::CallService {
            service: service.to_string(),
            method: method.to_string(),
            args: args.clone(),
            key: key.map(str::to_string),
            caller: caller.cloned(),
            site: self.site.lock().clone(),
        }) {
            Asked::Direct => match &self.service {
                Some(handler) => handler.call(service, method, args, key, caller),
                None => Err("no service handler configured".to_string()),
            },
            Asked::Answer(IoAnswer::Service(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(CALL_CANCELLED.to_string())
            }
            Asked::Answer(IoAnswer::ServiceDeferred) => {
                *self.verdict.lock() = Some(CallVerdict::Deferred);
                Ok(Value::Null)
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn cancel(&self, key: &str) -> Result<(), String> {
        if self.withdraw_pending_call(key) {
            return Ok(());
        }
        match self.ask(|| IoRequest::CancelCall {
            key: key.to_string(),
        }) {
            Asked::Direct => match &self.service {
                Some(handler) => handler.cancel(key),
                None => Err("no service handler configured".to_string()),
            },
            Asked::Answer(IoAnswer::Cancelled(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }
}

impl IoPort {
    /// One request of the `http` effect. A cancelled verdict is recorded for the kernel's completion route.
    pub(crate) fn http_send(
        &self,
        request: &HttpRequest,
        key: Option<&str>,
        delay_ms: Option<u64>,
    ) -> Result<HttpResponse, TransportError> {
        match self.ask(|| IoRequest::Http {
            request: request.clone(),
            key: key.map(str::to_string),
            delay_ms,
            site: self.site.lock().clone(),
        }) {
            Asked::Direct => Err(TransportError::Failed(NO_HTTP_HOST.to_string())),
            Asked::Answer(answer) => self.http_answer(answer),
            Asked::Withheld => Err(TransportError::Failed(AWAITING_HOST.to_string())),
        }
    }

    pub(crate) fn http_stream(
        &self,
        request: &HttpRequest,
        key: Option<&str>,
        framing: StreamFraming,
        continuation: Option<Box<crate::http_request::HttpCallDriver>>,
    ) -> Result<HttpResponse, TransportError> {
        match self.ask(|| IoRequest::HttpStream {
            request: request.clone(),
            key: key.map(str::to_string),
            framing,
            site: self.site.lock().clone(),
            continuation,
        }) {
            Asked::Direct => Err(TransportError::Failed(NO_HTTP_HOST.to_string())),
            Asked::Answer(answer) => self.http_answer(answer),
            Asked::Withheld => Err(TransportError::Failed(AWAITING_HOST.to_string())),
        }
    }

    /// The built request of a beacon, or `None` when no host is attached to answer it.
    pub(crate) fn http_beacon(
        &self,
        request: &HttpRequest,
        key: Option<&str>,
    ) -> Option<Result<BeaconOutcome, TransportError>> {
        match self.ask(|| IoRequest::HttpBeacon {
            request: request.clone(),
            key: key.map(str::to_string),
            site: self.site.lock().clone(),
        }) {
            Asked::Direct => None,
            Asked::Answer(IoAnswer::HttpBeacon(out)) => Some(out),
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Some(Err(TransportError::Failed(CALL_CANCELLED.to_string())))
            }
            Asked::Answer(_) | Asked::Withheld => Some(Err(TransportError::Failed(AWAITING_HOST.to_string()))),
        }
    }

    pub(crate) fn page_facts(&self, fields: &[PageFactField]) -> Result<PageFacts, String> {
        match self.ask(|| IoRequest::PageFacts { fields: fields.to_vec() }) {
            Asked::Direct => Err(NO_PAGE_FACTS_HOST.to_string()),
            Asked::Answer(IoAnswer::PageFacts(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    /// One `browser/prompt` ask. A host that answers it cancelled records the verdict for the kernel's route.
    pub(crate) fn prompt(&self, text: &str, schema: &str, key: Option<&str>) -> Result<String, ModelFailure> {
        match self.ask(|| IoRequest::Prompt {
            text: text.to_string(),
            schema: schema.to_string(),
            key: key.map(str::to_string),
        }) {
            Asked::Direct => Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_PROMPT_HOST)),
            Asked::Answer(IoAnswer::Prompt(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(ModelFailure::new(ModelFailureKind::Failed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(ModelFailure::new(ModelFailureKind::Failed, AWAITING_HOST)),
        }
    }

    /// One `browser/embed` ask, as [`Self::prompt`].
    pub(crate) fn embed(&self, texts: &[String], key: Option<&str>) -> Result<Vec<Vec<f64>>, ModelFailure> {
        match self.ask(|| IoRequest::Embed { texts: texts.to_vec(), key: key.map(str::to_string) }) {
            Asked::Direct => Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_EMBED_HOST)),
            Asked::Answer(IoAnswer::Embed(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(ModelFailure::new(ModelFailureKind::Failed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(ModelFailure::new(ModelFailureKind::Failed, AWAITING_HOST)),
        }
    }

    /// One raw `browser/tab-*` ask. A host that answers it cancelled records the verdict for the kernel's route.
    pub(crate) fn tab(&self, ask: &TabAsk, key: Option<&str>) -> Result<Value, TabFailure> {
        match self.ask(|| IoRequest::Tab { ask: ask.clone(), key: key.map(str::to_string) }) {
            Asked::Direct => Err(TabFailure::new(TabFailureKind::Unavailable, NO_TAB_HOST)),
            Asked::Answer(IoAnswer::Tab(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(TabFailure::new(TabFailureKind::Failed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(TabFailure::new(TabFailureKind::Failed, AWAITING_HOST)),
        }
    }

    fn http_answer(&self, answer: IoAnswer) -> Result<HttpResponse, TransportError> {
        match answer {
            IoAnswer::Http(out) => out,
            IoAnswer::ServiceDeferred => {
                *self.verdict.lock() = Some(CallVerdict::Deferred);
                Err(TransportError::Failed(AWAITING_HOST.to_string()))
            }
            IoAnswer::ServiceCancelled => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(TransportError::Failed(CALL_CANCELLED.to_string()))
            }
            _ => Err(TransportError::Failed(AWAITING_HOST.to_string())),
        }
    }

    pub(crate) fn secret(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
        match self.ask(|| IoRequest::Secret {
            name: name.to_string(),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::Secret(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn store_secret(
        &self,
        name: &str,
        value: &str,
        ttl_ms: u64,
        principal: Option<&str>,
    ) -> Result<(), String> {
        match self.ask(|| IoRequest::SecretSet {
            name: name.to_string(),
            value: value.to_string(),
            ttl_ms,
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::SecretSet(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn rotate_secret(&self, name: &str, value: &str, principal: Option<&str>) -> Result<(), String> {
        match self.ask(|| IoRequest::SecretRotate {
            name: name.to_string(),
            value: value.to_string(),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::SecretRotate(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    /// One OAuth ask, journaled; a kernel with no host to answer has no OAuth state.
    pub(crate) fn oauth(&self, ask: &OAuthAsk, principal: Option<&str>) -> Result<OAuthReply, String> {
        match self.ask(|| IoRequest::OAuth { ask: ask.clone(), principal: principal.map(str::to_string) }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::OAuth(out)) => out.and_then(|reply| {
                if ask.accepts(&reply) { Ok(reply) } else { Err("the host answered an OAuth ask with the wrong reply".to_string()) }
            }),
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    /// One `os/<op>` ask, journaled. A streaming op, and a keyed op that defers its completion under a declared
    /// route, carries the call site its events dispatch to; the host may answer the latter `ServiceDeferred`.
    pub(crate) fn os(&self, op: OsOp, args: &[Value], key: Option<&str>) -> Result<Value, OsError> {
        match self.ask(|| IoRequest::Os {
            op,
            args: args.to_vec(),
            key: key.map(str::to_string),
            site: self.os_site(op, key),
        }) {
            Asked::Direct => Err(OsError::new(OsErrorKind::Unsupported, NO_HTTP_HOST)),
            Asked::Answer(IoAnswer::Os(out)) => out,
            Asked::Answer(IoAnswer::ServiceDeferred) => {
                *self.verdict.lock() = Some(CallVerdict::Deferred);
                Ok(Value::Null)
            }
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(OsError::new(OsErrorKind::Closed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(OsError::new(OsErrorKind::Io, AWAITING_HOST)),
        }
    }

    fn os_site(&self, op: OsOp, key: Option<&str>) -> Option<CallSite> {
        let site = self.site.lock().clone()?;
        let routed = site.emit.success.is_some() || site.emit.failure.is_some();
        ((op.is_stream() && (op != OsOp::StdinRead || key.is_some()))
            || (op.defers_completion() && key.is_some() && routed))
            .then_some(site)
    }

    pub(crate) fn clock(&self) -> Result<i64, String> {
        if let Some(clock) = &self.clock {
            return Ok(clock() as i64);
        }
        match self.ask(|| IoRequest::Clock) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::Clock(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn random(&self, bytes: u32) -> Result<Vec<u8>, String> {
        match self.ask(|| IoRequest::Random { bytes }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::Random(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_status(&self, name: &str, principal: Option<&str>) -> Result<crate::credential_admin::CredentialStanding, String> {
        match self.ask(|| IoRequest::CredentialStatus {
            name: name.to_string(),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStatus(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_forget(&self, name: &str, principal: Option<&str>) -> Result<(), String> {
        match self.ask(|| IoRequest::CredentialForget {
            name: name.to_string(),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialForget(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_list(&self) -> Result<Option<StoreListing>, String> {
        match self.ask(|| IoRequest::CredentialStoreList) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreList(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_read(&self, name: &str) -> Result<Option<String>, String> {
        match self.ask(|| IoRequest::CredentialStoreRead { name: name.to_string() }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreRead(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_write(&self, service: &str, name: &str, value: &str) -> Result<(), String> {
        match self.ask(|| IoRequest::CredentialStoreWrite {
            service: service.to_string(),
            name: name.to_string(),
            value: value.to_string(),
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreWrite(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_remove(&self, name: &str) -> Result<bool, String> {
        match self.ask(|| IoRequest::CredentialStoreRemove { name: name.to_string() }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreRemove(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_rotate(&self) -> Result<RotateCounts, String> {
        match self.ask(|| IoRequest::CredentialStoreRotate) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreRotate(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_mode_read(&self) -> Result<Option<String>, String> {
        match self.ask(|| IoRequest::CredentialModeRead) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialModeRead(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_mode_write(&self, mode: &str) -> Result<String, String> {
        match self.ask(|| IoRequest::CredentialModeWrite { mode: mode.to_string() }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialModeWrite(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    /// One cross-unit delivery, journaled. A kernel with no step in flight has no router to hand it to: the
    /// delivery is refused `down`, so the emitter sees it instead of the emit vanishing.
    pub(crate) fn deliver(&self, envelope: &Envelope, site: CallSite) -> DeliverAsk {
        match self.ask(|| IoRequest::Deliver {
            envelope: envelope.clone(),
            key: None,
            site,
        }) {
            Asked::Answer(IoAnswer::Deliver(out)) => DeliverAsk::Answered(out),
            Asked::Direct | Asked::Answer(_) => DeliverAsk::Answered(Err(DeliverRefusal {
                kind: DeliverRefusalKind::Down,
                id: envelope.id.clone(),
            })),
            Asked::Withheld => DeliverAsk::Suspended,
        }
    }
}

/// What a [`IoPort::deliver`] came back with: the host's answer, or no answer yet (the step is suspended).
pub(crate) enum DeliverAsk {
    Answered(Result<Accepted, DeliverRefusal>),
    Suspended,
}

const NO_HTTP_HOST: &str = "no HTTP host is configured for this kernel";

impl SubstrateHandler for IoPort {
    fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
        let (key, site) = self.substrate_site();
        match self.ask(|| IoRequest::Substrate {
            op: op.to_string(),
            args: args.to_vec(),
            key,
            site,
        }) {
            Asked::Direct => match &self.substrate {
                Some(handler) => handler.run(op, args),
                None => Err(SubstrateFailure {
                    message: format!("no substrate host configured for {op}"),
                    errors: Vec::new(),
                }),
            },
            Asked::Answer(IoAnswer::Substrate(out)) => out,
            Asked::Answer(IoAnswer::ServiceDeferred) => {
                *self.verdict.lock() = Some(CallVerdict::Deferred);
                Ok(Value::Null)
            }
            Asked::Answer(_) | Asked::Withheld => Err(SubstrateFailure {
                message: AWAITING_HOST.to_string(),
                errors: Vec::new(),
            }),
        }
    }
}

/// Stands in for a store, service or substrate host that answers through [`Progress::Await`]: the
/// kernel is built with it where an async host has no synchronous handler. A call that reaches it
/// outside a resumable run fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostAnswered;

const HOST_ANSWERED: &str = "this host answers through Progress::Await";

impl EntitySource for HostAnswered {
    fn read(&self, _: &str, _: &str) -> Result<Arc<Value>, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn write(&self, _: &str, _: &str, _: &Value) -> Result<Value, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn create(&self, _: &str, _: &Value) -> Result<Value, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn delete(&self, _: &str, _: &str) -> Result<(), String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn clear(&self) {}
    fn count(&self) -> usize {
        0
    }
    fn list(&self, _: &str) -> Result<Vec<Arc<Value>>, String> {
        Err(HOST_ANSWERED.to_string())
    }
}

impl ServiceHandler for HostAnswered {
    fn call(
        &self,
        _: &str,
        _: &str,
        _: &Value,
        _: Option<&str>,
        _: Option<&ServiceCaller>,
    ) -> Result<Value, String> {
        Err(HOST_ANSWERED.to_string())
    }
    fn cancel(&self, _: &str) -> Result<(), String> {
        Err(HOST_ANSWERED.to_string())
    }
}

impl SubstrateHandler for HostAnswered {
    fn run(&self, _: &str, _: &[Value]) -> Result<Value, SubstrateFailure> {
        Err(SubstrateFailure {
            message: HOST_ANSWERED.to_string(),
            errors: Vec::new(),
        })
    }
}

/// The synchronous handlers of a blocking host, as one answerer of [`IoRequest`]s.
pub struct BlockingHost<'a> {
    pub source: &'a dyn EntitySource,
    pub service: Option<&'a dyn ServiceHandler>,
    pub substrate: Option<&'a dyn SubstrateHandler>,
}

impl BlockingHost<'_> {
    pub fn answer(&self, request: &IoRequest) -> IoAnswer {
        match request {
            IoRequest::ReadRow { entity_type, id } => IoAnswer::Row(
                self.source
                    .find(entity_type, id)
                    .map(|row| row.map_or(Value::Null, |r| (*r).clone())),
            ),
            IoRequest::ListRows { entity_type } => IoAnswer::Rows(self.source.list(entity_type)),
            IoRequest::QueryRows {
                entity_type,
                query,
                project: RowProjection::Rows,
            } => match self.source.query_page(entity_type, query) {
                Some(page) => IoAnswer::Page(page),
                None => IoAnswer::QueryUnsupported("the source has no query push-down".to_string()),
            },
            IoRequest::QueryRows {
                entity_type,
                query,
                project: RowProjection::Ids,
            } => match self.source.query_ids(entity_type, query) {
                Some(page) => IoAnswer::IdPage(page),
                None => IoAnswer::QueryUnsupported("the source has no query push-down".to_string()),
            },
            IoRequest::WriteRows { batch } => match self.source.write_batch(batch) {
                Some(outcomes) => IoAnswer::Wrote(outcomes),
                None => {
                    IoAnswer::WriteUnsupported("the source has no conditional writes".to_string())
                }
            },
            IoRequest::CreateRow { entity_type, data } => {
                IoAnswer::Row(self.source.create(entity_type, data))
            }
            IoRequest::WriteRow {
                entity_type,
                id,
                data,
            } => IoAnswer::Row(self.source.write(entity_type, id, data)),
            IoRequest::DeleteRow { entity_type, id } => {
                IoAnswer::Deleted(self.source.delete(entity_type, id))
            }
            IoRequest::CallService {
                service,
                method,
                args,
                key,
                caller,
                ..
            } => IoAnswer::Service(match self.service {
                Some(handler) => {
                    handler.call(service, method, args, key.as_deref(), caller.as_ref())
                }
                None => Err("no service handler configured".to_string()),
            }),
            IoRequest::CancelCall { key } => IoAnswer::Cancelled(match self.service {
                Some(handler) => handler.cancel(key),
                None => Err("no service handler configured".to_string()),
            }),
            IoRequest::Substrate { op, args, .. } => IoAnswer::Substrate(match self.substrate {
                Some(handler) => handler.run(op, args),
                None => Err(SubstrateFailure {
                    message: format!("no substrate host configured for {op}"),
                    errors: Vec::new(),
                }),
            }),
            IoRequest::Http { .. }
            | IoRequest::HttpStream { .. }
            | IoRequest::HttpBeacon { .. }
            | IoRequest::PageFacts { .. }
            | IoRequest::Prompt { .. }
            | IoRequest::Embed { .. }
            | IoRequest::Tab { .. }
            | IoRequest::Secret { .. }
            | IoRequest::SecretSet { .. }
            | IoRequest::SecretRotate { .. }
            | IoRequest::Random { .. }
            | IoRequest::Clock
            | IoRequest::Os { .. }
            | IoRequest::CredentialStatus { .. }
            | IoRequest::CredentialForget { .. }
            | IoRequest::CredentialStoreList
            | IoRequest::CredentialStoreRead { .. }
            | IoRequest::CredentialStoreWrite { .. }
            | IoRequest::CredentialStoreRemove { .. }
            | IoRequest::CredentialStoreRotate
            | IoRequest::CredentialModeRead
            | IoRequest::CredentialModeWrite { .. }
            | IoRequest::OAuth { .. }
            | IoRequest::Deliver { .. } => answer_http(None, request),
        }
    }

    /// [`Self::answer`], with the `http` effect's requests answered by `http`.
    pub fn answer_with_http(&self, http: &HttpHost<'_>, request: &IoRequest) -> IoAnswer {
        match request {
            IoRequest::Http { .. }
            | IoRequest::HttpStream { .. }
            | IoRequest::HttpBeacon { .. }
            | IoRequest::PageFacts { .. }
            | IoRequest::Prompt { .. }
            | IoRequest::Embed { .. }
            | IoRequest::Tab { .. }
            | IoRequest::Secret { .. }
            | IoRequest::SecretSet { .. }
            | IoRequest::SecretRotate { .. }
            | IoRequest::Random { .. }
            | IoRequest::Clock
            | IoRequest::Os { .. }
            | IoRequest::CredentialStatus { .. }
            | IoRequest::CredentialForget { .. }
            | IoRequest::CredentialStoreList
            | IoRequest::CredentialStoreRead { .. }
            | IoRequest::CredentialStoreWrite { .. }
            | IoRequest::CredentialStoreRemove { .. }
            | IoRequest::CredentialStoreRotate
            | IoRequest::CredentialModeRead
            | IoRequest::CredentialModeWrite { .. }
            | IoRequest::OAuth { .. }
            | IoRequest::Deliver { .. } => answer_http(Some(http), request),
            IoRequest::CancelCall { key } if os_completion_cancelled(http, key) => IoAnswer::Cancelled(Ok(())),
            other => self.answer(other),
        }
    }
}

/// What a blocking host holds to answer the `http` effect's requests.
pub struct HttpHost<'a> {
    pub transport: &'a dyn Transport,
    pub secrets: &'a dyn SecretSource,
    /// The program's `egress`: an IP-literal origin listed here may be a private address.
    pub egress: &'a [String],
    /// Receives each event of a stream, with the call site whose `onMessage` route it belongs to.
    pub on_event: &'a dyn Fn(Option<&CallSite>, Value),
    /// The native `os/` answerer's grants and handles; `None` denies every `os/` ask.
    #[cfg(not(target_arch = "wasm32"))]
    pub os: Option<&'a crate::os_host::OsHost<'a>>,
}

/// A request whose resolved address is not public is refused unless its origin is an IP literal listed in egress.
fn address_refusal(url: &str, egress: &[String]) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let default_port = if url.starts_with("https://") { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') => (h, p.parse::<u16>().unwrap_or(default_port)),
        _ => (authority, default_port),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let listed = egress
            .iter()
            .any(|e| http_request::origin_allowed(url, std::slice::from_ref(e)));
        return (!listed && !orbital_core_base::http_wire::is_public_address(ip))
            .then(|| format!("refused: {ip} is not a public address"));
    }
    use std::net::ToSocketAddrs;
    match (host, port).to_socket_addrs() {
        Ok(mut addrs) => addrs
            .find(|a| !orbital_core_base::http_wire::is_public_address(a.ip()))
            .map(|a| format!("refused: {host} resolves to {}, not a public address", a.ip())),
        Err(e) => Some(format!("{host} did not resolve: {e}")),
    }
}

fn blocking_send(
    host: &HttpHost<'_>,
    request: &HttpRequest,
) -> Result<HttpResponse, TransportError> {
    if let Some(refusal) = address_refusal(&request.url, host.egress) {
        return Err(TransportError::Refused(refusal));
    }
    host.transport.send(request)
}

/// `bytes` of operating-system CSPRNG output; wasm hosts answer `Random` themselves.
#[cfg(not(target_arch = "wasm32"))]
fn os_random(bytes: u32) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; bytes as usize];
    getrandom::getrandom(&mut buf).map_err(|e| format!("random: {e}"))?;
    Ok(buf)
}

#[cfg(target_arch = "wasm32")]
fn os_random(_bytes: u32) -> Result<Vec<u8>, String> {
    Err("random: the host must answer Random".to_string())
}

/// `cancel-call` of a deferred keyed `os/` call: its pending completion is dropped, so its outcome never fires a route.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn os_completion_cancelled(host: &HttpHost<'_>, key: &str) -> bool {
    host.os.is_some_and(|os| os.handles.cancel_completion(key))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn os_completion_cancelled(_host: &HttpHost<'_>, _key: &str) -> bool {
    false
}

/// An [`IoRequest::Os`] answered by the native answerer; wasm hosts answer `Os` themselves.
#[cfg(not(target_arch = "wasm32"))]
fn answer_os_host(host: Option<&HttpHost<'_>>, request: &IoRequest) -> IoAnswer {
    crate::os_host::answer_os(host.and_then(|h| h.os), request)
}

#[cfg(target_arch = "wasm32")]
fn answer_os_host(_host: Option<&HttpHost<'_>>, _request: &IoRequest) -> IoAnswer {
    IoAnswer::Os(Err(OsError::new(OsErrorKind::Unsupported, "os: the host must answer Os")))
}

/// Answer an [`IoRequest::Http`] / `HttpStream` / `Secret` / `SecretSet` / `Random` / `Os` / `CredentialStatus` /
/// `CredentialForget` with `host`; with no host an `http` request fails.
pub fn answer_http(host: Option<&HttpHost<'_>>, request: &IoRequest) -> IoAnswer {
    match (request, host) {
        (IoRequest::Http { request, delay_ms, .. }, Some(host)) => {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(ms) = delay_ms {
                std::thread::sleep(std::time::Duration::from_millis(*ms));
            }
            #[cfg(target_arch = "wasm32")]
            let _ = delay_ms;
            IoAnswer::Http(blocking_send(host, request))
        }
        (
            IoRequest::HttpStream {
                request,
                key,
                framing,
                site,
                ..
            },
            Some(host),
        ) => IoAnswer::Http((|| {
            if let Some(refusal) = address_refusal(&request.url, host.egress) {
                return Err(TransportError::Refused(refusal));
            }
            let mut decoder = http_request::StreamDecoder::new(*framing, key.as_deref());
            let reply = host.transport.stream(request, &mut |bytes| {
                for event in decoder.push(bytes) {
                    (host.on_event)(site.as_ref(), event);
                }
                Ok(())
            })?;
            for event in decoder.finish() {
                (host.on_event)(site.as_ref(), event);
            }
            Ok(reply)
        })()),
        (IoRequest::HttpBeacon { request, .. }, Some(host)) => IoAnswer::HttpBeacon(
            blocking_send(host, request).map(|reply| BeaconOutcome::Fetched { status: reply.status }),
        ),
        (IoRequest::HttpBeacon { .. }, None) => {
            IoAnswer::HttpBeacon(Err(TransportError::Failed(NO_HTTP_HOST.to_string())))
        }
        (IoRequest::PageFacts { .. }, _) => IoAnswer::PageFacts(Ok(PageFacts {
            available: false,
            hostname: String::new(),
            referrer: String::new(),
            screen_width: None,
        })),
        (IoRequest::Prompt { .. }, _) => IoAnswer::Prompt(Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_PROMPT_HOST))),
        (IoRequest::Embed { .. }, _) => IoAnswer::Embed(Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_EMBED_HOST))),
        (IoRequest::Tab { .. }, _) => IoAnswer::Tab(Err(TabFailure::new(TabFailureKind::Unavailable, NO_TAB_HOST))),
        (IoRequest::Http { .. } | IoRequest::HttpStream { .. }, None) => {
            IoAnswer::Http(Err(TransportError::Failed(NO_HTTP_HOST.to_string())))
        }
        (IoRequest::Secret { name, principal }, Some(host)) => {
            IoAnswer::Secret(host.secrets.secret_of(name, principal.as_deref()))
        }
        (IoRequest::Secret { .. }, None) => IoAnswer::Secret(Err(NO_HTTP_HOST.to_string())),
        (
            IoRequest::SecretSet {
                name,
                value,
                ttl_ms,
                principal,
            },
            Some(host),
        ) => IoAnswer::SecretSet(host.secrets.store_secret_of(name, value, *ttl_ms, principal.as_deref())),
        (IoRequest::SecretSet { .. }, None) => IoAnswer::SecretSet(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::SecretRotate { name, value, principal }, Some(host)) => {
            IoAnswer::SecretRotate(host.secrets.rotate_secret_of(name, value, principal.as_deref()))
        }
        (IoRequest::SecretRotate { .. }, None) => IoAnswer::SecretRotate(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::Random { bytes }, _) => IoAnswer::Random(os_random(*bytes)),
        (IoRequest::Clock, _) => IoAnswer::Clock(Ok(orbital_core_base::time_util::now_millis())),
        (IoRequest::Os { .. }, _) => answer_os_host(host, request),
        (IoRequest::CredentialStatus { name, principal }, Some(host)) => {
            IoAnswer::CredentialStatus(host.secrets.secret_of(name, principal.as_deref()).and_then(|value| {
                let connected = crate::http_service::credential_status_connected(
                    value.as_deref(),
                    principal.as_deref(),
                );
                let label = if connected { host.secrets.label_of(name, principal.as_deref())? } else { None };
                Ok(crate::credential_admin::CredentialStanding { connected, label })
            }))
        }
        (IoRequest::CredentialForget { name, principal }, Some(host)) => {
            IoAnswer::CredentialForget(host.secrets.forget_of(name, principal.as_deref()))
        }
        (IoRequest::OAuth { ask, principal }, Some(host)) => {
            IoAnswer::OAuth(host.secrets.oauth_of(ask, principal.as_deref()))
        }
        (IoRequest::OAuth { .. }, None) => IoAnswer::OAuth(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::Deliver { envelope, .. }, _) => IoAnswer::Deliver(Err(DeliverRefusal {
            kind: DeliverRefusalKind::Down,
            id: envelope.id.clone(),
        })),
        (IoRequest::CredentialStatus { .. }, None) => IoAnswer::CredentialStatus(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialForget { .. }, None) => IoAnswer::CredentialForget(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialStoreList, Some(host)) => IoAnswer::CredentialStoreList(host.secrets.credential_store_list()),
        (IoRequest::CredentialStoreRead { name }, Some(host)) => {
            IoAnswer::CredentialStoreRead(host.secrets.credential_store_read(name))
        }
        (IoRequest::CredentialStoreWrite { service, name, value }, Some(host)) => {
            IoAnswer::CredentialStoreWrite(host.secrets.credential_store_write(service, name, value))
        }
        (IoRequest::CredentialStoreRemove { name }, Some(host)) => {
            IoAnswer::CredentialStoreRemove(host.secrets.credential_store_remove(name))
        }
        (IoRequest::CredentialStoreRotate, Some(host)) => IoAnswer::CredentialStoreRotate(host.secrets.credential_store_rotate()),
        (IoRequest::CredentialModeRead, Some(host)) => IoAnswer::CredentialModeRead(host.secrets.credential_mode()),
        (IoRequest::CredentialModeWrite { mode }, Some(host)) => {
            IoAnswer::CredentialModeWrite(host.secrets.set_credential_mode(mode))
        }
        (IoRequest::CredentialStoreList, None) => IoAnswer::CredentialStoreList(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialStoreRead { .. }, None) => IoAnswer::CredentialStoreRead(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialStoreWrite { .. }, None) => IoAnswer::CredentialStoreWrite(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialStoreRemove { .. }, None) => IoAnswer::CredentialStoreRemove(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialStoreRotate, None) => IoAnswer::CredentialStoreRotate(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialModeRead, None) => IoAnswer::CredentialModeRead(Err(NO_HTTP_HOST.to_string())),
        (IoRequest::CredentialModeWrite { .. }, None) => IoAnswer::CredentialModeWrite(Err(NO_HTTP_HOST.to_string())),
        _ => IoAnswer::Service(Err("not an HTTP request".to_string())),
    }
}

/// Answer each [`Progress::Await`] on the spot with `answer` until the dispatch is done: the adapter
/// that lets a blocking host run the resumable code path.
pub fn drive_blocking(
    kernel: &crate::kernel::RuntimeKernel,
    progress: Progress<DispatchOutcomes>,
    answer: &dyn Fn(&IoRequest) -> IoAnswer,
) -> Result<DispatchOutcomes, DispatchError> {
    drive_with(kernel, progress, |requests| {
        requests
            .iter()
            .map(|p| IoReply { id: p.id, answer: answer(&p.request) })
            .collect()
    })
}

/// [`drive_blocking`] for a host whose answerer is `Sync`: the keyed `call-service` requests of one step run on
/// scoped threads at the same time (EXEC-QUEUE-8) and every other request is answered in order on this thread.
pub fn drive_concurrent(
    kernel: &crate::kernel::RuntimeKernel,
    progress: Progress<DispatchOutcomes>,
    answer: &(dyn Fn(&IoRequest) -> IoAnswer + Sync),
) -> Result<DispatchOutcomes, DispatchError> {
    drive_with(kernel, progress, |requests| answer_batch(requests, answer))
}

/// Answer one step's pending requests: each keyed `call-service` on its own scoped thread, the rest in
/// order on the calling thread. The replies keep the order of `requests`; the kernel matches them by id.
pub fn answer_batch(
    requests: &[PendingRequest],
    answer: &(dyn Fn(&IoRequest) -> IoAnswer + Sync),
) -> Vec<IoReply> {
    let dispatch_id = crate::dispatch_trace::answer_dispatch_id();
    std::thread::scope(|scope| {
        let overlapped: Vec<_> = requests
            .iter()
            .map(|p| {
                matches!(p.request, IoRequest::CallService { key: Some(_), .. })
                    .then(|| scope.spawn(move || {
                        let _context = crate::dispatch_trace::AnswerContext::enter(dispatch_id);
                        answer(&p.request)
                    }))
            })
            .collect();
        requests
            .iter()
            .zip(overlapped)
            .map(|(p, handle)| IoReply {
                id: p.id,
                answer: match handle {
                    Some(handle) => handle
                        .join()
                        .unwrap_or_else(|_| IoAnswer::Service(Err("the call-service answerer panicked".to_string()))),
                    None => answer(&p.request),
                },
            })
            .collect()
    })
}

fn drive_with(
    kernel: &crate::kernel::RuntimeKernel,
    mut progress: Progress<DispatchOutcomes>,
    mut serve: impl FnMut(&[PendingRequest]) -> Vec<IoReply>,
) -> Result<DispatchOutcomes, DispatchError> {
    loop {
        match progress {
            Progress::Done(outcomes) => return Ok(outcomes),
            Progress::Await { continuation, requests } => {
                let context = crate::dispatch_trace::AnswerContext::enter(Some(continuation.dispatch_id()));
                let replies = serve(&requests);
                drop(context);
                progress = kernel.resume(continuation, replies)?;
            }
        }
    }
}

/// Check `replies` against `pending` and turn them into the exchanges the replayed step is served from.
pub(crate) fn accept_replies(
    pending: &[PendingRequest],
    replies: Vec<IoReply>,
) -> Result<Vec<Exchange>, ResumeError> {
    let mut answers: Vec<Option<IoAnswer>> = vec![None; pending.len()];
    for reply in replies {
        let Some(at) = pending.iter().position(|p| p.id == reply.id) else {
            return Err(ResumeError::Unknown(reply.id));
        };
        if answers[at].is_some() {
            return Err(ResumeError::Duplicate(reply.id));
        }
        if !reply.answer.answers(&pending[at].request) {
            return Err(ResumeError::WrongKind(reply.id));
        }
        answers[at] = Some(reply.answer);
    }
    pending
        .iter()
        .zip(answers)
        .map(|(p, answer)| match answer {
            Some(answer) => Ok(Exchange::new(p.request.clone(), answer)),
            None => Err(ResumeError::Unanswered(p.id)),
        })
        .collect()
}

#[cfg(test)]
mod http_replay_tests {
    use super::*;
    use crate::http_request::StreamFraming;
    use std::time::Duration;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn call(id: u64, key: Option<&str>) -> PendingRequest {
        PendingRequest {
            id: RequestId(id),
            request: IoRequest::CallService {
                service: "svc".to_string(),
                method: "work".to_string(),
                args: Value::Null,
                key: key.map(str::to_string),
                caller: None,
                site: None,
            },
        }
    }

    fn widest_overlap(requests: &[PendingRequest]) -> (usize, Vec<IoReply>) {
        let inflight = AtomicUsize::new(0);
        let widest = AtomicUsize::new(0);
        let replies = answer_batch(requests, &|_| {
            let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
            widest.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(60));
            inflight.fetch_sub(1, Ordering::SeqCst);
            IoAnswer::Service(Ok(Value::Null))
        });
        (widest.load(Ordering::SeqCst), replies)
    }

    #[test]
    fn keyed_calls_of_one_step_are_answered_at_the_same_time_and_in_request_order() {
        let requests = [call(7, Some("a")), call(8, Some("b")), call(9, Some("c"))];
        let (widest, replies) = widest_overlap(&requests);
        assert!(widest >= 2, "keyed calls ran one at a time: {widest}");
        let ids: Vec<RequestId> = replies.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![RequestId(7), RequestId(8), RequestId(9)]);
    }

    #[test]
    fn control_unkeyed_calls_are_answered_one_at_a_time() {
        let requests = [call(1, None), call(2, None), call(3, None)];
        let (widest, replies) = widest_overlap(&requests);
        assert_eq!(widest, 1);
        assert_eq!(replies.len(), 3);
    }

    #[test]
    fn a_panicking_answerer_becomes_a_failed_call_not_a_crashed_host() {
        let requests = [call(1, Some("a")), call(2, Some("b"))];
        let replies = answer_batch(&requests, &|request| match request {
            IoRequest::CallService { key: Some(key), .. } if key == "a" => panic!("boom"),
            _ => IoAnswer::Service(Ok(Value::Null)),
        });
        assert!(matches!(&replies[0].answer, IoAnswer::Service(Err(message)) if message.contains("panicked")));
        assert!(matches!(&replies[1].answer, IoAnswer::Service(Ok(_))));
    }

    fn get(url: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: Some(Duration::from_secs(1)),
        }
    }

    fn port() -> IoPort {
        IoPort::new(Arc::new(HostAnswered), None, None)
    }

    fn recorded(request: &HttpRequest) -> Vec<Exchange> {
        vec![Exchange::new(
            IoRequest::Http {
                request: request.clone(),
                key: None,
                delay_ms: None,
                site: None,
            },
            IoAnswer::Http(Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: b"ok".to_vec(),
            })),
        )]
    }

    #[test]
    fn a_replayed_step_is_served_its_recorded_reply_and_asks_nothing() {
        let request = get("https://a.example.com/x");
        let port = port();
        let (reply, end) = port.run_step(recorded(&request), || port.http_send(&request, None, None));
        assert_eq!(reply.map(|r| r.status), Ok(200));
        assert!(end.pending.is_empty() && end.diverged.is_none());
    }

    #[test]
    fn a_replayed_step_that_asks_a_changed_request_diverges() {
        let port = port();
        let (_, end) = port.run_step(recorded(&get("https://a.example.com/x")), || {
            port.http_send(&get("https://a.example.com/y"), None, None)
        });
        assert!(
            matches!(end.diverged, Some(ResumeError::Diverged { .. })),
            "{:?}",
            end.diverged
        );
        assert!(end.pending.is_empty());
    }

    #[test]
    fn a_changed_key_or_a_stream_in_place_of_a_send_also_diverges() {
        let request = get("https://a.example.com/x");
        let port = port();
        let (_, end) = port.run_step(recorded(&request), || port.http_send(&request, Some("k"), None));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
        let (_, end) = port.run_step(recorded(&request), || {
            port.http_stream(&request, None, StreamFraming::Sse, None)
        });
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    }

    #[test]
    fn a_replayed_step_is_compared_with_its_delay() {
        let request = get("https://a.example.com/x");
        let port = port();
        let delayed = vec![Exchange::new(
            IoRequest::Http { request: request.clone(), key: None, delay_ms: Some(500), site: None },
            IoAnswer::Http(Ok(HttpResponse { status: 200, headers: Vec::new(), body: b"ok".to_vec() })),
        )];
        let (reply, end) = port.run_step(delayed.clone(), || port.http_send(&request, None, Some(500)));
        assert_eq!(reply.map(|r| r.status), Ok(200));
        assert!(end.pending.is_empty() && end.diverged.is_none());
        // Controls: the same request asked with another delay, or with none, diverges.
        for asked in [Some(501), None] {
            let (_, end) = port.run_step(delayed.clone(), || port.http_send(&request, None, asked));
            assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })), "{asked:?}");
        }
    }

    #[test]
    fn a_step_with_no_record_leaves_the_request_pending() {
        let request = get("https://a.example.com/x");
        let port = port();
        let (_, end) = port.run_step(Vec::new(), || port.http_send(&request, None, None));
        assert!(matches!(end.pending.as_slice(), [IoRequest::Http { .. }]));
    }

    struct Canned(Vec<u8>);

    impl Transport for Canned {
        fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: self.0.clone(),
            })
        }
    }

    struct Vaulted;

    impl SecretSource for Vaulted {
        fn secret(&self, _: &str) -> Result<Option<String>, String> {
            Ok(Some("sk-live-999".to_string()))
        }
    }

    fn streamed(key: Option<&str>) -> Vec<Value> {
        let transport = Canned(b"data: a\n\ndata: b".to_vec());
        let seen = Mutex::new(Vec::new());
        let on_event = |_: Option<&CallSite>, event: Value| seen.lock().push(event);
        let host = HttpHost {
            transport: &transport,
            secrets: &Vaulted,
            egress: &[],
            on_event: &on_event,
            os: None,
        };
        let request = IoRequest::HttpStream {
            request: get("https://8.8.8.8/x"),
            key: key.map(str::to_string),
            framing: StreamFraming::Sse,
            site: None,
            continuation: None,
        };
        let IoAnswer::Http(Ok(reply)) = answer_http(Some(&host), &request) else {
            panic!("expected an Http reply")
        };
        assert!(reply.body.is_empty());
        let out = seen.lock().clone();
        out
    }

    fn keys(events: &[Value]) -> Vec<Value> {
        events
            .iter()
            .map(|e| match e {
                Value::Object(fields) => fields.get("key").cloned().unwrap_or(Value::String("absent".into())),
                other => panic!("event is not an object: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_blocking_stream_answer_carries_the_key_the_framer_adds_flush_included() {
        let events = streamed(Some("job"));
        assert_eq!(keys(&events), vec![Value::String("job".into()); 2]);
    }

    #[test]
    fn control_an_unkeyed_blocking_stream_carries_a_null_key() {
        let events = streamed(None);
        assert_eq!(keys(&events), vec![Value::Null; 2]);
    }

    #[test]
    fn no_debug_of_a_request_answer_or_journal_shows_a_secret_value() {
        let secret = "sk-live-999";
        let authorized = HttpRequest {
            headers: vec![
                ("Authorization".to_string(), format!("Bearer {secret}")),
                ("X-Trace".to_string(), "t-1".to_string()),
            ],
            body: format!("client_secret={secret}").into_bytes(),
            ..get(&format!("https://a.example.com/x?api_key={secret}"))
        };
        let journal = vec![
            Exchange::new(
                IoRequest::Secret { name: "TOKEN".to_string(), principal: None },
                IoAnswer::Secret(Ok(Some(secret.to_string()))),
            ),
            Exchange::new(
                IoRequest::SecretSet {
                    name: "oauth2:t".to_string(),
                    value: secret.to_string(),
                    ttl_ms: 5,
                    principal: None,
                },
                IoAnswer::SecretSet(Ok(())),
            ),
            Exchange::new(
                IoRequest::Http {
                    request: authorized.clone(),
                    key: None,
                    delay_ms: None,
                    site: None,
                },
                IoAnswer::Http(Ok(HttpResponse {
                    status: 200,
                    headers: vec![("Set-Cookie".to_string(), secret.to_string())],
                    body: format!("{{\"access_token\":\"{secret}\"}}").into_bytes(),
                })),
            ),
        ];
        let shown = format!("{journal:?} {:?}", ResumeError::Diverged {
            recorded: Box::new(journal[1].request.clone()),
            got: Box::new(journal[2].request.clone()),
        });
        assert!(!shown.contains(secret), "leaked: {shown}");
        assert!(!shown.contains("api_key"), "the query is not shown: {shown}");
        assert!(shown.contains("TOKEN") && shown.contains("oauth2:t"), "control: names are shown");
        assert!(shown.contains("X-Trace") && shown.contains("https://a.example.com/x"), "control: headers and url");
    }

    #[test]
    fn a_blocking_random_answer_is_fresh_csprng_output_of_the_asked_length() {
        let ask = IoRequest::Random { bytes: 32 };
        let (IoAnswer::Random(Ok(a)), IoAnswer::Random(Ok(b))) =
            (answer_http(None, &ask), answer_http(None, &ask))
        else {
            panic!("expected random bytes")
        };
        assert_eq!((a.len(), b.len()), (32, 32));
        assert_ne!(a, b);
        assert!(a.iter().any(|byte| *byte != 0));
    }

    #[test]
    fn a_pinned_dispatch_clock_answers_the_clock_ask_without_asking_the_host() {
        let pinned: crate::config::DispatchClock = Arc::new(|| 1_700_000_000_123.0);
        let port = port().with_clock(Some(pinned));
        let (out, end) = port.run_step(Vec::new(), || port.clock());
        assert_eq!(out, Ok(1_700_000_000_123));
        assert!(end.pending.is_empty() && end.diverged.is_none(), "{:?}", end.pending);
    }

    #[test]
    fn control_with_no_pinned_clock_the_clock_ask_goes_to_the_host() {
        let port = port();
        let (_, end) = port.run_step(Vec::new(), || port.clock());
        assert!(matches!(end.pending.as_slice(), [IoRequest::Clock]));
    }

    #[test]
    fn a_random_ask_is_journaled_and_replayed_without_asking_again() {
        let port = port();
        let ask = IoRequest::Random { bytes: 4 };
        let recorded = vec![Exchange::new(ask, IoAnswer::Random(Ok(vec![1, 2, 3, 4])))];
        let (bytes, end) = port.run_step(recorded.clone(), || port.random(4));
        assert_eq!(bytes, Ok(vec![1, 2, 3, 4]));
        assert!(end.pending.is_empty() && end.diverged.is_none());
        let (_, end) = port.run_step(recorded, || port.random(5));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    }

    fn os_ask(op: OsOp, path: &str) -> IoRequest {
        IoRequest::Os { op, args: vec![Value::String(path.to_string())], key: None, site: None }
    }

    fn os_ok() -> IoAnswer {
        IoAnswer::Os(Ok(Value::Object(orbital_core_base::value::ValueMap::from([(
            "data".to_string(),
            Value::String("hi".to_string()),
        )]))))
    }

    #[test]
    fn an_os_ask_is_journaled_and_replayed_without_asking_again() {
        let port = port();
        let args = [Value::String("/a".to_string())];
        let recorded = vec![Exchange::new(os_ask(OsOp::ReadFile, "/a"), os_ok())];
        let (out, end) = port.run_step(recorded.clone(), || port.os(OsOp::ReadFile, &args, None));
        assert_eq!(out.map(|v| v.get_path("data").map(Value::to_string_value)), Ok(Some("hi".to_string())));
        assert!(end.pending.is_empty() && end.diverged.is_none());
    }

    #[test]
    fn a_changed_os_ask_is_diverged() {
        let port = port();
        let recorded = vec![Exchange::new(os_ask(OsOp::ReadFile, "/a"), os_ok())];
        let other_path = [Value::String("/b".to_string())];
        let (_, end) = port.run_step(recorded.clone(), || port.os(OsOp::ReadFile, &other_path, None));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
        let same_path = [Value::String("/a".to_string())];
        let (_, end) = port.run_step(recorded.clone(), || port.os(OsOp::WriteFile, &same_path, None));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
        let (_, end) = port.run_step(recorded, || port.os(OsOp::ReadFile, &same_path, Some("k")));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    }

    #[test]
    fn an_os_failure_answer_replays_as_the_same_failure() {
        let port = port();
        let failure = OsError::new(OsErrorKind::NotFound, "no such file");
        let recorded = vec![Exchange::new(os_ask(OsOp::Stat, "/gone"), IoAnswer::Os(Err(failure.clone())))];
        let (out, end) = port.run_step(recorded, || port.os(OsOp::Stat, &[Value::String("/gone".to_string())], None));
        assert_eq!(out, Err(failure));
        assert!(end.diverged.is_none());
    }

    #[test]
    fn an_os_ask_is_answered_only_by_an_os_answer_or_a_cancel() {
        let ask = os_ask(OsOp::ReadFile, "/a");
        assert!(os_ok().answers(&ask));
        assert!(IoAnswer::ServiceCancelled.answers(&ask));
        assert!(!IoAnswer::Random(Ok(vec![1])).answers(&ask));
        assert!(!IoAnswer::Http(Err(TransportError::Failed("x".into()))).answers(&ask));
        assert!(!os_ok().answers(&IoRequest::Random { bytes: 1 }));
        assert!(!os_ok().answers(&IoRequest::CancelCall { key: "k".into() }));
    }

    #[test]
    fn only_a_streaming_os_ask_carries_the_call_site() {
        let port = port();
        port.set_call_site(Some(CallSite {
            orbital: None,
            trait_name: "T".into(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: CallRoutes::default(),
        }));
        let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::ReadFile, &[Value::Null], None));
        let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
        assert!(site.is_none());
        let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::StdinRead, &[], Some("k")));
        let [IoRequest::Os { site, key, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
        assert_eq!(site.as_ref().map(|s| s.trait_name.clone()), Some("T".to_string()));
        assert_eq!(key.as_deref(), Some("k"));
    }

    fn routed_site() -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "T".into(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: CallRoutes { success: Some("DONE".into()), failure: Some("FAILED".into()), ..CallRoutes::default() },
        }
    }

    fn wait_ask(key: Option<&str>, site: Option<CallSite>) -> IoRequest {
        IoRequest::Os { op: OsOp::Wait, args: vec![Value::Number(7.0)], key: key.map(str::to_string), site }
    }

    #[test]
    fn a_keyed_wait_under_a_completion_route_carries_the_call_site_and_the_controls_do_not() {
        let port = port();
        port.set_call_site(Some(routed_site()));
        let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Wait, &[Value::Number(7.0)], Some("k")));
        let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
        assert_eq!(site.as_ref().map(|s| s.emit.success.clone()), Some(Some("DONE".to_string())));
        let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Wait, &[Value::Number(7.0)], None));
        let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
        assert!(site.is_none());
        let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Stat, &[Value::Null], Some("k")));
        let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
        assert!(site.is_none());
        port.set_call_site(Some(CallSite { emit: CallRoutes::default(), ..routed_site() }));
        let (_, end) = port.run_step(Vec::new(), || port.os(OsOp::Wait, &[Value::Number(7.0)], Some("k")));
        let [IoRequest::Os { site, .. }] = end.pending.as_slice() else { panic!("expected a pending os ask") };
        assert!(site.is_none());
    }

    #[test]
    fn only_a_keyed_wait_with_a_call_site_may_be_answered_deferred() {
        let deferred = IoAnswer::ServiceDeferred;
        assert!(deferred.answers(&wait_ask(Some("k"), Some(routed_site()))));
        assert!(!deferred.answers(&wait_ask(None, Some(routed_site()))));
        assert!(!deferred.answers(&wait_ask(Some("k"), None)));
        let stat = IoRequest::Os { op: OsOp::Stat, args: vec![Value::Null], key: Some("k".into()), site: Some(routed_site()) };
        assert!(!deferred.answers(&stat));
        assert!(os_ok().answers(&wait_ask(None, None)));
    }

    #[test]
    fn a_deferred_wait_returns_null_with_the_deferred_verdict_and_a_plain_answer_leaves_none() {
        let port = port();
        port.set_call_site(Some(routed_site()));
        let ask = wait_ask(Some("k"), Some(routed_site()));
        let args = [Value::Number(7.0)];
        let (out, end) = port.run_step(vec![Exchange::new(ask.clone(), IoAnswer::ServiceDeferred)], || {
            port.os(OsOp::Wait, &args, Some("k"))
        });
        assert_eq!(out, Ok(Value::Null));
        assert_eq!(port.take_call_verdict(), Some(CallVerdict::Deferred));
        assert!(end.pending.is_empty() && end.diverged.is_none());
        let (out, _) = port.run_step(vec![Exchange::new(ask, os_ok())], || port.os(OsOp::Wait, &args, Some("k")));
        assert!(out.is_ok_and(|v| v.get_path("data").is_some()));
        assert_eq!(port.take_call_verdict(), None);
    }

    #[test]
    fn the_native_answerer_is_reached_through_the_one_dispatch_point() {
        let ask = os_ask(OsOp::ReadFile, "/a");
        assert!(matches!(answer_http(None, &ask), IoAnswer::Os(Err(OsError { kind: OsErrorKind::Denied, .. }))));
        assert!(!matches!(answer_http(None, &IoRequest::CancelCall { key: "k".into() }), IoAnswer::Os(_)));
    }

    #[test]
    fn a_secret_ask_for_another_principal_diverges_from_the_recorded_one() {
        let port = port();
        let recorded = vec![Exchange::new(
            IoRequest::Secret { name: "t".into(), principal: Some("u1".into()) },
            IoAnswer::Secret(Ok(Some("v".into()))),
        )];
        let (value, end) = port.run_step(recorded.clone(), || port.secret("t", Some("u1")));
        assert_eq!(value, Ok(Some("v".to_string())));
        assert!(end.diverged.is_none());
        let (_, end) = port.run_step(recorded.clone(), || port.secret("t", Some("u2")));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
        let (_, end) = port.run_step(recorded, || port.secret("t", None));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    }

    struct PerUser;

    impl SecretSource for PerUser {
        fn secret(&self, _: &str) -> Result<Option<String>, String> {
            Ok(Some("app".to_string()))
        }
        fn secret_of(&self, name: &str, principal: Option<&str>) -> Result<Option<String>, String> {
            match principal {
                None => self.secret(name),
                Some("u1") => Ok(Some("mine".to_string())),
                Some(_) => Ok(None),
            }
        }
        fn forget_of(&self, _: &str, principal: Option<&str>) -> Result<(), String> {
            principal.map(|_| ()).ok_or_else(|| "no app-wide forget".to_string())
        }
    }

    #[test]
    fn a_blocking_host_answers_credential_asks_per_principal() {
        let transport = Canned(Vec::new());
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let host = HttpHost { transport: &transport, secrets: &PerUser, egress: &[], on_event: &on_event, os: None };
        let status = |principal: Option<&str>| {
            answer_http(
                Some(&host),
                &IoRequest::CredentialStatus { name: "t".into(), principal: principal.map(str::to_string) },
            )
        };
        assert_eq!(status(Some("u1")), IoAnswer::CredentialStatus(Ok(crate::credential_admin::CredentialStanding::of(true))));
        assert_eq!(status(Some("u2")), IoAnswer::CredentialStatus(Ok(crate::credential_admin::CredentialStanding::of(false))));
        let forget = answer_http(
            Some(&host),
            &IoRequest::CredentialForget { name: "t".into(), principal: Some("u1".into()) },
        );
        assert_eq!(forget, IoAnswer::CredentialForget(Ok(())));
        assert_eq!(
            answer_http(None, &IoRequest::CredentialStatus { name: "t".into(), principal: None }),
            IoAnswer::CredentialStatus(Err(NO_HTTP_HOST.to_string()))
        );
    }

    #[test]
    fn a_default_secret_source_keeps_no_user_credentials() {
        let user = Vaulted.secret_of("t", Some("u1"));
        assert!(user.is_err(), "{user:?}");
        assert_eq!(Vaulted.secret_of("t", None), Ok(Some("sk-live-999".to_string())));
        assert!(Vaulted.forget_of("t", Some("u1")).is_err());
    }

    #[test]
    fn outside_a_step_an_http_ask_fails_for_want_of_a_host() {
        let port = port();
        assert_eq!(
            port.http_send(&get("https://a.example.com/x"), None, None).map(|r| r.status),
            Err(TransportError::Failed(NO_HTTP_HOST.to_string()))
        );
    }

    fn keyed(key: &str) -> IoRequest {
        IoRequest::CallService {
            service: "svc".into(),
            method: "go".into(),
            args: Value::Null,
            key: Some(key.into()),
            caller: None,
            site: None,
        }
    }

    fn answered(key: &str, text: &str) -> Exchange {
        Exchange::new(
            keyed(key),
            IoAnswer::Service(Ok(Value::String(text.into()))),
        )
    }

    fn call_keyed(port: &IoPort, key: &str) -> Result<Value, String> {
        port.call("svc", "go", &Value::Null, Some(key), None)
    }

    fn call_unkeyed(port: &IoPort) -> Result<Value, String> {
        port.call("svc", "go", &Value::Null, None, None)
    }

    #[test]
    fn keyed_calls_are_all_pending_in_issue_order_and_each_returns_the_deferred_value() {
        let port = port();
        let (out, end) = port.run_step(Vec::new(), || {
            (
                call_keyed(&port, "a"),
                call_keyed(&port, "b"),
                call_keyed(&port, "c"),
            )
        });
        assert_eq!(out, (Ok(Value::Null), Ok(Value::Null), Ok(Value::Null)));
        assert_eq!(end.pending.as_slice(), vec![keyed("a"), keyed("b"), keyed("c")].as_slice());
        assert!(end.diverged.is_none());
        assert_eq!(port.take_call_verdict(), Some(CallVerdict::Deferred));
    }

    #[test]
    fn control_an_unkeyed_ask_is_the_one_pending_request_and_withholds_every_later_ask() {
        let port = port();
        let (out, end) = port.run_step(Vec::new(), || {
            (
                call_unkeyed(&port),
                call_keyed(&port, "a"),
                call_unkeyed(&port),
            )
        });
        assert_eq!(out.0, Err(AWAITING_HOST.to_string()));
        assert_eq!(out.1, Err(AWAITING_HOST.to_string()));
        assert_eq!(out.2, Err(AWAITING_HOST.to_string()));
        assert_eq!(end.pending.len(), 1);
        assert!(matches!(
            &end.pending[0],
            IoRequest::CallService { key: None, .. }
        ));
    }

    #[test]
    fn control_an_unkeyed_ask_after_keyed_ones_is_withheld_and_not_recorded() {
        let port = port();
        let (out, end) = port.run_step(Vec::new(), || {
            (
                call_keyed(&port, "a"),
                call_unkeyed(&port),
                call_keyed(&port, "b"),
            )
        });
        assert_eq!(out.0, Ok(Value::Null));
        assert_eq!(out.1, Err(AWAITING_HOST.to_string()));
        assert_eq!(out.2, Err(AWAITING_HOST.to_string()));
        assert_eq!(end.pending.as_slice(), vec![keyed("a")].as_slice());
    }

    #[test]
    fn a_keyed_call_is_served_its_answer_by_key_whatever_order_the_journal_holds() {
        let port = port();
        let expected = (Ok(Value::String("A".into())), Ok(Value::String("B".into())));
        for journal in [
            vec![answered("a", "A"), answered("b", "B")],
            vec![answered("b", "B"), answered("a", "A")],
        ] {
            let (out, end) =
                port.run_step(journal, || (call_keyed(&port, "a"), call_keyed(&port, "b")));
            assert_eq!(out, expected);
            assert!(end.pending.is_empty() && end.diverged.is_none());
        }
    }

    #[test]
    fn control_a_keyed_call_whose_request_changed_since_it_was_recorded_diverges() {
        let port = port();
        let (_, end) = port.run_step(vec![answered("a", "A")], || {
            port.call("svc", "go", &Value::Bool(true), Some("a"), None)
        });
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
        let (_, end) = port.run_step(vec![answered("a", "A")], || call_keyed(&port, "b"));
        assert!(end.diverged.is_none());
        assert_eq!(end.pending.as_slice(), vec![keyed("b")].as_slice());
    }

    #[test]
    fn a_key_used_twice_in_one_step_is_a_visible_error_and_the_first_call_stands() {
        let port = port();
        let (out, end) = port.run_step(Vec::new(), || {
            (call_keyed(&port, "a"), call_keyed(&port, "a"))
        });
        assert_eq!(out.0, Ok(Value::Null));
        assert!(
            out.1
                .as_ref()
                .is_err_and(|e| e.contains("already used by another call")),
            "{:?}",
            out.1
        );
        assert_eq!(end.pending.as_slice(), vec![keyed("a")].as_slice());
        let (out, end) = port.run_step(vec![answered("a", "A")], || {
            (call_keyed(&port, "a"), call_keyed(&port, "a"))
        });
        assert_eq!(out.0, Ok(Value::String("A".into())));
        assert!(
            out.1
                .as_ref()
                .is_err_and(|e| e.contains("already used by another call")),
            "{:?}",
            out.1
        );
        assert!(end.pending.is_empty());
    }

    #[test]
    fn cancelling_a_pending_keyed_call_removes_only_that_request_and_the_cancel_asks_nothing() {
        let port = port();
        let (out, end) = port.run_step(Vec::new(), || {
            let _ = call_keyed(&port, "a");
            let _ = call_keyed(&port, "b");
            let _ = call_keyed(&port, "c");
            port.cancel("b")
        });
        assert_eq!(out, Ok(()));
        assert_eq!(end.pending.as_slice(), vec![keyed("a"), keyed("c")].as_slice());
    }

    #[test]
    fn control_cancelling_a_key_that_is_not_pending_in_the_step_is_an_ask_of_its_own() {
        let port = port();
        let (out, end) = port.run_step(Vec::new(), || {
            let _ = call_keyed(&port, "a");
            port.cancel("zzz")
        });
        assert_eq!(out, Err(AWAITING_HOST.to_string()));
        assert_eq!(end.pending.as_slice(), vec![keyed("a")].as_slice());
        let (out, end) = port.run_step(Vec::new(), || port.cancel("zzz"));
        assert_eq!(out, Err(AWAITING_HOST.to_string()));
        assert_eq!(
            end.pending.as_slice(),
            [IoRequest::CancelCall { key: "zzz".into() }]
        );
    }

    #[test]
    fn a_cancelled_call_replays_without_diverging_from_the_answers_of_the_others() {
        let port = port();
        let (out, end) = port.run_step(vec![answered("b", "B")], || {
            let _ = call_keyed(&port, "a");
            let kept = call_keyed(&port, "b");
            (kept, port.cancel("a"))
        });
        assert_eq!(out, (Ok(Value::String("B".into())), Ok(())));
        assert!(end.pending.is_empty() && end.diverged.is_none());
    }

    #[test]
    fn replies_in_any_order_become_the_same_exchanges_in_issue_order() {
        let pending: Vec<PendingRequest> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, key)| PendingRequest {
                id: RequestId::from_raw(i as u64),
                request: keyed(key),
            })
            .collect();
        let reply = |i: u64| IoReply {
            id: RequestId::from_raw(i),
            answer: IoAnswer::Service(Ok(Value::Number(i as f64))),
        };
        let in_order = accept_replies(&pending, vec![reply(0), reply(1), reply(2)]).unwrap();
        for order in [[2, 1, 0], [1, 2, 0], [0, 2, 1], [2, 0, 1], [1, 0, 2]] {
            let got = accept_replies(&pending, order.iter().map(|i| reply(*i)).collect()).unwrap();
            assert_eq!(got.len(), in_order.len());
            for (g, e) in got.iter().zip(&in_order) {
                assert!(g.request == e.request && g.answer == e.answer);
            }
        }
    }

    #[test]
    fn replies_that_do_not_fit_the_pending_requests_are_refused_by_kind() {
        let pending: Vec<PendingRequest> = ["a", "b"]
            .iter()
            .enumerate()
            .map(|(i, key)| PendingRequest {
                id: RequestId::from_raw(i as u64),
                request: keyed(key),
            })
            .collect();
        let reply = |i: u64| IoReply {
            id: RequestId::from_raw(i),
            answer: IoAnswer::Service(Ok(Value::Null)),
        };
        assert!(
            matches!(accept_replies(&pending, vec![reply(1)]), Err(ResumeError::Unanswered(id)) if id.raw() == 0)
        );
        assert!(
            matches!(accept_replies(&pending, vec![reply(0), reply(1), reply(7)]), Err(ResumeError::Unknown(id)) if id.raw() == 7)
        );
        assert!(
            matches!(accept_replies(&pending, vec![reply(0), reply(0), reply(1)]), Err(ResumeError::Duplicate(id)) if id.raw() == 0)
        );
        assert!(accept_replies(&pending, vec![reply(1), reply(0)]).is_ok());
    }

    #[test]
    fn a_keyed_os_or_http_ask_may_be_left_pending_and_an_unkeyed_one_may_not() {
        let deferred = |request: IoRequest| {
            accept_replies(
                &[PendingRequest { id: RequestId::from_raw(0), request }],
                vec![IoReply { id: RequestId::from_raw(0), answer: IoAnswer::ServiceDeferred }],
            )
        };
        let os = |key: Option<&str>| IoRequest::Os {
            op: OsOp::Getenv,
            args: vec![Value::String("HOME".into())],
            key: key.map(str::to_string),
            site: None,
        };
        let http = |key: Option<&str>| IoRequest::Http {
            request: get("https://a.example.com/x"),
            key: key.map(str::to_string),
            delay_ms: None,
            site: None,
        };
        assert!(deferred(os(Some("home"))).is_ok(), "a keyed os/getenv may stay pending");
        assert!(deferred(http(Some("fetch"))).is_ok(), "a keyed http may stay pending");
        assert!(
            matches!(deferred(os(None)), Err(ResumeError::WrongKind(_))),
            "control: an unkeyed os ask has nothing to answer it later"
        );
        assert!(matches!(deferred(http(None)), Err(ResumeError::WrongKind(_))), "control: nor an unkeyed http");
    }
}

#[cfg(test)]
mod read_cache_tests {
    use super::*;

    fn port() -> IoPort {
        IoPort::new(Arc::new(HostAnswered), None, None)
    }

    fn row(id: &str) -> Value {
        serde_json::from_value(serde_json::json!({ "id": id })).unwrap()
    }

    fn read(id: &str) -> IoRequest {
        IoRequest::ReadRow { entity_type: "T".to_string(), id: id.to_string() }
    }

    fn read_answer(id: &str) -> Exchange {
        Exchange::new(read(id), IoAnswer::Row(Ok(row(id))))
    }

    #[test]
    fn a_row_read_twice_in_a_dispatch_is_asked_once() {
        let port = port();
        let (found, end) = port.run_step(vec![read_answer("1")], || (port.find("T", "1"), port.find("T", "1")));
        assert!(matches!(&found, (Ok(Some(_)), Ok(Some(_)))), "{found:?}");
        assert!(end.pending.is_empty() && end.diverged.is_none());
    }

    #[test]
    fn a_later_step_reads_what_an_earlier_one_was_answered_and_asks_only_for_a_new_row() {
        let port = port();
        let (_, first) = port.run_step(vec![read_answer("1")], || port.find("T", "1"));
        let (_, second) = port.run_step_reading(Vec::new(), first.reads, || (port.find("T", "1"), port.find("T", "2")));
        assert_eq!(second.pending.as_slice(), &[read("2")]);
    }

    #[test]
    fn control_a_write_to_the_entity_makes_the_next_read_ask_again() {
        let port = port();
        let write = Exchange::new(
            IoRequest::WriteRow { entity_type: "T".to_string(), id: "1".to_string(), data: row("1") },
            IoAnswer::Row(Ok(row("1"))),
        );
        let (_, end) = port.run_step(vec![read_answer("1"), write], || {
            port.find("T", "1").unwrap();
            port.write("T", "1", &row("1")).unwrap();
            port.find("T", "1")
        });
        assert_eq!(end.pending.as_slice(), &[read("1")]);
    }

    #[test]
    fn a_step_that_stops_for_the_host_keeps_none_of_its_reads() {
        let port = port();
        let (_, end) = port.run_step(vec![read_answer("1")], || (port.find("T", "1"), port.find("T", "2")));
        assert_eq!(end.pending.as_slice(), &[read("2")]);
        assert!(end.reads.is_empty(), "{:?}", end.reads.keys().collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod store_ask_tests {
    use super::*;

    fn bound_port() -> IoPort {
        let port = IoPort::new(Arc::new(HostAnswered), None, None);
        port.bind_store("Ticket", "Rows");
        port
    }

    fn take(guard: Option<crate::row_query::RowPredicate>) -> IoRequest {
        IoRequest::WriteRows {
            batch: RowWriteBatch {
                ops: vec![RowWriteOp::Delete { entity_type: "Ticket".into(), id: "t1".into(), guard }],
                on_failure: RowWriteFailureMode::Stop,
            },
        }
    }

    #[test]
    fn a_row_ask_on_a_store_bound_entity_is_a_store_ask_never_a_host_request() {
        let port = bound_port();
        let (found, end) = port.run_step(Vec::new(), || port.find("Ticket", "t1"));
        assert_eq!(found, Err(AWAITING_HOST.to_string()));
        assert!(end.pending.is_empty(), "{:?}", end.pending);
        assert_eq!(end.store_asks.len(), 1);
        assert_eq!(end.store_asks[0].driver, "Rows");
        assert_eq!(end.store_asks[0].request, IoRequest::ReadRow { entity_type: "Ticket".into(), id: "t1".into() });
        assert!(crate::store_driver::verb_for(&end.store_asks[0].request, &end.store_asks[0].key).is_some());
    }

    #[test]
    fn control_an_unbound_entity_still_asks_the_host() {
        let port = bound_port();
        let (_, end) = port.run_step(Vec::new(), || port.find("Note", "n1"));
        assert!(end.store_asks.is_empty());
        assert_eq!(end.pending.as_slice(), &[IoRequest::ReadRow { entity_type: "Note".into(), id: "n1".into() }]);
        let (_, end) = port.run_step(Vec::new(), || port.delete("Note", "n1"));
        assert_eq!(end.pending.as_slice(), &[IoRequest::DeleteRow { entity_type: "Note".into(), id: "n1".into() }]);
    }

    #[test]
    fn a_single_delete_on_a_bound_entity_is_one_conditional_write_op() {
        let port = bound_port();
        let (_, end) = port.run_step(Vec::new(), || port.delete("Ticket", "t1"));
        assert_eq!(end.store_asks.len(), 1);
        assert_eq!(end.store_asks[0].request, take(None));
    }

    #[test]
    fn a_replayed_take_succeeds_once_and_a_missing_row_fails() {
        let port = bound_port();
        let deleted = Exchange::new(take(None), IoAnswer::Wrote(Ok(vec![RowWriteOutcome::Deleted { before: None }])));
        let (done, end) = port.run_step(vec![deleted], || port.delete("Ticket", "t1"));
        assert_eq!(done, Ok(()));
        assert!(end.store_asks.is_empty() && end.pending.is_empty());
        let gone = Exchange::new(take(None), IoAnswer::Wrote(Ok(vec![RowWriteOutcome::NotFound])));
        let (done, _) = port.run_step(vec![gone], || port.delete("Ticket", "t1"));
        assert_eq!(done, Err("Entity not found: Ticket:t1".to_string()));
    }

    #[test]
    fn a_single_update_on_a_bound_entity_is_one_update_op_and_a_missing_row_fails() {
        let port = bound_port();
        let data: Value = serde_json::from_value(serde_json::json!({"used": true})).unwrap();
        let (_, end) = port.run_step(Vec::new(), || port.write("Ticket", "t1", &data));
        let update = IoRequest::WriteRows {
            batch: RowWriteBatch {
                ops: vec![RowWriteOp::Update { entity_type: "Ticket".into(), id: "t1".into(), data: data.clone(), guard: None }],
                on_failure: RowWriteFailureMode::Stop,
            },
        };
        assert_eq!(end.store_asks[0].request, update);
        let gone = Exchange::new(update, IoAnswer::Wrote(Ok(vec![RowWriteOutcome::NotFound])));
        let (done, _) = port.run_step(vec![gone], || port.write("Ticket", "t1", &data));
        assert_eq!(done, Err("Entity not found: Ticket:t1".to_string()));
    }
}
