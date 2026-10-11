//! The rust-target operators of the installed `@almadar/std` operator table
//! (`crate::language_data`), in the shape the validator and the TS
//! shell's type inference read. Derived once per process; a new operator or a
//! metadata change needs no compiler rebuild.

use std::sync::LazyLock;

use super::registry::{categories, operators};

/// Operator metadata derived from the canonical @almadar/std registry.
#[derive(Debug, Clone)]
pub struct OperatorMeta {
    pub name: &'static str,
    pub category: &'static str,
    pub min_arity: usize,
    pub max_arity: Option<usize>,
    pub return_type: &'static str,
    pub description: &'static str,
    pub has_side_effects: bool,
    /// Input-dependent return-type tag for operators whose actual return type
    /// depends on the argument expressions at the call site, not the static
    /// `return_type` field. `None` for fixed-return-type operators.
    pub return_semantics: Option<&'static str>,
}

/// All operators whose canonical category targets Rust, sorted by name.
pub static CANONICAL_OPERATORS: LazyLock<Vec<OperatorMeta>> = LazyLock::new(|| {
    let mut ops: Vec<OperatorMeta> = operators()
        .iter()
        .filter(|(_, op)| {
            categories()
                .get(&op.category)
                .is_some_and(|c| c.target.iter().any(|t| t == "rust"))
        })
        .map(|(name, op)| OperatorMeta {
            name: name.as_str(),
            category: op.category.as_str(),
            min_arity: op.min_arity as usize,
            max_arity: op.max_arity.map(|n| n as usize),
            return_type: op.return_type.as_str(),
            description: op.description.as_str(),
            has_side_effects: op.has_side_effects,
            return_semantics: op.return_semantics.as_deref(),
        })
        .collect();
    ops.sort_by(|a, b| a.name.cmp(b.name));
    ops
});

/// Check if an operator should be implemented in Rust
pub fn is_canonical_operator(name: &str) -> bool {
    get_canonical_operator(name).is_some()
}

/// Get metadata for a canonical operator
pub fn get_canonical_operator(name: &str) -> Option<&'static OperatorMeta> {
    CANONICAL_OPERATORS.iter().find(|op| op.name == name)
}

/// Get all operators that are missing implementations
pub fn get_missing_operators(implemented: &[&str]) -> Vec<&'static str> {
    CANONICAL_OPERATORS
        .iter()
        .map(|op| op.name)
        .filter(|name| !implemented.contains(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rust_target_categories_are_listed() {
        let rust_cats: std::collections::HashSet<&str> = categories()
            .iter()
            .filter(|(_, c)| c.target.iter().any(|t| t == "rust"))
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(!CANONICAL_OPERATORS.is_empty());
        assert!(CANONICAL_OPERATORS
            .iter()
            .all(|op| rust_cats.contains(op.category)));
        let listed = operators()
            .values()
            .filter(|op| rust_cats.contains(op.category.as_str()))
            .count();
        assert_eq!(CANONICAL_OPERATORS.len(), listed);
    }

    #[test]
    fn test_core_categories_present() {
        let categories: std::collections::HashSet<_> =
            CANONICAL_OPERATORS.iter().map(|op| op.category).collect();
        assert!(categories.contains("arithmetic"));
        assert!(categories.contains("comparison"));
        assert!(categories.contains("logic"));
        assert!(categories.contains("control"));
    }
}
