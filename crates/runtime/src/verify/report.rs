//! Report — fold frames + verdicts into a `verify-report.json` (mirrors the TS
//! `ReportShape`) and decide overall pass/fail (fail-closed on any failed verdict).

use crate::verify::frame::{Frame, FrameStatus};
use crate::verify::observers::Verdict;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub total_checks: usize,
    pub passed: usize,
    pub failed: usize,
    pub frames: usize,
    /// Steps whose hermetic preamble could not reach `from` — a coverage gap (not a failure).
    pub inconclusive_frames: usize,
    /// Render-ui `@…` bindings the kernel passed through for the client to resolve. NOT
    /// failures here — they are the obligation **Layer 2 must resolve and enforce**.
    pub deferred_client_bindings: usize,
}

/// Result of [`Report::classify_persona_failures`] — labels of Fail frames
/// bucketed by whether every failed effect was a declared access-policy
/// denial (`access_denied`, expected under a weaker persona, not a defect) or
/// at least one was something else (`genuine`, a real regression).
#[derive(Debug, Clone, Default, Serialize)]
pub struct PersonaFailureBreakdown {
    pub access_denied: Vec<String>,
    pub genuine: Vec<String>,
}

impl PersonaFailureBreakdown {
    /// No unexplained (non-denial) failures — the persona run for this report is green.
    pub fn ok(&self) -> bool {
        self.genuine.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub item: String,
    pub summary: Summary,
    pub verdicts: Vec<Verdict>,
    pub frames: Vec<Frame>,
    /// `--trait` scope this walk was restricted to (resolved names) — absent on a
    /// full, unscoped walk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<String>>,
}

impl Report {
    pub fn new(item: String, frames: Vec<Frame>, verdicts: Vec<Verdict>) -> Self {
        let passed = verdicts.iter().filter(|v| v.passed).count();
        let failed = verdicts.len() - passed;
        let inconclusive_frames = frames
            .iter()
            .filter(|f| f.status == FrameStatus::Inconclusive)
            .count();
        let deferred_client_bindings = frames.iter().map(|f| f.deferred_bindings.len()).sum();
        let summary = Summary {
            total_checks: verdicts.len(),
            passed,
            failed,
            frames: frames.len(),
            inconclusive_frames,
            deferred_client_bindings,
        };
        Report {
            item,
            summary,
            verdicts,
            frames,
            traits: None,
        }
    }

    /// Fail-closed: green only when no verdict failed.
    pub fn ok(&self) -> bool {
        self.summary.failed == 0
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Bucket this report's Fail frames: which failed ONLY because a declared
    /// `@create`/`@update`/`@delete` policy denied the row (task #129's "cheap
    /// tier" — an access denial reported separately, never counted alongside a
    /// genuine failure) vs a real effect/transition failure. A frame with BOTH
    /// a denial and some other failed effect counts as genuine — the denial
    /// does not mask it.
    pub fn classify_persona_failures(&self) -> PersonaFailureBreakdown {
        let mut access_denied = Vec::new();
        let mut genuine = Vec::new();
        for f in &self.frames {
            if f.status != FrameStatus::Fail {
                continue;
            }
            let failed: Vec<_> = f.effects.iter().filter(|e| e.outcome == "failed").collect();
            let all_denials = !failed.is_empty()
                && failed.iter().all(|e| {
                    e.error
                        .as_deref()
                        .is_some_and(crate::entity_access::is_access_denied_error)
                });
            let label = format!(
                "{}:{}--{}-->{}",
                f.cause.trait_name, f.cause.from, f.cause.event, f.cause.to
            );
            if all_denials {
                access_denied.push(label);
            } else {
                genuine.push(label);
            }
        }
        PersonaFailureBreakdown {
            access_denied,
            genuine,
        }
    }

    /// One-line human summary.
    pub fn headline(&self) -> String {
        let status = if self.ok() { "PASS" } else { "FAIL" };
        let trait_tag = self
            .traits
            .as_ref()
            .map(|t| format!("[trait {}] ", t.join(", ")))
            .unwrap_or_default();
        format!(
            "{trait_tag}[{status}] {} — {}/{} checks, {} frames ({} inconclusive, {} client-deferred bindings → Layer 2)",
            self.item,
            self.summary.passed,
            self.summary.total_checks,
            self.summary.frames,
            self.summary.inconclusive_frames,
            self.summary.deferred_client_bindings
        )
    }
}
