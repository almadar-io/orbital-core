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

pub use crate::evaluator::operators::arg_types::operator_signature;
pub use crate::evaluator::operators::system::{TimeUnit, TIME_UNIT_VOCABULARY};
pub use crate::evaluator::operators::xml::bake_list_shapes;
pub use context::{BindingContext, BindingContextBuilder, LocalName};
pub use operators::{
    callback_index_position, eval_expr, get_operator, raw_callback_params, EvalResult, Operator,
};

pub use orbital_core_base::value::{Value, ValueMap};

use lru::LruCache;
use orbital_core_base::error::OrbitalError;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::sync::Mutex;
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
        Ok(self.evaluate(guard, ctx)?.boolean()?)
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
mod tests;
