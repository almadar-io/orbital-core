//! Shared boilerplate macros for operator implementations.
//!
//! These de-duplicate the per-operator arity / type / lambda checks. Each macro
//! expands to exactly the inline code the operators used before this module
//! split, so behavior is unchanged.

/// Require at least `$n` arguments, else return an `ArityMismatch` error.
///
/// Two forms:
/// - `require_arity!(args, "op", 1)` for the single-arg `args.is_empty()` check.
/// - `require_arity!(args, "op", >= 2)` for the `args.len() < n` check.
macro_rules! require_arity {
    ($args:expr, $op:expr, 1) => {
        if $args.is_empty() {
            return Err($crate::error::OrbitalError::Eval(
                $crate::error::EvalError::ArityMismatch {
                    operator: $op.to_string(),
                    expected: 1,
                    actual: 0,
                },
            ));
        }
    };
    ($args:expr, $op:expr, >= $n:expr) => {
        if $args.len() < $n {
            return Err($crate::error::OrbitalError::Eval(
                $crate::error::EvalError::ArityMismatch {
                    operator: $op.to_string(),
                    expected: $n,
                    actual: $args.len(),
                },
            ));
        }
    };
}

/// Unwrap a `Value` into its inner `Vec<Value>`, else return a `TypeMismatch`
/// error reporting the actual type name.
macro_rules! require_array {
    ($val:expr) => {
        match $val {
            $crate::evaluator::value::Value::Array(a) => a,
            other => {
                return Err($crate::error::OrbitalError::Eval(
                    $crate::error::EvalError::TypeMismatch {
                        expected: "array".to_string(),
                        actual: other.type_name().to_string(),
                    },
                ))
            }
        }
    };
}

/// Coerce a `Value` to a number, else return a `TypeMismatch` error with the
/// `"non-number"` actual marker (matching the historical inline form).
macro_rules! require_number {
    ($val:expr) => {
        $val.to_number().ok_or_else(|| {
            $crate::error::OrbitalError::Eval($crate::error::EvalError::TypeMismatch {
                expected: "number".to_string(),
                actual: "non-number".to_string(),
            })
        })?
    };
}

/// Destructure a `Value::Lambda` into `(params, body)` clones, else return a
/// `TypeMismatch` error reporting `"function"` expected.
macro_rules! require_lambda {
    ($val:expr) => {
        match &$val {
            $crate::evaluator::value::Value::Lambda { params, body } => {
                (params.clone(), body.clone())
            }
            other => {
                return Err($crate::error::OrbitalError::Eval(
                    $crate::error::EvalError::TypeMismatch {
                        expected: "function".to_string(),
                        actual: other.type_name().to_string(),
                    },
                ))
            }
        }
    };
}

pub(crate) use {require_arity, require_array, require_lambda, require_number};
