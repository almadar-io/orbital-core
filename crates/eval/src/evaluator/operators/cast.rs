//! `as` - the explicit cast. `Explicit` conversions are validated against `type_compat::sub`
//! at `orb validate`; at run time a cast is the identity on the value, guarded per target.

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
            other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "as: unknown target type {other}"
            )))),
        }
    }
}
