//! `row/matches`: whether a row satisfies a typed `RowPredicate`, decided by the kernel's one predicate evaluator
//! (`RowPredicate::matches`, the evaluator every store and `compile_predicate`'s output run through).

use super::macros::*;
use super::*;
use orbital_core_base::json_value::value_to_json;
use orbital_core_base::row_set::RowPredicate;

/// row/matches - `(row/matches pred row)`: `true` when `row` satisfies `pred`. An absent operand is an absent result.
pub struct RowMatchesOp;
impl Operator for RowMatchesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "row/matches", == 2);
        let pred = eval_expr(&args[0], ctx)?;
        let row = eval_expr(&args[1], ctx)?;
        if pred.is_null() || row.is_null() {
            return Ok(Value::Null);
        }
        if !matches!(row, Value::Object(_)) {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "Row".into(),
                actual: row.type_name().into(),
            }));
        }
        let pred: RowPredicate = serde_json::from_value(value_to_json(&pred)).map_err(|e| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "row/matches: not a RowPredicate: {e}"
            )))
        })?;
        Ok(Value::Bool(pred.matches(&row)))
    }
}
