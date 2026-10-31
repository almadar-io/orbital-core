//! A dispatch that waits: progress, replies and the continuations that resume it.

use super::*;

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
    /// The keyed calls this dispatch is parked on without the lane: their completions wait for it to end.
    pub(crate) keyed: Option<Box<crate::kernel::lane::KeyedHold>>,
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
pub(crate) fn accept_into(
    c: &mut DispatchContinuation,
    replies: Vec<IoReply>,
    check: &dyn Fn(&IoRequest, IoAnswer) -> IoAnswer,
) -> Result<(), ResumeError> {
    if let Some(leg) = c.store.as_mut() {
        accept_into(&mut leg.driver, replies, check)?;
    } else {
        let answered = accept_replies(&c.pending, replies, check)?;
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
    /// The session the resolve reads as: its bound identity is the viewer whose `@read` applies.
    pub(crate) session: Option<Arc<str>>,
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
