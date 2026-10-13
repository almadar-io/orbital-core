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
use crate::runtime::kernel::{DispatchRun, KernelError};
use crate::runtime::process_result::ProcessEventResult;
use parking_lot::{Mutex, MutexGuard};
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
    Rows(Result<Vec<Value>, String>),
    Deleted(Result<(), String>),
    Service(Result<Value, String>),
    /// The call was aborted by `cancel-call` while it ran.
    ServiceCancelled,
    /// The call was started and its outcome will arrive as a dispatch of its own.
    ServiceDeferred,
    Cancelled(Result<(), String>),
    Substrate(Result<Value, SubstrateFailure>),
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
                | (IoRequest::DeleteRow { .. }, IoAnswer::Deleted(_))
                | (
                    IoRequest::CallService { .. },
                    IoAnswer::Service(_) | IoAnswer::ServiceCancelled | IoAnswer::ServiceDeferred
                )
                | (IoRequest::CancelCall { .. }, IoAnswer::Cancelled(_))
                | (IoRequest::Substrate { .. }, IoAnswer::Substrate(_))
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
    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String> {
        match self.find(entity_type, id)? {
            Some(row) => Ok(row),
            None => Err(format!("Entity not found: {entity_type}:{id}")),
        }
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        match self.ask(|| IoRequest::ReadRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
        }) {
            Asked::Direct => self.source.find(entity_type, id),
            Asked::Answer(IoAnswer::Row(Ok(Value::Null))) => Ok(None),
            Asked::Answer(IoAnswer::Row(row)) => row.map(Some),
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

    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String> {
        match self.ask(|| IoRequest::ListRows {
            entity_type: entity_type.to_string(),
        }) {
            Asked::Direct => self.source.list(entity_type),
            Asked::Answer(IoAnswer::Rows(rows)) => rows,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.source.bind_collection(entity_type, collection)
    }
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
    fn read(&self, _: &str, _: &str) -> Result<Value, String> {
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
    fn list(&self, _: &str) -> Result<Vec<Value>, String> {
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
                    .map(|row| row.unwrap_or(Value::Null)),
            ),
            IoRequest::ListRows { entity_type } => IoAnswer::Rows(self.source.list(entity_type)),
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
                Some(handler) => handler.call(service, method, args, key.as_deref(), caller.as_ref()),
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
        }
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
