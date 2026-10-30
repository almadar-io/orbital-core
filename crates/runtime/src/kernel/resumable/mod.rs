//! Resumable dispatch: a dispatch that stops at its I/O points and goes on when the host has answered.
//!
//! The kernel never performs I/O itself. Its reads, writes, service calls and substrate operators go
//! through an [`IoPort`]. A blocking host (the Rust server, `orb`, tests) leaves the port passing
//! straight through to its synchronous handlers. An async host starts a dispatch with
//! [`RuntimeKernel::start_dispatch`], gets [`Progress::Await`] at the first I/O the in-flight step has no
//! answer for, and calls [`RuntimeKernel::resume`] with the answers, in any order and as late as it likes.
//! The step that asked is run again from the state the dispatch had before it, now with the answers in
//! hand, so a request is asked for once however often the step is replayed.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::effects::executor::{ServiceCaller, ServiceHandler, SubstrateFailure, SubstrateHandler};
use orbital_core_base::secret::Secret;
use orbital_core_base::value::{Value, ValueMap};
use crate::credential_admin::{RotateCounts, StoreListing};
use crate::entity_source::{EntitySource, QueryAnswer};
use orbital_core_base::ir::effects::PageFactField;
use orbital_core_base::envelope::{Accepted, DeliverRefusal, DeliverRefusalKind, Envelope};
use crate::secret_source::SecretSource;
use crate::kernel::dispatch_run::DispatchRun;
use crate::kernel::KernelError;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::tab::{TabAsk, TabFailure, TabFailureKind};
use crate::process_result::ProcessEventResult;
use crate::row_query::{apply_write_batch_across, guard_unsupported_reason, query_unsupported_reason, touched_ids, CreateIds, QueryCapabilities, RowCommit, RowCommitWrite, RowTable, QueryRun, RowIdPage, RowPage, RowProjection, RowWriteBatch, RowWriteFailureMode, RowWriteOp, RowWriteOutcome, ScanControl, WriteCapabilities};
use orbital_core_base::row_set::{RowQuery, RowSetRef};
use orbital_core_base::sync::{Mutex, MutexGuard, RwLock};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use orbital_core_base::sync::ThreadKey as ThreadId;

/// What a dispatch produced: one outcome per delivery, in dispatch order.
pub type DispatchOutcomes = Vec<(String, ProcessEventResult)>;

mod asks;
mod continuation;
mod ledger;
mod io_port;
mod io_port_rows;
mod io_port_calls;
mod hosts;
mod answer;
use ledger::*;
use io_port_calls::*;
use answer::*;
pub(crate) use answer::{accept_replies, os_completion_cancelled};
pub use answer::{answer_batch, answer_ports, drive_blocking, drive_concurrent};
pub use asks::{
    completion_event, substrate_completion_event, CallRoutes, CallSite, CallVerdict, IoAnswer,
    IoRequest, ModelFailure, ModelFailureKind, PageFacts, RequestId,
};
pub(crate) use continuation::{accept_into, StoreLeg};
pub use continuation::{
    DispatchContinuation, DispatchError, IoReply, PendingRequest, Progress, ResolveContinuation,
    ResolveProgress, ResumeError,
};
pub use hosts::{BlockingHost, HostAnswered, PortHost};
pub(crate) use io_port::IoPort;
pub(crate) use io_port_calls::DeliverAsk;
pub(crate) use ledger::{
    number_requests, Asks, EnsureIndexes, Exchange, ReadCache, StepEnd, StoreAsk, AWAITING_HOST,
    NO_EMBED_HOST, NO_PAGE_FACTS_HOST, NO_PROMPT_HOST, NO_TAB_HOST,
};

#[cfg(test)]
mod replay_tests;

#[cfg(test)]
mod read_cache_tests;

#[cfg(test)]
mod store_ask_tests;

#[cfg(test)]
mod runtime_row_tests;
