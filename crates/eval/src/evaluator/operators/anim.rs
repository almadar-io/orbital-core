//! G6 Game: animation operators (`anim/*`). Pure, total.

use orbital_core_base::value::ValueMap;
use super::macros::*;
use super::*;

pub struct AnimFrameAtOp;
impl Operator for AnimFrameAtOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "anim/frame-at", >= 3);
        let elapsed = require_number!(eval_expr(&args[0], ctx)?);
        let frame_dur = require_number!(eval_expr(&args[1], ctx)?);
        let frame_count = require_number!(eval_expr(&args[2], ctx)?);
        let looped = args
            .get(3)
            .map(|a| eval_expr(a, ctx))
            .transpose()?
            .map(|v| v.is_truthy())
            .unwrap_or(true);
        if frame_dur == 0.0 || frame_count <= 0.0 {
            return Ok(Value::Number(0.0));
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
        let (sx, sy) = if cols == 0.0 {
            (0.0, 0.0)
        } else {
            (
                (frame_index % cols) * frame_w,
                (frame_index / cols).floor() * frame_h,
            )
        };
        let mut m = ValueMap::new();
        m.insert("sx".to_string(), Value::Number(sx));
        m.insert("sy".to_string(), Value::Number(sy));
        m.insert("sw".to_string(), Value::Number(frame_w));
        m.insert("sh".to_string(), Value::Number(frame_h));
        Ok(Value::Object(m))
    }
}

pub struct AnimDirectionFromDeltaOp;
impl Operator for AnimDirectionFromDeltaOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "anim/direction-from-delta", >= 2);
        let dx = require_number!(eval_expr(&args[0], ctx)?);
        let dy = require_number!(eval_expr(&args[1], ctx)?);
        let ways = args
            .get(2)
            .map(|a| eval_expr(a, ctx))
            .transpose()?
            .and_then(|v| v.to_number())
            .unwrap_or(4.0);
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
        Ok(Value::String(dir.to_string()))
    }
}
