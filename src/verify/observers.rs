//! Observers — pure verdicts over the captured frame stream (+ schema for structural
//! invariants). Each is the Rust analogue of a `packages/almadar-verify/src/observer/*`
//! assertion.

use crate::verify::frame::{Frame, FrameStatus};
use crate::TraitDefinition;
use serde::Serialize;

/// One pass/fail verdict with a human detail.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl Verdict {
    fn new(name: &str, offenders: Vec<String>) -> Self {
        let passed = offenders.is_empty();
        let detail = if passed {
            String::new()
        } else {
            format!("{} offender(s): {}", offenders.len(), offenders.join("; "))
        };
        Verdict {
            name: name.to_string(),
            passed,
            detail,
        }
    }

    /// Pass when `offenders` is empty, else fail with an "N offender(s): …" detail.
    /// Public constructor for verdicts produced outside this module (e.g.
    /// `crate::shell`).
    pub fn from_offenders(name: &str, offenders: Vec<String>) -> Self {
        Self::new(name, offenders)
    }

    /// A passing verdict that still carries a human-readable note (e.g. a
    /// "gate skipped" reason).
    pub fn pass_with_detail(name: &str, detail: &str) -> Self {
        Verdict {
            name: name.to_string(),
            passed: true,
            detail: detail.to_string(),
        }
    }

    /// A structural verdict: pass when `offenders` is empty. On either outcome
    /// the detail records that the real compile is CI/SDK-deferred (so a green
    /// structural pass isn't mistaken for a full compile).
    pub fn structural(name: &str, offenders: Vec<String>, deferral: &str) -> Self {
        let passed = offenders.is_empty();
        let detail = if passed {
            deferral.to_string()
        } else {
            format!(
                "{} offender(s): {} ({})",
                offenders.len(),
                offenders.join("; "),
                deferral
            )
        };
        Verdict {
            name: name.to_string(),
            passed,
            detail,
        }
    }
}

/// All frame-based verdicts. (Render-ui binding resolution is NOT checked here — it is
/// client-only at the kernel layer; the deferred bindings are recorded in the report and
/// enforced by Layer 2.)
pub fn run_all(frames: &[Frame]) -> Vec<Verdict> {
    vec![state_transition(frames), effects_fired(frames)]
}

/// Every CONCLUSIVE step reached its declared target. `Inconclusive` frames (preamble
/// could not reach `from`) are a coverage gap surfaced in the summary, not a failure.
fn state_transition(frames: &[Frame]) -> Verdict {
    let offenders = frames
        .iter()
        .filter(|f| f.status == FrameStatus::Fail)
        .map(|f| {
            format!(
                "{}:{}--{}-->{} (got {})",
                f.cause.trait_name, f.cause.from, f.cause.event, f.cause.to, f.state_after
            )
        })
        .collect();
    Verdict::new("state_transition", offenders)
}

/// No declared effect failed during execution.
fn effects_fired(frames: &[Frame]) -> Verdict {
    let mut offenders = Vec::new();
    for f in frames {
        for e in &f.effects {
            if e.outcome == "failed" {
                offenders.push(format!(
                    "{}:{} {}({}) failed: {}",
                    f.cause.trait_name,
                    f.cause.event,
                    e.kind,
                    e.target,
                    e.error.as_deref().unwrap_or("?")
                ));
            }
        }
    }
    Verdict::new("effects_fired", offenders)
}

/// Structural: every WALKED trait has a non-empty state machine (≥1 state, ≥1
/// transition) — `traits` is the same trait-scoped set the walk just drove, so a
/// `--trait`-scoped run reports only on the named trait(s), never on siblings that
/// were never walked.
pub fn ref_trait_invariant(traits: &[&TraitDefinition]) -> Verdict {
    let mut offenders = Vec::new();
    for td in traits {
        let sm = &td.state_machine;
        if sm.states.is_empty() {
            offenders.push(format!("{} has no states", td.name));
        }
        if sm.transitions.is_empty() {
            offenders.push(format!("{} has no transitions", td.name));
        }
    }
    Verdict::new("ref_trait_invariant", offenders)
}
