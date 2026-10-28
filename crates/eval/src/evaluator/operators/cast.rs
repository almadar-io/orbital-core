//! `as` - the explicit cast. `Explicit` conversions are validated against `type_compat::sub`
//! at `orb validate`; at run time a cast is the identity on the value, guarded per target.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

/// as - `(as value TypeName)`: the value, converted to the named type. A `CallKey` is a
/// non-empty string at rest, so the cast is the identity once that holds.
pub struct AsOp;
impl Operator for AsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "as", == 2);
        let target = super::json::shape_operand(&args[1])?;
        let value = eval_expr(&args[0], ctx)?;
        match target.as_str() {
            "CallKey" => match value {
                Value::String(ref s) if !s.is_empty() => Ok(value),
                Value::String(_) => Err(OrbitalError::Eval(EvalError::InvalidArgument(
                    "as: a CallKey cannot be empty".into(),
                ))),
                other => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "string".into(),
                    actual: other.type_name().into(),
                })),
            },
            other => match super::json::run_time_shape(&args[1])? {
                // A declared shape narrows: the value when it fits, null when it does not (a failable result).
                Some((shape, declared)) => {
                    match super::arg_types::misfit_of_shape(&value, &declared) {
                        None => Ok(value),
                        Some((path, actual)) => {
                            tracing::debug!(
                                target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                                "as: the value is not of the shape {shape}: at `{path}` it is {actual}"
                            );
                            Ok(Value::Null)
                        }
                    }
                }
                None => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "as: unknown target type {other}"
                )))),
            },
        }
    }
}

/// typed - `["typed", value, <declared type>]`: a static ascription. The declared type is the validator's (a `let` local
/// annotation, a knob's type); the kernel evaluates the value and answers it, checking nothing. A run-time shape check
/// is written in `.lolo` as `as T` or `json/parse T`.
pub struct TypedOp;
impl Operator for TypedOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "typed", == 2);
        eval_expr(&args[0], ctx)
    }
}
