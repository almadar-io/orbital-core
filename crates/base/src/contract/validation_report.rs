//! What `orb validate --json` and `program/validate { report }` print: the one JSON report of a validated program.
//! `@almadar/core` reads it from the generated mirror of this type.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

/// The report of validating one program.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationJsonOutput {
    pub success: bool,
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<ValidationJsonError>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<ValidationJsonWarning>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<ValidationStats>,
}

/// One error of a validation report.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationJsonError {
    pub code: String,
    pub path: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_guidance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approximate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_install: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// Where a diagnostic sits in the `.lolo` source: 1-based `line`/`column`, `end_column` just past the last character.
/// `approximate`: the span is the enclosing construct's, not the offending token's. A report prints it as the flat
/// `line`, `column`, `endLine`, `endColumn` and `approximate` fields of its error or warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub approximate: bool,
}

/// One warning of a validation report.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationJsonWarning {
    pub code: String,
    pub path: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approximate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

macro_rules! placed {
    ($ty:ident) => {
        impl $ty {
            /// The same finding at `position`.
            pub fn placed(mut self, position: Option<SourcePosition>) -> Self {
                self.line = position.map(|p| p.line);
                self.column = position.map(|p| p.column);
                self.end_line = position.map(|p| p.end_line);
                self.end_column = position.map(|p| p.end_column);
                self.approximate = position.is_some_and(|p| p.approximate);
                self
            }

            /// Where the finding sits; `None` when it carries no position.
            pub fn position(&self) -> Option<SourcePosition> {
                Some(SourcePosition {
                    line: self.line?,
                    column: self.column?,
                    end_line: self.end_line?,
                    end_column: self.end_column?,
                    approximate: self.approximate,
                })
            }
        }
    };
}
placed!(ValidationJsonError);
placed!(ValidationJsonWarning);

/// Counts of what a validated program holds.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationStats {
    pub orbitals: usize,
    pub entities: usize,
    pub traits: usize,
    pub pages: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error() -> ValidationJsonError {
        ValidationJsonError {
            code: "X".to_string(),
            path: "p".to_string(),
            message: "m".to_string(),
            suggestion: None,
            fix_guidance: None,
            expected_shape: None,
            valid_values: None,
            line: None,
            column: None,
            end_line: None,
            end_column: None,
            approximate: false,
            needs_install: None,
            entity: None,
            field: None,
        }
    }

    #[test]
    fn a_placed_error_prints_flat_position_fields_and_reads_back() {
        let at = SourcePosition { line: 3, column: 4, end_line: 3, end_column: 9, approximate: true };
        let json = serde_json::to_value(error().placed(Some(at))).unwrap();
        assert_eq!(json["line"], 3);
        assert_eq!(json["endColumn"], 9);
        assert_eq!(json["approximate"], true);
        assert!(json.get("position").is_none(), "the position is flat, never nested: {json}");
        let back: ValidationJsonError = serde_json::from_value(json).unwrap();
        assert_eq!(back.position(), Some(at));
    }

    #[test]
    fn an_unplaced_error_prints_no_position_and_a_non_approximate_one_omits_the_flag() {
        let json = serde_json::to_value(error()).unwrap();
        for key in ["line", "column", "endLine", "endColumn", "approximate"] {
            assert!(json.get(key).is_none(), "{key} is absent: {json}");
        }
        assert_eq!(serde_json::from_value::<ValidationJsonError>(json).unwrap().position(), None);
        let exact = SourcePosition { line: 1, column: 1, end_line: 1, end_column: 2, approximate: false };
        let json = serde_json::to_value(error().placed(Some(exact))).unwrap();
        assert!(json.get("approximate").is_none(), "control: an exact position prints no flag: {json}");
        assert_eq!(json["line"], 1);
    }

    #[test]
    fn a_partial_position_is_no_position() {
        let mut half = error();
        half.line = Some(2);
        assert_eq!(half.position(), None);
    }
}
