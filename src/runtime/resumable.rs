//! Resumable dispatch: a dispatch that stops at its I/O points and goes on when the host has answered.
//!
//! The kernel never performs I/O itself. Its reads, writes, service calls and substrate operators go
//! through an [`IoPort`]. A blocking host (the Rust server, `orb`, tests) leaves the port passing
//! straight through to its synchronous handlers. An async host starts a dispatch with
//! [`RuntimeKernel::start_dispatch`], gets [`Progress::Await`] at the first I/O the in-flight step has no
//! answer for, and calls [`RuntimeKernel::resume`] with the answers, in any order and as late as it likes.
//! The step that asked is run again from the state the dispatch had before it, now with the answers in
//! hand, so a request is asked for once however often the step is replayed.

use crate::effects::{ServiceCaller, ServiceHandler, SubstrateFailure, SubstrateHandler};
use crate::evaluator::Value;
use crate::runtime::entity_source::EntitySource;
use crate::runtime::http_request::{self, StreamFraming};
use crate::runtime::http_service::{
    HttpRequest, HttpResponse, SecretSource, Transport, TransportError,
};
use crate::runtime::kernel::{DispatchRun, KernelError};
use crate::runtime::process_result::ProcessEventResult;
use crate::runtime::row_query::{
    guard_unsupported_reason, query_unsupported_reason, QueryCapabilities, QueryRun, RowIdPage,
    RowPage, RowProjection, RowQuery, RowSetRef, RowWriteBatch, RowWriteFailureMode, RowWriteOp,
    RowWriteOutcome, ScanControl, WriteCapabilities,
};
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
/// around these, so a host answers plain storage reads and writes.
#[derive(Debug, Clone, PartialEq)]
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
    Substrate {
        op: String,
        args: Vec<Value>,
    },
    /// One request of the `http` effect.
    Http {
        request: HttpRequest,
        /// The effect's key, so `cancel-call` can abort the request in flight.
        key: Option<String>,
    },
    /// A request of the `http` effect whose reply is a stream: the host dispatches each event to `site`'s
    /// `onMessage` route as it arrives, then answers [`IoAnswer::Http`] with the final reply, body empty.
    HttpStream {
        request: HttpRequest,
        key: Option<String>,
        framing: StreamFraming,
        site: Option<CallSite>,
    },
    /// A credential by name.
    Secret {
        name: String,
    },
    /// Keep a derived secret (an OAuth token) in the host's store.
    SecretSet {
        name: String,
        value: String,
        ttl_ms: u64,
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
    pub emit: CallRoutes,
}

/// How a host settled a `call-service` other than by a result or an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallVerdict {
    /// The call was aborted by `cancel-call`: the cancelled route fires with `{ key }`.
    Cancelled,
    /// The host started the call and runs its outcome later as its own dispatch: nothing fires now.
    Deferred,
}

/// A host's answer to one [`IoRequest`]; the variant must be the one the request calls for.
#[derive(Debug, Clone, PartialEq)]
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
    Secret(Result<Option<String>, String>),
    SecretSet(Result<(), String>),
}

impl IoAnswer {
    fn answers(&self, request: &IoRequest) -> bool {
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
                | (IoRequest::Secret { .. }, IoAnswer::Secret(_))
                | (IoRequest::SecretSet { .. }, IoAnswer::SecretSet(_))
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
}

impl std::fmt::Debug for DispatchContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchContinuation")
            .field("pending", &self.pending)
            .field("answered", &self.journal.len())
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

/// What the replayed step left behind: the request it could not go on without, or a request that
/// disagrees with the recorded one.
pub(crate) struct StepEnd {
    pub(crate) journal: Vec<Exchange>,
    pub(crate) pending: Option<IoRequest>,
    pub(crate) diverged: Option<ResumeError>,
}

struct StepLedger {
    thread: ThreadId,
    journal: Vec<Exchange>,
    cursor: usize,
    pending: Option<IoRequest>,
    diverged: Option<ResumeError>,
}

/// The error a handler returns when the step cannot get an answer yet; the step's result is dropped.
pub(crate) const AWAITING_HOST: &str = "awaiting host I/O";

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
    /// What the host's query push-down honours, per entity type; an entity not listed is never sent a query.
    query_capabilities: RwLock<HashMap<String, QueryCapabilities>>,
    /// What the host's conditional writes honour, per entity type; an entity not listed is never sent `WriteRows`.
    write_capabilities: RwLock<HashMap<String, WriteCapabilities>>,
    /// Fallback notices already logged, one per (kind, entity, reason).
    noticed: Mutex<HashSet<String>>,
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
            query_capabilities: RwLock::new(HashMap::new()),
            write_capabilities: RwLock::new(HashMap::new()),
            noticed: Mutex::new(HashSet::new()),
        }
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
                target: crate::logging::ns::RUNTIME_FETCH,
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
            l.thread == std::thread::current().id() && (l.pending.is_some() || l.diverged.is_some())
        })
    }

    /// Run `step` with `journal` as the answers its I/O is served from. Steps from different threads
    /// take turns; I/O from threads that are not running a step goes straight to the host handlers.
    pub(crate) fn run_step<R>(
        &self,
        journal: Vec<Exchange>,
        step: impl FnOnce() -> R,
    ) -> (R, StepEnd) {
        let gate = self.gate.lock();
        *self.ledger.lock() = Some(StepLedger {
            thread: std::thread::current().id(),
            journal,
            cursor: 0,
            pending: None,
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
                journal: l.journal,
                pending: l.pending,
                diverged: l.diverged,
            },
            None => StepEnd {
                journal: Vec::new(),
                pending: None,
                diverged: None,
            },
        };
        (result, end)
    }

    fn ask(&self, request: impl FnOnce() -> IoRequest) -> Asked {
        let mut slot = self.ledger.lock();
        let Some(ledger) = slot.as_mut() else {
            return Asked::Direct;
        };
        if ledger.thread != std::thread::current().id() {
            return Asked::Direct;
        }
        if ledger.pending.is_some() || ledger.diverged.is_some() {
            return Asked::Withheld;
        }
        let request = request();
        match ledger.journal.get(ledger.cursor) {
            Some(recorded) if recorded.request == request => {
                ledger.cursor += 1;
                Asked::Answer(recorded.answer.clone())
            }
            Some(recorded) => {
                ledger.diverged = Some(ResumeError::Diverged {
                    recorded: Box::new(recorded.request.clone()),
                    got: Box::new(request),
                });
                Asked::Withheld
            }
            None => {
                ledger.pending = Some(request);
                Asked::Withheld
            }
        }
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
        match self.ask(|| IoRequest::ReadRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
        }) {
            Asked::Direct => self.source.find(entity_type, id),
            Asked::Answer(IoAnswer::Row(Ok(Value::Null))) => Ok(None),
            Asked::Answer(IoAnswer::Row(row)) => row.map(|r| Some(Arc::new(r))),
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        match self.ask(|| IoRequest::WriteRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
            data: data.clone(),
        }) {
            Asked::Direct => self.source.write(entity_type, id, data),
            Asked::Answer(IoAnswer::Row(row)) => row,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        match self.ask(|| IoRequest::CreateRow {
            entity_type: entity_type.to_string(),
            data: data.clone(),
        }) {
            Asked::Direct => self.source.create(entity_type, data),
            Asked::Answer(IoAnswer::Row(row)) => row,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        match self.ask(|| IoRequest::DeleteRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
        }) {
            Asked::Direct => self.source.delete(entity_type, id),
            Asked::Answer(IoAnswer::Deleted(done)) => done,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn clear(&self) {
        self.source.clear()
    }

    fn count(&self) -> usize {
        self.source.count()
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String> {
        match self.ask(|| IoRequest::ListRows {
            entity_type: entity_type.to_string(),
        }) {
            Asked::Direct => self.source.list(entity_type),
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
            return self.source.scan(entity_type, visit);
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
            return self.source.query_page(entity_type, query);
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
                    Asked::Direct => return self.source.query_page(entity_type, query),
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
        Some(Ok(run.finish()))
    }

    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowIdPage, String>> {
        if self.direct() {
            return self.source.query_ids(entity_type, query);
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
                    Asked::Direct => return self.source.query_ids(entity_type, query),
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
        Some(Ok(run.finish_ids()))
    }

    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        if self.direct() {
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
            Asked::Answer(IoAnswer::Wrote(outcomes)) => Some(outcomes),
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
                target: crate::logging::ns::RUNTIME_FETCH,
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
    ) -> Result<HttpResponse, TransportError> {
        match self.ask(|| IoRequest::Http {
            request: request.clone(),
            key: key.map(str::to_string),
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
    ) -> Result<HttpResponse, TransportError> {
        match self.ask(|| IoRequest::HttpStream {
            request: request.clone(),
            key: key.map(str::to_string),
            framing,
            site: self.site.lock().clone(),
        }) {
            Asked::Direct => Err(TransportError::Failed(NO_HTTP_HOST.to_string())),
            Asked::Answer(answer) => self.http_answer(answer),
            Asked::Withheld => Err(TransportError::Failed(AWAITING_HOST.to_string())),
        }
    }

    fn http_answer(&self, answer: IoAnswer) -> Result<HttpResponse, TransportError> {
        match answer {
            IoAnswer::Http(out) => out,
            IoAnswer::ServiceCancelled => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(TransportError::Failed(CALL_CANCELLED.to_string()))
            }
            _ => Err(TransportError::Failed(AWAITING_HOST.to_string())),
        }
    }

    pub(crate) fn secret(&self, name: &str) -> Result<Option<String>, String> {
        match self.ask(|| IoRequest::Secret {
            name: name.to_string(),
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::Secret(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn store_secret(&self, name: &str, value: &str, ttl_ms: u64) -> Result<(), String> {
        match self.ask(|| IoRequest::SecretSet {
            name: name.to_string(),
            value: value.to_string(),
            ttl_ms,
        }) {
            Asked::Direct => Err(NO_HTTP_HOST.to_string()),
            Asked::Answer(IoAnswer::SecretSet(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }
}

const NO_HTTP_HOST: &str = "no HTTP host is configured for this kernel";

impl SubstrateHandler for IoPort {
    fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
        match self.ask(|| IoRequest::Substrate {
            op: op.to_string(),
            args: args.to_vec(),
        }) {
            Asked::Direct => match &self.substrate {
                Some(handler) => handler.run(op, args),
                None => Err(SubstrateFailure {
                    message: format!("no substrate host configured for {op}"),
                    errors: Vec::new(),
                }),
            },
            Asked::Answer(IoAnswer::Substrate(out)) => out,
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
            IoRequest::Substrate { op, args } => IoAnswer::Substrate(match self.substrate {
                Some(handler) => handler.run(op, args),
                None => Err(SubstrateFailure {
                    message: format!("no substrate host configured for {op}"),
                    errors: Vec::new(),
                }),
            }),
            IoRequest::Http { .. }
            | IoRequest::HttpStream { .. }
            | IoRequest::Secret { .. }
            | IoRequest::SecretSet { .. } => answer_http(None, request),
        }
    }

    /// [`Self::answer`], with the `http` effect's requests answered by `http`.
    pub fn answer_with_http(&self, http: &HttpHost<'_>, request: &IoRequest) -> IoAnswer {
        match request {
            IoRequest::Http { .. }
            | IoRequest::HttpStream { .. }
            | IoRequest::Secret { .. }
            | IoRequest::SecretSet { .. } => answer_http(Some(http), request),
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
        return (!listed && !http_request::is_public_address(ip))
            .then(|| format!("refused: {ip} is not a public address"));
    }
    use std::net::ToSocketAddrs;
    match (host, port).to_socket_addrs() {
        Ok(mut addrs) => addrs
            .find(|a| !http_request::is_public_address(a.ip()))
            .map(|a| format!("refused: {host} resolves to {}, not a public address", a.ip())),
        Err(e) => Some(format!("{host} did not resolve: {e}")),
    }
}

fn blocking_send(
    host: &HttpHost<'_>,
    request: &HttpRequest,
) -> Result<HttpResponse, TransportError> {
    if let Some(refusal) = address_refusal(&request.url, host.egress) {
        return Err(TransportError::Failed(refusal));
    }
    host.transport.send(request)
}

/// Answer an [`IoRequest::Http`] / `HttpStream` / `Secret` / `SecretSet` with `host`; with no host the request fails.
pub fn answer_http(host: Option<&HttpHost<'_>>, request: &IoRequest) -> IoAnswer {
    match (request, host) {
        (IoRequest::Http { request, .. }, Some(host)) => {
            IoAnswer::Http(blocking_send(host, request))
        }
        (IoRequest::HttpStream { request, framing, site, .. }, Some(host)) => {
            IoAnswer::Http(blocking_send(host, request).map(|mut reply| {
                let text = reply.text().into_owned();
                let mut carry = String::new();
                let mut events = http_request::stream_events(*framing, &text, &mut carry);
                if *framing == StreamFraming::JsonLines && !carry.is_empty() {
                    events.extend(http_request::stream_events(*framing, "\n", &mut carry));
                }
                for event in events {
                    (host.on_event)(site.as_ref(), event);
                }
                reply.body = Vec::new();
                reply
            }))
        }
        (IoRequest::Http { .. } | IoRequest::HttpStream { .. }, None) => {
            IoAnswer::Http(Err(TransportError::Failed(NO_HTTP_HOST.to_string())))
        }
        (IoRequest::Secret { name }, Some(host)) => IoAnswer::Secret(host.secrets.secret(name)),
        (IoRequest::Secret { .. }, None) => IoAnswer::Secret(Err(NO_HTTP_HOST.to_string())),
        (
            IoRequest::SecretSet {
                name,
                value,
                ttl_ms,
            },
            Some(host),
        ) => IoAnswer::SecretSet(host.secrets.store_secret(name, value, *ttl_ms)),
        (IoRequest::SecretSet { .. }, None) => IoAnswer::SecretSet(Err(NO_HTTP_HOST.to_string())),
        _ => IoAnswer::Service(Err("not an HTTP request".to_string())),
    }
}

/// Answer each [`Progress::Await`] on the spot with `answer` until the dispatch is done: the adapter
/// that lets a blocking host run the resumable code path.
pub fn drive_blocking(
    kernel: &crate::runtime::kernel::RuntimeKernel,
    mut progress: Progress<DispatchOutcomes>,
    answer: &dyn Fn(&IoRequest) -> IoAnswer,
) -> Result<DispatchOutcomes, DispatchError> {
    loop {
        match progress {
            Progress::Done(outcomes) => return Ok(outcomes),
            Progress::Await {
                continuation,
                requests,
            } => {
                let replies = requests
                    .iter()
                    .map(|p| IoReply {
                        id: p.id,
                        answer: answer(&p.request),
                    })
                    .collect();
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
    use crate::runtime::http_request::StreamFraming;
    use std::time::Duration;

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
        let (reply, end) = port.run_step(recorded(&request), || port.http_send(&request, None));
        assert_eq!(reply.map(|r| r.status), Ok(200));
        assert!(end.pending.is_none() && end.diverged.is_none());
    }

    #[test]
    fn a_replayed_step_that_asks_a_changed_request_diverges() {
        let port = port();
        let (_, end) = port.run_step(recorded(&get("https://a.example.com/x")), || {
            port.http_send(&get("https://a.example.com/y"), None)
        });
        assert!(
            matches!(end.diverged, Some(ResumeError::Diverged { .. })),
            "{:?}",
            end.diverged
        );
        assert!(end.pending.is_none());
    }

    #[test]
    fn a_changed_key_or_a_stream_in_place_of_a_send_also_diverges() {
        let request = get("https://a.example.com/x");
        let port = port();
        let (_, end) = port.run_step(recorded(&request), || port.http_send(&request, Some("k")));
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
        let (_, end) = port.run_step(recorded(&request), || {
            port.http_stream(&request, None, StreamFraming::Sse)
        });
        assert!(matches!(end.diverged, Some(ResumeError::Diverged { .. })));
    }

    #[test]
    fn a_step_with_no_record_leaves_the_request_pending() {
        let request = get("https://a.example.com/x");
        let port = port();
        let (_, end) = port.run_step(Vec::new(), || port.http_send(&request, None));
        assert!(matches!(end.pending, Some(IoRequest::Http { .. })));
    }

    #[test]
    fn outside_a_step_an_http_ask_fails_for_want_of_a_host() {
        let port = port();
        assert_eq!(
            port.http_send(&get("https://a.example.com/x"), None).map(|r| r.status),
            Err(TransportError::Failed(NO_HTTP_HOST.to_string()))
        );
    }
}
