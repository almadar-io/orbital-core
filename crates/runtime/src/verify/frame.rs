//! Per-step verification frame — the deterministic snapshot the observers read.
//!
//! Mirrors the TS verifier's `Frame` (`packages/almadar-verify/src/observer/types.ts`),
//! trimmed to what the headless kernel circuit can witness.

use serde::Serialize;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrameStatus {
    Pass,
    Fail,
    Inconclusive,
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
    pub errors: Vec<String>,
}
