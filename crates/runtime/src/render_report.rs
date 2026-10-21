//! The Layer 2 client render report (`orb verify`): produced by the desktop
//! program's render probe, read by `orb`.

use serde::{Deserialize, Serialize};

/// One Layer 2 check. The probe fills `name` and `offenders`; `orb verify` judges
/// `passed` and `detail` through `std-render-report`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderVerdict {
    pub name: String,
    pub passed: bool,
    pub detail: String,
    #[serde(skip)]
    pub offenders: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_round_trips_as_json_without_its_offenders() {
        let report = RenderReport {
            item: "x".into(),
            verdicts: vec![RenderVerdict {
                name: "a".into(),
                passed: true,
                detail: String::new(),
                offenders: vec![],
            }],
            slots_rendered: 2,
            access_scoped_empty: vec![],
            traits: None,
            stubbed: vec![],
        };
        let back: RenderReport =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(back, report);
        let mut judged = report.clone();
        judged.verdicts[0].offenders = vec!["o".into()];
        assert_eq!(
            serde_json::to_string(&judged).unwrap(),
            serde_json::to_string(&report).unwrap(),
            "offenders never reach verify-report.json"
        );
    }
}
