//! Arithmetic and math operators.

use super::macros::*;
use super::*;

// ============================================================================
// Arithmetic Operators
// ============================================================================

pub struct AddOp;
impl Operator for AddOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Number(0.0));
        }
        let mut sum = 0.0;
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            sum += val.to_number().ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "number".to_string(),
                    actual: val.type_name().to_string(),
                })
            })?;
        }
        Ok(Value::Number(sum))
    }
}

pub struct SubOp;
impl Operator for SubOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Number(0.0));
        }
        let first = eval_expr(&args[0], ctx)?;
        let mut result = first.to_number().ok_or_else(|| {
            OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "number".to_string(),
                actual: first.type_name().to_string(),
            })
        })?;

        if args.len() == 1 {
            return Ok(Value::Number(-result));
        }

        for arg in &args[1..] {
            let val = eval_expr(arg, ctx)?;
            result -= val.to_number().ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "number".to_string(),
                    actual: val.type_name().to_string(),
                })
            })?;
        }
        Ok(Value::Number(result))
    }
}

pub struct MulOp;
impl Operator for MulOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Number(1.0));
        }
        let mut product = 1.0;
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            product *= val.to_number().ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "number".to_string(),
                    actual: val.type_name().to_string(),
                })
            })?;
        }
        Ok(Value::Number(product))
    }
}

fn nan_to_null(n: f64) -> Value {
    if n.is_nan() {
        Value::Null
    } else {
        Value::Number(n)
    }
}

pub struct DivOp;
impl Operator for DivOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "/", == 2);
        let numerator = require_number!(eval_expr(&args[0], ctx)?);

        let denominator = require_number!(eval_expr(&args[1], ctx)?);

        if denominator == 0.0 {
            return Ok(Value::Null);
        }

        Ok(Value::Number(numerator / denominator))
    }
}

pub struct ModOp;
impl Operator for ModOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "%", == 2);
        let a = require_number!(eval_expr(&args[0], ctx)?);

        let b = require_number!(eval_expr(&args[1], ctx)?);

        if b == 0.0 {
            return Ok(Value::Null);
        }

        Ok(Value::Number(a % b))
    }
}

// ============================================================================
// Math Operators
// ============================================================================

pub struct MinOp;
impl Operator for MinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Null);
        }
        let mut min = f64::INFINITY;
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            if let Some(n) = val.to_number() {
                if n < min {
                    min = n;
                }
            }
        }
        if min.is_infinite() {
            Ok(Value::Null)
        } else {
            Ok(Value::Number(min))
        }
    }
}

pub struct MaxOp;
impl Operator for MaxOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Null);
        }
        let mut max = f64::NEG_INFINITY;
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            if let Some(n) = val.to_number() {
                if n > max {
                    max = n;
                }
            }
        }
        if max.is_infinite() {
            Ok(Value::Null)
        } else {
            Ok(Value::Number(max))
        }
    }
}

pub struct ClampOp;
impl Operator for ClampOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "clamp", >= 3);
        let value = require_number!(eval_expr(&args[0], ctx)?);
        let min = require_number!(eval_expr(&args[1], ctx)?);
        let max = require_number!(eval_expr(&args[2], ctx)?);

        Ok(Value::Number(value.max(min).min(max)))
    }
}

pub struct AbsOp;
impl Operator for AbsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "abs", 1);
        let val = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(val.abs()))
    }
}

pub struct FloorOp;
impl Operator for FloorOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "floor", 1);
        let val = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(val.floor()))
    }
}

pub struct CeilOp;
impl Operator for CeilOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ceil", 1);
        let val = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(val.ceil()))
    }
}

pub struct RoundOp;
impl Operator for RoundOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "round", 1);
        let val = require_number!(eval_expr(&args[0], ctx)?);
        let decimals = match args.get(1) {
            Some(arg) => require_number!(eval_expr(arg, ctx)?),
            None => 0.0,
        };
        if decimals == 0.0 {
            return Ok(Value::Number(val.round()));
        }
        let factor = 10f64.powf(decimals);
        Ok(Value::Number((val * factor).round() / factor))
    }
}

// ============================================================================
// Phase 2.4: Math Operations
// ============================================================================

/// math/pow - Exponentiation
pub struct MathPowOp;
impl Operator for MathPowOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "pow", >= 2);
        let base = require_number!(eval_expr(&args[0], ctx)?);
        let exp = require_number!(eval_expr(&args[1], ctx)?);

        Ok(nan_to_null(base.powf(exp)))
    }
}

/// math/sqrt - Square root
pub struct MathSqrtOp;
impl Operator for MathSqrtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "sqrt", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);

        Ok(nan_to_null(n.sqrt()))
    }
}

/// math/log10 - Base-10 logarithm
pub struct MathLog10Op;
impl Operator for MathLog10Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "log10", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);

        Ok(nan_to_null(n.log10()))
    }
}

/// math/ln - Natural logarithm (base e)
pub struct MathLnOp;
impl Operator for MathLnOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ln", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);

        Ok(nan_to_null(n.ln()))
    }
}

// ============================================================================
// Phase 3.4: Math Advanced
// ============================================================================

/// math/sign - Returns -1, 0, or 1
pub struct MathSignOp;
impl Operator for MathSignOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "sign", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);

        let sign = if n > 0.0 {
            1.0
        } else if n < 0.0 {
            -1.0
        } else {
            0.0
        };

        Ok(Value::Number(sign))
    }
}

/// math/lerp - Linear interpolation
pub struct MathLerpOp;
impl Operator for MathLerpOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "lerp", >= 3);
        let a = require_number!(eval_expr(&args[0], ctx)?);
        let b = require_number!(eval_expr(&args[1], ctx)?);
        let t = require_number!(eval_expr(&args[2], ctx)?);

        Ok(Value::Number(a + (b - a) * t))
    }
}

/// math/map - Map value from one range to another
pub struct MathMapOp;
impl Operator for MathMapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/map", == 5);
        let value = require_number!(eval_expr(&args[0], ctx)?);
        let in_min = require_number!(eval_expr(&args[1], ctx)?);
        let in_max = require_number!(eval_expr(&args[2], ctx)?);
        let out_min = require_number!(eval_expr(&args[3], ctx)?);
        let out_max = require_number!(eval_expr(&args[4], ctx)?);

        if (in_max - in_min).abs() < f64::EPSILON {
            return Ok(Value::Number(out_min));
        }

        let result = (value - in_min) / (in_max - in_min) * (out_max - out_min) + out_min;
        Ok(Value::Number(result))
    }
}

// ============================================================================
// Phase 4.4: Math Extras
// ============================================================================

/// math/random - Random float 0-1
pub struct MathRandomOp;
impl Operator for MathRandomOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        let seed = orbital_core_base::time_util::now_millis() as u64;

        // Simple LCG to generate a float between 0 and 1
        let rng = seed.wrapping_mul(1103515245).wrapping_add(12345);
        let random = (rng as f64) / (u64::MAX as f64);

        Ok(Value::Number(random))
    }
}

/// math/randomInt - Random integer in range
pub struct MathRandomIntOp;
impl Operator for MathRandomIntOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "randomInt", >= 2);
        let min = require_number!(eval_expr(&args[0], ctx)?) as i64;
        let max = require_number!(eval_expr(&args[1], ctx)?) as i64;

        if max < min {
            return Ok(Value::Null);
        }

        let seed = orbital_core_base::time_util::now_millis() as u64;

        let rng = seed.wrapping_mul(1103515245).wrapping_add(12345);
        let range = (i128::from(max) - i128::from(min) + 1) as u128;
        let result = i128::from(min) + (u128::from(rng) % range) as i128;

        Ok(Value::Number(result as f64))
    }
}

/// math/default - Default value if null
pub struct MathDefaultOp;
impl Operator for MathDefaultOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/default", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let fallback = eval_expr(&args[1], ctx)?;

        match val {
            Value::Null => Ok(fallback),
            _ => Ok(val),
        }
    }
}

/// math/mod - Modulo (remainder) - aliases existing ModOp
pub struct MathModOp;
impl Operator for MathModOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        ModOp.evaluate(args, ctx)
    }
}

// ============================================================================
// G6 Game: Trigonometry & angle helpers
// ============================================================================

/// math/sin
pub struct MathSinOp;
impl Operator for MathSinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/sin", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(n.sin()))
    }
}

/// math/cos
pub struct MathCosOp;
impl Operator for MathCosOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/cos", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(n.cos()))
    }
}

/// math/tan
pub struct MathTanOp;
impl Operator for MathTanOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/tan", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(n.tan()))
    }
}

/// math/atan2 (y, x)
pub struct MathAtan2Op;
impl Operator for MathAtan2Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/atan2", >= 2);
        let y = require_number!(eval_expr(&args[0], ctx)?);
        let x = require_number!(eval_expr(&args[1], ctx)?);
        Ok(Value::Number(y.atan2(x)))
    }
}

/// math/asin
pub struct MathAsinOp;
impl Operator for MathAsinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/asin", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(nan_to_null(n.asin()))
    }
}

/// math/acos
pub struct MathAcosOp;
impl Operator for MathAcosOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/acos", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(nan_to_null(n.acos()))
    }
}

/// math/atan
pub struct MathAtanOp;
impl Operator for MathAtanOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/atan", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(n.atan()))
    }
}

/// math/hypot (a, b) = sqrt(a²+b²)
pub struct MathHypotOp;
impl Operator for MathHypotOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/hypot", >= 2);
        let a = require_number!(eval_expr(&args[0], ctx)?);
        let b = require_number!(eval_expr(&args[1], ctx)?);
        Ok(Value::Number(a.hypot(b)))
    }
}

/// math/deg-rad = deg·π/180
pub struct MathDegRadOp;
impl Operator for MathDegRadOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/deg-rad", 1);
        let d = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(d * std::f64::consts::PI / 180.0))
    }
}

/// math/rad-deg = rad·180/π
pub struct MathRadDegOp;
impl Operator for MathRadDegOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/rad-deg", 1);
        let r = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(r * 180.0 / std::f64::consts::PI))
    }
}

/// math/wrap (v, min, max) → wrap into [min,max)
pub struct MathWrapOp;
impl Operator for MathWrapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/wrap", >= 3);
        let v = require_number!(eval_expr(&args[0], ctx)?);
        let min = require_number!(eval_expr(&args[1], ctx)?);
        let max = require_number!(eval_expr(&args[2], ctx)?);
        let r = max - min;
        if r <= 0.0 {
            return Ok(Value::Number(min));
        }
        let wrapped = min + (((v - min) % r) + r) % r;
        Ok(Value::Number(wrapped))
    }
}

/// math/approach (cur, target, maxD)
pub struct MathApproachOp;
impl Operator for MathApproachOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/approach", >= 3);
        let cur = require_number!(eval_expr(&args[0], ctx)?);
        let target = require_number!(eval_expr(&args[1], ctx)?);
        let max_d = require_number!(eval_expr(&args[2], ctx)?);
        let diff = target - cur;
        let result = if diff.abs() <= max_d {
            target
        } else {
            cur + diff.signum() * max_d
        };
        Ok(Value::Number(result))
    }
}
