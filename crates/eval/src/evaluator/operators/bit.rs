//! `bit/*` operators over 32-bit unsigned words. Out-of-range operands and counts answer null, never wrapped.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

fn bounded(arg: &SExpression, ctx: &BindingContext, max: i64) -> Result<Option<u32>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        value @ (Value::Number(_) | Value::Int(_)) => {
            Ok(value.as_int().filter(|n| (0..=max).contains(n)).map(|n| n as u32))
        }
        other => Err(other.kind_error("number").into()),
    }
}

fn word(arg: &SExpression, ctx: &BindingContext) -> Result<Option<u32>, OrbitalError> {
    bounded(arg, ctx, i64::from(u32::MAX))
}

fn count(arg: &SExpression, ctx: &BindingContext) -> Result<Option<u32>, OrbitalError> {
    bounded(arg, ctx, 31)
}

fn fold(
    args: &[SExpression],
    ctx: &BindingContext,
    op: &str,
    f: fn(u32, u32) -> u32,
) -> EvalResult {
    require_arity!(args, op, >= 2);
    let mut words = Vec::with_capacity(args.len());
    for arg in args {
        words.push(word(arg, ctx)?);
    }
    let Some(mut acc) = words[0] else {
        return Ok(Value::Null);
    };
    for next in &words[1..] {
        let Some(next) = *next else {
            return Ok(Value::Null);
        };
        acc = f(acc, next);
    }
    Ok(Value::Int(i64::from(acc)))
}

fn shift(
    args: &[SExpression],
    ctx: &BindingContext,
    op: &str,
    f: fn(u32, u32) -> u32,
) -> EvalResult {
    require_arity!(args, op, == 2);
    let (Some(a), Some(n)) = (word(&args[0], ctx)?, count(&args[1], ctx)?) else {
        return Ok(Value::Null);
    };
    Ok(Value::Int(i64::from(f(a, n))))
}

/// bit/xor (...words) - XOR folded left
pub struct BitXorOp;
impl Operator for BitXorOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        fold(args, ctx, "bit/xor", |a, b| a ^ b)
    }
}

/// bit/and (...words) - AND folded left
pub struct BitAndOp;
impl Operator for BitAndOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        fold(args, ctx, "bit/and", |a, b| a & b)
    }
}

/// bit/or (...words) - OR folded left
pub struct BitOrOp;
impl Operator for BitOrOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        fold(args, ctx, "bit/or", |a, b| a | b)
    }
}

/// bit/not (a) - NOT over 32 bits
pub struct BitNotOp;
impl Operator for BitNotOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "bit/not", == 1);
        Ok(match word(&args[0], ctx)? {
            Some(a) => Value::Int(i64::from(!a)),
            None => Value::Null,
        })
    }
}

/// bit/shl (a, n) - left shift; bits past bit 31 are discarded
pub struct BitShlOp;
impl Operator for BitShlOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        shift(args, ctx, "bit/shl", |a, n| a << n)
    }
}

/// bit/shr (a, n) - logical right shift
pub struct BitShrOp;
impl Operator for BitShrOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        shift(args, ctx, "bit/shr", |a, n| a >> n)
    }
}

/// bit/rotl (a, n) - rotate left within 32 bits
pub struct BitRotlOp;
impl Operator for BitRotlOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        shift(args, ctx, "bit/rotl", u32::rotate_left)
    }
}

/// bit/rotr (a, n) - rotate right within 32 bits
pub struct BitRotrOp;
impl Operator for BitRotrOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        shift(args, ctx, "bit/rotr", u32::rotate_right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(form: &str) -> EvalResult {
        let expr: SExpression = serde_json::from_str(form).expect("s-expression");
        eval_expr(&expr, &BindingContext::default())
    }

    #[test]
    fn values() {
        let cases: &[(&str, f64)] = &[
            (r#"["bit/xor", 0, 0]"#, 0.0),
            (r#"["bit/xor", 4294967295, 0]"#, 4294967295.0),
            (r#"["bit/xor", 4294967295, 4294967295]"#, 0.0),
            (r#"["bit/xor", 305419896, 2596069104]"#, 2290649224.0),
            (r#"["bit/xor", 1, 2, 4, 8]"#, 15.0),
            (
                r#"["bit/xor", 2147483648, 2147483648, 2147483648]"#,
                2147483648.0,
            ),
            (r#"["bit/and", 4294967295, 2147483648]"#, 2147483648.0),
            (r#"["bit/and", 305419896, 255]"#, 120.0),
            (
                r#"["bit/and", 4042322160, 267390960, 4294901760]"#,
                15728640.0,
            ),
            (r#"["bit/or", 2147483648, 1]"#, 2147483649.0),
            (r#"["bit/or", 0, 0]"#, 0.0),
            (r#"["bit/or", 252645135, 4042322160]"#, 4294967295.0),
            (r#"["bit/not", 0]"#, 4294967295.0),
            (r#"["bit/not", 4294967295]"#, 0.0),
            (r#"["bit/not", 2147483648]"#, 2147483647.0),
            (r#"["bit/not", 305419896]"#, 3989547399.0),
            (r#"["bit/shl", 1, 0]"#, 1.0),
            (r#"["bit/shl", 1, 31]"#, 2147483648.0),
            (r#"["bit/shl", 4294967295, 1]"#, 4294967294.0),
            (r#"["bit/shl", 2147483648, 1]"#, 0.0),
            (r#"["bit/shl", 305419896, 4]"#, 591751040.0),
            (r#"["bit/shr", 2147483648, 31]"#, 1.0),
            (r#"["bit/shr", 4294967295, 0]"#, 4294967295.0),
            (r#"["bit/shr", 4294967295, 31]"#, 1.0),
            (r#"["bit/shr", 305419896, 8]"#, 1193046.0),
            (r#"["bit/shr", 1, 1]"#, 0.0),
            (r#"["bit/rotl", 2147483648, 1]"#, 1.0),
            (r#"["bit/rotl", 1, 31]"#, 2147483648.0),
            (r#"["bit/rotl", 305419896, 0]"#, 305419896.0),
            (r#"["bit/rotl", 305419896, 8]"#, 878082066.0),
            (r#"["bit/rotl", 4294967295, 13]"#, 4294967295.0),
            (r#"["bit/rotr", 1, 1]"#, 2147483648.0),
            (r#"["bit/rotr", 2147483648, 31]"#, 1.0),
            (r#"["bit/rotr", 305419896, 0]"#, 305419896.0),
            (r#"["bit/rotr", 305419896, 8]"#, 2014458966.0),
            (r#"["bit/rotr", 1779033703, 7]"#, 3470005196.0),
            (r#"["bit/rotr", 1779033703, 18]"#, 2040126082.0),
        ];
        for (form, expected) in cases {
            match eval(form) {
                Ok(Value::Int(n)) => assert_eq!(n as f64, *expected, "{form}"),
                other => panic!("{form}: expected {expected}, got {other:?}"),
            }
        }
    }

    #[test]
    fn nested_results_feed_back() {
        let h0: u32 = 1779033703;
        let sigma0 = h0.rotate_right(2) ^ h0.rotate_right(13) ^ h0.rotate_right(22);
        match eval(
            r#"["bit/xor", ["bit/rotr", 1779033703, 2], ["bit/rotr", 1779033703, 13], ["bit/rotr", 1779033703, 22]]"#,
        ) {
            Ok(Value::Int(n)) => assert_eq!(n, i64::from(sigma0)),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn an_int_operand_works_and_the_result_is_an_int() {
        let ctx = BindingContext::default();
        let expr: SExpression = serde_json::from_str(r#"["bit/xor", 1, 3]"#).unwrap();
        assert!(matches!(eval_expr(&expr, &ctx), Ok(Value::Int(2))));
        let mut ctx = BindingContext::default();
        ctx.set_local("w", Value::Int(4294967295));
        let expr: SExpression = serde_json::from_str(r#"["bit/not", "@w"]"#).unwrap();
        assert!(matches!(eval_expr(&expr, &ctx), Ok(Value::Int(0))));
        ctx.set_local("w", Value::Int(4294967296));
        assert_eq!(eval_expr(&expr, &ctx).unwrap(), Value::Null, "control: an int past 32 bits answers null");
    }

    #[test]
    fn out_of_range_answers_null() {
        for form in [
            r#"["bit/xor", 4294967296, 0]"#,
            r#"["bit/and", -1, 0]"#,
            r#"["bit/or", 1.5, 0]"#,
            r#"["bit/xor", 0, 1, 4294967296]"#,
            r#"["bit/shl", 1, 32]"#,
            r#"["bit/shr", 1, -1]"#,
            r#"["bit/rotl", 1, 0.5]"#,
            r#"["bit/rotl", 1, 32]"#,
            r#"["bit/rotr", 1, 32]"#,
            r#"["bit/rotr", 4294967296, 1]"#,
            r#"["bit/shr", 4294967296, 0]"#,
            r#"["bit/not", 4294967296]"#,
        ] {
            assert_eq!(eval(form).unwrap(), Value::Null, "{form}");
        }
    }

    #[test]
    fn non_numbers_stay_errors() {
        let cases: &[(&str, &str)] = &[
            (
                r#"["bit/not", null]"#,
                "Type mismatch: expected number, got null",
            ),
            (
                r#"["bit/xor", "1", 0]"#,
                "Type mismatch: expected number, got string",
            ),
        ];
        for (form, message) in cases {
            match eval(form) {
                Err(e) => assert!(e.to_string().contains(message), "{form}: {e}"),
                Ok(v) => panic!("{form}: expected an error, got {v:?}"),
            }
        }
    }

    #[test]
    fn arity() {
        for form in [
            r#"["bit/xor", 1]"#,
            r#"["bit/and", 1]"#,
            r#"["bit/or"]"#,
            r#"["bit/not", 1, 2]"#,
            r#"["bit/shl", 1]"#,
            r#"["bit/rotr", 1, 2, 3]"#,
        ] {
            match eval(form) {
                Err(OrbitalError::Eval(EvalError::ArityMismatch { .. })) => {}
                other => panic!("{form}: expected ArityMismatch, got {other:?}"),
            }
        }
    }
}
