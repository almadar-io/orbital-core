//! The declared answer a renderer gives for a pattern it does not implement
//! (`docs/Almadar_Behavior_Packages.md` §14, renderer packs): an explicit
//! error naming the pattern, the renderer and its version — never a silent
//! placeholder and never a substitute render. Every client renders this one
//! message in its own idiom.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

pub const CODE_PATTERN_UNSUPPORTED: &str = "ORB_PATTERN_UNSUPPORTED_BY_RENDERER";

/// A pattern the renderer `renderer` at `version` does not implement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedPattern {
    pub pattern: String,
    pub renderer: &'static str,
    pub version: &'static str,
}

impl UnsupportedPattern {
    /// Logs the first occurrence per (renderer, pattern) — a client renders
    /// every frame, so repeats are dropped from the log, never from the screen.
    pub fn new(pattern: &str, renderer: &'static str, version: &'static str) -> Self {
        static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
        let first = SEEN
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .map(|mut seen| seen.insert((renderer.to_string(), pattern.to_string())))
            .unwrap_or(true);
        let unsupported = Self {
            pattern: pattern.to_string(),
            renderer,
            version,
        };
        if first {
            tracing::warn!(code = CODE_PATTERN_UNSUPPORTED, "{}", unsupported.message());
        }
        unsupported
    }

    pub fn message(&self) -> String {
        let pattern = if self.pattern.is_empty() {
            "(no type)"
        } else {
            &self.pattern
        };
        format!(
            "pattern `{pattern}` is not supported by renderer {} {}",
            self.renderer, self.version
        )
    }
}

/// The registry patterns a renderer does not cover, sorted — the coverage
/// report a renderer pack publishes beside its coverage list.
pub fn unsupported_patterns<'a>(
    registry_patterns: impl IntoIterator<Item = &'a str>,
    covers: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut missing: Vec<String> = registry_patterns
        .into_iter()
        .filter(|p| !covers(p))
        .map(str::to_string)
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_names_pattern_renderer_and_version() {
        let u = UnsupportedPattern::new("zz-sprite", "rust-test", "1.2.3");
        assert_eq!(
            u.message(),
            "pattern `zz-sprite` is not supported by renderer rust-test 1.2.3"
        );
    }

    #[test]
    fn edge_a_node_without_a_type_still_says_what_is_missing() {
        let u = UnsupportedPattern::new("", "rust-test", "1.2.3");
        assert!(u.message().contains("(no type)"), "{}", u.message());
    }

    #[test]
    fn report_lists_only_uncovered_patterns_sorted_once() {
        let covered = ["button", "card"];
        let report = unsupported_patterns(["card", "zz-b", "button", "zz-a", "zz-a"], |p| {
            covered.contains(&p)
        });
        assert_eq!(report, vec!["zz-a", "zz-b"]);
    }

    #[test]
    fn control_full_coverage_reports_nothing() {
        assert!(unsupported_patterns(["button"], |_| true).is_empty());
    }
}
