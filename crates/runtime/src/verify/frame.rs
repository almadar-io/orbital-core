//! Per-step verification frame — the deterministic snapshot the observers read.
//!
//! Mirrors the TS verifier's `Frame` (`packages/almadar-verify/src/observer/types.ts`),
//! trimmed to what the headless kernel circuit can witness.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use crate::planner::Variant;

/// What this step was trying to exercise (the planned transition).
#[derive(Debug, Clone, Serialize)]
pub struct Cause {
    pub trait_name: String,
    pub from: String,
    pub event: String,
    /// Declared target state from the `.orb` topology (the oracle).
    pub to: String,
    pub variant: Variant,
}

/// Per-frame outcome. `Inconclusive` means the hermetic preamble could not place the
/// machine in `from`, so the step's transition could not be exercised — it is a coverage
/// gap, NOT a transition failure (avoids mislabeling unreachable setups as bugs).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrameStatus {
    Pass,
    Fail,
    Inconclusive,
    /// The step is not driven: the transition cannot be reached in this walk, for the typed [`GapReason`] on
    /// the frame. Counted apart from passes and failures.
    Gap,
}

/// Why a [`FrameStatus::Gap`] step was not driven.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GapReason {
    /// The step's event is the reply route (`emit.success`, `cancelled`, `onMessage`, or a failure no
    /// dispatch delivered) of an `http` effect. In real semantics the reply arrives inside the request's own
    /// dispatch, and the walk has no network to produce one.
    NotReachableWithoutNetwork,
}

/// A transition a dispatch fired after its seed (a cascaded emit or listener).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiredHop {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub event: String,
    pub to: String,
    /// Position of the arm among the state's `from --event-->` arms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<usize>,
}

/// One effect observed in the kernel's response, flattened to serializable facts.
#[derive(Debug, Clone, Serialize)]
pub struct EffectSummary {
    /// `render-ui` | `persist` | `fetch` | `emit` | `set` | `navigate` | `notify` | `log` | …
    pub kind: String,
    /// slot (render-ui) / entity-type (persist/fetch) / event (emit) / path (set).
    pub target: String,
    /// `executed` | `noop` | `delegated` | `failed`.
    pub outcome: String,
    pub error: Option<String>,
    /// The effect failed because an access policy refused it (`ServerEffectCode::Denied`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub denied: bool,
    /// An `http` effect that failed: the declared `emit.failure` event the program routed that failure to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed: Option<String>,
}

/// One driven step.
#[derive(Debug, Clone, Serialize)]
pub struct Frame {
    pub index: usize,
    pub cause: Cause,
    pub state_before: String,
    pub state_after: String,
    pub executed: bool,
    pub payload: serde_json::Value,
    pub effects: Vec<EffectSummary>,
    /// Entity-type → row count after this step (from the entity source).
    pub entity_counts: BTreeMap<String, usize>,
    /// Render-ui `@…` binding literals the kernel passed through unresolved (`@trait`,
    /// `@entity`, `@config`, …). NOT a kernel failure — render-ui resolution is client-only
    /// (the kernel marks it noop). These are an explicit obligation that **Layer 2 must
    /// resolve and enforce**; recorded here so they are visible and counted, never ignored.
    pub deferred_bindings: Vec<String>,
    pub status: FrameStatus,
    /// Set exactly when `status` is [`FrameStatus::Gap`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<GapReason>,
    /// Transitions the step's dispatch fired after the seed, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fired: Vec<FiredHop>,
    pub errors: Vec<String>,
}
