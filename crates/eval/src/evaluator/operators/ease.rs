//! G6 Game: easing operators (`ease/*`). Pure, total. Penner formulas pinned
//! so JS and Rust paths match byte-for-byte.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

fn ease_curve(curve: &str, t: f64) -> f64 {
    use std::f64::consts::PI;
    match curve {
        "in-quad" => t * t,
        "out-quad" => t * (2.0 - t),
        "in-out-quad" => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                -1.0 + (4.0 - 2.0 * t) * t
            }
        }
        "in-cubic" => t * t * t,
        "out-cubic" => {
            let f = t - 1.0;
            f * f * f + 1.0
        }
        "in-out-cubic" => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                let f = 2.0 * t - 2.0;
                0.5 * f * f * f + 1.0
            }
        }
        "elastic-out" => {
            if t == 0.0 || t == 1.0 {
                t
            } else {
                let p = 0.3;
                let s = p / 4.0;
                2.0_f64.powf(-10.0 * t) * ((t - s) * (2.0 * PI) / p).sin() + 1.0
            }
        }
        "bounce-out" => bounce_out(t),
        // "linear" and unknown curves
        _ => t,
    }
}

fn bounce_out(t: f64) -> f64 {
    let n1 = 7.5625;
    let d1 = 2.75;
    if t < 1.0 / d1 {
        n1 * t * t
    } else if t < 2.0 / d1 {
        let t2 = t - 1.5 / d1;
        n1 * t2 * t2 + 0.75
    } else if t < 2.5 / d1 {
        let t2 = t - 2.25 / d1;
        n1 * t2 * t2 + 0.9375
    } else {
        let t2 = t - 2.625 / d1;
        n1 * t2 * t2 + 0.984375
    }
}

pub struct EaseApplyOp;
impl Operator for EaseApplyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ease/apply", >= 2);
        let curve = eval_expr(&args[0], ctx)?;
        let curve = curve.text()?;
        let t = require_number!(eval_expr(&args[1], ctx)?).clamp(0.0, 1.0);
        Ok(Value::Number(ease_curve(curve, t)))
    }
}

pub struct EaseSmoothstepOp;
impl Operator for EaseSmoothstepOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "ease/smoothstep", >= 3);
        let edge0 = require_number!(eval_expr(&args[0], ctx)?);
        let edge1 = require_number!(eval_expr(&args[1], ctx)?);
        let x = require_number!(eval_expr(&args[2], ctx)?);
        if edge0 == edge1 {
            return Ok(Value::Number(0.0));
        }
        let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
        Ok(Value::Number(t * t * (3.0 - 2.0 * t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(form: &str) -> EvalResult {
        let expr: SExpression = serde_json::from_str(form).expect("s-expression");
        eval_expr(&expr, &BindingContext::default())
    }

    fn error_text(form: &str) -> String {
        format!("{:?}", eval(form).expect_err("expected a typed error"))
    }

    #[test]
    fn curve_must_be_text() {
        assert_eq!(
            eval(r#"["ease/apply", "in-quad", 0.5]"#).unwrap(),
            Value::Number(0.25)
        );
        assert_eq!(
            eval(r#"["ease/apply", "unknown", 0.5]"#).unwrap(),
            Value::Number(0.5)
        );
        assert!(error_text(r#"["ease/apply", null, 0.5]"#).contains("string"));
        assert!(error_text(r#"["ease/apply", 3, 0.5]"#).contains("string"));
    }
}
