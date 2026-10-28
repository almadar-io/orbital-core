//! G6 Game: geometry / collision operators (`geo/*`).
//!
//! Rect = `{x,y,w,h}` (top-left origin), Circle = `{x,y,r}`, Point = `{x,y}`,
//! Segment = `{x1,y1,x2,y2}`. Overlaps/containment return `Value::Bool`. All
//! ops pure; operands are read strictly (see `vector::read_fields`).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::vector::{eval_fields, eval_vector, make_vec, CIRCLE, POINT, RECT, SEGMENT};
use super::*;

pub struct GeoAabbOverlapOp;
impl Operator for GeoAabbOverlapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "geo/aabb-overlap";
        require_arity!(args, OP, >= 2);
        let [ax, ay, aw, ah] = eval_fields(OP, "a", &args[0], ctx, RECT)?;
        let [bx, by, bw, bh] = eval_fields(OP, "b", &args[1], ctx, RECT)?;
        Ok(Value::Bool(
            ax < bx + bw && ax + aw > bx && ay < by + bh && ay + ah > by,
        ))
    }
}

pub struct GeoCircleOverlapOp;
impl Operator for GeoCircleOverlapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "geo/circle-overlap";
        require_arity!(args, OP, >= 2);
        let [ax, ay, ar] = eval_fields(OP, "a", &args[0], ctx, CIRCLE)?;
        let [bx, by, br] = eval_fields(OP, "b", &args[1], ctx, CIRCLE)?;
        let dx = ax - bx;
        let dy = ay - by;
        Ok(Value::Bool((dx * dx + dy * dy).sqrt() <= ar + br))
    }
}

pub struct GeoRectCircleOverlapOp;
impl Operator for GeoRectCircleOverlapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "geo/rect-circle-overlap";
        require_arity!(args, OP, >= 2);
        let [rx, ry, rw, rh] = eval_fields(OP, "rect", &args[0], ctx, RECT)?;
        let [cx, cy, cr] = eval_fields(OP, "circle", &args[1], ctx, CIRCLE)?;
        let nx = cx.clamp(rx, rx + rw);
        let ny = cy.clamp(ry, ry + rh);
        let dx = cx - nx;
        let dy = cy - ny;
        Ok(Value::Bool(dx * dx + dy * dy <= cr * cr))
    }
}

pub struct GeoPointInRectOp;
impl Operator for GeoPointInRectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "geo/point-in-rect";
        require_arity!(args, OP, >= 2);
        let [px, py] = eval_fields(OP, "p", &args[0], ctx, POINT)?;
        let [rx, ry, rw, rh] = eval_fields(OP, "rect", &args[1], ctx, RECT)?;
        Ok(Value::Bool(
            px >= rx && px <= rx + rw && py >= ry && py <= ry + rh,
        ))
    }
}

pub struct GeoPointInCircleOp;
impl Operator for GeoPointInCircleOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "geo/point-in-circle";
        require_arity!(args, OP, >= 2);
        let [px, py] = eval_fields(OP, "p", &args[0], ctx, POINT)?;
        let [cx, cy, cr] = eval_fields(OP, "circle", &args[1], ctx, CIRCLE)?;
        let dx = px - cx;
        let dy = py - cy;
        Ok(Value::Bool(dx * dx + dy * dy <= cr * cr))
    }
}

pub struct GeoReflectOp;
impl Operator for GeoReflectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "geo/reflect";
        require_arity!(args, OP, >= 2);
        let v = eval_vector(OP, "v", &args[0], ctx)?;
        let n = eval_vector(OP, "n", &args[1], ctx)?;
        let dot = v.x * n.x + v.y * n.y + v.z * n.z;
        Ok(make_vec(
            v.x - 2.0 * dot * n.x,
            v.y - 2.0 * dot * n.y,
            v.z - 2.0 * dot * n.z,
            v.has_z,
        ))
    }
}

pub struct GeoSegmentIntersectOp;
impl Operator for GeoSegmentIntersectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "geo/segment-intersect";
        require_arity!(args, OP, >= 2);
        let [x1, y1, x2, y2] = eval_fields(OP, "a", &args[0], ctx, SEGMENT)?;
        let [x3, y3, x4, y4] = eval_fields(OP, "b", &args[1], ctx, SEGMENT)?;
        let denom = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4);
        if denom == 0.0 {
            return Ok(Value::Null);
        }
        let t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / denom;
        let u = ((x1 - x3) * (y1 - y2) - (y1 - y3) * (x1 - x2)) / denom;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            Ok(make_vec(x1 + t * (x2 - x1), y1 + t * (y2 - y1), 0.0, false))
        } else {
            Ok(Value::Null)
        }
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
    fn operands_read_strictly() {
        assert_eq!(
            eval(r#"["geo/point-in-rect", {"x":5,"y":5}, {"x":0,"y":0,"w":10,"h":10}]"#).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"["geo/segment-intersect", {"x1":0,"y1":0,"x2":4,"y2":4}, {"x1":0,"y1":4,"x2":4,"y2":0}]"#)
                .unwrap(),
            make_vec(2.0, 2.0, 0.0, false)
        );
        assert!(
            error_text(r#"["geo/point-in-rect", null, {"x":0,"y":0,"w":1,"h":1}]"#)
                .contains("argument `p`")
        );
        assert!(error_text(
            r#"["geo/aabb-overlap", {"x":0,"y":0,"w":1}, {"x":0,"y":0,"w":1,"h":1}]"#
        )
        .contains("missing field `h`"));
        assert!(error_text(
            r#"["geo/circle-overlap", {"x":0,"y":0,"r":"1"}, {"x":0,"y":0,"r":1}]"#
        )
        .contains("field `r`"));
        assert!(
            error_text(r#"["geo/segment-intersect", {"x1":0,"y1":0,"x2":1,"y2":1}, {}]"#)
                .contains("argument `b`")
        );
    }
}
