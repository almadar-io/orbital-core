//! Route matching shared by the compiler's navigate checks and `expects` derivation.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::sexpr::SExpression;

/// A concrete path matches a pattern when the segment counts agree and every pattern
/// segment is either a `:param` or the same literal.
pub fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    let path_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let pattern_parts: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    if path_parts.len() != pattern_parts.len() {
        return false;
    }
    path_parts
        .iter()
        .zip(pattern_parts.iter())
        .all(|(path_part, pattern_part)| pattern_part.starts_with(':') || path_part == pattern_part)
}

/// The static prefix of a dynamic navigate target: `(str/concat "/inspection/" @entity.id)`
/// gives `/inspection/`. Anything else is runtime-resolved.
pub fn extract_static_path_prefix(expr: &SExpression) -> Option<String> {
    let SExpression::Array(arr) = expr else {
        return None;
    };
    match (arr.first(), arr.get(1)) {
        (Some(SExpression::String(op)), Some(SExpression::String(first))) if op == "str/concat" => {
            Some(first.clone().into_inner())
        }
        _ => None,
    }
}
