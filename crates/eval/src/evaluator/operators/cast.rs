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
        let SExpression::String(target) = &args[1] else {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "as: the target must be a type name".into(),
            )));
        };
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
                Some((shape, declared)) => match super::arg_types::misfit_of_shape(&value, &declared) {
                    None => Ok(value),
                    Some((path, actual)) => {
                        tracing::warn!(
                            target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                            "as: the value is not of the shape {shape}: at `{path}` it is {actual}"
                        );
                        Ok(Value::Null)
                    }
                },
                None => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "as: unknown target type {other}"
                )))),
            },
        }
    }
}

/// typed - `["typed", value, <declared type>]`: the value, checked to be of the type the program declared for it.
/// Written by `orb resolve` (never `.lolo`) at an operator argument whose declared type is a list of a union or of
/// `SExpr`, so the argument check binds that type instead of reading one from the items.
pub struct TypedOp;
impl Operator for TypedOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "typed", == 2);
        let declared = super::arg_types::declared_shape(&args[1])?;
        let value = eval_expr(&args[0], ctx)?;
        super::arg_types::value_of_shape(&value, &declared)?;
        Ok(value)
    }
}

/// proven - `["proven", [positions], op, arg0, arg1, …]`: the value of the call `[op, arg0, arg1, …]`, whose arguments at
/// `positions` the static checker proved and the kernel does not check again (EXEC-EVAL-1). Flat, so a mark adds no
/// nesting. Written by `orb resolve` (never `.lolo`); a program arriving as data has it removed before it is resolved.
pub struct ProvenOp;
impl Operator for ProvenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let invalid = |reason: &str| OrbitalError::Eval(EvalError::InvalidArgument(format!("proven: {reason}")));
        let Some(SExpression::Array(listed)) = args.first() else {
            return Err(invalid("the positions must be a list"));
        };
        let mut proven = 0u64;
        for position in crate::lower::strip_literal_list_marker(listed) {
            match position {
                SExpression::Number(n) if *n >= 0.0 && n.fract() == 0.0 => {
                    if *n < 64.0 {
                        proven |= 1 << (*n as u32);
                    }
                }
                _ => return Err(invalid("a position must be a non-negative integer")),
            }
        }
        let Some(operator) = args.get(1).and_then(SExpression::as_str).and_then(get_operator) else {
            return Err(invalid("the second argument must name an operator"));
        };
        let call = &args[2..];
        let _frame = super::arg_types::signature_for(&operator)
            .map(|signature| super::arg_types::CallFrame::enter_proven(signature, call, proven));
        operator.evaluate(call, ctx)
    }
}
