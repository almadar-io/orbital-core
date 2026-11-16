//! The symbols compiled step code calls, derived as data from the installed operator table: one symbol for each
//! operator with a value entry (`strict` or `site`) and one for each effect. A `form` operator, and a head the table
//! gives no mode and no side effects, has none.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::language::registry::{EmbeddedOperator, OperatorEvaluation};

/// How compiled code enters a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Entry {
    /// Operands are values, evaluated left to right before the call.
    Strict,
    /// Values plus the call site (literal marks and callables).
    Site,
    /// An effect the kernel runs by name on its operand values.
    Effect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    /// The operator or effect head as programs write it (`str/concat`).
    pub name: String,
    /// The link name compiled code calls.
    pub symbol: String,
    pub entry: Entry,
    pub min_arity: u32,
    /// `None` when variadic.
    pub max_arity: Option<u32>,
    /// Operand positions a `site` operator evaluates on demand.
    pub lazy: Vec<u32>,
}

/// The link name of a head: `orb_op_` then each byte, kept when an ASCII lowercase letter or digit and otherwise
/// written `_` and two lowercase hex digits (injective, so two heads never share a link name).
pub fn link_name(name: &str) -> String {
    let mut out = String::from("orb_op_");
    for byte in name.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("_{byte:02x}"));
        }
    }
    out
}

/// The symbol for one table entry; `None` when the head has no value entry and is not an effect.
pub fn symbol_of(name: &str, operator: &EmbeddedOperator) -> Option<Symbol> {
    let (entry, lazy) = match operator.evaluation {
        Some(OperatorEvaluation::Strict) => (Entry::Strict, Vec::new()),
        Some(OperatorEvaluation::Site) => (Entry::Site, operator.lazy_operands.clone()),
        Some(OperatorEvaluation::Form) => return None,
        None if operator.has_side_effects => (Entry::Effect, Vec::new()),
        None => return None,
    };
    Some(Symbol {
        name: name.to_string(),
        symbol: link_name(name),
        entry,
        min_arity: operator.min_arity,
        max_arity: operator.max_arity,
        lazy,
    })
}

/// Every symbol the operator table defines, sorted by name so the table is the same whatever the map order.
pub fn symbol_table<'a>(
    operators: impl IntoIterator<Item = (&'a String, &'a EmbeddedOperator)>,
) -> Vec<Symbol> {
    let mut table: Vec<Symbol> = operators
        .into_iter()
        .filter_map(|(name, operator)| symbol_of(name, operator))
        .collect();
    table.sort_by(|a, b| a.name.cmp(&b.name));
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_value::from_json_value;

    fn operator(extra: serde_json::Value) -> EmbeddedOperator {
        let mut base = serde_json::json!({
            "category": "test",
            "minArity": 1,
            "returnType": "number",
        });
        if let (Some(base), serde_json::Value::Object(extra)) = (base.as_object_mut(), extra) {
            base.extend(extra);
        }
        from_json_value(&base).unwrap()
    }

    #[test]
    fn a_link_name_keeps_lowercase_letters_and_digits_and_escapes_every_other_byte() {
        assert_eq!(link_name("str/concat"), "orb_op_str_2fconcat");
        assert_eq!(link_name("+"), "orb_op__2b");
        assert_eq!(link_name("array/groupBy"), "orb_op_array_2fgroup_42y");
        assert_eq!(link_name("a_b"), "orb_op_a_5fb");
    }

    #[test]
    fn control_two_names_that_differ_only_in_an_escaped_byte_get_different_link_names() {
        let names = [link_name("a/b"), link_name("a_b"), link_name("a_2fb")];
        assert_ne!(names[0], names[1]);
        assert_ne!(names[0], names[2]);
        assert_ne!(names[1], names[2]);
    }

    #[test]
    fn a_strict_a_site_and_an_effect_head_each_get_their_entry() {
        let strict = operator(serde_json::json!({
            "evaluation": "strict", "minArity": 2, "maxArity": 2, "lazyOperands": [1]
        }));
        let site = operator(serde_json::json!({
            "evaluation": "site", "minArity": 2, "lazyOperands": [2]
        }));
        let effect = operator(serde_json::json!({
            "hasSideEffects": true, "minArity": 0, "maxArity": 3
        }));

        let strict = symbol_of("math/add", &strict).unwrap();
        assert_eq!(strict.entry, Entry::Strict);
        assert_eq!((strict.min_arity, strict.max_arity), (2, Some(2)));
        assert!(strict.lazy.is_empty());
        assert_eq!(strict.symbol, "orb_op_math_2fadd");

        let site = symbol_of("array/map", &site).unwrap();
        assert_eq!(site.entry, Entry::Site);
        assert_eq!((site.min_arity, site.max_arity), (2, None));
        assert_eq!(site.lazy, vec![2]);

        let effect = symbol_of("persist", &effect).unwrap();
        assert_eq!(effect.entry, Entry::Effect);
        assert_eq!((effect.min_arity, effect.max_arity), (0, Some(3)));
        assert!(effect.lazy.is_empty());
    }

    #[test]
    fn control_a_form_operator_and_a_pure_head_with_no_mode_get_no_symbol() {
        let form = operator(serde_json::json!({ "evaluation": "form" }));
        let form_with_effects = operator(serde_json::json!({
            "evaluation": "form", "hasSideEffects": true
        }));
        let pure = operator(serde_json::json!({}));
        assert_eq!(symbol_of("if", &form), None);
        assert_eq!(symbol_of("do", &form_with_effects), None);
        assert_eq!(symbol_of("pure", &pure), None);
    }

    #[test]
    fn the_table_is_sorted_by_name_whatever_order_it_is_given() {
        let strict = operator(serde_json::json!({ "evaluation": "strict" }));
        let names = ["b".to_string(), "c".to_string(), "a".to_string()];
        let forward = symbol_table(names.iter().map(|n| (n, &strict)));
        let backward = symbol_table(names.iter().rev().map(|n| (n, &strict)));
        let order: Vec<&str> = forward.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"]);
        assert_eq!(forward, backward);
    }
}
