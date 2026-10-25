//! G6 Game: grid / tilemap operators (`grid/*`).
//!
//! Cells are `{x,y}` integers. Cell arrays are returned in the exact
//! deterministic order specified (parity depends on order). All ops pure.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::vector::{eval_obj, make_vec, vget};
use super::*;

fn cell(x: f64, y: f64) -> Value {
    make_vec(x, y, 0.0, false)
}

pub struct GridToWorldOp;
impl Operator for GridToWorldOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/to-world", >= 2);
        let c = eval_obj(&args[0], ctx)?;
        let size = require_number!(eval_expr(&args[1], ctx)?);
        Ok(make_vec(
            (vget(&c, "x") + 0.5) * size,
            (vget(&c, "y") + 0.5) * size,
            0.0,
            false,
        ))
    }
}

pub struct GridFromWorldOp;
impl Operator for GridFromWorldOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/from-world", >= 2);
        let p = eval_obj(&args[0], ctx)?;
        let size = require_number!(eval_expr(&args[1], ctx)?);
        Ok(cell(
            (vget(&p, "x") / size).floor(),
            (vget(&p, "y") / size).floor(),
        ))
    }
}

pub struct GridIsoToScreenOp;
impl Operator for GridIsoToScreenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/iso-to-screen", >= 3);
        let c = eval_obj(&args[0], ctx)?;
        let tile_w = require_number!(eval_expr(&args[1], ctx)?);
        let tile_h = require_number!(eval_expr(&args[2], ctx)?);
        Ok(make_vec(
            (vget(&c, "x") - vget(&c, "y")) * tile_w / 2.0,
            (vget(&c, "x") + vget(&c, "y")) * tile_h / 2.0,
            0.0,
            false,
        ))
    }
}

pub struct GridScreenToIsoOp;
impl Operator for GridScreenToIsoOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/screen-to-iso", >= 3);
        let p = eval_obj(&args[0], ctx)?;
        let tile_w = require_number!(eval_expr(&args[1], ctx)?);
        let tile_h = require_number!(eval_expr(&args[2], ctx)?);
        let hw = tile_w / 2.0;
        let hh = tile_h / 2.0;
        let px = vget(&p, "x");
        let py = vget(&p, "y");
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
        require_arity!(args, "grid/distance", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let dx = (vget(&a, "x") - vget(&b, "x")).abs();
        let dy = (vget(&a, "y") - vget(&b, "y")).abs();
        Ok(Value::Number(dx.max(dy)))
    }
}

pub struct GridManhattanDistanceOp;
impl Operator for GridManhattanDistanceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/manhattan-distance", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let dx = (vget(&a, "x") - vget(&b, "x")).abs();
        let dy = (vget(&a, "y") - vget(&b, "y")).abs();
        Ok(Value::Number(dx + dy))
    }
}

pub struct GridNeighborsOp;
impl Operator for GridNeighborsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/neighbors", 1);
        let c = eval_obj(&args[0], ctx)?;
        let cx = vget(&c, "x");
        let cy = vget(&c, "y");
        let diagonal = args
            .get(1)
            .map(|a| eval_expr(a, ctx))
            .transpose()?
            .map(|v| v.is_truthy())
            .unwrap_or(false);
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
        Ok(Value::Array(out))
    }
}

pub struct GridCellsInRadiusOp;
impl Operator for GridCellsInRadiusOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/cells-in-radius", >= 2);
        let c = eval_obj(&args[0], ctx)?;
        let r = require_number!(eval_expr(&args[1], ctx)?);
        let cx = vget(&c, "x");
        let cy = vget(&c, "y");
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
        Ok(Value::Array(out))
    }
}

pub struct GridLineOp;
impl Operator for GridLineOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/line", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let mut x0 = vget(&a, "x") as i64;
        let mut y0 = vget(&a, "y") as i64;
        let x1 = vget(&b, "x") as i64;
        let y1 = vget(&b, "y") as i64;
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
        Ok(Value::Array(out))
    }
}

pub struct GridInBoundsOp;
impl Operator for GridInBoundsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "grid/in-bounds", >= 3);
        let c = eval_obj(&args[0], ctx)?;
        let w = require_number!(eval_expr(&args[1], ctx)?);
        let h = require_number!(eval_expr(&args[2], ctx)?);
        let x = vget(&c, "x");
        let y = vget(&c, "y");
        Ok(Value::Bool(x >= 0.0 && x < w && y >= 0.0 && y < h))
    }
}
