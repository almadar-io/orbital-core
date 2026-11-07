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

/// Orders two patterns most-specific-first (EXEC-ENDPOINT-3): at the first segment where they disagree in
/// kind, a literal outranks a `:param`; patterns that agree in kind everywhere are `Equal`.
pub fn compare_path_specificity(a: &str, b: &str) -> core::cmp::Ordering {
    let a_parts = a.split('/').filter(|s| !s.is_empty());
    let b_parts = b.split('/').filter(|s| !s.is_empty());
    for (a_part, b_part) in a_parts.zip(b_parts) {
        let (a_param, b_param) = (a_part.starts_with(':'), b_part.starts_with(':'));
        if a_param != b_param {
            return if a_param { core::cmp::Ordering::Greater } else { core::cmp::Ordering::Less };
        }
    }
    core::cmp::Ordering::Equal
}

/// The pattern a concrete path resolves to among `patterns`: the most specific match, the first declared on a tie.
pub fn best_path_match<'a>(path: &str, patterns: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<&'a str> = None;
    for pattern in patterns {
        if !path_matches_pattern(path, pattern) {
            continue;
        }
        if best.map_or(true, |held| compare_path_specificity(pattern, held) == core::cmp::Ordering::Less) {
            best = Some(pattern);
        }
    }
    best
}

/// The `:param` values `path` gives `pattern`, by name (none when it does not match); a query or fragment is ignored.
pub fn route_params(pattern: &str, path: &str) -> Option<crate::value::ValueMap> {
    let route: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let actual: Vec<&str> = path.split(['?', '#']).next().unwrap_or("").trim_matches('/').split('/').collect();
    if route.len() != actual.len() {
        return None;
    }
    let mut params = crate::value::ValueMap::default();
    for (r, a) in route.iter().zip(&actual) {
        match r.strip_prefix(':') {
            Some(name) => {
                params.insert(name.to_string().into(), crate::value::Value::string((*a).to_string()));
            }
            None if r == a => {}
            None => return None,
        }
    }
    Some(params)
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::cmp::Ordering;

    #[test]
    fn route_precedence_literal_beats_param_at_the_first_disagreeing_segment() {
        assert_eq!(compare_path_specificity("/notes/tree", "/notes/:id"), Ordering::Less);
        assert_eq!(compare_path_specificity("/notes/:id", "/notes/tree"), Ordering::Greater);
        assert_eq!(compare_path_specificity("/x/:a/y", "/:b/c/y"), Ordering::Less);
        assert_eq!(compare_path_specificity("/notes/:id", "/orders/:id"), Ordering::Equal);
    }

    #[test]
    fn route_precedence_best_match_ignores_declaration_order_except_on_ties() {
        let declared = ["/notes/:id", "/notes/tree", "/notes"];
        assert_eq!(best_path_match("/notes/tree", declared), Some("/notes/tree"));
        assert_eq!(best_path_match("/notes/n-4", declared), Some("/notes/:id"));
        assert_eq!(best_path_match("/notes", declared), Some("/notes"));
        assert_eq!(best_path_match("/other", declared), None);
        assert_eq!(best_path_match("/a/b", ["/a/:x", "/a/:y"]), Some("/a/:x"));
    }
}
