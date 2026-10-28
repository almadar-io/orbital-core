//! G6 Game: grid / tilemap operators (`grid/*`).
//!
//! Cells are `{x,y}` integers. Cell arrays are returned in the exact
//! deterministic order specified (parity depends on order). All ops pure.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::vector::{eval_fields, make_vec, whole, POINT};
use super::*;

fn cell(x: f64, y: f64) -> Value {
    make_vec(x, y, 0.0, false)
}

pub struct GridToWorldOp;
impl Operator for GridToWorldOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/to-world";
        require_arity!(args, OP, >= 2);
        let [c_x, c_y] = eval_fields(OP, "cell", &args[0], ctx, POINT)?;
        let size = require_number!(eval_expr(&args[1], ctx)?);
        Ok(make_vec((c_x + 0.5) * size, (c_y + 0.5) * size, 0.0, false))
    }
}

pub struct GridFromWorldOp;
impl Operator for GridFromWorldOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/from-world";
        require_arity!(args, OP, >= 2);
        let [p_x, p_y] = eval_fields(OP, "point", &args[0], ctx, POINT)?;
        let size = require_number!(eval_expr(&args[1], ctx)?);
        if size == 0.0 {
            return Err(EvalError::InvalidArgument(format!("{OP}: cellSize must not be 0")).into());
        }
        Ok(cell((p_x / size).floor(), (p_y / size).floor()))
    }
}

pub struct GridIsoToScreenOp;
impl Operator for GridIsoToScreenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/iso-to-screen";
        require_arity!(args, OP, >= 3);
        let [c_x, c_y] = eval_fields(OP, "cell", &args[0], ctx, POINT)?;
        let tile_w = require_number!(eval_expr(&args[1], ctx)?);
        let tile_h = require_number!(eval_expr(&args[2], ctx)?);
        Ok(make_vec(
            (c_x - c_y) * tile_w / 2.0,
            (c_x + c_y) * tile_h / 2.0,
            0.0,
            false,
        ))
    }
}

pub struct GridScreenToIsoOp;
impl Operator for GridScreenToIsoOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/screen-to-iso";
        require_arity!(args, OP, >= 3);
        let [px, py] = eval_fields(OP, "point", &args[0], ctx, POINT)?;
        let tile_w = require_number!(eval_expr(&args[1], ctx)?);
        let tile_h = require_number!(eval_expr(&args[2], ctx)?);
        let hw = tile_w / 2.0;
        let hh = tile_h / 2.0;
        if hw == 0.0 || hh == 0.0 {
            return Err(
                EvalError::InvalidArgument(format!("{OP}: tileW and tileH must not be 0")).into(),
            );
        }
        Ok(make_vec(
            (px / hw + py / hh) / 2.0,
            (py / hh - px / hw) / 2.0,
            0.0,
            false,
        ))
    }
}

pub struct GridDistanceOp;
impl Operator for GridDistanceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/distance";
        require_arity!(args, OP, >= 2);
        let [a_x, a_y] = eval_fields(OP, "a", &args[0], ctx, POINT)?;
        let [b_x, b_y] = eval_fields(OP, "b", &args[1], ctx, POINT)?;
        let dx = (a_x - b_x).abs();
        let dy = (a_y - b_y).abs();
        Ok(Value::Number(dx.max(dy)))
    }
}

pub struct GridManhattanDistanceOp;
impl Operator for GridManhattanDistanceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/manhattan-distance";
        require_arity!(args, OP, >= 2);
        let [a_x, a_y] = eval_fields(OP, "a", &args[0], ctx, POINT)?;
        let [b_x, b_y] = eval_fields(OP, "b", &args[1], ctx, POINT)?;
        let dx = (a_x - b_x).abs();
        let dy = (a_y - b_y).abs();
        Ok(Value::Number(dx + dy))
    }
}

pub struct GridNeighborsOp;
impl Operator for GridNeighborsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/neighbors";
        require_arity!(args, OP, 1);
        let [cx, cy] = eval_fields(OP, "cell", &args[0], ctx, POINT)?;
        let diagonal = match args.get(1) {
            Some(a) => eval_expr(a, ctx)?.boolean()?,
            None => false,
        };
        // N, E, S, W
        let mut out = vec![
            cell(cx, cy - 1.0),
            cell(cx + 1.0, cy),
            cell(cx, cy + 1.0),
            cell(cx - 1.0, cy),
        ];
        if diagonal {
            // NE, SE, SW, NW
            out.push(cell(cx + 1.0, cy - 1.0));
            out.push(cell(cx + 1.0, cy + 1.0));
            out.push(cell(cx - 1.0, cy + 1.0));
            out.push(cell(cx - 1.0, cy - 1.0));
        }
        Ok(Value::array(out))
    }
}

pub struct GridCellsInRadiusOp;
impl Operator for GridCellsInRadiusOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/cells-in-radius";
        require_arity!(args, OP, >= 2);
        let [cx, cy] = eval_fields(OP, "cell", &args[0], ctx, POINT)?;
        let r = require_number!(eval_expr(&args[1], ctx)?);
        let rf = r.floor();
        let ri = rf as i64;
        let r2 = r * r;
        let mut out = Vec::new();
        let mut dy = -ri;
        while dy <= ri {
            let mut dx = -ri;
            while dx <= ri {
                let fdx = dx as f64;
                let fdy = dy as f64;
                if fdx * fdx + fdy * fdy <= r2 {
                    out.push(cell(cx + fdx, cy + fdy));
                }
                dx += 1;
            }
            dy += 1;
        }
        Ok(Value::array(out))
    }
}

pub struct GridLineOp;
impl Operator for GridLineOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/line";
        require_arity!(args, OP, >= 2);
        let [a_x, a_y] = eval_fields(OP, "a", &args[0], ctx, POINT)?;
        let [b_x, b_y] = eval_fields(OP, "b", &args[1], ctx, POINT)?;
        let mut x0 = whole(OP, "cell a.x", a_x)?;
        let mut y0 = whole(OP, "cell a.y", a_y)?;
        let x1 = whole(OP, "cell b.x", b_x)?;
        let y1 = whole(OP, "cell b.y", b_y)?;
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut out = Vec::new();
        loop {
            out.push(cell(x0 as f64, y0 as f64));
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
        Ok(Value::array(out))
    }
}

pub struct GridInBoundsOp;
impl Operator for GridInBoundsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        const OP: &str = "grid/in-bounds";
        require_arity!(args, OP, >= 3);
        let [x, y] = eval_fields(OP, "cell", &args[0], ctx, POINT)?;
        let w = require_number!(eval_expr(&args[1], ctx)?);
        let h = require_number!(eval_expr(&args[2], ctx)?);
        Ok(Value::Bool(x >= 0.0 && x < w && y >= 0.0 && y < h))
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
    fn cells_read_strictly_and_sizes_are_nonzero() {
        assert_eq!(
            eval(r#"["grid/from-world", {"x":80,"y":112}, 32]"#).unwrap(),
            cell(2.0, 3.0)
        );
        assert!(error_text(r#"["grid/from-world", {"x":80,"y":112}, 0]"#).contains("cellSize"));
        assert!(error_text(r#"["grid/screen-to-iso", {"x":1,"y":1}, 0, 32]"#).contains("tileW"));
        assert!(error_text(r#"["grid/distance", null, {"x":0,"y":0}]"#).contains("argument `a`"));
        assert!(error_text(r#"["grid/neighbors", {"x":0}]"#).contains("missing field `y`"));
        assert!(error_text(r#"["grid/neighbors", {"x":0,"y":0}, null]"#).contains("boolean"));
        assert!(error_text(r#"["grid/line", {"x":0.5,"y":0}, {"x":2,"y":2}]"#).contains("whole"));
    }
}
