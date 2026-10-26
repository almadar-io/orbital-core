//! The one dispatch loop. A dispatch is a [`DispatchRun`]: the seeds to deliver, and for the seed in
//! flight its worklist, visited keys, dispatch memory and place in the seed's stages. The loop moves it
//! one unit at a time (one delivery of the worklist, or one seed scope), and a unit commits to the run
//! only once all its I/O is answered. A blocking host runs the units straight through; an async host
//! runs each with the answers recorded so far and gets [`Progress::Await`] at the first one missing.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::envelope::Origin;
use super::*;
use crate::effects::response::MountSeed;
use crate::resumable::{
    accept_into, number_requests, DispatchContinuation, DispatchError, DispatchOutcomes, Exchange,
    IoAnswer, IoReply, IoRequest, PendingRequest, Progress, StoreAsk, StoreLeg,
};
use crate::row_query::RowWriteOutcome;
use orbital_core_base::envelope::{DeliverRefusal, DeliverRefusalKind, Envelope, EnvelopeVerdict};
use orbital_core_base::value::ValueMap;
use orbital_core_eval::util::visit_key::VisitFrame;

/// Why a unit stopped short: a kernel error, or an I/O answer the unit does not have yet (the unit
/// has put the run and the kernel back as they were before it).
pub(crate) enum Fault {
    Kernel(KernelError),
    Suspended,
}

impl From<KernelError> for Fault {
    fn from(e: KernelError) -> Self {
        Fault::Kernel(e)
    }
}

impl Fault {
    pub(crate) fn into_kernel(self) -> KernelError {
        match self {
            Fault::Kernel(e) => e,
            Fault::Suspended => {
                KernelError::Internal("a dispatch suspended outside a resumable run".to_string())
            }
        }
    }
}

struct Cascade {
    queue: VecDeque<CascadeItem>,
    steps: usize,
}

enum Stage {
    Released(Cascade),
    Scopes {
        idx: usize,
        own: Vec<CascadeItem>,
        fanned: Vec<CascadeItem>,
    },
    Main(Cascade),
}

/// A seed that fires a trait's tick instead of delivering an event.
#[derive(Clone)]
struct TickSeed {
    name: String,
    /// The entity id the trait's state is held under.
    state_entity: String,
    /// Run over every row of the trait's entity (the plan's `entity_id` is the placeholder).
    all_rows: bool,
}

struct SeedPlan {
    trait_name: String,
    entity_id: Option<String>,
    event_name: String,
    payload: Value,
    delivery: Option<DeliveryRecord>,
    log: Option<DispatchLog>,
    lifecycle_targets: Vec<String>,
    tick: Option<TickSeed>,
}

struct SeedRun {
    eid: String,
    event_name: String,
    payload: Arc<Value>,
    now: f64,
    seed_scopes: Vec<String>,
    seed_delivery: Arc<Value>,
    visited: DispatchVisits,
    memory: DispatchMemory,
    stage: Stage,
    tick: Option<TickSeed>,
    /// One event's delivery to one trait, open until its step is done.
    span: tracing::Span,
}

pub(crate) struct DispatchRun {
    trace_seeds: Vec<crate::dispatch_trace::TraceSeed>,
    plans: VecDeque<SeedPlan>,
    seed: Option<SeedRun>,
    outcomes: DispatchOutcomes,
    locale: Option<String>,
    /// The viewer the dispatch was started as (see `enter_viewer`); `None` leaves the kernel's `@user` as it is.
    user: Option<Arc<Value>>,
    origin: Origin,
    /// The transition the step in flight committed when it suspended (G-CROSS-259).
    provisional: Option<Box<ProvisionalCommit>>,
    /// Open from the run's start to its end, across every ask it parks on.
    span: tracing::Span,
}

impl DispatchRun {
    pub(crate) fn dispatch_id(&self) -> u64 {
        self.origin.seq
    }

    /// The event of the seed in flight, which names a dispatch parked on its ask.
    fn parked_event(&self) -> String {
        self.seed
            .as_ref()
            .map(|seed| seed.event_name.clone())
            .unwrap_or_default()
    }
}

enum Flow {
    More,
    Done,
}


/// How an inbound envelope was received: judged and refused, or admitted and started as a dispatch.
pub enum InboundStart {
    Declined(EnvelopeVerdict),
    Admitted(Box<Result<Progress<DispatchOutcomes>, DispatchError>>),
}

impl RuntimeKernel {
    /// Run an inbound [`Envelope`] as a fresh dispatch of `envelope.event` with `envelope.payload` at its target,
    /// carrying the envelope's hop and causal id so the emits it makes continue the chain (EXEC-QUEUE-9). The
    /// verdict is `Accepted` once the dispatch has run, `Duplicate` for an id already accepted from the sender,
    /// `Rejected` for `hop-cap`, `unknown-instance`, or `down` when the dispatch itself failed.
    pub fn dispatch_envelope(&self, envelope: Envelope) -> EnvelopeVerdict {
        let _lane = match self.lane.enter(None) {
            Ok(lane) => lane,
            Err(error) => return Self::envelope_down(&envelope, &error),
        };
        let admitted = match self.admit_envelope(&envelope) {
            Ok(admitted) => admitted,
            Err(verdict) => return verdict,
        };
        let run = self.run_of_envelope(envelope.event.clone(), envelope.payload.clone(), admitted);
        match self.run_straight_through(run) {
            Ok(_) => EnvelopeVerdict::Accepted,
            Err(error) => Self::envelope_down(&envelope, &error),
        }
    }

    fn envelope_down(envelope: &Envelope, error: &KernelError) -> EnvelopeVerdict {
        tracing::error!(
            target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
            envelope = %envelope.id,
            error = %error,
            "envelope:dispatch-failed",
        );
        EnvelopeVerdict::Rejected(DeliverRefusal {
            kind: DeliverRefusalKind::Down,
            id: envelope.id.clone(),
        })
    }

    /// [`Self::dispatch_envelope`] as a resumable dispatch: the host answers the dispatch's asks, and `Accepted`
    /// is its `Progress::Done`.
    pub fn start_dispatch_envelope(&self, envelope: Envelope) -> InboundStart {
        let lane = match self.lane.enter(None) {
            Ok(lane) => lane,
            Err(error) => return InboundStart::Admitted(Box::new(Err(error.into()))),
        };
        let admitted = match self.admit_envelope(&envelope) {
            Ok(admitted) => admitted,
            Err(verdict) => return InboundStart::Declined(verdict),
        };
        let run = self.run_of_envelope(envelope.event, envelope.payload, admitted);
        InboundStart::Admitted(Box::new(self.drive_entry(lane, Self::continuation_of(run))))
    }

    fn run_of_envelope(
        &self,
        event: String,
        payload: Value,
        admitted: super::envelope::Admitted,
    ) -> DispatchRun {
        let mut run = self.run_of_each(&admitted.targets, None, &event, payload, None, None);
        run.origin.inbound = Some(admitted.inbound);
        run
    }
}

use super::lane;

mod entry;
mod drive;
mod units;
