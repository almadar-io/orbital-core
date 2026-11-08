//! Arithmetic and math operators.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::util::int_or_number;
use super::*;

// ============================================================================
// Arithmetic Operators
// ============================================================================

fn int_overflow(op: &str) -> OrbitalError {
    EvalError::InvalidArgument(format!("{op}: int overflow")).into()
}

fn require_numeric(v: &Value) -> Result<(), OrbitalError> {
    if v.is_numeric() {
        Ok(())
    } else {
        Err(v.kind_error("number").into())
    }
}

fn arith(
    op: &str,
    a: &Value,
    b: &Value,
    int_op: fn(i64, i64) -> Option<i64>,
    float_op: fn(f64, f64) -> f64,
) -> EvalResult {
    require_numeric(a)?;
    require_numeric(b)?;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => int_op(*x, *y).map(Value::Int).ok_or_else(|| int_overflow(op)),
        _ => Ok(Value::Number(float_op(a.number()?, b.number()?))),
    }
}

pub struct AddOp;
impl Operator for AddOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Number(0.0));
        }
        let mut sum = Value::Int(0);
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            sum = arith("+", &sum, &val, i64::checked_add, |a, b| a + b)?;
        }
        Ok(sum)
    }
}

pub struct SubOp;
impl Operator for SubOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Number(0.0));
        }
        let mut result = eval_expr(&args[0], ctx)?;
        require_numeric(&result)?;

        if args.len() == 1 {
            return match result {
                Value::Int(i) => i.checked_neg().map(Value::Int).ok_or_else(|| int_overflow("-")),
                other => Ok(Value::Number(-other.number()?)),
            };
        }

        for arg in &args[1..] {
            let val = eval_expr(arg, ctx)?;
            result = arith("-", &result, &val, i64::checked_sub, |a, b| a - b)?;
        }
        Ok(result)
    }
}

pub struct MulOp;
impl Operator for MulOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Ok(Value::Number(1.0));
        }
        let mut product = Value::Int(1);
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            product = arith("*", &product, &val, i64::checked_mul, |a, b| a * b)?;
        }
        Ok(product)
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
        let a = eval_expr(&args[0], ctx)?;
        let b = eval_expr(&args[1], ctx)?;
        require_numeric(&a)?;
        require_numeric(&b)?;

        if let (Value::Int(x), Value::Int(y)) = (&a, &b) {
            if *y == 0 {
                return Ok(Value::Null);
            }
            return x.checked_rem(*y).map(Value::Int).ok_or_else(|| int_overflow("%"));
        }
        let (a, b) = (a.number()?, b.number()?);
        if b == 0.0 {
            return Ok(Value::Null);
        }

        Ok(Value::Number(a % b))
    }
}

// ============================================================================
// Math Operators
// ============================================================================

fn extreme(args: &[SExpression], ctx: &BindingContext, keep: std::cmp::Ordering) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    let mut best: Option<Value> = None;
    let mut all_int = true;
    for arg in args {
        let val = eval_expr(arg, ctx)?;
        require_numeric(&val)?;
        all_int &= matches!(val, Value::Int(_));
        if matches!(val, Value::Number(n) if n.is_nan()) {
            continue;
        }
        best = match best {
            Some(current) if val.compare(&current)? != keep => Some(current),
            _ => Some(val),
        };
    }
    match best {
        Some(Value::Int(i)) if all_int => Ok(Value::Int(i)),
        Some(value) => {
            let n = value.number()?;
            if n.is_infinite() {
                Ok(Value::Null)
            } else {
                Ok(Value::Number(n))
            }
        }
        None => Ok(Value::Null),
    }
}

pub struct MinOp;
impl Operator for MinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        extreme(args, ctx, std::cmp::Ordering::Less)
    }
}

pub struct MaxOp;
impl Operator for MaxOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        extreme(args, ctx, std::cmp::Ordering::Greater)
    }
}

pub struct ClampOp;
impl Operator for ClampOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "clamp", >= 3);
        let value = eval_expr(&args[0], ctx)?;
        let min = eval_expr(&args[1], ctx)?;
        let max = eval_expr(&args[2], ctx)?;
        if let (Value::Int(v), Value::Int(lo), Value::Int(hi)) = (&value, &min, &max) {
            return Ok(Value::Int((*v).max(*lo).min(*hi)));
        }
        let (value, min, max) = (require_number!(value), require_number!(min), require_number!(max));

        Ok(Value::Number(value.max(min).min(max)))
    }
}

pub struct AbsOp;
impl Operator for AbsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "abs", 1);
        match eval_expr(&args[0], ctx)? {
            Value::Int(i) => i.checked_abs().map(Value::Int).ok_or_else(|| int_overflow("abs")),
            other => Ok(Value::Number(require_number!(other).abs())),
        }
    }
}

pub struct FloorOp;
impl Operator for FloorOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "floor", 1);
        match eval_expr(&args[0], ctx)? {
            Value::Int(i) => Ok(Value::Int(i)),
            other => Ok(int_or_number(require_number!(other).floor())),
        }
    }
}

pub struct CeilOp;
impl Operator for CeilOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ceil", 1);
        match eval_expr(&args[0], ctx)? {
            Value::Int(i) => Ok(Value::Int(i)),
            other => Ok(int_or_number(require_number!(other).ceil())),
        }
    }
}

pub struct RoundOp;
impl Operator for RoundOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "round", 1);
        let value = eval_expr(&args[0], ctx)?;
        let decimals = match args.get(1) {
            Some(arg) => require_number!(eval_expr(arg, ctx)?),
            None => 0.0,
        };
        if let Value::Int(i) = value {
            if decimals >= 0.0 {
                return Ok(Value::Int(i));
            }
        }
        let val = require_number!(value);
        if decimals == 0.0 {
            return Ok(int_or_number(val.round()));
        }
        let factor = libm::pow(10f64, decimals);
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

        Ok(nan_to_null(libm::pow(base, exp)))
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

        Ok(nan_to_null(libm::log10(n)))
    }
}

/// math/ln - Natural logarithm (base e)
pub struct MathLnOp;
impl Operator for MathLnOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ln", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);

        Ok(nan_to_null(libm::log(n)))
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

        Ok(Value::Int(sign as i64))
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

/// The stream a `math/random`, `math/randomInt` or `array/shuffle` call draws from. Inside the kernel it is a ChaCha20
/// stream seeded from the 32 bytes of sealed entropy the kernel draws for the call (EXEC-CRYPTO-2): the seed is never
/// journaled, the journal records the operator's inputs and answer, and a replay returns the answer without drawing again.
/// An evaluation with no sealed boundary (a bare evaluator, outside any dispatch) draws from one process-wide ChaCha20
/// stream, which no replay can reproduce.
pub(super) fn drawn(
    ctx: &BindingContext,
    op: &str,
    args: &[Value],
    mut compute: impl FnMut(&mut rand_chacha::ChaCha20Rng) -> Value,
) -> EvalResult {
    use rand_chacha::rand_core::SeedableRng;
    if ctx.has_sealed_outputs() {
        return ctx.sealed(op, args, |seed| compute(&mut rand_chacha::ChaCha20Rng::from_seed(seed)));
    }
    static PROCESS_STREAM: std::sync::OnceLock<std::sync::Mutex<rand_chacha::ChaCha20Rng>> = std::sync::OnceLock::new();
    let stream = PROCESS_STREAM.get_or_init(|| {
        let local = 0u8;
        let mut material = Vec::new();
        material.extend_from_slice(&orbital_core_base::time_util::now_millis().to_le_bytes());
        material.extend_from_slice(&(std::ptr::addr_of!(local) as usize).to_le_bytes());
        material.extend_from_slice(&(std::ptr::addr_of!(PROCESS_STREAM) as usize).to_le_bytes());
        std::sync::Mutex::new(rand_chacha::ChaCha20Rng::from_seed(orbital_core_base::codec::sha256(&material)))
    });
    let mut rng = stream.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(compute(&mut rng))
}

/// An unbiased draw in `0..bound` (`bound` at least 1) by rejection.
pub(super) fn below(rng: &mut rand_chacha::ChaCha20Rng, bound: u64) -> u64 {
    use rand_chacha::rand_core::RngCore;
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let draw = rng.next_u64();
        if draw >= threshold {
            return draw % bound;
        }
    }
}

/// math/random - Random float in [0, 1)
pub struct MathRandomOp;
impl Operator for MathRandomOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "random", == 0);
        drawn(ctx, "math/random", &[], |rng| {
            use rand_chacha::rand_core::RngCore;
            Value::Number((rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64)
        })
    }
}

/// math/randomInt - Random integer in range
pub struct MathRandomIntOp;
impl Operator for MathRandomIntOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "randomInt", >= 2);
        let whole = |arg: &SExpression, what: &str| -> Result<i64, OrbitalError> {
            let v = eval_expr(arg, ctx)?;
            if !v.is_numeric() {
                return Err(v.kind_error("int").into());
            }
            v.as_int().ok_or_else(|| {
                EvalError::InvalidArgument(format!("randomInt: {what} {} is not a whole number", v.to_string_value()))
                    .into()
            })
        };
        let min = whole(&args[0], "min")?;
        let max = whole(&args[1], "max")?;

        if max < min {
            return Ok(Value::Null);
        }

        let range = (i128::from(max) - i128::from(min) + 1) as u128;
        drawn(ctx, "math/randomInt", &[Value::Int(min), Value::Int(max)], |rng| {
            use rand_chacha::rand_core::RngCore;
            let offset = match u64::try_from(range) {
                Ok(bound) => u128::from(below(rng, bound)),
                Err(_) => u128::from(rng.next_u64()),
            };
            Value::Int((i128::from(min) + offset as i128) as i64)
        })
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
        Ok(Value::Number(libm::sin(n)))
    }
}

/// math/cos
pub struct MathCosOp;
impl Operator for MathCosOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/cos", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(libm::cos(n)))
    }
}

/// math/tan
pub struct MathTanOp;
impl Operator for MathTanOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/tan", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(libm::tan(n)))
    }
}

/// math/atan2 (y, x)
pub struct MathAtan2Op;
impl Operator for MathAtan2Op {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/atan2", >= 2);
        let y = require_number!(eval_expr(&args[0], ctx)?);
        let x = require_number!(eval_expr(&args[1], ctx)?);
        Ok(Value::Number(libm::atan2(y, x)))
    }
}

/// math/asin
pub struct MathAsinOp;
impl Operator for MathAsinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/asin", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(nan_to_null(libm::asin(n)))
    }
}

/// math/acos
pub struct MathAcosOp;
impl Operator for MathAcosOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/acos", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(nan_to_null(libm::acos(n)))
    }
}

/// math/atan
pub struct MathAtanOp;
impl Operator for MathAtanOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/atan", 1);
        let n = require_number!(eval_expr(&args[0], ctx)?);
        Ok(Value::Number(libm::atan(n)))
    }
}

/// math/hypot (a, b) = sqrt(a²+b²)
pub struct MathHypotOp;
impl Operator for MathHypotOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "math/hypot", >= 2);
        let a = require_number!(eval_expr(&args[0], ctx)?);
        let b = require_number!(eval_expr(&args[1], ctx)?);
        Ok(Value::Number(libm::hypot(a, b)))
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
