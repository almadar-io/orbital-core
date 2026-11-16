//! Format operators that turn values into formatted strings.

use super::*;

// ============================================================================
// Format operators - convert values to formatted strings
// ============================================================================

fn whole_i64(value: &Value) -> Result<i64, OrbitalError> {
    if let Value::Int(i) = value {
        return Ok(*i);
    }
    let n = value.number()?;
    if n.fract() != 0.0 || n.abs() > 9_007_199_254_740_991.0 {
        return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "expected a whole number, got {n}"
        ))));
    }
    Ok(n as i64)
}

/// format/ordinal - Format number as ordinal (1st, 2nd, etc.)
pub struct FormatOrdinalOp;
impl StrictOperator for FormatOrdinalOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/ordinal", 1);
        let val = args[0];
        let num = whole_i64(val)?;
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
impl StrictOperator for FormatPluralOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/plural", >= 3);
        let count = whole_i64(args[0])?;
        let singular = args[1].text()?.to_string();
        let plural = args[2].text()?.to_string();
        let word = if count == 1 { singular } else { plural };
        Ok(Value::string(format!("{} {}", count, word)))
    }
}
