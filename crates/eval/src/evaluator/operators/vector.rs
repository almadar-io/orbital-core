//! G6 Game: vector operators (`vec/*`).
//!
//! Vectors are `Value::Object` `{x,y}` or `{x,y,z}`. Result dimensionality
//! follows the FIRST vector arg: include `z` only if that arg had a `z` key.
//! Missing `z` reads as 0. All ops are pure and total (degenerate input never
//! errors — e.g. normalize of a zero-length vector returns the zero vector).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::value::ValueMap;

/// Read a vector field, defaulting to 0 when absent.
pub fn vget(obj: &ValueMap, key: &str) -> f64 {
    obj.get(key).and_then(|v| v.as_number()).unwrap_or(0.0)
}

/// Whether a vector object declares a `z` component (drives 2D vs 3D output).
pub fn has_z(obj: &ValueMap) -> bool {
    obj.contains_key("z")
}

/// Build a vector object; includes `z` only when `with_z` is true.
pub fn make_vec(x: f64, y: f64, z: f64, with_z: bool) -> Value {
    let mut m = ValueMap::default();
    m.insert("x".to_string(), Value::Number(x));
    m.insert("y".to_string(), Value::Number(y));
    if with_z {
        m.insert("z".to_string(), Value::Number(z));
    }
    Value::object(m)
}

/// Evaluate an argument and require it to be an object (returns empty map when not).
pub fn eval_obj(arg: &SExpression, ctx: &BindingContext) -> Result<ValueMap, OrbitalError> {
    let v = eval_expr(arg, ctx)?;
    match v {
        Value::Object(o) => Ok(o.into_inner()),
        _ => Ok(ValueMap::default()),
    }
}

fn dot3(a: &ValueMap, b: &ValueMap) -> f64 {
    vget(a, "x") * vget(b, "x") + vget(a, "y") * vget(b, "y") + vget(a, "z") * vget(b, "z")
}

pub struct VecAddOp;
impl Operator for VecAddOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/add", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let with_z = has_z(&a);
        Ok(make_vec(
            vget(&a, "x") + vget(&b, "x"),
            vget(&a, "y") + vget(&b, "y"),
            vget(&a, "z") + vget(&b, "z"),
            with_z,
        ))
    }
}

pub struct VecSubOp;
impl Operator for VecSubOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/sub", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let with_z = has_z(&a);
        Ok(make_vec(
            vget(&a, "x") - vget(&b, "x"),
            vget(&a, "y") - vget(&b, "y"),
            vget(&a, "z") - vget(&b, "z"),
            with_z,
        ))
    }
}

pub struct VecScaleOp;
impl Operator for VecScaleOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/scale", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let k = require_number!(eval_expr(&args[1], ctx)?);
        let with_z = has_z(&a);
        Ok(make_vec(
            vget(&a, "x") * k,
            vget(&a, "y") * k,
            vget(&a, "z") * k,
            with_z,
        ))
    }
}

pub struct VecDotOp;
impl Operator for VecDotOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/dot", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        Ok(Value::Number(dot3(&a, &b)))
    }
}

pub struct VecCrossOp;
impl Operator for VecCrossOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/cross", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        if has_z(&a) || has_z(&b) {
            let ax = vget(&a, "x");
            let ay = vget(&a, "y");
            let az = vget(&a, "z");
            let bx = vget(&b, "x");
            let by = vget(&b, "y");
            let bz = vget(&b, "z");
            Ok(make_vec(
                ay * bz - az * by,
                az * bx - ax * bz,
                ax * by - ay * bx,
                true,
            ))
        } else {
            Ok(Value::Number(
                vget(&a, "x") * vget(&b, "y") - vget(&a, "y") * vget(&b, "x"),
            ))
        }
    }
}

pub struct VecLengthOp;
impl Operator for VecLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/length", 1);
        let a = eval_obj(&args[0], ctx)?;
        Ok(Value::Number(dot3(&a, &a).sqrt()))
    }
}

pub struct VecLengthSqOp;
impl Operator for VecLengthSqOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/length-sq", 1);
        let a = eval_obj(&args[0], ctx)?;
        Ok(Value::Number(dot3(&a, &a)))
    }
}

pub struct VecNormalizeOp;
impl Operator for VecNormalizeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/normalize", 1);
        let a = eval_obj(&args[0], ctx)?;
        let with_z = has_z(&a);
        let len = dot3(&a, &a).sqrt();
        if len == 0.0 {
            return Ok(make_vec(0.0, 0.0, 0.0, with_z));
        }
        Ok(make_vec(
            vget(&a, "x") / len,
            vget(&a, "y") / len,
            vget(&a, "z") / len,
            with_z,
        ))
    }
}

pub struct VecDistanceOp;
impl Operator for VecDistanceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/distance", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let dx = vget(&a, "x") - vget(&b, "x");
        let dy = vget(&a, "y") - vget(&b, "y");
        let dz = vget(&a, "z") - vget(&b, "z");
        Ok(Value::Number((dx * dx + dy * dy + dz * dz).sqrt()))
    }
}

pub struct VecDistanceSqOp;
impl Operator for VecDistanceSqOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/distance-sq", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let dx = vget(&a, "x") - vget(&b, "x");
        let dy = vget(&a, "y") - vget(&b, "y");
        let dz = vget(&a, "z") - vget(&b, "z");
        Ok(Value::Number(dx * dx + dy * dy + dz * dz))
    }
}

pub struct VecLerpOp;
impl Operator for VecLerpOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/lerp", >= 3);
        let a = eval_obj(&args[0], ctx)?;
        let b = eval_obj(&args[1], ctx)?;
        let t = require_number!(eval_expr(&args[2], ctx)?);
        let with_z = has_z(&a);
        let lerp = |x: f64, y: f64| x + (y - x) * t;
        Ok(make_vec(
            lerp(vget(&a, "x"), vget(&b, "x")),
            lerp(vget(&a, "y"), vget(&b, "y")),
            lerp(vget(&a, "z"), vget(&b, "z")),
            with_z,
        ))
    }
}

pub struct VecAngleOp;
impl Operator for VecAngleOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/angle", 1);
        let a = eval_obj(&args[0], ctx)?;
        if args.len() == 1 {
            return Ok(Value::Number(vget(&a, "y").atan2(vget(&a, "x"))));
        }
        let b = eval_obj(&args[1], ctx)?;
        let la = dot3(&a, &a).sqrt();
        let lb = dot3(&b, &b).sqrt();
        if la == 0.0 || lb == 0.0 {
            return Ok(Value::Number(0.0));
        }
        let cos = (dot3(&a, &b) / (la * lb)).clamp(-1.0, 1.0);
        Ok(Value::Number(cos.acos()))
    }
}

pub struct VecRotateOp;
impl Operator for VecRotateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/rotate", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let rad = require_number!(eval_expr(&args[1], ctx)?);
        let with_z = has_z(&a);
        let x = vget(&a, "x");
        let y = vget(&a, "y");
        let (s, c) = rad.sin_cos();
        Ok(make_vec(
            x * c - y * s,
            x * s + y * c,
            vget(&a, "z"),
            with_z,
        ))
    }
}

pub struct VecClampLengthOp;
impl Operator for VecClampLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/clamp-length", >= 2);
        let a = eval_obj(&args[0], ctx)?;
        let max = require_number!(eval_expr(&args[1], ctx)?);
        let with_z = has_z(&a);
        let len = dot3(&a, &a).sqrt();
        if len > max && len != 0.0 {
            let k = max / len;
            Ok(make_vec(
                vget(&a, "x") * k,
                vget(&a, "y") * k,
                vget(&a, "z") * k,
                with_z,
            ))
        } else {
            Ok(make_vec(
                vget(&a, "x"),
                vget(&a, "y"),
                vget(&a, "z"),
                with_z,
            ))
        }
    }
}
