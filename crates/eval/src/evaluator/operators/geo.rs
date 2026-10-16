//! G6 Game: geometry / collision operators (`geo/*`).
//!
//! Rect = `{x,y,w,h}` (top-left origin), Circle = `{x,y,r}`, Point = `{x,y}`,
//! Segment = `{x1,y1,x2,y2}`. Overlaps/containment return `Value::Bool`. All
//! ops pure and total.

use super::macros::*;
use super::vector::{eval_obj, has_z, make_vec, vget};
use super::*;

pub struct GeoAabbOverlapOp;
impl Operator for GeoAabbOverlapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "geo/aabb-overlap", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let overlap = vget(&a, "x") < vget(&b, "x") + vget(&b, "w")
            && vget(&a, "x") + vget(&a, "w") > vget(&b, "x")
            && vget(&a, "y") < vget(&b, "y") + vget(&b, "h")
            && vget(&a, "y") + vget(&a, "h") > vget(&b, "y");
        Ok(Value::Bool(overlap))
    }
}

pub struct GeoCircleOverlapOp;
impl Operator for GeoCircleOverlapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "geo/circle-overlap", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let dx = vget(&a, "x") - vget(&b, "x");
        let dy = vget(&a, "y") - vget(&b, "y");
        let dist = (dx * dx + dy * dy).sqrt();
        Ok(Value::Bool(dist <= vget(&a, "r") + vget(&b, "r")))
    }
}

pub struct GeoRectCircleOverlapOp;
impl Operator for GeoRectCircleOverlapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "geo/rect-circle-overlap", >= 2);
        let r = eval_obj(&args[0], ctx)?;
        let c = eval_obj(&args[1], ctx)?;
        let cx = vget(&c, "x").clamp(vget(&r, "x"), vget(&r, "x") + vget(&r, "w"));
        let cy = vget(&c, "y").clamp(vget(&r, "y"), vget(&r, "y") + vget(&r, "h"));
        let dx = vget(&c, "x") - cx;
        let dy = vget(&c, "y") - cy;
        let cr = vget(&c, "r");
        Ok(Value::Bool(dx * dx + dy * dy <= cr * cr))
    }
}

pub struct GeoPointInRectOp;
impl Operator for GeoPointInRectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "geo/point-in-rect", >= 2);
        let p = eval_obj(&args[0], ctx)?;
        let r = eval_obj(&args[1], ctx)?;
        let inside = vget(&p, "x") >= vget(&r, "x")
            && vget(&p, "x") <= vget(&r, "x") + vget(&r, "w")
            && vget(&p, "y") >= vget(&r, "y")
            && vget(&p, "y") <= vget(&r, "y") + vget(&r, "h");
        Ok(Value::Bool(inside))
    }
}

pub struct GeoPointInCircleOp;
impl Operator for GeoPointInCircleOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "geo/point-in-circle", >= 2);
        let p = eval_obj(&args[0], ctx)?;
        let c = eval_obj(&args[1], ctx)?;
        let dx = vget(&p, "x") - vget(&c, "x");
        let dy = vget(&p, "y") - vget(&c, "y");
        let cr = vget(&c, "r");
        Ok(Value::Bool(dx * dx + dy * dy <= cr * cr))
    }
}

pub struct GeoReflectOp;
impl Operator for GeoReflectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "geo/reflect", >= 2);
        let v = eval_obj(&args[0], ctx)?;
        let n = eval_obj(&args[1], ctx)?;
        let with_z = has_z(&v);
        let dot = vget(&v, "x") * vget(&n, "x")
            + vget(&v, "y") * vget(&n, "y")
            + vget(&v, "z") * vget(&n, "z");
        Ok(make_vec(
            vget(&v, "x") - 2.0 * dot * vget(&n, "x"),
            vget(&v, "y") - 2.0 * dot * vget(&n, "y"),
            vget(&v, "z") - 2.0 * dot * vget(&n, "z"),
            with_z,
        ))
    }
}

pub struct GeoSegmentIntersectOp;
impl Operator for GeoSegmentIntersectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "geo/segment-intersect", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let (x1, y1, x2, y2) = (
            vget(&a, "x1"),
            vget(&a, "y1"),
            vget(&a, "x2"),
            vget(&a, "y2"),
        );
        let (x3, y3, x4, y4) = (
            vget(&b, "x1"),
            vget(&b, "y1"),
            vget(&b, "x2"),
            vget(&b, "y2"),
        );
        let denom = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4);
        if denom == 0.0 {
            return Ok(Value::Null);
        }
        let t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / denom;
        let u = ((x1 - x3) * (y1 - y2) - (y1 - y3) * (x1 - x2)) / denom;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            let px = x1 + t * (x2 - x1);
            let py = y1 + t * (y2 - y1);
            Ok(make_vec(px, py, 0.0, false))
        } else {
            Ok(Value::Null)
        }
    }
}
