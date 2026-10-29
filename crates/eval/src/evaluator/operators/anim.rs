//! G6 Game: animation operators (`anim/*`). Pure, total.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::value::ValueMap;

pub struct AnimFrameAtOp;
impl Operator for AnimFrameAtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "anim/frame-at", >= 3);
        let elapsed = require_number!(eval_expr(&args[0], ctx)?);
        let frame_dur = require_number!(eval_expr(&args[1], ctx)?);
        let frame_count = require_number!(eval_expr(&args[2], ctx)?);
        let looped = match args.get(3) {
            Some(a) => eval_expr(a, ctx)?.boolean()?,
            None => true,
        };
        if frame_dur == 0.0 {
            return Err(EvalError::InvalidArgument(
                "anim/frame-at: frameDur must not be 0".to_string(),
            )
            .into());
        }
        if frame_count <= 0.0 {
            return Err(EvalError::InvalidArgument(format!(
                "anim/frame-at: frameCount {frame_count} must be positive"
            ))
            .into());
        }
        let i = (elapsed / frame_dur).floor();
        let result = if looped {
            let m = i % frame_count;
            if m < 0.0 {
                m + frame_count
            } else {
                m
            }
        } else {
            i.min(frame_count - 1.0)
        };
        Ok(Value::Number(result))
    }
}

pub struct AnimSheetRectOp;
impl Operator for AnimSheetRectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "anim/sheet-rect", >= 4);
        let frame_index = require_number!(eval_expr(&args[0], ctx)?);
        let cols = require_number!(eval_expr(&args[1], ctx)?);
        let frame_w = require_number!(eval_expr(&args[2], ctx)?);
        let frame_h = require_number!(eval_expr(&args[3], ctx)?);
        if cols <= 0.0 {
            return Err(EvalError::InvalidArgument(format!(
                "anim/sheet-rect: cols {cols} must be positive"
            ))
            .into());
        }
        let (sx, sy) = (
            (frame_index % cols) * frame_w,
            (frame_index / cols).floor() * frame_h,
        );
        let mut m = ValueMap::default();
        m.insert("sx".into(), Value::Number(sx));
        m.insert("sy".into(), Value::Number(sy));
        m.insert("sw".into(), Value::Number(frame_w));
        m.insert("sh".into(), Value::Number(frame_h));
        Ok(Value::object(m))
    }
}

pub struct AnimDirectionFromDeltaOp;
impl Operator for AnimDirectionFromDeltaOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "anim/direction-from-delta", >= 2);
        let dx = require_number!(eval_expr(&args[0], ctx)?);
        let dy = require_number!(eval_expr(&args[1], ctx)?);
        let ways = match args.get(2) {
            Some(a) => eval_expr(a, ctx)?.number()?,
            None => 4.0,
        };
        if ways != 4.0 && ways != 8.0 {
            return Err(EvalError::InvalidArgument(format!(
                "anim/direction-from-delta: ways {ways}: expected 4 or 8"
            ))
            .into());
        }
        // Screen coords: +y is down. atan2(dy,dx) in (-PI, PI].
        let angle = dy.atan2(dx);
        let two_pi = std::f64::consts::PI * 2.0;
        let dir = if ways == 8.0 {
            // 8 buckets of 45°, centered on each direction starting at "right" (0 rad).
            let sector = ((angle / (two_pi / 8.0)).round().rem_euclid(8.0)) as i64;
            match sector {
                0 => "right",
                1 => "down-right",
                2 => "down",
                3 => "down-left",
                4 => "left",
                5 => "up-left",
                6 => "up",
                7 => "up-right",
                _ => "right",
            }
        } else {
            // 4 buckets of 90°, centered on right/down/left/up.
            let sector = ((angle / (two_pi / 4.0)).round().rem_euclid(4.0)) as i64;
            match sector {
                0 => "right",
                1 => "down",
                2 => "left",
                3 => "up",
                _ => "right",
            }
        };
        Ok(Value::string(dir.to_string()))
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
    fn zero_divisors_are_invalid_arguments() {
        assert_eq!(
            eval(r#"["anim/frame-at", 0.35, 0.1, 3]"#).unwrap(),
            Value::Number(0.0)
        );
        assert!(error_text(r#"["anim/frame-at", 1, 0, 3]"#).contains("frameDur"));
        assert!(error_text(r#"["anim/frame-at", 1, 0.1, 0]"#).contains("frameCount"));
        assert!(error_text(r#"["anim/sheet-rect", 5, 0, 16, 16]"#).contains("cols"));
        let rect = eval(r#"["anim/sheet-rect", 5, 4, 16, 16]"#).unwrap();
        assert_eq!(
            rect.as_object().unwrap().get("sx"),
            Some(&Value::Number(16.0))
        );
    }
}
