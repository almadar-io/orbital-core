//! The embedded kernel's wire: what every host (C ABI, JNI, WASM) hands across its boundary as
//! JSON. Field names are the hosts' contract; `tests/fixtures/embed/golden` pins them.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::BTreeMap;
use crate::credential_admin::{ClientAuth, ConnectFailure, CredentialGrant, CredentialStanding, EnvTails, RotateCounts, StoreListing};
use crate::effects::executor::{ServiceCaller, SubstrateFailure};
use crate::resumable::{CallRoutes, CallSite, IoAnswer, IoReply, IoRequest, ModelFailure, ModelFailureKind, PageFacts, PendingRequest, RequestId};
use orbital_core_base::ir::effects::PageFactField;
use orbital_core_base::envelope::{Accepted, DeliverRefusal, Envelope};
use orbital_core_base::os::{OsError, OsOp};
use orbital_core_base::tab::{TabAsk, TabFailure};
use crate::row_query::{RowCommit, RowIdPage, RowPage, RowProjection, RowWriteBatch, RowWriteOutcome};
use orbital_core_base::row_set::RowQuery;
use crate::kernel::{AwaitingTrait, TraitSnapshot};
use orbital_core_base::dispatch_mode::DispatchMode;
use orbital_core_base::theme::ResolvedTheme;
use crate::effects::response::{EventRequest, EventResponse};
use orbital_core_base::secret::Secret;
use orbital_core_base::value::Value;
use std::sync::Arc;
use serde::{Deserialize, Serialize};

/// A server leg and the orbital that owns the trait it is addressed to (the route it posts to).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct RoutedLeg {
    pub orbital: String,
    pub leg: EventRequest,
}

/// The `(trait, event)` that began a dispatch; a leg failure is reported against its event.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct SeedWire {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
}

/// One dispatch: the local response, the legs to post, the mode, the pre-dispatch snapshot
/// (`RuntimeOptimistic` only), the `(trait, event)` pairs already realized locally and the
/// traits awaiting the legs.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchWire {
    pub orbital: String,
    /// The flight this dispatch holds while it owes the host a leg: its answers fold in flight order
    /// (`foldInOrder`) and the host ends it when the last is folded (`endFlight`). Absent when nothing is owed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flight: Option<u64>,
    pub seed: SeedWire,
    pub response: EventResponse,
    pub server_legs: Vec<RoutedLeg>,
    pub mode: DispatchMode,
    pub snapshot: Option<TraitSnapshot>,
    pub already_delivered: Vec<(String, String)>,
    pub awaiting: Vec<AwaitingTrait>,
    /// The plain leg a stateful host is posted when the dispatch delegated no effect.
    pub stateful_leg: Option<StatefulLegWire>,
    /// The traits whose pre-dispatch frame is the base a posted answer is folded against (`foldBase`).
    pub fold_seeded: Vec<String>,
}

/// The leg of a server-backed dispatch that delegated nothing, and the traits awaiting its answer.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatefulLegWire {
    pub orbital: String,
    pub leg: EventRequest,
    pub awaiting: Vec<AwaitingTrait>,
}

/// One `mount` leg: the seeds of one owning orbital that need the host, posted together.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountLegWire {
    pub orbital: String,
    pub seeds: Vec<SeedWire>,
    pub leg: EventRequest,
    /// The union of the seeds' already-realized `(trait, event)` pairs.
    pub already_delivered: Vec<(String, String)>,
    pub awaiting: Vec<AwaitingTrait>,
    /// The seeds whose pre-dispatch frame is the fold base (`foldBase`).
    pub fold_seeded: Vec<String>,
}

/// A mount batch: each seed's local dispatch and the legs to post, per host topology.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountWire {
    pub seeds: Vec<DispatchWire>,
    /// For a host that carries the circuit state (stateless): a leg per orbital whose seeds delegated effects.
    pub legs: Vec<MountLegWire>,
    /// For a host that holds it (stateful): the legs of every server-backed seed, delegated effect or not.
    pub stateful_legs: Vec<MountLegWire>,
    /// The seed that failed to dispatch; the batch stopped there and carries no legs.
    pub failed: Option<MountFailure>,
}

/// The seed a mount batch stopped at, and why.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct MountFailure {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    pub message: String,
}

/// A trait the page entered that has left, and the leg that releases it on its host.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Left {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub orbital: String,
    pub leg: EventRequest,
}

/// What entering a page's traits answers: the seeds to mount, and the traits that left.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entered {
    pub seeds: Vec<crate::effects::response::MountSeed>,
    pub left: Vec<Left>,
}

/// A folded server response: each applied `(trait, newState)` and the merged response.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedWire {
    pub applied: Vec<(String, String)>,
    pub response: EventResponse,
    /// The legs the fold's own arms owe the host (a refetch after a browser-stored write), to post and fold in turn.
    pub server_legs: Vec<RoutedLeg>,
    /// Set when the response's frame delta started past the version this client holds: ask the host for
    /// `frames_since(since)` and fold the snapshot (EXEC-DIST-7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resync: Option<crate::kernel::FramesResync>,
}

/// Where an answer folded in flight order stands: folded, or queued behind an older flight.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
#[expect(clippy::large_enum_variant, reason = "mirrors ServerProgressWire's unboxed layout")]
pub enum FoldProgressWire {
    Applied { fold: AppliedWire },
    Queued { ticket: u64 },
}

/// A queued answer the kernel folded once its flight was the oldest open one, under the ticket `foldInOrder` answered with.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
#[expect(clippy::large_enum_variant, reason = "mirrors NextWire's unboxed layout")]
pub enum FoldedNextWire {
    Applied { ticket: u64, fold: AppliedWire },
    Failed { ticket: u64, error: String },
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct FiredTick {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub tick: String,
}

/// The listener cascade of one tick `emit`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct FrameDispatch {
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[serde(flatten)]
    pub dispatch: DispatchWire,
}

/// One host frame of the tick clock.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameWire {
    /// The fired ticks under the orbital that declares them: the one a `runTick` request is addressed to.
    pub fired: BTreeMap<String, Vec<FiredTick>>,
    pub response: EventResponse,
    pub dispatches: Vec<FrameDispatch>,
    /// The `runTick` legs of fired ticks that write browser-stored data, for the host that owns the store.
    pub browser_legs: Vec<RoutedLeg>,
    pub dropped_server_effects: usize,
    pub errors: Vec<String>,
}

/// A trait name more than one orbital declares; never listed under `states`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct AmbiguousTrait {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub orbitals: Vec<String>,
}

/// Every trait state plus the resolved theme and what it derives.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotWire {
    #[cfg_attr(test, ts(type = "JsonObject"))]
    pub states: serde_json::Map<String, serde_json::Value>,
    pub ambiguous_traits: Vec<AmbiguousTrait>,
    pub theme: ResolvedTheme,
    pub canvas_theme: crate::canvas_theme::CanvasTheme,
    pub motion: crate::motion::ResolvedMotion,
}

/// Who a `call-service` runs for.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerWire {
    pub principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The events a `call-service` call site routes its outcome and live messages to.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRoutesWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_message: Option<String>,
}

/// Where a `call-service` is made from: the trait (and orbital) whose transition runs it, its routes,
/// and whether the step is a follow-on of the dispatch.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSiteWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbital: Option<String>,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub follow_on: bool,
    /// The frame the call was made on; absent for the singleton (an id-less dispatch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    pub emit: CallRoutesWire,
}

/// A substrate operator's failure.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubstrateFailureWire {
    pub message: String,
    pub errors: Vec<Value>,
}

/// A host's result for one request: the value, or the message it failed with.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Settled<T, E> {
    Ok(T),
    Error(E),
}

impl<T, E> From<Result<T, E>> for Settled<T, E> {
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Settled::Ok(value),
            Err(error) => Settled::Error(error),
        }
    }
}

impl<T, E> From<Settled<T, E>> for Result<T, E> {
    fn from(settled: Settled<T, E>) -> Self {
        match settled {
            Settled::Ok(value) => Ok(value),
            Settled::Error(error) => Err(error),
        }
    }
}

pub use orbital_core_base::secret::SecretText;

#[cfg(test)]
pub mod ts_export;
#[cfg(test)]
mod serde_index;

mod io_wire;
mod io_convert;
pub use io_wire::{
    CredentialGrantWire, IoAnswerWire, IoReplyWire, IoRequestWire, ModelFailureKindWire,
    ModelFailureWire, PageFactFieldWire, PageFactsWire, PendingRequestWire, ResolveProgressWire,
    NextWire, ServerProgressWire,
};

#[cfg(test)]
mod call_site_tests;

#[cfg(test)]
mod tab_wire_tests;
