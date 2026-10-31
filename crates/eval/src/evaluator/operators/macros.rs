//! Shared boilerplate macros for operator implementations.
//!
//! These de-duplicate the per-operator arity / type / lambda checks. Each macro
//! expands to exactly the inline code the operators used before this module
//! split; the kind checks are thin wrappers over the strict `Value` accessors.

/// Require an argument count, else return an `ArityMismatch` error.
///
/// - `require_arity!(args, "op", 1)` for the single-arg `args.is_empty()` check.
/// - `require_arity!(args, "op", >= 2)` for the `args.len() < n` check.
/// - `require_arity!(args, "op", == 2)` for the `args.len() != n` check.
macro_rules! require_arity {
    ($args:expr, $op:expr, == $n:expr) => {
        if $args.len() != $n {
            return Err(::orbital_core_base::error::OrbitalError::Eval(
                ::orbital_core_base::error::EvalError::ArityMismatch {
                    operator: $op.to_string(),
                    expected: $n,
                    actual: $args.len(),
                },
            ));
        }
    };
    ($args:expr, $op:expr, 1) => {
        if $args.is_empty() {
            return Err(::orbital_core_base::error::OrbitalError::Eval(
                ::orbital_core_base::error::EvalError::ArityMismatch {
                    operator: $op.to_string(),
                    expected: 1,
                    actual: 0,
                },
            ));
        }
    };
    ($args:expr, $op:expr, >= $n:expr) => {
        if $args.len() < $n {
            return Err(::orbital_core_base::error::OrbitalError::Eval(
                ::orbital_core_base::error::EvalError::ArityMismatch {
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
            ::orbital_core_base::value::Value::Array(a) => a,
            other => {
                return Err(::orbital_core_base::error::OrbitalError::Eval(
                    ::orbital_core_base::error::EvalError::TypeMismatch {
                        expected: "array".to_string(),
                        actual: other.type_name().to_string(),
                    },
                ))
            }
        }
    };
}

/// Read a `Value` as a number; any other kind is a `TypeMismatch` naming the actual kind.
macro_rules! require_number {
    ($val:expr) => {
        $val.number()?
    };
}

/// Destructure a `Value::Lambda` into `(params, body)` clones, else return a
/// `TypeMismatch` error reporting `"function"` expected.
macro_rules! require_lambda {
    ($val:expr) => {
        match &$val {
            ::orbital_core_base::value::Value::Lambda(l) => (l.params.clone(), l.body.clone()),
            other => {
                return Err(::orbital_core_base::error::OrbitalError::Eval(
                    ::orbital_core_base::error::EvalError::TypeMismatch {
                        expected: "function".to_string(),
                        actual: other.type_name().to_string(),
                    },
                ))
            }
        }
    };
}

pub(crate) use {require_arity, require_array, require_lambda, require_number};
