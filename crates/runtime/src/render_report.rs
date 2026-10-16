//! The Layer 2 client render report (`orb verify`): produced by the desktop
//! program's render probe, read by `orb`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderVerdict {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl RenderVerdict {
    /// Passed when there are no offenders; otherwise the count and the sorted distinct offenders.
    pub fn new(name: &str, offenders: Vec<String>) -> Self {
        let passed = offenders.is_empty();
        let detail = if passed {
            String::new()
        } else {
            let mut uniq: Vec<String> = offenders
                .iter()
                .cloned()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            uniq.sort();
            format!("{} offender(s): {}", offenders.len(), uniq.join(", "))
        };
        RenderVerdict {
            name: name.to_string(),
            passed,
            detail,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderReport {
    pub item: String,
    pub verdicts: Vec<RenderVerdict>,
    pub slots_rendered: usize,
    /// Entity bindings that rendered empty on an entity whose own `@read`
    /// directive can explain it — declared behavior under this viewer, not a
    /// defect. The L1 twin of this bucket is `classify_persona_failures`'s
    /// `access_denied`; without it here, a correct role-gated organism reported
    /// `L2 FAIL entity_data_bound` for obeying its own policy
    /// (`V-UNPERSONIFIED-VERIFY-FALSE-FAILS-ROLE-GATED-READS`). Reported, never
    /// silent — an empty list nobody can ever see is still worth reading.
    pub access_scoped_empty: Vec<String>,
    /// `--trait` scope this probe was restricted to (resolved names) — absent on a
    /// full, unscoped probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<String>>,
    /// Patterns the renderer declares a stub for (rendered as the declared
    /// unsupported answer, owed a real renderer) — reported, not failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stubbed: Vec<String>,
}

impl RenderReport {
    pub fn ok(&self) -> bool {
        self.verdicts.iter().all(|v| v.passed)
    }
    pub fn headline(&self) -> String {
        let status = if self.ok() { "PASS" } else { "FAIL" };
        let passed = self.verdicts.iter().filter(|v| v.passed).count();
        let trait_tag = self
            .traits
            .as_ref()
            .map(|t| format!("[trait {}] ", t.join(", ")))
            .unwrap_or_default();
        let stubs = if self.stubbed.is_empty() { String::new() } else { format!(", {} stubbed", self.stubbed.len()) };
        format!(
            "{trait_tag}[{status}] {} (client render) — {}/{} checks, {} slots{stubs}",
            self.item,
            passed,
            self.verdicts.len(),
            self.slots_rendered
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offenders_are_counted_and_listed_once_sorted() {
        let v = RenderVerdict::new("renderer_coverage", vec!["b".into(), "a".into(), "b".into()]);
        assert!(!v.passed);
        assert_eq!(v.detail, "3 offender(s): a, b");
    }

    #[test]
    fn control_no_offenders_passes_with_no_detail() {
        let v = RenderVerdict::new("renderer_coverage", vec![]);
        assert!(v.passed && v.detail.is_empty());
    }

    #[test]
    fn a_report_round_trips_as_json() {
        let report = RenderReport {
            item: "x".into(),
            verdicts: vec![RenderVerdict::new("a", vec![])],
            slots_rendered: 2,
            access_scoped_empty: vec![],
            traits: None,
            stubbed: vec![],
        };
        let back: RenderReport = serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(back, report);
        assert_eq!(back.headline(), "[PASS] x (client render) — 1/1 checks, 2 slots");
    }
}
