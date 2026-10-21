//! S-Expression Evaluator
//!
//! Evaluates S-expressions for guards and computed values.
//!
//! # Overview
//!
//! The evaluator processes S-expressions (nested arrays representing function calls)
//! with support for:
//! - Arithmetic: `+`, `-`, `*`, `/`, `%`
//! - Comparison: `=`, `!=`, `<`, `>`, `<=`, `>=`
//! - Logic: `and`, `or`, `not`, `if`
//! - Arrays: `count`, `first`, `last`, `nth`, `includes`, `map`, `filter`, `find`
//! - Strings: `str/concat`, `upper`, `lower`, `trim`, `substring`, `split`, `join`
//! - Math: `min`, `max`, `clamp`, `abs`, `floor`, `ceil`, `round`
//! - Control flow: `do`, `when`, `let`
//!
//! # Example
//!
//! ```
//! use orbital_core::evaluator::{Evaluator, BindingContext, Value};
//! use orbital_core::schema::SExpression;
//!
//! let evaluator = Evaluator::new();
//! let ctx = BindingContext::new();
//!
//! // Simple arithmetic
//! let expr: SExpression = serde_json::from_str(r#"["+", 1, 2, 3]"#).unwrap();
//! let result = evaluator.evaluate(&expr, &ctx).unwrap();
//! assert_eq!(result, Value::Number(6.0));
//! ```

pub mod context;
pub mod operators;
pub mod trace;

#[cfg(test)]
mod ruling_tests;

pub use crate::evaluator::operators::arg_types::{actual_of, operator_signature};
pub use crate::evaluator::operators::system::{TimeUnit, TIME_UNIT_VOCABULARY};
pub use crate::evaluator::operators::xml::bake_list_shapes;
pub use context::{BindingContext, BindingContextBuilder};
pub use operators::{
    callback_index_position, eval_expr, get_operator, raw_callback_params, EvalResult, Operator,
};

pub use orbital_core_base::value::{Value, ValueMap};

use lru::LruCache;
use orbital_core_base::error::OrbitalError;
use orbital_core_base::schema::sexpr::SExpression;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Default cache size for compiled expressions
const DEFAULT_CACHE_SIZE: usize = 1000;

/// S-Expression evaluator with optional caching
#[derive(Clone)]
pub struct Evaluator {
    /// LRU cache for expression hashes -> evaluation results
    /// Cache is keyed by (expression_hash, context_hash) for deterministic caching
    cache: Option<Arc<Mutex<LruCache<u64, Value>>>>,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl Evaluator {
    /// Create a new evaluator without caching
    pub fn new() -> Self {
        Self { cache: None }
    }

    /// Create a new evaluator with an LRU cache of the specified size
    pub fn with_cache(size: usize) -> Self {
        let cache_size =
            NonZeroUsize::new(size).unwrap_or(NonZeroUsize::new(DEFAULT_CACHE_SIZE).unwrap());
        Self {
            cache: Some(Arc::new(Mutex::new(LruCache::new(cache_size)))),
        }
    }

    /// Create a new evaluator with default cache size
    pub fn with_default_cache() -> Self {
        Self::with_cache(DEFAULT_CACHE_SIZE)
    }

    /// Evaluate an S-expression in the given context
    pub fn evaluate(
        &self,
        expr: &SExpression,
        ctx: &BindingContext,
    ) -> Result<Value, OrbitalError> {
        // For now, we don't use caching for expressions with bindings
        // since the result depends on the context
        // Future: implement context hashing for cacheable expressions
        eval_expr(expr, ctx)
    }

    /// Evaluate a guard expression (returns bool)
    pub fn evaluate_guard(
        &self,
        guard: &SExpression,
        ctx: &BindingContext,
    ) -> Result<bool, OrbitalError> {
        let result = self.evaluate(guard, ctx)?;
        Ok(result.is_truthy())
    }

    /// Evaluate a guard expressed as lowered OIR (`OirExpr`). The single
    /// OirExpr-guard entry point: it bridges to `SExpression` via
    /// [`OirExpr::to_sexpression`](crate::ir::OirExpr::to_sexpression) and runs
    /// the one canonical evaluator — so a consumer holding a lowered guard (the
    /// headless `test --execute` executor, etc.) never re-implements evaluation.
    /// A guard with no S-expression form (an `Effect`) evaluates permissively
    /// (`true`) rather than blocking.
    pub fn evaluate_guard_oir(
        &self,
        guard: &orbital_core_base::ir::expr::OirExpr,
        ctx: &BindingContext,
    ) -> Result<bool, OrbitalError> {
        match guard.to_sexpression() {
            Some(sexpr) => self.evaluate_guard(&sexpr, ctx),
            None => Ok(true),
        }
    }

    /// Evaluate multiple guards with AND semantics
    pub fn evaluate_guards(
        &self,
        guards: &[SExpression],
        ctx: &BindingContext,
    ) -> Result<bool, OrbitalError> {
        for guard in guards {
            if !self.evaluate_guard(guard, ctx)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Clear the evaluation cache
    pub fn clear_cache(&self) {
        if let Some(cache) = &self.cache {
            cache.lock().clear();
        }
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> Option<CacheStats> {
        self.cache.as_ref().map(|cache| {
            let cache = cache.lock();
            CacheStats {
                len: cache.len(),
                cap: cache.cap().get(),
            }
        })
    }
}

/// Cache statistics
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub len: usize,
    pub cap: usize,
}

impl std::fmt::Debug for Evaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaluator")
            .field("cache_enabled", &self.cache.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::value::ValueMap;

    fn eval(json: &str) -> Value {
        let expr: SExpression = serde_json::from_str(json).unwrap();
        let evaluator = Evaluator::new();
        let ctx = BindingContext::new();
        evaluator.evaluate(&expr, &ctx).unwrap()
    }

    fn eval_err(json: &str) -> String {
        let expr: SExpression = serde_json::from_str(json).unwrap();
        Evaluator::new()
            .evaluate(&expr, &BindingContext::new())
            .unwrap_err()
            .to_string()
    }

    /// A payload key that is not a `\w` word (a header such as `x-goog-resource-state`) is addressed by the same
    /// `@payload.<path>` binding as in the JS runtime, in a guard and as an operator argument.
    #[test]
    fn a_hyphenated_payload_key_is_addressable_by_a_binding() {
        let payload: Value =
            serde_json::from_str(r#"{ "headers": { "x-goog-resource-state": "sync" } }"#).unwrap();
        let ctx = crate::evaluator::context::BindingContextBuilder::new()
            .payload(payload)
            .build();
        assert_eq!(
            eval_with_ctx(
                r#"["=", "@payload.headers.x-goog-resource-state", "sync"]"#,
                &ctx
            ),
            Value::Bool(true)
        );
        assert_eq!(
            eval_with_ctx(
                r#"["=", "@payload.headers.x-goog-resource-state", "exists"]"#,
                &ctx
            ),
            Value::Bool(false)
        );
        assert_eq!(
            eval_with_ctx(
                r#"["str/concat", "@payload.headers.x-goog-resource-state", "!"]"#,
                &ctx
            ),
            Value::String("sync!".to_string())
        );
    }

    fn eval_with_ctx(json: &str, ctx: &BindingContext) -> Value {
        let expr: SExpression = serde_json::from_str(json).unwrap();
        let evaluator = Evaluator::new();
        evaluator.evaluate(&expr, ctx).unwrap()
    }

    #[test]
    fn random_int_with_max_below_min_is_an_invalid_argument_not_a_panic() {
        for call in [r#"["randomInt", 5, 1]"#, r#"["math/randomInt", 1, 0]"#, r#"["randomInt", 3, null]"#] {
            let message = eval_err(call);
            assert!(message.contains("math/randomInt: max must not be below min"), "{call}: {message}");
        }
    }

    #[test]
    fn random_int_control_an_equal_or_wider_range_draws_inside_it() {
        assert_eq!(eval(r#"["randomInt", 3, 3]"#), Value::Number(3.0));
        for _ in 0..20 {
            let Value::Number(n) = eval(r#"["randomInt", -2, 2]"#) else { panic!("not a number") };
            assert!((-2.0..=2.0).contains(&n) && n.fract() == 0.0, "{n}");
        }
    }

    // ========================================================================
    // Arithmetic Tests
    // ========================================================================

    #[test]
    fn test_add() {
        assert_eq!(eval(r#"["+", 1, 2, 3]"#), Value::Number(6.0));
        assert_eq!(eval(r#"["+", 10]"#), Value::Number(10.0));
        assert_eq!(eval(r#"["+"]"#), Value::Number(0.0));
    }

    // ========================================================================
    // C-SEXPR-DATA-ARRAY-CONTRACT — pinned. An array whose head is NOT a
    // registered operator is DATA: elements evaluate verbatim (recursively),
    // the array shape survives. A registered head is a CALL. Both paths (this
    // evaluator and @almadar/evaluator's UNKNOWN_OPERATOR sentinel fallback)
    // share this contract; these tests keep it from regressing silently.
    // ========================================================================

    #[test]
    fn data_array_contract_unregistered_head_is_data() {
        let v = eval(r#"["static", "walk"]"#);
        match v {
            Value::Array(items) => {
                assert_eq!(items.len(), 2, "data array keeps its shape");
                assert_eq!(items[0], Value::String("static".to_string()));
                assert_eq!(items[1], Value::String("walk".to_string()));
            }
            other => panic!("unregistered head must yield a data array, got {other:?}"),
        }
    }

    #[test]
    fn data_array_contract_nested_calls_still_evaluate() {
        // Elements of a data array that ARE call forms evaluate in place.
        let v = eval(r#"["frames", ["+", 1, 2]]"#);
        match v {
            Value::Array(items) => {
                assert_eq!(items[0], Value::String("frames".to_string()));
                assert_eq!(items[1], Value::Number(3.0));
            }
            other => panic!("expected data array with evaluated element, got {other:?}"),
        }
    }

    #[test]
    fn test_subtract() {
        assert_eq!(eval(r#"["-", 10, 3]"#), Value::Number(7.0));
        assert_eq!(eval(r#"["-", 10, 3, 2]"#), Value::Number(5.0));
        assert_eq!(eval(r#"["-", 5]"#), Value::Number(-5.0)); // Unary negation
    }

    #[test]
    fn test_multiply() {
        assert_eq!(eval(r#"["*", 2, 3, 4]"#), Value::Number(24.0));
        assert_eq!(eval(r#"["*"]"#), Value::Number(1.0));
    }

    #[test]
    fn test_divide() {
        assert_eq!(eval(r#"["/", 10, 2]"#), Value::Number(5.0));
    }

    #[test]
    fn test_modulo() {
        assert_eq!(eval(r#"["%", 10, 3]"#), Value::Number(1.0));
    }

    // ========================================================================
    // Comparison Tests
    // ========================================================================

    #[test]
    fn test_equality() {
        assert_eq!(eval(r#"["=", 5, 5]"#), Value::Bool(true));
        assert_eq!(eval(r#"["=", 5, 6]"#), Value::Bool(false));
        assert_eq!(eval(r#"["=", "hello", "hello"]"#), Value::Bool(true));
        // Both operands share one type: no loose equality across kinds.
        let err = eval_err(r#"["=", 1, "1"]"#);
        assert_eq!(
            err,
            "Evaluation error: Type mismatch: expected number, got string"
        );
        assert_eq!(eval(r#"["=", 1, null]"#), Value::Bool(false));
    }

    #[test]
    fn a_list_is_equal_to_the_empty_list_only_when_it_is_empty() {
        assert_eq!(eval(r#"["!=", ["list", 1, 2], ["list"]]"#), Value::Bool(true));
        assert_eq!(eval(r#"["=", ["list", 1, 2], ["list"]]"#), Value::Bool(false));
        assert_eq!(eval(r#"["=", ["list"], ["list", "a"]]"#), Value::Bool(false));
        assert_eq!(eval(r#"["=", ["list"], ["list"]]"#), Value::Bool(true));
        assert_eq!(eval(r#"["!=", ["list", 1, 2], ["list", 1, 2]]"#), Value::Bool(false));
    }

    #[test]
    fn control_lists_of_other_lengths_compare_as_before() {
        assert_eq!(eval(r#"["=", ["list", 1, 2], ["list", 3]]"#), Value::Bool(false));
        assert_eq!(eval(r#"["=", ["list", 1, 2], ["list", 1, 2]]"#), Value::Bool(true));
    }

    #[test]
    fn test_inequality() {
        assert_eq!(eval(r#"["!=", 5, 6]"#), Value::Bool(true));
        assert_eq!(eval(r#"["!=", 5, 5]"#), Value::Bool(false));
        assert_eq!(eval(r#"["!=", "", ""]"#), Value::Bool(false));
    }

    #[test]
    fn test_less_than() {
        assert_eq!(eval(r#"["<", 3, 5]"#), Value::Bool(true));
        assert_eq!(eval(r#"["<", 5, 3]"#), Value::Bool(false));
        assert_eq!(eval(r#"["<", 5, 5]"#), Value::Bool(false));
    }

    #[test]
    fn test_greater_than() {
        assert_eq!(eval(r#"[">", 5, 3]"#), Value::Bool(true));
        assert_eq!(eval(r#"[">", 3, 5]"#), Value::Bool(false));
    }

    #[test]
    fn test_less_than_or_equal() {
        assert_eq!(eval(r#"["<=", 3, 5]"#), Value::Bool(true));
        assert_eq!(eval(r#"["<=", 5, 5]"#), Value::Bool(true));
        assert_eq!(eval(r#"["<=", 6, 5]"#), Value::Bool(false));
    }

    #[test]
    fn test_greater_than_or_equal() {
        assert_eq!(eval(r#"[">=", 5, 3]"#), Value::Bool(true));
        assert_eq!(eval(r#"[">=", 5, 5]"#), Value::Bool(true));
        assert_eq!(eval(r#"[">=", 4, 5]"#), Value::Bool(false));
    }

    // ========================================================================
    // Logic Tests
    // ========================================================================

    #[test]
    fn test_and() {
        assert_eq!(eval(r#"["and", true, true]"#), Value::Bool(true));
        assert_eq!(eval(r#"["and", true, false]"#), Value::Bool(false));
        assert_eq!(eval(r#"["and", true, true, true]"#), Value::Bool(true));
        assert_eq!(eval(r#"["and"]"#), Value::Bool(true)); // Empty and is true
    }

    #[test]
    fn test_or() {
        assert_eq!(eval(r#"["or", false, true]"#), Value::Bool(true));
        assert_eq!(eval(r#"["or", false, false]"#), Value::Bool(false));
        assert_eq!(eval(r#"["or"]"#), Value::Bool(false)); // Empty or is false
    }

    // Twin of almadar-evaluator/__tests__/lisp-audit-parity.test.ts (R-OR-AND-RETURN-BOOLEAN).
    #[test]
    fn test_and_or_return_operand_values() {
        assert_eq!(eval(r#"["or", 0, 42]"#), Value::Number(42.0));
        assert_eq!(eval(r#"["and", "y", "x"]"#), Value::String("x".to_string()));
        assert_eq!(eval(r#"["or", 0, null, 7]"#), Value::Number(7.0));
        assert_eq!(eval(r#"["and", 1, 0, 5]"#), Value::Number(0.0));
        assert_eq!(eval(r#"["or", 0, null]"#), Value::Null);
        assert_eq!(eval(r#"["and", 1, 2]"#), Value::Number(2.0));
    }

    #[test]
    fn test_and_or_operands_share_one_type() {
        for expr in [r#"["or", false, 42]"#, r#"["and", true, "x"]"#] {
            let err = eval_err(expr);
            assert!(err.contains("Type mismatch"), "{expr}: {err}");
        }
        assert_eq!(eval(r#"["or", false, true]"#), Value::Bool(true));
    }

    #[test]
    fn test_and_or_short_circuit() {
        assert_eq!(eval(r#"["or", 3, ["no/such-op"]]"#), Value::Number(3.0));
        assert_eq!(eval(r#"["and", 0, ["no/such-op"]]"#), Value::Number(0.0));
    }

    #[test]
    fn test_not() {
        assert_eq!(eval(r#"["not", true]"#), Value::Bool(false));
        assert_eq!(eval(r#"["not", false]"#), Value::Bool(true));
        // `not` takes a boolean: a number is not coerced.
        let err = eval_err(r#"["not", 0]"#);
        assert_eq!(
            err,
            "Evaluation error: Type mismatch: expected boolean, got number"
        );
    }

    #[test]
    fn test_if() {
        assert_eq!(
            eval(r#"["if", true, "yes", "no"]"#),
            Value::String("yes".to_string())
        );
        assert_eq!(
            eval(r#"["if", false, "yes", "no"]"#),
            Value::String("no".to_string())
        );
        assert_eq!(eval(r#"["if", false, "yes"]"#), Value::Null); // No else
    }

    // ========================================================================
    // Array Tests
    // ========================================================================

    #[test]
    fn test_count() {
        assert_eq!(eval(r#"["count", [1, 2, 3]]"#), Value::Number(3.0));
        assert_eq!(
            eval_err(r#"["count", "hello"]"#),
            "Evaluation error: Type mismatch: expected array, got string"
        );
    }

    #[test]
    fn test_first_last() {
        assert_eq!(eval(r#"["first", [1, 2, 3]]"#), Value::Number(1.0));
        assert_eq!(eval(r#"["last", [1, 2, 3]]"#), Value::Number(3.0));
        assert_eq!(eval(r#"["first", []]"#), Value::Null);
    }

    #[test]
    fn test_nth() {
        assert_eq!(eval(r#"["nth", [10, 20, 30], 1]"#), Value::Number(20.0));
        assert_eq!(eval(r#"["nth", [10, 20, 30], 5]"#), Value::Null);
    }

    #[test]
    fn test_includes() {
        assert_eq!(eval(r#"["includes", [1, 2, 3], 2]"#), Value::Bool(true));
        assert_eq!(eval(r#"["includes", [1, 2, 3], 5]"#), Value::Bool(false));
        assert_eq!(
            eval_err(r#"["includes", "hello", "ell"]"#),
            "Evaluation error: Type mismatch: expected array, got string"
        );
    }

    // ========================================================================
    // String Tests
    // ========================================================================

    #[test]
    fn test_concat() {
        assert_eq!(
            eval(r#"["str/concat", "hello", " ", "world"]"#),
            Value::String("hello world".to_string())
        );
    }

    // R-EVAL-CONCAT-PATH-DIVERGENCE conformance pins (Rust side; mirrored in
    // packages/almadar-evaluator/__tests__/SExpressionEvaluator.test.ts).

    #[test]
    fn concat_conformance_str_concat_joins_mixed_args() {
        assert_eq!(
            eval(r#"["str/concat", "x=", "1", " y=", "2"]"#),
            Value::String("x=1 y=2".to_string())
        );
    }

    #[test]
    fn concat_conformance_array_concat_concatenates_arrays() {
        assert_eq!(
            eval(r#"["array/concat", [1, 2], [3, 4]]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
            ])
        );
    }

    #[test]
    fn concat_conformance_bare_concat_errors() {
        // Bare "concat" was removed as an alias — ambiguous between
        // str/concat and array/concat — it must error, not silently succeed.
        let result = eval_expr(
            &serde_json::from_str(r#"["concat", "a", "b"]"#).unwrap(),
            &BindingContext::default(),
        );
        assert!(result.is_err(), "bare concat must not evaluate");
    }

    #[test]
    fn test_upper_lower() {
        assert_eq!(
            eval(r#"["upper", "hello"]"#),
            Value::String("HELLO".to_string())
        );
        assert_eq!(
            eval(r#"["lower", "HELLO"]"#),
            Value::String("hello".to_string())
        );
    }

    #[test]
    fn as_call_key_is_the_identity_on_a_non_empty_string() {
        assert_eq!(eval(r#"["as", "search", "CallKey"]"#), Value::String("search".to_string()));
    }

    #[test]
    fn as_call_key_rejects_an_empty_string_a_non_string_and_an_unknown_target() {
        assert!(eval_err(r#"["as", "", "CallKey"]"#).contains("empty"));
        assert!(eval_err(r#"["as", 7, "CallKey"]"#).contains("string"));
        assert!(eval_err(r#"["as", "k", "Nope"]"#).contains("unknown target"));
        assert!(eval_err(r#"["as", "k"]"#).contains("as"), "control: arity is two");
    }

    #[test]
    fn an_empty_string_is_a_present_value_and_not_null() {
        assert_eq!(eval(r#"["=", "", null]"#), Value::Bool(false));
        assert_eq!(eval(r#"["=", null, ""]"#), Value::Bool(false));
        assert_eq!(eval(r#"["!=", "", null]"#), Value::Bool(true));
        assert_eq!(eval(r#"["=", null, null]"#), Value::Bool(true), "control: null is null");
        assert_eq!(eval(r#"["=", "", ""]"#), Value::Bool(true), "control: an empty string is itself");
        assert_eq!(eval(r#"["array/includes", ["a"], null]"#), Value::Bool(false));
        assert_eq!(eval(r#"["validate/required", ""]"#), Value::Bool(false), "the one empty-or-absent test");
        assert_eq!(eval(r#"["validate/required", null]"#), Value::Bool(false));
    }

    #[test]
    fn the_semantic_validators_read_the_one_pattern_table() {
        let yes = |op: &str, v: &str| eval(&format!(r#"["{op}", "{v}"]"#)) == Value::Bool(true);
        assert!(yes("validate/url", "http://x.y"), "a short host is a URL");
        assert!(yes("validate/url", "https://example.com/a?b=1"));
        assert!(!yes("validate/url", "ftp://example.com"), "control: only http and https");
        assert!(!yes("validate/url", "http://a b.com"), "control: no whitespace");
        assert!(yes("validate/email", "ann@example.com"));
        assert!(!yes("validate/email", "ann@localhost"), "control: the domain needs a dot");
        assert!(!yes("validate/email", "a@b@c.com"), "control: one at sign");
        assert!(yes("validate/phone", "+1 (555) 123-4567"));
        assert!(!yes("validate/phone", "12345"), "control: seven characters at least");
        assert!(yes("validate/uuid", "550E8400-e29b-41d4-a716-446655440000"));
        assert!(!yes("validate/uuid", "550e8400e29b41d4a716446655440000"), "control: dashes are part of it");
        assert_eq!(
            crate::evaluator::operators::system::semantic_string_value_valid("image", "https://picsum.photos/seed/a/400/400").unwrap(),
            true
        );
        assert_eq!(
            crate::evaluator::operators::system::semantic_string_value_valid("image", "Cover Image").unwrap(),
            false,
            "control: the entity-default check reads the same table"
        );
    }

    // json/parse conformance pins (mirrors
    // packages/almadar-evaluator/__tests__/json.test.ts): invalid JSON or
    // non-string input is Null, never an error — guards branch on null.

    #[test]
    fn json_parse_object_array_scalar_nested() {
        let mut obj = ValueMap::new();
        obj.insert("a".to_string(), Value::Number(1.0));
        obj.insert("b".to_string(), Value::String("x".to_string()));
        assert_eq!(
            eval(r#"["json/parse", "{\"a\": 1, \"b\": \"x\"}", "Row"]"#),
            Value::Object(obj)
        );

        assert_eq!(
            eval(r#"["json/parse", "[1, 2, 3]", "Row"]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
            ])
        );

        assert_eq!(eval(r#"["json/parse", "42", "Row"]"#), Value::Number(42.0));
        assert_eq!(
            eval(r#"["json/parse", "\"hello\"", "Row"]"#),
            Value::String("hello".to_string())
        );
        assert_eq!(eval(r#"["json/parse", "true", "Row"]"#), Value::Bool(true));
        assert_eq!(eval(r#"["json/parse", "null", "Row"]"#), Value::Null);

        let tags = Value::Array(vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
        ]);
        let mut row = ValueMap::new();
        row.insert("id".to_string(), Value::Number(1.0));
        row.insert("tags".to_string(), tags);
        let mut rows = ValueMap::new();
        rows.insert("rows".to_string(), Value::Array(vec![Value::Object(row)]));
        assert_eq!(
            eval(r#"["json/parse", "{\"rows\": [{\"id\": 1, \"tags\": [\"a\", \"b\"]}]}", "Row"]"#),
            Value::Object(rows)
        );
    }

    #[test]
    fn json_parse_invalid_text_or_absent_input_is_null_a_non_string_is_a_type_error() {
        assert_eq!(eval(r#"["json/parse", "{not json", "Row"]"#), Value::Null);
        assert_eq!(eval(r#"["json/parse", "", "Row"]"#), Value::Null);
        assert_eq!(eval(r#"["json/parse", null, "Row"]"#), Value::Null);
        assert_eq!(
            eval_err(r#"["json/parse", 42, "Row"]"#),
            "Evaluation error: Type mismatch: expected string, got number"
        );
        assert_eq!(
            eval_err(r#"["json/parse", [1, 2], "Row"]"#),
            "Evaluation error: Type mismatch: expected string, got array"
        );
    }

    fn parse_as(text: &str, shape: serde_json::Value) -> Value {
        let shape = match shape {
            serde_json::Value::String(name) => name,
            declared => declared.to_string(),
        };
        eval(&serde_json::json!(["json/parse", text, shape]).to_string())
    }

    fn row_shape() -> serde_json::Value {
        serde_json::json!({ "kind": "struct", "fields": [
            { "name": "id", "fieldType": { "kind": "number" } },
            { "name": "tags", "fieldType": { "kind": "array", "inner": { "kind": "string" } } },
            { "name": "note", "fieldType": { "kind": "string" }, "required": false }
        ] })
    }

    #[test]
    fn json_parse_value_of_its_shape_is_returned() {
        assert_eq!(parse_as("42", "number".into()), Value::Number(42.0));
        let row = parse_as(r#"{"id": 1, "tags": ["a"]}"#, row_shape());
        assert!(matches!(row, Value::Object(ref m) if m.get("id") == Some(&Value::Number(1.0))), "{row:?}");
    }

    #[test]
    fn json_parse_value_not_of_its_shape_takes_the_failure_path() {
        assert_eq!(parse_as(r#""abc""#, "number".into()), Value::Null);
        assert_eq!(parse_as(r#"{"tags": ["a"]}"#, row_shape()), Value::Null, "missing required field");
        assert_eq!(parse_as(r#"[{"id": 1, "tags": []}]"#, row_shape()), Value::Null, "bare array for a record");
        assert_eq!(parse_as(r#"{"id": 1, "tags": ["a", 2]}"#, row_shape()), Value::Null, "nested element");
        let rows = serde_json::json!({ "kind": "array", "inner": row_shape() });
        assert_eq!(parse_as(r#"[{"id": 1, "tags": []}, {"id": "2", "tags": []}]"#, rows.clone()), Value::Null);
        assert!(matches!(parse_as(r#"[{"id": 1, "tags": []}]"#, rows), Value::Array(ref v) if v.len() == 1));
    }

    #[test]
    fn control_json_parse_shape_without_a_run_time_declaration_is_unchanged() {
        assert_eq!(parse_as(r#""abc""#, "Row".into()), Value::String("abc".to_string()));
        assert_eq!(
            parse_as("[1]", "Row".into()),
            Value::Array(vec![Value::Number(1.0)])
        );
    }

    // G-ORB-420: a shape naming the program's own alias checks against the declared type.
    fn parse_in_program(text: &str, shape: &str) -> Value {
        let aliases = serde_json::from_value(serde_json::json!({
            "RowFile": [
                { "name": "rows", "type": "[Row]", "required": true },
                { "name": "changes", "type": "[object]", "required": true, "properties": [
                    { "name": "id", "type": "string", "required": true },
                    { "name": "row", "type": "Row" }
                ] }
            ],
            "Row": [{ "name": "", "type": "Map<string,RowCell>" }],
            "RowCell": [{ "name": "", "type": "union", "properties": [
                { "name": "string", "type": "string" },
                { "name": "number", "type": "number" },
                { "name": "array", "type": "[RowCell]" }
            ] }]
        }))
        .unwrap();
        let ctx = crate::evaluator::context::BindingContextBuilder::new()
            .declared_types(crate::evaluator::context::DeclaredTypes::of_aliases(aliases))
            .build();
        let expr: SExpression = serde_json::from_value(serde_json::json!(["json/parse", text, shape])).unwrap();
        Evaluator::new().evaluate(&expr, &ctx).unwrap()
    }

    #[test]
    fn json_parse_program_alias_record_missing_a_required_field_fails() {
        assert_eq!(parse_in_program(r#"{"rows": []}"#, "RowFile"), Value::Null);
    }

    #[test]
    fn json_parse_program_alias_record_given_a_bare_array_fails() {
        assert_eq!(parse_in_program(r#"[{"a": "x"}]"#, "RowFile"), Value::Null);
    }

    #[test]
    fn json_parse_program_alias_checks_through_nested_and_recursive_aliases() {
        assert_eq!(parse_in_program(r#"{"rows": [{"a": true}], "changes": []}"#, "RowFile"), Value::Null);
        assert_eq!(
            parse_in_program(r#"{"rows": [], "changes": [{"id": "c", "row": {"a": [1, [false]]}}]}"#, "RowFile"),
            Value::Null
        );
        assert_eq!(parse_in_program(r#"{"a": 1}"#, "Row"), Value::Object(ValueMap::from_iter([("a".to_string(), Value::Number(1.0))])));
    }

    #[test]
    fn json_parse_program_alias_matching_value_passes() {
        let text = r#"{"rows": [{"a": "x", "n": [1, ["y"]]}], "changes": [{"id": "c1", "row": {"a": 1}}, {"id": "c2"}]}"#;
        assert!(matches!(parse_in_program(text, "RowFile"), Value::Object(ref m) if m.contains_key("rows")));
    }

    #[test]
    fn control_json_parse_undeclared_alias_name_stays_unchecked() {
        assert_eq!(parse_in_program("[1]", "Other"), Value::Array(vec![Value::Number(1.0)]));
        assert_eq!(parse_in_program(r#""abc""#, "Other"), Value::String("abc".to_string()));
    }

    #[test]
    fn json_stringify_round_trip() {
        assert_eq!(
            eval(r#"["json/stringify", {"a": 1}]"#),
            Value::String("{\"a\":1}".to_string())
        );
        let s = match eval(r#"["json/stringify", {"id": 7, "flags": [true, false]}]"#) {
            Value::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        let reparsed = eval(&format!(
            r#"["json/parse", {}, "Row"]"#,
            serde_json::to_string(&s).unwrap()
        ));
        assert_eq!(
            reparsed,
            eval(r#"["json/parse", "{\"id\": 7, \"flags\": [true, false]}", "Row"]"#)
        );
    }

    #[test]
    fn test_trim() {
        assert_eq!(
            eval(r#"["trim", "  hello  "]"#),
            Value::String("hello".to_string())
        );
    }

    #[test]
    fn test_split_join() {
        assert_eq!(
            eval(r#"["split", "a,b,c", ","]"#),
            Value::Array(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
                Value::String("c".to_string()),
            ])
        );
        assert_eq!(
            eval(r#"["join", ["a", "b", "c"], "-"]"#),
            Value::String("a-b-c".to_string())
        );
    }

    // ========================================================================
    // Math Tests
    // ========================================================================

    #[test]
    fn test_min_max() {
        assert_eq!(eval(r#"["min", 5, 3, 8, 1]"#), Value::Number(1.0));
        assert_eq!(eval(r#"["max", 5, 3, 8, 1]"#), Value::Number(8.0));
    }

    #[test]
    fn test_clamp() {
        assert_eq!(eval(r#"["clamp", 5, 0, 10]"#), Value::Number(5.0));
        assert_eq!(eval(r#"["clamp", -5, 0, 10]"#), Value::Number(0.0));
        assert_eq!(eval(r#"["clamp", 15, 0, 10]"#), Value::Number(10.0));
    }

    #[test]
    fn test_abs() {
        assert_eq!(eval(r#"["abs", -5]"#), Value::Number(5.0));
        assert_eq!(eval(r#"["abs", 5]"#), Value::Number(5.0));
    }

    #[test]
    fn test_floor_ceil_round() {
        assert_eq!(eval(r#"["floor", 3.7]"#), Value::Number(3.0));
        assert_eq!(eval(r#"["ceil", 3.2]"#), Value::Number(4.0));
        assert_eq!(eval(r#"["round", 3.5]"#), Value::Number(4.0));
        assert_eq!(eval(r#"["round", 3.4]"#), Value::Number(3.0));
    }

    #[test]
    fn round_honours_its_decimals_argument() {
        assert_eq!(eval(r#"["math/round", 3.14159, 3]"#), Value::Number(3.142));
        assert_eq!(eval(r#"["math/round", 3.456, 2]"#), Value::Number(3.46));
        assert_eq!(eval(r#"["math/round", 343.55749, 1]"#), Value::Number(343.6));
    }

    #[test]
    fn control_round_with_zero_or_no_decimals_rounds_to_an_integer() {
        assert_eq!(eval(r#"["math/round", 3.14159, 0]"#), Value::Number(3.0));
        assert_eq!(eval(r#"["math/round", 3.6]"#), Value::Number(4.0));
    }

    // ========================================================================
    // Control Flow Tests
    // ========================================================================

    #[test]
    fn test_do() {
        assert_eq!(eval(r#"["do", 1, 2, 3]"#), Value::Number(3.0));
    }

    #[test]
    fn test_when() {
        assert_eq!(
            eval(r#"["when", true, "yes"]"#),
            Value::String("yes".to_string())
        );
        assert_eq!(eval(r#"["when", false, "yes"]"#), Value::Null);
    }

    #[test]
    fn test_let() {
        assert_eq!(
            eval(r#"["let", [["x", 10]], ["+", "@x", 5]]"#),
            Value::Number(15.0)
        );
        // Nested let
        assert_eq!(
            eval(r#"["let", [["x", 10], ["y", 20]], ["+", "@x", "@y"]]"#),
            Value::Number(30.0)
        );
    }

    // ========================================================================
    // Binding Tests
    // ========================================================================

    #[test]
    fn test_binding_resolution() {
        let mut payload = ValueMap::new();
        let mut data = ValueMap::new();
        data.insert(
            "email".to_string(),
            Value::String("test@example.com".to_string()),
        );
        data.insert("connectionSlots".to_string(), Value::Number(10.0));
        payload.insert("data".to_string(), Value::Object(data));

        let ctx = BindingContextBuilder::new()
            .payload(Value::Object(payload))
            .build();

        assert_eq!(
            eval_with_ctx(r#"["!=", "@payload.data.email", ""]"#, &ctx),
            Value::Bool(true)
        );

        assert_eq!(
            eval_with_ctx(r#"[">=", "@payload.data.connectionSlots", 1]"#, &ctx),
            Value::Bool(true)
        );
    }

    // ========================================================================
    // Complex Guard Tests (from Orbital_Pattern.orb)
    // ========================================================================

    #[test]
    fn test_complex_guard_from_orbital_pattern() {
        // Guard: ["and", ["!=", "@payload.data.email", ""], ["!=", "@payload.data.name", ""]]
        let mut payload = ValueMap::new();
        let mut data = ValueMap::new();
        data.insert(
            "email".to_string(),
            Value::String("test@example.com".to_string()),
        );
        data.insert("name".to_string(), Value::String("Test User".to_string()));
        payload.insert("data".to_string(), Value::Object(data));

        let ctx = BindingContextBuilder::new()
            .payload(Value::Object(payload))
            .build();

        let guard =
            r#"["and", ["!=", "@payload.data.email", ""], ["!=", "@payload.data.name", ""]]"#;
        assert_eq!(eval_with_ctx(guard, &ctx), Value::Bool(true));

        // Test with empty email
        let mut payload2 = ValueMap::new();
        let mut data2 = ValueMap::new();
        data2.insert("email".to_string(), Value::String("".to_string()));
        data2.insert("name".to_string(), Value::String("Test User".to_string()));
        payload2.insert("data".to_string(), Value::Object(data2));

        let ctx2 = BindingContextBuilder::new()
            .payload(Value::Object(payload2))
            .build();

        assert_eq!(eval_with_ctx(guard, &ctx2), Value::Bool(false));
    }

    #[test]
    fn test_connection_slots_guard() {
        // Guard: [">=", "@payload.data.connectionSlots", 1]
        // Guard: ["<=", "@payload.data.connectionSlots", 100]
        let mut payload = ValueMap::new();
        let mut data = ValueMap::new();
        data.insert("connectionSlots".to_string(), Value::Number(50.0));
        payload.insert("data".to_string(), Value::Object(data));

        let ctx = BindingContextBuilder::new()
            .payload(Value::Object(payload))
            .build();

        assert_eq!(
            eval_with_ctx(r#"[">=", "@payload.data.connectionSlots", 1]"#, &ctx),
            Value::Bool(true)
        );
        assert_eq!(
            eval_with_ctx(r#"["<=", "@payload.data.connectionSlots", 100]"#, &ctx),
            Value::Bool(true)
        );
    }

    #[test]
    fn test_dunbar_number_guard() {
        // Guard: ["<", ["count", "Connection", ["and", ...]], 150]
        // This tests the count operator with entity store

        let connections: Vec<Value> = (0..50)
            .map(|i| {
                let mut c = ValueMap::new();
                c.insert(
                    "requesterId".to_string(),
                    Value::String("user-1".to_string()),
                );
                c.insert(
                    "status".to_string(),
                    Value::String(if i < 30 { "accepted" } else { "pending" }.to_string()),
                );
                Value::Object(c)
            })
            .collect();

        let mut user = ValueMap::new();
        user.insert("id".to_string(), Value::String("user-1".to_string()));

        let ctx = BindingContextBuilder::new()
            .user(Value::Object(user))
            .entity_store("Connection", connections)
            .build();

        // Count accepted connections for current user
        let count_expr = r#"["count", "Connection", ["and", ["=", "@entity.requesterId", "@user.id"], ["=", "@entity.status", "accepted"]]]"#;
        let count = eval_with_ctx(count_expr, &ctx);
        assert_eq!(count, Value::Number(30.0));

        // Full Dunbar guard: < 150 connections
        let guard = r#"["<", ["count", "Connection", ["and", ["=", "@entity.requesterId", "@user.id"], ["=", "@entity.status", "accepted"]]], 150]"#;
        assert_eq!(eval_with_ctx(guard, &ctx), Value::Bool(true));
    }

    // ========================================================================
    // Evaluator API Tests
    // ========================================================================

    #[test]
    fn test_evaluate_guard() {
        let evaluator = Evaluator::new();
        let ctx = BindingContext::new();

        let guard: SExpression = serde_json::from_str(r#"["and", true, true]"#).unwrap();
        assert!(evaluator.evaluate_guard(&guard, &ctx).unwrap());

        let guard2: SExpression = serde_json::from_str(r#"["and", true, false]"#).unwrap();
        assert!(!evaluator.evaluate_guard(&guard2, &ctx).unwrap());
    }

    #[test]
    fn test_evaluate_guards() {
        let evaluator = Evaluator::new();
        let ctx = BindingContext::new();

        let guards: Vec<SExpression> = vec![
            serde_json::from_str(r#"["=", 1, 1]"#).unwrap(),
            serde_json::from_str(r#"[">", 5, 3]"#).unwrap(),
        ];

        assert!(evaluator.evaluate_guards(&guards, &ctx).unwrap());

        let guards_fail: Vec<SExpression> = vec![
            serde_json::from_str(r#"["=", 1, 1]"#).unwrap(),
            serde_json::from_str(r#"["<", 5, 3]"#).unwrap(), // This fails
        ];

        assert!(!evaluator.evaluate_guards(&guards_fail, &ctx).unwrap());
    }

    #[test]
    fn test_cache_stats() {
        let evaluator = Evaluator::with_cache(100);
        let stats = evaluator.cache_stats().unwrap();
        assert_eq!(stats.len, 0);
        assert_eq!(stats.cap, 100);
    }

    // ========================================================================
    // Phase 1: Lambda Tests
    // ========================================================================

    #[test]
    fn test_lambda_creation() {
        let result = eval(r#"["fn", "x", ["+", "@x", 1]]"#);
        assert!(result.is_lambda());
    }

    #[test]
    fn test_lambda_multi_param() {
        let result = eval(r#"["fn", ["a", "b"], ["+", "@a", "@b"]]"#);
        assert!(result.is_lambda());
        if let Value::Lambda { params, .. } = result {
            assert_eq!(params, vec!["a", "b"]);
        }
    }

    // ========================================================================
    // Phase 1: Reduce Tests
    // ========================================================================

    #[test]
    fn test_reduce_sum() {
        // Sum array using reduce
        let result =
            eval(r#"["reduce", [1, 2, 3, 4, 5], 0, ["fn", ["acc", "x"], ["+", "@acc", "@x"]]]"#);
        assert_eq!(result, Value::Number(15.0));
    }

    #[test]
    fn test_reduce_product() {
        // Product using reduce
        let result =
            eval(r#"["reduce", [1, 2, 3, 4], 1, ["fn", ["acc", "x"], ["*", "@acc", "@x"]]]"#);
        assert_eq!(result, Value::Number(24.0));
    }

    #[test]
    fn test_reduce_concat_strings() {
        // Concatenate strings
        let result = eval(
            r#"["reduce", ["a", "b", "c"], "", ["fn", ["acc", "x"], ["str/concat", "@acc", "@x"]]]"#,
        );
        assert_eq!(result, Value::String("abc".to_string()));
    }

    #[test]
    fn test_reduce_empty_array() {
        // Reduce on empty array returns initial value
        let result = eval(r#"["reduce", [], 42, ["fn", ["acc", "x"], ["+", "@acc", "@x"]]]"#);
        assert_eq!(result, Value::Number(42.0));
    }

    #[test]
    fn test_reduce_legacy_raw_body() {
        // Legacy raw-body reducer: `@acc`/`@item` bound per element, JS-path parity
        let result = eval(r#"["reduce", [1, 2, 3, 4, 5], 0, ["+", "@acc", "@item"]]"#);
        assert_eq!(result, Value::Number(15.0));
    }

    // ========================================================================
    // Phase 1: Object Operators Tests
    // ========================================================================

    #[test]
    fn test_object_keys() {
        let result = eval(r#"["object/keys", {"a": 1, "b": 2}]"#);
        if let Value::Array(keys) = result {
            assert_eq!(keys.len(), 2);
            let key_strings: Vec<String> = keys.iter().map(|v| v.to_string_value()).collect();
            assert!(key_strings.contains(&"a".to_string()));
            assert!(key_strings.contains(&"b".to_string()));
        } else {
            panic!("Expected array");
        }
    }

    #[test]
    fn test_object_values() {
        let result = eval(r#"["object/values", {"a": 1, "b": 2}]"#);
        if let Value::Array(values) = result {
            assert_eq!(values.len(), 2);
            let nums: Vec<f64> = values.iter().filter_map(|v| v.to_number()).collect();
            assert!(nums.contains(&1.0));
            assert!(nums.contains(&2.0));
        } else {
            panic!("Expected array");
        }
    }

    #[test]
    fn test_object_get_simple() {
        let result = eval(r#"["object/get", {"name": "John", "age": 30}, "name"]"#);
        assert_eq!(result, Value::String("John".to_string()));
    }

    #[test]
    fn test_object_get_nested() {
        let result = eval(
            r#"["object/get", {"user": {"name": "John", "address": {"city": "NYC"}}}, "user.address.city"]"#,
        );
        assert_eq!(result, Value::String("NYC".to_string()));
    }

    #[test]
    fn test_object_get_missing() {
        let result = eval(r#"["object/get", {"a": 1}, "b"]"#);
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_object_keys_null() {
        let result = eval(r#"["object/keys", null]"#);
        assert_eq!(result, Value::Array(vec![]));
    }

    // ========================================================================
    // Phase 1: Time Operators Tests
    // ========================================================================

    #[test]
    fn test_time_now() {
        let result = eval(r#"["time/now"]"#);
        if let Value::Number(ts) = result {
            // Should be a reasonable Unix timestamp in milliseconds (after year 2020)
            assert!(ts > 1577836800000.0); // Jan 1, 2020
            assert!(ts < 4102444800000.0); // Year 2100
        } else {
            panic!("Expected number");
        }
    }

    #[test]
    fn test_time_now_alias() {
        let result = eval(r#"["now"]"#);
        if let Value::Number(ts) = result {
            assert!(ts > 1577836800000.0);
        } else {
            panic!("Expected number");
        }
    }

    // ========================================================================
    // Phase 2.1: Array Essentials Tests
    // ========================================================================

    #[test]
    fn test_array_slice() {
        assert_eq!(
            eval(r#"["slice", [1, 2, 3, 4, 5], 1, 4]"#),
            Value::Array(vec![
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0)
            ])
        );
        assert_eq!(
            eval(r#"["slice", [1, 2, 3, 4, 5], 2]"#),
            Value::Array(vec![
                Value::Number(3.0),
                Value::Number(4.0),
                Value::Number(5.0)
            ])
        );
        // Negative indices
        assert_eq!(
            eval(r#"["slice", [1, 2, 3, 4, 5], -2]"#),
            Value::Array(vec![Value::Number(4.0), Value::Number(5.0)])
        );
    }

    #[test]
    fn test_array_concat() {
        assert_eq!(
            eval(r#"["array/concat", [1, 2], [3, 4]]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0)
            ])
        );
    }

    #[test]
    fn test_array_append() {
        assert_eq!(
            eval(r#"["append", [1, 2], 3]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
    }

    #[test]
    fn test_array_sort() {
        assert_eq!(
            eval(r#"["sort", [3, 1, 4, 1, 5, 9, 2, 6]]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
                Value::Number(5.0),
                Value::Number(6.0),
                Value::Number(9.0)
            ])
        );
    }

    #[test]
    fn test_array_unique() {
        assert_eq!(
            eval(r#"["unique", [1, 2, 2, 3, 3, 3, 4]]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0)
            ])
        );
    }

    #[test]
    fn test_array_index_of() {
        assert_eq!(eval(r#"["indexOf", [10, 20, 30], 20]"#), Value::Number(1.0));
        assert_eq!(
            eval(r#"["indexOf", [10, 20, 30], 99]"#),
            Value::Number(-1.0)
        );
    }

    #[test]
    fn test_array_every() {
        assert_eq!(
            eval(r#"["every", [2, 4, 6, 8], [">", "@item", 0]]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["every", [2, 4, 6, -1], [">", "@item", 0]]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_array_some() {
        assert_eq!(
            eval(r#"["some", [1, 2, 3, 4], [">", "@item", 3]]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["some", [1, 2, 3], [">", "@item", 10]]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_array_sum() {
        assert_eq!(eval(r#"["sum", [1, 2, 3, 4, 5]]"#), Value::Number(15.0));
        assert_eq!(eval(r#"["sum", []]"#), Value::Number(0.0));
    }

    /// Twin of `@almadar/evaluator`'s std/array.ts cosine/nearest semantics —
    /// identical results AND identical error conditions on both paths.
    #[test]
    fn test_array_cosine() {
        assert_eq!(
            eval(r#"["array/cosine", [1, 0], [1, 0]]"#),
            Value::Number(1.0)
        );
        assert_eq!(
            eval(r#"["array/cosine", [1, 0], [0, 1]]"#),
            Value::Number(0.0)
        );
        assert_eq!(
            eval(r#"["array/cosine", [1, 0], [-1, 0]]"#),
            Value::Number(-1.0)
        );
        // Length mismatch, empty vector, and zero magnitude all error.
        for bad in [
            r#"["array/cosine", [1, 0], [1, 0, 0]]"#,
            r#"["array/cosine", [], []]"#,
            r#"["array/cosine", [0, 0], [1, 0]]"#,
        ] {
            let expr: SExpression = serde_json::from_str(bad).unwrap();
            let ctx = BindingContext::new();
            assert!(eval_expr(&expr, &ctx).is_err(), "must error: {bad}");
        }
    }

    #[test]
    fn test_array_nearest() {
        let result = eval(r#"["array/nearest", [1, 0], [[0, 1], [1, 0], [0.5, 0.5]]]"#);
        let Value::Object(obj) = result else {
            panic!("array/nearest must return an object, got {result:?}");
        };
        assert_eq!(obj.get("index"), Some(&Value::Number(1.0)));
        assert_eq!(obj.get("score"), Some(&Value::Number(1.0)));
        // Runner-up is [0.5,0.5] at cos 45° ≈ 0.7071; margin ≈ 0.2929.
        let Some(Value::Number(margin)) = obj.get("margin") else {
            panic!("margin missing");
        };
        assert!(
            (margin - (1.0 - 0.5_f64.sqrt())).abs() < 1e-9,
            "margin {margin}"
        );

        // Single candidate: no runner-up, margin defined as 0.
        let single = eval(r#"["array/nearest", [1, 0], [[1, 0]]]"#);
        let Value::Object(obj) = single else {
            panic!("object expected");
        };
        assert_eq!(obj.get("margin"), Some(&Value::Number(0.0)));

        // Empty candidates errors.
        let expr: SExpression = serde_json::from_str(r#"["array/nearest", [1, 0], []]"#).unwrap();
        let ctx = BindingContext::new();
        assert!(eval_expr(&expr, &ctx).is_err());
    }

    // ========================================================================
    // Phase 2.2: String Operations Tests
    // ========================================================================

    #[test]
    fn test_str_len() {
        assert_eq!(eval(r#"["str/len", "hello"]"#), Value::Number(5.0));
        assert_eq!(eval(r#"["str/len", ""]"#), Value::Number(0.0));
        // Unicode characters
        assert_eq!(eval(r#"["str/len", "café"]"#), Value::Number(4.0));
    }

    #[test]
    fn test_str_includes() {
        assert_eq!(
            eval(r#"["str/includes", "hello world", "world"]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["str/includes", "hello world", "xyz"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_str_starts_with() {
        assert_eq!(
            eval(r#"["startsWith", "hello world", "hello"]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["startsWith", "hello world", "world"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_str_ends_with() {
        assert_eq!(
            eval(r#"["endsWith", "hello world", "world"]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["endsWith", "hello world", "hello"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_str_replace() {
        assert_eq!(
            eval(r#"["replace", "hello world world", "world", "universe"]"#),
            Value::String("hello universe world".to_string())
        );
    }

    #[test]
    fn test_str_replace_all() {
        assert_eq!(
            eval(r#"["replaceAll", "hello world world", "world", "universe"]"#),
            Value::String("hello universe universe".to_string())
        );
    }

    #[test]
    fn test_str_template() {
        assert_eq!(
            eval(
                r#"["template", "Hello {{name}}, you are {{age}} years old", {"name": "John", "age": 30}]"#
            ),
            Value::String("Hello John, you are 30 years old".to_string())
        );
    }

    #[test]
    fn test_str_index_of() {
        assert_eq!(
            eval(r#"["str/indexOf", "hello world", "world"]"#),
            Value::Number(6.0)
        );
        assert_eq!(
            eval(r#"["str/indexOf", "hello world", "xyz"]"#),
            Value::Number(-1.0)
        );
    }

    // ========================================================================
    // Phase 2.3: Object Operations Tests
    // ========================================================================

    #[test]
    fn test_object_entries() {
        let result = eval(r#"["object/entries", {"a": 1, "b": 2}]"#);
        if let Value::Array(entries) = result {
            assert_eq!(entries.len(), 2);
        } else {
            panic!("Expected array");
        }
    }

    #[test]
    fn test_object_set() {
        let result = eval(r#"["object/set", {"a": 1}, "b", 2]"#);
        if let Value::Object(obj) = result {
            assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
        } else {
            panic!("Expected object");
        }
    }

    #[test]
    fn test_object_has() {
        assert_eq!(eval(r#"["has", {"a": 1, "b": 2}, "a"]"#), Value::Bool(true));
        assert_eq!(
            eval(r#"["has", {"a": 1, "b": 2}, "c"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_object_merge() {
        let result = eval(r#"["merge", {"a": 1}, {"b": 2}, {"c": 3}]"#);
        if let Value::Object(obj) = result {
            assert_eq!(obj.len(), 3);
            assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
            assert_eq!(obj.get("c"), Some(&Value::Number(3.0)));
        } else {
            panic!("Expected object");
        }
    }

    // ========================================================================
    // Phase 2.4: Math Operations Tests
    // ========================================================================

    #[test]
    fn test_math_pow() {
        assert_eq!(eval(r#"["pow", 2, 3]"#), Value::Number(8.0));
        assert_eq!(eval(r#"["pow", 10, 2]"#), Value::Number(100.0));
        assert_eq!(eval(r#"["pow", 4, 0.5]"#), Value::Number(2.0));
    }

    #[test]
    fn test_math_sqrt() {
        assert_eq!(eval(r#"["sqrt", 16]"#), Value::Number(4.0));
        assert_eq!(
            eval(r#"["sqrt", 2]"#),
            Value::Number(std::f64::consts::SQRT_2)
        );
    }

    // ========================================================================
    // Phase 2.5: Time Operations Tests
    // ========================================================================

    #[test]
    fn test_time_today() {
        let result = eval(r#"["today"]"#);
        if let Value::Number(ts) = result {
            // Should be at midnight (divisible by 86400000)
            assert_eq!(ts % (24.0 * 60.0 * 60.0 * 1000.0), 0.0);
        } else {
            panic!("Expected number");
        }
    }

    #[test]
    fn test_time_add() {
        // Add 1 day to a known timestamp
        let base = 1704067200000.0; // Jan 1, 2024 00:00:00 UTC
        let expr = format!(r#"["time/add", {}, 1, "day"]"#, base);
        let result = eval(&expr);
        assert_eq!(result, Value::Number(base + 86400000.0));
    }

    #[test]
    fn test_time_subtract() {
        let base = 1704067200000.0;
        let expr = format!(r#"["time/subtract", {}, 1, "hour"]"#, base);
        let result = eval(&expr);
        assert_eq!(result, Value::Number(base - 3600000.0));
    }

    #[test]
    fn test_time_diff() {
        let ts1 = 1704067200000.0; // Jan 1, 2024
        let ts2 = 1704153600000.0; // Jan 2, 2024 (1 day later)
        let expr = format!(r#"["time/diff", {}, {}, "days"]"#, ts2, ts1);
        let result = eval(&expr);
        assert_eq!(result, Value::Number(1.0));
    }

    #[test]
    fn test_time_is_before() {
        assert_eq!(eval(r#"["isBefore", 1000, 2000]"#), Value::Bool(true));
        assert_eq!(eval(r#"["isBefore", 2000, 1000]"#), Value::Bool(false));
    }

    #[test]
    fn test_time_is_after() {
        assert_eq!(eval(r#"["isAfter", 2000, 1000]"#), Value::Bool(true));
        assert_eq!(eval(r#"["isAfter", 1000, 2000]"#), Value::Bool(false));
    }

    #[test]
    fn test_time_format() {
        // Format Unix epoch (0)
        let result = eval(r#"["time/format", 0]"#);
        if let Value::String(s) = result {
            assert!(s.starts_with("1970-01-01"));
        } else {
            panic!("Expected string");
        }
    }

    // C-TIME-FORMAT-TOKEN-ARG-DROPPED: the 2-arg (timestamp, token-string)
    // form used to pass `require_arity!`'s "at least 1 arg" check, then
    // silently drop the format string and fall through to the plain ISO
    // branch — every `(time/format ts "MMM D, YYYY")` in a `.lolo` render-ui
    // evaluated through this canonical Evaluator got the wrong string.
    #[test]
    fn test_time_format_two_arg_token_form() {
        // Epoch 0 = 1970-01-01T00:00:00Z, a Thursday.
        assert_eq!(
            eval(r#"["time/format", 0, "YYYY-MM-DD"]"#),
            Value::String("1970-01-01".to_string())
        );
        assert_eq!(
            eval(r#"["time/format", 0, "ddd, MMM D, YYYY"]"#),
            Value::String("Thu, Jan 1, 1970".to_string())
        );
        // Longest-token-first: "MMMM"/"dddd" must not be read as "MM"+"MM"
        // or "M" repeated, and "MM" must not consume the head of "MMM".
        assert_eq!(
            eval(r#"["time/format", 0, "dddd, MMMM D, YYYY"]"#),
            Value::String("Thursday, January 1, 1970".to_string())
        );
    }

    // ========================================================================
    // Phase 2.6: Validation Operations Tests
    // ========================================================================

    #[test]
    fn test_validate_required() {
        assert_eq!(eval(r#"["required", "hello"]"#), Value::Bool(true));
        assert_eq!(eval(r#"["required", ""]"#), Value::Bool(false));
        assert_eq!(eval(r#"["required", null]"#), Value::Bool(false));
        assert_eq!(eval(r#"["required", 0]"#), Value::Bool(true));
    }

    #[test]
    fn test_validate_string() {
        assert_eq!(eval(r#"["validate/string", "hello"]"#), Value::Bool(true));
        assert_eq!(eval(r#"["validate/string", 123]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_number() {
        assert_eq!(eval(r#"["validate/number", 123]"#), Value::Bool(true));
        assert_eq!(eval(r#"["validate/number", "hello"]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_boolean() {
        assert_eq!(eval(r#"["validate/boolean", true]"#), Value::Bool(true));
        assert_eq!(eval(r#"["validate/boolean", false]"#), Value::Bool(true));
        assert_eq!(eval(r#"["validate/boolean", "true"]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_array() {
        assert_eq!(eval(r#"["validate/array", [1, 2, 3]]"#), Value::Bool(true));
        assert_eq!(
            eval(r#"["validate/array", "not array"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_validate_object() {
        assert_eq!(eval(r#"["validate/object", {"a": 1}]"#), Value::Bool(true));
        assert_eq!(eval(r#"["validate/object", [1, 2]]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_email() {
        assert_eq!(
            eval(r#"["validate/email", "test@example.com"]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["validate/email", "invalid-email"]"#),
            Value::Bool(false)
        );
        assert_eq!(
            eval(r#"["validate/email", "@example.com"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_validate_url() {
        assert_eq!(
            eval(r#"["validate/url", "https://example.com"]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["validate/url", "http://test.org/path"]"#),
            Value::Bool(true)
        );
        assert_eq!(eval(r#"["validate/url", "not-a-url"]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_min_length() {
        assert_eq!(eval(r#"["minLength", "hello", 3]"#), Value::Bool(true));
        assert_eq!(eval(r#"["minLength", "hi", 3]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_max_length() {
        assert_eq!(eval(r#"["maxLength", "hello", 10]"#), Value::Bool(true));
        assert_eq!(
            eval(r#"["maxLength", "hello world", 5]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_validate_pattern() {
        assert_eq!(eval(r#"["pattern", "hello123", "123"]"#), Value::Bool(true));
        assert_eq!(eval(r#"["pattern", "hello", "xyz"]"#), Value::Bool(false));
    }

    // =========================================================================
    // Phase 3.1: Array Advanced
    // =========================================================================

    #[test]
    fn test_array_prepend() {
        assert_eq!(
            eval(r#"["prepend", [2, 3], 1]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
        assert_eq!(
            eval(r#"["array/prepend", [], "first"]"#),
            Value::Array(vec![Value::String("first".to_string())])
        );
    }

    #[test]
    fn test_array_insert() {
        assert_eq!(
            eval(r#"["insert", [1, 3], 1, 2]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
        assert_eq!(
            eval(r#"["array/insert", ["a", "c"], 1, "b"]"#),
            Value::Array(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
                Value::String("c".to_string())
            ])
        );
    }

    #[test]
    fn test_array_remove() {
        assert_eq!(
            eval(r#"["remove", [1, 2, 3], 1]"#),
            Value::Array(vec![Value::Number(1.0), Value::Number(3.0)])
        );
        assert_eq!(
            eval(r#"["array/remove", ["a", "b", "c"], 0]"#),
            Value::Array(vec![
                Value::String("b".to_string()),
                Value::String("c".to_string())
            ])
        );
    }

    #[test]
    fn test_array_reverse() {
        assert_eq!(
            eval(r#"["reverse", [1, 2, 3]]"#),
            Value::Array(vec![
                Value::Number(3.0),
                Value::Number(2.0),
                Value::Number(1.0)
            ])
        );
        assert_eq!(
            eval(r#"["array/reverse", ["a", "b", "c"]]"#),
            Value::Array(vec![
                Value::String("c".to_string()),
                Value::String("b".to_string()),
                Value::String("a".to_string())
            ])
        );
    }

    #[test]
    fn test_array_flatten() {
        // One-level flatten
        assert_eq!(
            eval(r#"["flatten", [[1, 2], [3, 4]]]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0)
            ])
        );
    }

    #[test]
    fn test_array_zip() {
        let result = eval(r#"["zip", [1, 2], ["a", "b"]]"#);
        if let Value::Array(arr) = result {
            assert_eq!(arr.len(), 2);
            if let Value::Array(ref first) = arr[0] {
                assert_eq!(first[0], Value::Number(1.0));
                assert_eq!(first[1], Value::String("a".to_string()));
            }
        }
    }

    #[test]
    fn test_array_find_index() {
        assert_eq!(
            eval(r#"["findIndex", [1, 2, 3, 4], ["fn", ["x"], [">", "@x", 2]]]"#),
            Value::Number(2.0)
        );
        assert_eq!(
            eval(r#"["findIndex", [1, 2], ["fn", ["x"], [">", "@x", 10]]]"#),
            Value::Number(-1.0)
        );
    }

    #[test]
    fn test_array_avg() {
        assert_eq!(eval(r#"["avg", [2, 4, 6]]"#), Value::Number(4.0));
        assert_eq!(
            eval(r#"["average", [10, 20, 30, 40]]"#),
            Value::Number(25.0)
        );
    }

    #[test]
    fn test_array_group_by() {
        let result = eval(
            r#"["groupBy", [{"type": "a", "v": 1}, {"type": "b", "v": 2}, {"type": "a", "v": 3}], ["fn", ["x"], ["object/get", "@x", "type"]]]"#,
        );
        if let Value::Object(obj) = result {
            assert!(obj.contains_key("a"));
            assert!(obj.contains_key("b"));
            if let Some(Value::Array(a_items)) = obj.get("a") {
                assert_eq!(a_items.len(), 2);
            }
        }
    }

    #[test]
    fn test_array_take() {
        assert_eq!(
            eval(r#"["take", [1, 2, 3, 4, 5], 3]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
        assert_eq!(
            eval(r#"["array/take", [1, 2], 5]"#),
            Value::Array(vec![Value::Number(1.0), Value::Number(2.0)])
        );
    }

    #[test]
    fn test_array_drop() {
        assert_eq!(
            eval(r#"["drop", [1, 2, 3, 4, 5], 2]"#),
            Value::Array(vec![
                Value::Number(3.0),
                Value::Number(4.0),
                Value::Number(5.0)
            ])
        );
        assert_eq!(eval(r#"["array/drop", [1, 2], 5]"#), Value::Array(vec![]));
    }

    #[test]
    fn test_array_range() {
        assert_eq!(
            eval(r#"["range", 1, 5]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0)
            ])
        );
        assert_eq!(
            eval(r#"["range", 0, 3]"#),
            Value::Array(vec![
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(2.0)
            ])
        );
    }

    // =========================================================================
    // Phase 3.2: String Advanced
    // =========================================================================

    #[test]
    fn test_str_slice() {
        assert_eq!(
            eval(r#"["str/slice", "hello world", 0, 5]"#),
            Value::String("hello".to_string())
        );
        assert_eq!(
            eval(r#"["str/slice", "hello", 2]"#),
            Value::String("llo".to_string())
        );
    }

    #[test]
    fn test_str_pad_start() {
        assert_eq!(
            eval(r#"["padStart", "5", 3, "0"]"#),
            Value::String("005".to_string())
        );
        assert_eq!(
            eval(r#"["str/padStart", "hello", 10, "-"]"#),
            Value::String("-----hello".to_string())
        );
    }

    #[test]
    fn test_str_pad_end() {
        assert_eq!(
            eval(r#"["padEnd", "5", 3, "0"]"#),
            Value::String("500".to_string())
        );
        assert_eq!(
            eval(r#"["str/padEnd", "hi", 5, "."]"#),
            Value::String("hi...".to_string())
        );
    }

    #[test]
    fn test_str_capitalize() {
        assert_eq!(
            eval(r#"["capitalize", "hello"]"#),
            Value::String("Hello".to_string())
        );
        assert_eq!(
            eval(r#"["str/capitalize", "WORLD"]"#),
            Value::String("World".to_string())
        );
    }

    #[test]
    fn test_str_truncate() {
        assert_eq!(
            eval(r#"["truncate", "hello world", 5]"#),
            Value::String("hello...".to_string())
        );
        assert_eq!(
            eval(r#"["str/truncate", "hi", 10]"#),
            Value::String("hi".to_string())
        );
    }

    // =========================================================================
    // Phase 3.3: Object Advanced
    // =========================================================================

    #[test]
    fn test_object_from_entries() {
        let result = eval(r#"["fromEntries", [["a", 1], ["b", 2]]]"#);
        if let Value::Object(obj) = result {
            assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
        }
    }

    #[test]
    fn test_object_deep_merge() {
        let result = eval(r#"["deepMerge", {"a": {"x": 1}}, {"a": {"y": 2}, "b": 3}]"#);
        if let Value::Object(obj) = result {
            if let Some(Value::Object(a)) = obj.get("a") {
                assert_eq!(a.get("x"), Some(&Value::Number(1.0)));
                assert_eq!(a.get("y"), Some(&Value::Number(2.0)));
            }
            assert_eq!(obj.get("b"), Some(&Value::Number(3.0)));
        }
    }

    #[test]
    fn test_object_pick() {
        let result = eval(r#"["pick", {"a": 1, "b": 2, "c": 3}, ["a", "c"]]"#);
        if let Value::Object(obj) = result {
            assert_eq!(obj.len(), 2);
            assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(obj.get("c"), Some(&Value::Number(3.0)));
            assert_eq!(obj.get("b"), None);
        }
    }

    #[test]
    fn test_object_omit() {
        let result = eval(r#"["omit", {"a": 1, "b": 2, "c": 3}, ["b"]]"#);
        if let Value::Object(obj) = result {
            assert_eq!(obj.len(), 2);
            assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(obj.get("c"), Some(&Value::Number(3.0)));
            assert_eq!(obj.get("b"), None);
        }
    }

    #[test]
    fn test_object_map_values() {
        let result = eval(r#"["mapValues", {"a": 1, "b": 2}, ["fn", ["v"], ["*", "@v", 2]]]"#);
        if let Value::Object(obj) = result {
            assert_eq!(obj.get("a"), Some(&Value::Number(2.0)));
            assert_eq!(obj.get("b"), Some(&Value::Number(4.0)));
        }
    }

    #[test]
    fn test_object_filter() {
        let result = eval(
            r#"["object/filter", {"a": 1, "b": 5, "c": 3}, ["fn", ["k", "v"], [">", "@v", 2]]]"#,
        );
        if let Value::Object(obj) = result {
            assert_eq!(obj.len(), 2);
            assert!(obj.contains_key("b"));
            assert!(obj.contains_key("c"));
        }
    }

    // =========================================================================
    // Phase 3.4: Math Advanced
    // =========================================================================

    #[test]
    fn test_math_sign() {
        assert_eq!(eval(r#"["sign", 5]"#), Value::Number(1.0));
        assert_eq!(eval(r#"["math/sign", -3]"#), Value::Number(-1.0));
        assert_eq!(eval(r#"["sign", 0]"#), Value::Number(0.0));
    }

    #[test]
    fn test_math_lerp() {
        assert_eq!(eval(r#"["lerp", 0, 10, 0.5]"#), Value::Number(5.0));
        assert_eq!(eval(r#"["math/lerp", 0, 100, 0.25]"#), Value::Number(25.0));
    }

    #[test]
    fn test_math_map() {
        // Map 5 from range [0,10] to range [0,100] should give 50
        assert_eq!(
            eval(r#"["math/map", 5, 0, 10, 0, 100]"#),
            Value::Number(50.0)
        );
        // Map 0 from range [0,10] to range [50,100] should give 50
        assert_eq!(
            eval(r#"["math/map", 0, 0, 10, 50, 100]"#),
            Value::Number(50.0)
        );
    }

    // =========================================================================
    // Phase 3.5: Time Advanced
    // =========================================================================

    #[test]
    fn test_time_year() {
        // 2020-06-15 00:00:00 UTC = 1592179200000 ms
        assert_eq!(
            eval(r#"["time/year", 1592179200000]"#),
            Value::Number(2020.0)
        );
    }

    #[test]
    fn test_time_month() {
        // 2020-06-15 00:00:00 UTC = 1592179200000 ms (June = 6)
        assert_eq!(eval(r#"["time/month", 1592179200000]"#), Value::Number(6.0));
    }

    #[test]
    fn test_time_day() {
        // 2020-06-15 00:00:00 UTC = 1592179200000 ms
        assert_eq!(eval(r#"["time/day", 1592179200000]"#), Value::Number(15.0));
    }

    #[test]
    fn test_time_weekday() {
        // 2020-06-15 was a Monday = 1
        assert_eq!(
            eval(r#"["time/weekday", 1592179200000]"#),
            Value::Number(1.0)
        );
    }

    #[test]
    fn test_time_hour() {
        // 2020-06-15 14:30:00 UTC = 1592231400000 ms
        assert_eq!(eval(r#"["time/hour", 1592231400000]"#), Value::Number(14.0));
    }

    #[test]
    fn test_time_minute() {
        // 2020-06-15 14:30:00 UTC = 1592231400000 ms
        assert_eq!(
            eval(r#"["time/minute", 1592231400000]"#),
            Value::Number(30.0)
        );
    }

    #[test]
    fn test_time_second() {
        // 2020-06-15 14:30:45 UTC = 1592231445000 ms
        assert_eq!(
            eval(r#"["time/second", 1592231445000]"#),
            Value::Number(45.0)
        );
    }

    #[test]
    fn test_time_is_between() {
        // Check if time is between two other times
        assert_eq!(
            eval(r#"["time/isBetween", 1592179200000, 1592000000000, 1593000000000]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["time/isBetween", 1590000000000, 1592000000000, 1593000000000]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_time_is_same() {
        // Same day check - both timestamps are 2020-06-15
        assert_eq!(
            eval(r#"["time/isSame", 1592179200000, 1592231400000, "day"]"#),
            Value::Bool(true)
        );
        // Different days
        assert_eq!(
            eval(r#"["time/isSame", 1592179200000, 1592265600000, "day"]"#),
            Value::Bool(false)
        );
    }

    // =========================================================================
    // Phase 3.6: Validation Advanced
    // =========================================================================

    #[test]
    fn test_validate_uuid() {
        assert_eq!(
            eval(r#"["validate/uuid", "550e8400-e29b-41d4-a716-446655440000"]"#),
            Value::Bool(true)
        );
        assert_eq!(eval(r#"["uuid", "not-a-uuid"]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_date() {
        assert_eq!(
            eval(r#"["validate/date", "2024-01-15"]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["validate/date", "2024/01/15"]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["validate/date", "not-a-date"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_validate_length() {
        assert_eq!(
            eval(r#"["validate/length", "hello", 5]"#),
            Value::Bool(true)
        );
        assert_eq!(eval(r#"["validate/length", "hi", 5]"#), Value::Bool(false));
        assert_eq!(
            eval(r#"["validate/length", [1, 2, 3], 3]"#),
            Value::Bool(true)
        );
    }

    #[test]
    fn test_validate_range() {
        assert_eq!(eval(r#"["validate/range", 5, 1, 10]"#), Value::Bool(true));
        assert_eq!(eval(r#"["validate/range", 15, 1, 10]"#), Value::Bool(false));
        assert_eq!(eval(r#"["validate/range", 0, 1, 10]"#), Value::Bool(false));
    }

    #[test]
    fn test_validate_one_of() {
        assert_eq!(
            eval(r#"["oneOf", "a", ["a", "b", "c"]]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["validate/oneOf", "d", ["a", "b", "c"]]"#),
            Value::Bool(false)
        );
        assert_eq!(eval(r#"["oneOf", 2, [1, 2, 3]]"#), Value::Bool(true));
    }

    #[test]
    fn test_validate_equals() {
        assert_eq!(
            eval(r#"["validate/equals", "hello", "hello"]"#),
            Value::Bool(true)
        );
        assert_eq!(eval(r#"["validate/equals", 5, 5]"#), Value::Bool(true));
        assert_eq!(eval(r#"["validate/equals", "a", "b"]"#), Value::Bool(false));
    }

    // =========================================================================
    // Phase 4.1: Array Extras
    // =========================================================================

    #[test]
    fn test_array_shuffle() {
        // Shuffle returns an array of the same length
        let result = eval(r#"["shuffle", [1, 2, 3, 4, 5]]"#);
        if let Value::Array(arr) = result {
            assert_eq!(arr.len(), 5);
        }
    }

    #[test]
    fn test_array_reject() {
        // Reject keeps elements that DON'T match the predicate
        assert_eq!(
            eval(r#"["reject", [1, 2, 3, 4, 5], ["fn", ["x"], [">", "@x", 3]]]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
    }

    #[test]
    fn test_array_partition() {
        let result = eval(r#"["partition", [1, 2, 3, 4, 5], ["fn", ["x"], [">", "@x", 3]]]"#);
        if let Value::Array(arr) = result {
            assert_eq!(arr.len(), 2);
            // First array: matches (> 3)
            if let Value::Array(ref matches) = arr[0] {
                assert_eq!(matches.len(), 2); // 4, 5
            }
            // Second array: non-matches (<= 3)
            if let Value::Array(ref non_matches) = arr[1] {
                assert_eq!(non_matches.len(), 3); // 1, 2, 3
            }
        }
    }

    #[test]
    fn test_array_take_last() {
        assert_eq!(
            eval(r#"["takeLast", [1, 2, 3, 4, 5], 2]"#),
            Value::Array(vec![Value::Number(4.0), Value::Number(5.0)])
        );
        assert_eq!(
            eval(r#"["array/takeLast", [1, 2], 5]"#),
            Value::Array(vec![Value::Number(1.0), Value::Number(2.0)])
        );
    }

    #[test]
    fn test_array_drop_last() {
        assert_eq!(
            eval(r#"["dropLast", [1, 2, 3, 4, 5], 2]"#),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
        assert_eq!(
            eval(r#"["array/dropLast", [1, 2], 5]"#),
            Value::Array(vec![])
        );
    }

    #[test]
    fn test_array_repeat() {
        assert_eq!(
            eval(r#"["array/repeat", "x", 3]"#),
            Value::Array(vec![
                Value::String("x".to_string()),
                Value::String("x".to_string()),
                Value::String("x".to_string())
            ])
        );
    }

    // =========================================================================
    // Phase 4.2: String Extras
    // =========================================================================

    #[test]
    fn test_str_trim_start() {
        assert_eq!(
            eval(r#"["trimStart", "  hello  "]"#),
            Value::String("hello  ".to_string())
        );
    }

    #[test]
    fn test_str_trim_end() {
        assert_eq!(
            eval(r#"["trimEnd", "  hello  "]"#),
            Value::String("  hello".to_string())
        );
    }

    #[test]
    fn test_str_repeat() {
        assert_eq!(
            eval(r#"["str/repeat", "ab", 3]"#),
            Value::String("ababab".to_string())
        );
    }

    #[test]
    fn test_str_reverse() {
        assert_eq!(
            eval(r#"["str/reverse", "hello"]"#),
            Value::String("olleh".to_string())
        );
    }

    #[test]
    fn test_str_title_case() {
        assert_eq!(
            eval(r#"["titleCase", "hello world"]"#),
            Value::String("Hello World".to_string())
        );
    }

    #[test]
    fn test_str_camel_case() {
        assert_eq!(
            eval(r#"["camelCase", "hello-world"]"#),
            Value::String("helloWorld".to_string())
        );
        assert_eq!(
            eval(r#"["str/camelCase", "Hello World"]"#),
            Value::String("helloWorld".to_string())
        );
    }

    #[test]
    fn test_str_kebab_case() {
        assert_eq!(
            eval(r#"["kebabCase", "helloWorld"]"#),
            Value::String("hello-world".to_string())
        );
        assert_eq!(
            eval(r#"["str/kebabCase", "Hello World"]"#),
            Value::String("hello-world".to_string())
        );
    }

    #[test]
    fn test_str_snake_case() {
        assert_eq!(
            eval(r#"["snakeCase", "helloWorld"]"#),
            Value::String("hello_world".to_string())
        );
        assert_eq!(
            eval(r#"["str/snakeCase", "Hello World"]"#),
            Value::String("hello_world".to_string())
        );
    }

    #[test]
    fn test_str_default() {
        assert_eq!(
            eval(r#"["str/default", "", "fallback"]"#),
            Value::String("fallback".to_string())
        );
        assert_eq!(
            eval(r#"["str/default", "hello", "fallback"]"#),
            Value::String("hello".to_string())
        );
        assert_eq!(
            eval(r#"["str/default", null, "fallback"]"#),
            Value::String("fallback".to_string())
        );
    }

    // =========================================================================
    // Phase 4.3: Object Extras
    // =========================================================================

    #[test]
    fn test_object_map_keys() {
        let result = eval(
            r#"["mapKeys", {"a": 1, "b": 2}, ["fn", ["k"], ["str/concat", "prefix_", "@k"]]]"#,
        );
        if let Value::Object(obj) = result {
            assert!(obj.contains_key("prefix_a"));
            assert!(obj.contains_key("prefix_b"));
        }
    }

    #[test]
    fn test_object_equals() {
        assert_eq!(
            eval(r#"["object/equals", {"a": 1}, {"a": 1}]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["object/equals", {"a": 1}, {"a": 2}]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_object_clone() {
        let result = eval(r#"["object/clone", {"a": 1, "b": 2}]"#);
        if let Value::Object(obj) = result {
            assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
        }
    }

    #[test]
    fn test_object_deep_clone() {
        let result = eval(r#"["deepClone", {"a": {"nested": 1}}]"#);
        if let Value::Object(obj) = result {
            if let Some(Value::Object(nested)) = obj.get("a") {
                assert_eq!(nested.get("nested"), Some(&Value::Number(1.0)));
            }
        }
    }

    // =========================================================================
    // Phase 4.4: Math Extras
    // =========================================================================

    #[test]
    fn test_math_random() {
        let result = eval(r#"["math/random"]"#);
        if let Value::Number(n) = result {
            assert!((0.0..=1.0).contains(&n));
        }
    }

    #[test]
    fn test_math_random_int() {
        let result = eval(r#"["randomInt", 1, 10]"#);
        if let Value::Number(n) = result {
            assert!((1.0..=10.0).contains(&n));
        }
    }

    #[test]
    fn test_math_default() {
        assert_eq!(eval(r#"["math/default", null, 42]"#), Value::Number(42.0));
        assert_eq!(eval(r#"["math/default", 10, 42]"#), Value::Number(10.0));
    }

    // =========================================================================
    // Phase 4.5: Time Extras
    // =========================================================================

    #[test]
    fn test_time_is_past() {
        // A timestamp from year 2000 should be in the past
        assert_eq!(eval(r#"["time/isPast", 946684800000]"#), Value::Bool(true));
    }

    #[test]
    fn test_time_is_future() {
        // A timestamp from year 2100 should be in the future
        assert_eq!(
            eval(r#"["time/isFuture", 4102444800000]"#),
            Value::Bool(true)
        );
    }

    #[test]
    fn test_time_duration() {
        // 3661 seconds = 1h 1m 1s
        assert_eq!(
            eval(r#"["time/duration", 3661000]"#),
            Value::String("1h 1m 1s".to_string())
        );
        // 90061 seconds = 1d 1h 1m 1s
        assert_eq!(
            eval(r#"["duration", 90061000]"#),
            Value::String("1d 1h 1m 1s".to_string())
        );
    }

    // =========================================================================
    // Phase 4.6: Validation Extras
    // =========================================================================

    #[test]
    fn test_validate_phone() {
        assert_eq!(
            eval(r#"["validate/phone", "+1 (555) 123-4567"]"#),
            Value::Bool(true)
        );
        assert_eq!(eval(r#"["phone", "1234567"]"#), Value::Bool(true));
        assert_eq!(eval(r#"["phone", "123"]"#), Value::Bool(false)); // Too short
    }

    #[test]
    fn test_validate_credit_card() {
        // Valid test card number (Visa test)
        assert_eq!(
            eval(r#"["validate/creditCard", "4111111111111111"]"#),
            Value::Bool(true)
        );
        // Invalid (fails Luhn)
        assert_eq!(
            eval(r#"["creditCard", "1234567890123456"]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_validate_none_of() {
        assert_eq!(
            eval(r#"["noneOf", "d", ["a", "b", "c"]]"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["validate/noneOf", "a", ["a", "b", "c"]]"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_validate_check() {
        let Value::Object(passed) = eval(r#"["validate/check", {"n": 10}, {"n": [["number"]]}]"#) else { panic!("not an object") };
        assert_eq!(passed["valid"], Value::Bool(true));
        let Value::Object(failed) = eval(r#"["check", {"n": "x"}, {"n": [["number"]]}]"#) else { panic!("not an object") };
        assert_eq!(failed["valid"], Value::Bool(false));
        assert!(eval_err(r#"["validate/check", 10, ["fn", ["x"], [">", "@x", 5]]]"#).contains("rules by field"));
    }

    // ========================================================================
    // Raw-body callbacks: the names a callback without `(fn …)` is evaluated
    // with. The compiler lowers a raw body to a lambda over exactly these names,
    // so each row pins the contract to what this evaluator actually binds.
    // ========================================================================

    fn nums(values: &[f64]) -> Value {
        Value::Array(values.iter().map(|n| Value::Number(*n)).collect())
    }

    #[test]
    fn map_fn_lambda_also_binds_index_like_the_ts_evaluator() {
        // std-stats maps metrics with `(fn metric { position: @index … })`.
        let out =
            eval(r#"["array/map", [10, 20], ["fn", "m", {"v": "@m", "position": "@index"}]]"#);
        let rows = out.as_array().expect("array");
        assert_eq!(
            rows[0].as_object().and_then(|o| o.get("position")),
            Some(&Value::Number(0.0))
        );
        assert_eq!(
            rows[1].as_object().and_then(|o| o.get("position")),
            Some(&Value::Number(1.0))
        );
        // Controls: a named index param and the raw-body form still bind as before.
        assert_eq!(
            eval(r#"["array/map", [10, 20], ["fn", ["m", "i"], ["+", "@m", "@i"]]]"#),
            nums(&[10.0, 21.0])
        );
        assert_eq!(
            eval(r#"["array/map", [10, 20], ["+", "@item", "@index"]]"#),
            nums(&[10.0, 21.0])
        );
    }

    #[test]
    fn group_by_a_field_name_like_the_ts_evaluator() {
        // std-graphs groups rows by a declared field (`["array/groupBy", @rows, "stage"]`).
        let out = eval(
            r#"["array/groupBy", [{"stage": "a"}, {"stage": "b"}, {"stage": "a"}, {"x": 1}], "stage"]"#,
        );
        let groups = out.as_object().expect("object");
        assert_eq!(
            groups.get("a").and_then(|v| v.as_array()).map(|v| v.len()),
            Some(2)
        );
        assert_eq!(
            groups.get("b").and_then(|v| v.as_array()).map(|v| v.len()),
            Some(1)
        );
        assert_eq!(
            groups
                .get("undefined")
                .and_then(|v| v.as_array())
                .map(|v| v.len()),
            Some(1)
        );
    }

    #[test]
    fn control_group_by_a_key_function_still_works() {
        let out = eval(r#"["array/groupBy", [1, 2, 3, 4], ["fn", "n", ["%", "@n", 2]]]"#);
        let groups = out.as_object().expect("object");
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn raw_callback_params_match_what_the_evaluator_binds() {
        use super::operators::raw_callback_params;
        assert_eq!(
            raw_callback_params("array/map"),
            Some(&["item", "index"][..])
        );
        assert_eq!(
            eval(r#"["array/map", [10, 20], ["+", "@item", "@index"]]"#),
            nums(&[10.0, 21.0])
        );

        assert_eq!(
            raw_callback_params("array/filter"),
            Some(&["item", "index"][..])
        );
        assert_eq!(
            eval(r#"["array/filter", [1, 2, 3], [">", "@item", 1]]"#),
            nums(&[2.0, 3.0])
        );
        // The index is bound whether the predicate is a raw body or a `(fn …)` that names only the element, as
        // the JS evaluator's `evalWithItem` always does.
        assert_eq!(
            eval(r#"["array/filter", [10, 20, 30], ["<", "@index", 2]]"#),
            nums(&[10.0, 20.0])
        );
        assert_eq!(
            eval(r#"["array/filter", [10, 20, 30], ["fn", "b", ["<", "@index", 2]]]"#),
            nums(&[10.0, 20.0])
        );
        assert_eq!(
            eval(r#"["array/filter", [10, 20, 30], ["fn", ["b", "i"], ["<", "@i", 2]]]"#),
            nums(&[10.0, 20.0]),
            "a second named parameter is the index too"
        );

        assert_eq!(
            raw_callback_params("array/reduce"),
            Some(&["acc", "item"][..])
        );
        assert_eq!(
            eval(r#"["array/reduce", [1, 2, 3], 10, ["+", "@acc", "@item"]]"#),
            Value::Number(16.0)
        );

        for (op, expr, expected) in [
            (
                "array/count",
                r#"["array/count", [5, 6, 7], [">", "@index", 0]]"#,
                Value::Number(2.0),
            ),
            (
                "array/find",
                r#"["array/find", [5, 6, 7], ["==", "@index", 1]]"#,
                Value::Number(6.0),
            ),
            (
                "array/every",
                r#"["array/every", [5, 6], [">", "@item", "@index"]]"#,
                Value::Bool(true),
            ),
            (
                "array/some",
                r#"["array/some", [5, 6], ["==", "@index", 1]]"#,
                Value::Bool(true),
            ),
        ] {
            assert_eq!(
                raw_callback_params(op),
                Some(&["item", "index"][..]),
                "{op}"
            );
            assert_eq!(eval(expr), expected, "{op}");
        }

        // object/filter binds the key as `item` and the value as `index`.
        assert_eq!(
            raw_callback_params("object/filter"),
            Some(&["item", "index"][..])
        );
        let Value::Object(kept) = eval(
            r#"["object/filter", {"a": 1, "b": 2}, ["and", ["==", "@item", "b"], ["==", "@index", 2]]]"#,
        ) else {
            panic!("object/filter must return an object");
        };
        assert_eq!(kept.keys().collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn the_callback_index_is_the_parameter_the_evaluator_binds_as_index() {
        use super::operators::{callback_index_position, raw_callback_params};
        for op in [
            "array/map",
            "map",
            "array/filter",
            "filter",
            "array/count",
            "count",
            "array/find",
            "find",
            "array/every",
            "array/some",
        ] {
            let at = callback_index_position(op).unwrap_or_else(|| panic!("{op} has an index"));
            assert_eq!(
                raw_callback_params(op).map(|names| names[at]),
                Some("index"),
                "{op}"
            );
        }
        for op in [
            "array/reduce",
            "object/filter",
            "array/findIndex",
            "array/len",
        ] {
            assert_eq!(
                callback_index_position(op),
                None,
                "control: {op} hands its callback no numeric index"
            );
        }
    }

    #[test]
    fn raw_callback_params_follow_operator_aliases() {
        use super::operators::raw_callback_params;
        assert_eq!(raw_callback_params("map"), raw_callback_params("array/map"));
        assert_eq!(
            raw_callback_params("filter"),
            raw_callback_params("array/filter")
        );
        assert_eq!(
            raw_callback_params("fold"),
            raw_callback_params("array/reduce")
        );
        assert_eq!(
            raw_callback_params("count"),
            raw_callback_params("array/count")
        );
    }

    #[test]
    fn operators_without_a_raw_body_form_have_no_raw_callback_params() {
        use super::operators::raw_callback_params;
        // These take only a real `(fn …)`; a non-callback operator has none at all.
        for op in [
            "array/findIndex",
            "array/groupBy",
            "array/reject",
            "array/partition",
            "object/mapValues",
            "object/mapKeys",
            "array/len",
            "+",
            "no/such-op",
        ] {
            assert_eq!(raw_callback_params(op), None, "{op}");
        }
        let expr: SExpression =
            serde_json::from_str(r#"["array/findIndex", [1, 2], [">", "@item", 1]]"#).unwrap();
        assert!(
            Evaluator::new()
                .evaluate(&expr, &BindingContext::new())
                .is_err(),
            "findIndex rejects a raw body"
        );
    }

    /// The one `array/max` / `array/min` contract (G-CROSS-082): same-typed elements read through
    /// `Value::to_number`, an optional field form reading `item[key]`, and `null` when nothing counts.
    /// A list of unlike element types is refused (G-ORBITAL-CORE-014).
    #[test]
    fn array_max_and_min_follow_the_one_contract() {
        assert_eq!(eval(r#"["array/max", [3, 7, 1]]"#), Value::Number(7.0));
        assert_eq!(eval(r#"["array/min", [3, 7, 1]]"#), Value::Number(1.0));
        assert_eq!(eval(r#"["array/max", ["3", "7"]]"#), Value::Number(7.0));
        assert_eq!(eval(r#"["array/min", [true, false]]"#), Value::Number(0.0));
        assert_eq!(eval(r#"["array/max", []]"#), Value::Null);
        assert_eq!(
            eval(r#"["array/max", [{"price": 3}, {"price": 9}], "price"]"#),
            Value::Number(9.0)
        );
        assert_eq!(
            eval(r#"["array/min", [{"price": 3}, {"price": 9}], "price"]"#),
            Value::Number(3.0)
        );
        assert_eq!(
            eval(r#"["array/max", [{"name": "n"}], "price"]"#),
            Value::Null,
            "an absent field doesn't count"
        );
        for op in ["array/max", "array/min"] {
            let err = eval_err(&format!(r#"["{op}", [3, "7", true, "x"]]"#));
            assert!(err.contains("number") && err.contains("string"), "{op}: {err}");
        }
    }

    fn names(v: Value) -> Vec<String> {
        let Value::Array(rows) = v else { panic!("expected a list") };
        rows.iter()
            .map(|r| r.as_object().and_then(|o| o.get("name")).and_then(Value::as_str).unwrap().to_string())
            .collect()
    }

    /// `str/collate` orders by the CLDR root collation: case-insensitive first, accents beside their letter.
    #[test]
    fn str_collate_orders_by_the_root_collation() {
        assert_eq!(eval(r#"["str/collate", "apple", "Banana"]"#), Value::Number(-1.0));
        assert_eq!(eval(r#"["str/collate", "Banana", "apple"]"#), Value::Number(1.0));
        assert_eq!(eval(r#"["str/collate", "\u00e9crou", "Zed"]"#), Value::Number(-1.0));
        assert_eq!(eval(r#"["str/collate", "same", "same"]"#), Value::Number(0.0));
        assert_eq!(eval(r#"["str/collate", "a", "A"]"#), Value::Number(-1.0), "lower case first at the third level");
        assert_eq!(eval(r#"["str/collate", null, "a"]"#), Value::Null);
        let mistyped = eval_err(r#"["str/collate", 1, "a"]"#);
        assert!(mistyped.contains("string") && mistyped.contains("number"), "{mistyped}");
        eval_err(r#"["str/collate", "a"]"#);
    }

    /// `str/collate-key` makes `array/sort` collate: `Banana, Zed, apple, écrou` sorts to `apple, Banana, écrou, Zed`.
    #[test]
    fn str_collate_key_sorts_by_collation() {
        let keyed = r#"["array/map", ["Banana", "Zed", "apple", "\u00e9crou"], ["fn", "n", {"name": "@n", "k": ["str/collate-key", "@n"]}]]"#;
        assert_eq!(
            names(eval(&format!(r#"["array/sort", {keyed}, "k"]"#))),
            ["apple", "Banana", "\u{e9}crou", "Zed"]
        );
        assert_eq!(
            names(eval(&format!(r#"["array/sort", {keyed}, "name"]"#))),
            ["Banana", "Zed", "apple", "\u{e9}crou"],
            "control: sorting by the name itself is code-point order"
        );
        assert_eq!(eval(r#"["str/collate-key", null]"#), Value::Null);
        assert_eq!(
            eval(r#"["str/collate-key", "a"]"#),
            eval(r#"["str/collate-key", "a"]"#),
            "deterministic"
        );
        assert_ne!(eval(r#"["str/collate-key", "a"]"#), eval(r#"["str/collate-key", "b"]"#));
    }

    fn by_name(shape: &str) -> serde_json::Value {
        serde_json::json!({ "type": "union", "values": [shape] })
    }

    fn tag(value: &str) -> serde_json::Value {
        serde_json::json!({ "type": "string", "required": true, "values": [value] })
    }

    /// RowPredicate's `.orb` definition, as lowering writes a field of it: a tagged union whose back-references
    /// are by-name references.
    fn predicate_definition() -> String {
        let text = serde_json::json!({ "type": "string", "required": true });
        serde_json::json!({
            "name": "RowPredicate",
            "type": "union",
            "values": ["Compare", "All", "Any", "Not"],
            "properties": {
                "Compare": { "type": "object", "properties": {
                    "kind": tag("compare"), "field": text, "op": text, "value": text } },
                "All": { "type": "object", "properties": {
                    "kind": tag("all"), "of": { "type": "array", "required": true, "items": by_name("RowPredicate") } } },
                "Any": { "type": "object", "properties": {
                    "kind": tag("any"), "of": { "type": "array", "required": true, "items": by_name("RowPredicate") } } },
                "Not": { "type": "object", "properties": {
                    "kind": tag("not"), "of": by_name("RowPredicate") } }
            }
        })
        .to_string()
    }

    fn fold_with(value: serde_json::Value, step: serde_json::Value, shape: &str, definition: &str) -> Result<Value, String> {
        let ctx = crate::evaluator::context::BindingContextBuilder::new()
            .payload(serde_json::from_value(serde_json::json!({ "v": value })).unwrap())
            .build();
        let expr: SExpression =
            serde_json::from_value(serde_json::json!(["tree/fold", "@payload.v", step, shape, definition])).unwrap();
        Evaluator::new().evaluate(&expr, &ctx).map_err(|e| e.to_string())
    }

    fn json_value(json: serde_json::Value) -> Value {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn tree_fold_translates_a_nested_predicate_bottom_up() {
        let step = serde_json::json!(["fn", ["node", "kids"],
            ["if", ["=", "@node.kind", "compare"], ["str/concat", "@node.field", "@node.op", "@node.value"],
            ["if", ["=", "@node.kind", "all"], ["str/concat", "(", ["str/join", "@kids.of", " AND "], ")"],
            ["if", ["=", "@node.kind", "any"], ["str/concat", "(", ["str/join", "@kids.of", " OR "], ")"],
            ["str/concat", "NOT ", "@kids.of"]]]]]);
        let pred = serde_json::json!({ "kind": "all", "of": [
            { "kind": "compare", "field": "a", "op": "==", "value": "1" },
            { "kind": "not", "of": { "kind": "any", "of": [
                { "kind": "compare", "field": "b", "op": "<", "value": "2" },
                { "kind": "compare", "field": "c", "op": ">", "value": "3" } ] } } ] });
        assert_eq!(
            fold_with(pred, step, "RowPredicate", &predicate_definition()).unwrap(),
            Value::String("(a==1 AND NOT (b<2 OR c>3))".into())
        );
    }

    #[test]
    fn tree_fold_child_results_mirror_list_map_and_field_positions() {
        let definition = serde_json::json!({
            "name": "Tree", "type": "object", "properties": {
                "label": { "type": "string", "required": true },
                "kids": { "type": "array", "items": by_name("Tree") },
                "named": { "type": "object", "items": by_name("Tree") },
                "parent": by_name("Tree"),
                "tags": { "type": "array", "items": { "type": "string" } }
            }
        })
        .to_string();
        let step = serde_json::json!(["fn", ["n", "k"], { "label": "@n.label", "k": "@k" }]);
        let leaf = |label: &str| serde_json::json!({ "label": label, "tags": ["t"] });
        let tree = serde_json::json!({
            "label": "root", "tags": ["x"],
            "kids": [leaf("a"), { "label": "b", "kids": [leaf("c")] }],
            "named": { "m": leaf("d") },
            "parent": leaf("p")
        });
        let done = |label: &str| serde_json::json!({ "label": label, "k": {} });
        assert_eq!(
            fold_with(tree, step, "Tree", &definition).unwrap(),
            json_value(serde_json::json!({ "label": "root", "k": {
                "kids": [done("a"), { "label": "b", "k": { "kids": [done("c")] } }],
                "named": { "m": done("d") },
                "parent": done("p")
            } }))
        );
    }

    #[test]
    fn tree_fold_calls_the_step_once_on_a_leaf() {
        let step = serde_json::json!(["fn", ["n", "k"], { "kind": "@n.kind", "k": "@k" }]);
        let leaf = serde_json::json!({ "kind": "compare", "field": "a", "op": "==", "value": "1" });
        assert_eq!(
            fold_with(leaf, step, "RowPredicate", &predicate_definition()).unwrap(),
            json_value(serde_json::json!({ "kind": "compare", "k": {} }))
        );
    }

    #[test]
    fn control_tree_fold_over_a_non_recursive_type_folds_once() {
        let definition = serde_json::json!({
            "name": "Note", "type": "object", "properties": {
                "of": { "type": "array", "items": { "type": "string" } },
                "Note": { "type": "string" }
            }
        })
        .to_string();
        let step = serde_json::json!(["fn", ["n", "k"], { "of": "@n.of", "k": "@k" }]);
        assert_eq!(
            fold_with(serde_json::json!({ "of": ["a", "b"], "Note": "x" }), step, "Note", &definition).unwrap(),
            json_value(serde_json::json!({ "of": ["a", "b"], "k": {} })),
            "a list of strings and a field named like the type are not children"
        );
    }

    #[test]
    fn tree_fold_reads_positions_from_the_selected_variant_never_the_value_shape() {
        let step = serde_json::json!(["fn", ["n", "k"], "@k"]);
        let not_with_a_list = serde_json::json!({ "kind": "not", "of": [{ "kind": "compare", "field": "a", "op": "==", "value": "1" }] });
        let err = fold_with(not_with_a_list, step.clone(), "RowPredicate", &predicate_definition()).unwrap_err();
        assert!(err.contains("RowPredicate") && err.contains("array"), "{err}");
        let untagged = serde_json::json!({ "kind": "xor", "of": [] });
        let err = fold_with(untagged, step.clone(), "RowPredicate", &predicate_definition()).unwrap_err();
        assert!(err.contains("no variant"), "{err}");
        assert_eq!(
            fold_with(serde_json::Value::Null, step.clone(), "RowPredicate", &predicate_definition()).unwrap(),
            Value::Null,
            "an absent value folds to null"
        );
        let unbaked: SExpression = serde_json::from_value(serde_json::json!(["tree/fold", null, step, "RowPredicate"])).unwrap();
        let err = Evaluator::new().evaluate(&unbaked, &BindingContext::new()).unwrap_err().to_string();
        assert!(err.contains("was not written into the call"), "{err}");
    }

    fn row_matches(pred: &serde_json::Value, row: &serde_json::Value) -> Value {
        let ctx = crate::evaluator::context::BindingContextBuilder::new()
            .payload(json_value(serde_json::json!({ "pred": pred, "row": row })))
            .build();
        eval_with_ctx(r#"["row/matches", "@payload.pred", "@payload.row"]"#, &ctx)
    }

    #[test]
    fn row_matches_decides_a_typed_predicate() {
        let open = serde_json::json!({ "kind": "compare", "field": "status", "op": "==", "value": "open" });
        assert_eq!(row_matches(&open, &serde_json::json!({ "id": "t1", "status": "open" })), Value::Bool(true));
        assert_eq!(row_matches(&open, &serde_json::json!({ "id": "t2", "status": "closed" })), Value::Bool(false));
        let negated = serde_json::json!({ "kind": "not", "of": open });
        assert_eq!(row_matches(&negated, &serde_json::json!({ "id": "t2", "status": "closed" })), Value::Bool(true));
        assert_eq!(row_matches(&serde_json::Value::Null, &serde_json::json!({ "id": "t1" })), Value::Null);
        let ctx = crate::evaluator::context::BindingContextBuilder::new()
            .payload(json_value(serde_json::json!({ "pred": { "kind": "nope" }, "row": { "id": "t1" } })))
            .build();
        let expr: SExpression = serde_json::from_str(r#"["row/matches", "@payload.pred", "@payload.row"]"#).unwrap();
        let err = Evaluator::new().evaluate(&expr, &ctx).unwrap_err().to_string();
        assert!(err.contains("not a RowPredicate"), "{err}");
    }

    #[test]
    fn control_row_matches_agrees_with_compile_predicate_on_the_same_inputs() {
        use orbital_core::runtime::row_query::{compile_predicate, PredicateRole};
        let source: Value = json_value(serde_json::json!(["and",
            ["==", "@entity.status", "open"],
            ["not", [">", "@entity.n", 3]]]));
        let compiled = compile_predicate(
            &source,
            &orbital_core::evaluator::context::BindingContext::new(),
            PredicateRole::Filter,
            &|_: &str| true,
        )
        .unwrap();
        let pred = serde_json::to_value(&compiled).unwrap();
        for row in [
            serde_json::json!({ "id": "a", "status": "open", "n": 1 }),
            serde_json::json!({ "id": "b", "status": "open", "n": 5 }),
            serde_json::json!({ "id": "c", "status": "closed", "n": 1 }),
        ] {
            assert_eq!(row_matches(&pred, &row), Value::Bool(compiled.matches(&json_value(row.clone()))), "{row}");
        }
    }
}
