//! Format operators that turn values into formatted strings.

use super::*;

// ============================================================================
// Format operators - convert values to formatted strings
// ============================================================================

/// format/ordinal - Format number as ordinal (1st, 2nd, etc.)
pub struct FormatOrdinalOp;
impl Operator for FormatOrdinalOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/ordinal", 1);
        let val = eval_expr(&args[0], ctx)?;
        let num = val.to_number().unwrap_or(0.0) as i64;
        let suffix = match (num % 10, num % 100) {
            (1, 11) | (2, 12) | (3, 13) => "th",
            (1, _) => "st",
            (2, _) => "nd",
            (3, _) => "rd",
            _ => "th",
        };
        Ok(Value::string(format!("{}{}", num, suffix)))
    }
}

/// format/plural - Format with plural form
pub struct FormatPluralOp;
impl Operator for FormatPluralOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/plural", >= 3);
        let count = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0) as i64;
        let singular = eval_expr(&args[1], ctx)?.to_string_value();
        let plural = eval_expr(&args[2], ctx)?.to_string_value();
        let word = if count == 1 { singular } else { plural };
        Ok(Value::string(format!("{} {}", count, word)))
    }
}
