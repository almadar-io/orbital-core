//! G6 Game: vector operators (`vec/*`).
//!
//! Vectors are `Value::Object` `{x,y}` or `{x,y,z}`. Result dimensionality
//! follows the FIRST vector arg: include `z` only if that arg had a `z` key.
//! Missing `z` reads as 0. Every vector/point/cell/rect/circle/segment operand
//! is read through `read_fields`/`read_vector`: a non-object, a missing field
//! or a non-number field is a typed error naming the operator argument.
//! Degenerate math (e.g. normalize of a zero-length vector) still answers the
//! zero vector, as the operator table declares.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::value::ValueMap;

pub const POINT: [&str; 2] = ["x", "y"];
pub const RECT: [&str; 4] = ["x", "y", "w", "h"];
pub const CIRCLE: [&str; 3] = ["x", "y", "r"];
pub const SEGMENT: [&str; 4] = ["x1", "y1", "x2", "y2"];

/// A decoded vector operand; `z` reads 0 when the object declares none.
#[derive(Clone, Copy)]
pub struct Vector {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub has_z: bool,
}

fn field_number(op: &str, param: &str, field: &str, v: &Value) -> Result<f64, EvalError> {
    v.as_number().ok_or_else(|| EvalError::TypeMismatch {
        expected: format!("number for field `{field}` of {op} argument `{param}`"),
        actual: v.type_name().to_string(),
    })
}

fn operand_object<'a>(
    op: &str,
    param: &str,
    v: &'a Value,
    fields: &[&str],
) -> Result<&'a ValueMap, EvalError> {
    v.as_object().ok_or_else(|| EvalError::TypeMismatch {
        expected: format!("object with number fields {fields:?} ({op} argument `{param}`)"),
        actual: v.type_name().to_string(),
    })
}

/// Read the declared numeric fields of an operand object, in the order given.
pub fn read_fields<const N: usize>(
    op: &str,
    param: &str,
    v: &Value,
    fields: [&str; N],
) -> Result<[f64; N], EvalError> {
    let obj = operand_object(op, param, v, &fields)?;
    let mut out = [0.0; N];
    for (slot, field) in out.iter_mut().zip(fields) {
        let fv = obj.get(field).ok_or_else(|| {
            EvalError::InvalidArgument(format!(
                "{op}: argument `{param}` is missing field `{field}`"
            ))
        })?;
        *slot = field_number(op, param, field, fv)?;
    }
    Ok(out)
}

/// Read a `{x,y}` or `{x,y,z}` operand.
pub fn read_vector(op: &str, param: &str, v: &Value) -> Result<Vector, EvalError> {
    let [x, y] = read_fields(op, param, v, POINT)?;
    let z = operand_object(op, param, v, &POINT)?.get("z");
    Ok(match z {
        Some(zv) => Vector {
            x,
            y,
            z: field_number(op, param, "z", zv)?,
            has_z: true,
        },
        None => Vector {
            x,
            y,
            z: 0.0,
            has_z: false,
        },
    })
}

/// Evaluate an argument and read it as a vector.
pub fn eval_vector(
    op: &str,
    param: &str,
    arg: &SExpression,
    ctx: &BindingContext,
) -> Result<Vector, OrbitalError> {
    Ok(read_vector(op, param, &eval_expr(arg, ctx)?)?)
}

/// Evaluate an argument and read the declared numeric fields of its object.
pub fn eval_fields<const N: usize>(
    op: &str,
    param: &str,
    arg: &SExpression,
    ctx: &BindingContext,
    fields: [&str; N],
) -> Result<[f64; N], OrbitalError> {
    Ok(read_fields(op, param, &eval_expr(arg, ctx)?, fields)?)
}

/// A grid coordinate or extent must be a whole finite number.
pub fn whole(op: &str, what: &str, n: f64) -> Result<i64, EvalError> {
    if n.is_finite() && n.fract() == 0.0 {
        Ok(n as i64)
    } else {
        Err(EvalError::InvalidArgument(format!(
            "{op}: {what} {n} is not a whole number"
        )))
    }
}

/// Build a vector object; includes `z` only when `with_z` is true.
pub fn make_vec(x: f64, y: f64, z: f64, with_z: bool) -> Value {
    let mut m = ValueMap::default();
    m.insert("x".into(), Value::Number(x));
    m.insert("y".into(), Value::Number(y));
    if with_z {
        m.insert("z".into(), Value::Number(z));
    }
    Value::object(m)
}

fn dot3(a: &Vector, b: &Vector) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

pub struct VecAddOp;
impl Operator for VecAddOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/add", >= 2);
        let a = eval_vector("vec/add", "a", &args[0], ctx)?;
        let b = eval_vector("vec/add", "b", &args[1], ctx)?;
        let with_z = a.has_z;
        Ok(make_vec(a.x + b.x, a.y + b.y, a.z + b.z, with_z))
    }
}

pub struct VecSubOp;
impl Operator for VecSubOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/sub", >= 2);
        let a = eval_vector("vec/sub", "a", &args[0], ctx)?;
        let b = eval_vector("vec/sub", "b", &args[1], ctx)?;
        let with_z = a.has_z;
        Ok(make_vec(a.x - b.x, a.y - b.y, a.z - b.z, with_z))
    }
}

pub struct VecScaleOp;
impl Operator for VecScaleOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/scale", >= 2);
        let a = eval_vector("vec/scale", "a", &args[0], ctx)?;
        let k = require_number!(eval_expr(&args[1], ctx)?);
        let with_z = a.has_z;
        Ok(make_vec(a.x * k, a.y * k, a.z * k, with_z))
    }
}

pub struct VecDotOp;
impl Operator for VecDotOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/dot", >= 2);
        let a = eval_vector("vec/dot", "a", &args[0], ctx)?;
        let b = eval_vector("vec/dot", "b", &args[1], ctx)?;
        Ok(Value::Number(dot3(&a, &b)))
    }
}

pub struct VecCrossOp;
impl Operator for VecCrossOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/cross", >= 2);
        let a = eval_vector("vec/cross", "a", &args[0], ctx)?;
        let b = eval_vector("vec/cross", "b", &args[1], ctx)?;
        if a.has_z || b.has_z {
            let ax = a.x;
            let ay = a.y;
            let az = a.z;
            let bx = b.x;
            let by = b.y;
            let bz = b.z;
            Ok(make_vec(
                ay * bz - az * by,
                az * bx - ax * bz,
                ax * by - ay * bx,
                true,
            ))
        } else {
            Ok(Value::Number(a.x * b.y - a.y * b.x))
        }
    }
}

pub struct VecLengthOp;
impl Operator for VecLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/length", 1);
        let a = eval_vector("vec/length", "a", &args[0], ctx)?;
        Ok(Value::Number(dot3(&a, &a).sqrt()))
    }
}

pub struct VecLengthSqOp;
impl Operator for VecLengthSqOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/length-sq", 1);
        let a = eval_vector("vec/length-sq", "a", &args[0], ctx)?;
        Ok(Value::Number(dot3(&a, &a)))
    }
}

pub struct VecNormalizeOp;
impl Operator for VecNormalizeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/normalize", 1);
        let a = eval_vector("vec/normalize", "a", &args[0], ctx)?;
        let with_z = a.has_z;
        let len = dot3(&a, &a).sqrt();
        if len == 0.0 {
            return Ok(make_vec(0.0, 0.0, 0.0, with_z));
        }
        Ok(make_vec(a.x / len, a.y / len, a.z / len, with_z))
    }
}

pub struct VecDistanceOp;
impl Operator for VecDistanceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/distance", >= 2);
        let a = eval_vector("vec/distance", "a", &args[0], ctx)?;
        let b = eval_vector("vec/distance", "b", &args[1], ctx)?;
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        let dz = a.z - b.z;
        Ok(Value::Number((dx * dx + dy * dy + dz * dz).sqrt()))
    }
}

pub struct VecDistanceSqOp;
impl Operator for VecDistanceSqOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/distance-sq", >= 2);
        let a = eval_vector("vec/distance-sq", "a", &args[0], ctx)?;
        let b = eval_vector("vec/distance-sq", "b", &args[1], ctx)?;
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        let dz = a.z - b.z;
        Ok(Value::Number(dx * dx + dy * dy + dz * dz))
    }
}

pub struct VecLerpOp;
impl Operator for VecLerpOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/lerp", >= 3);
        let a = eval_vector("vec/lerp", "a", &args[0], ctx)?;
        let b = eval_vector("vec/lerp", "b", &args[1], ctx)?;
        let t = require_number!(eval_expr(&args[2], ctx)?);
        let with_z = a.has_z;
        let lerp = |x: f64, y: f64| x + (y - x) * t;
        Ok(make_vec(
            lerp(a.x, b.x),
            lerp(a.y, b.y),
            lerp(a.z, b.z),
            with_z,
        ))
    }
}

pub struct VecAngleOp;
impl Operator for VecAngleOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/angle", 1);
        let a = eval_vector("vec/angle", "a", &args[0], ctx)?;
        if args.len() == 1 {
            return Ok(Value::Number(a.y.atan2(a.x)));
        }
        let b = eval_vector("vec/angle", "b", &args[1], ctx)?;
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
        let a = eval_vector("vec/rotate", "a", &args[0], ctx)?;
        let rad = require_number!(eval_expr(&args[1], ctx)?);
        let with_z = a.has_z;
        let x = a.x;
        let y = a.y;
        let (s, c) = rad.sin_cos();
        Ok(make_vec(x * c - y * s, x * s + y * c, a.z, with_z))
    }
}

pub struct VecClampLengthOp;
impl Operator for VecClampLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "vec/clamp-length", >= 2);
        let a = eval_vector("vec/clamp-length", "a", &args[0], ctx)?;
        let max = require_number!(eval_expr(&args[1], ctx)?);
        let with_z = a.has_z;
        let len = dot3(&a, &a).sqrt();
        if len > max && len != 0.0 {
            let k = max / len;
            Ok(make_vec(a.x * k, a.y * k, a.z * k, with_z))
        } else {
            Ok(make_vec(a.x, a.y, a.z, with_z))
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
    fn vectors_read_strictly() {
        assert_eq!(
            eval(r#"["vec/add", {"x":1,"y":2}, {"x":3,"y":4}]"#).unwrap(),
            make_vec(4.0, 6.0, 0.0, false)
        );
        assert_eq!(
            eval(r#"["vec/dot", {"x":1,"y":2,"z":3}, {"x":1,"y":1,"z":1}]"#).unwrap(),
            Value::Number(6.0)
        );
        assert!(error_text(r#"["vec/add", null, {"x":3,"y":4}]"#).contains("argument `a`"));
        assert!(error_text(r#"["vec/add", {"x":1,"y":2}, 5]"#).contains("argument `b`"));
        assert!(error_text(r#"["vec/length", {"x":1}]"#).contains("missing field `y`"));
        assert!(error_text(r#"["vec/length", {"x":1,"y":"2"}]"#).contains("field `y`"));
        assert!(error_text(r#"["vec/length", {"x":1,"y":2,"z":null}]"#).contains("field `z`"));
        assert!(error_text(r#"["vec/angle", {"x":1,"y":2}, null]"#).contains("argument `b`"));
    }

    #[test]
    fn whole_numbers_only() {
        assert_eq!(whole("op", "w", 3.0).unwrap(), 3);
        assert!(whole("op", "w", 2.5).is_err());
        assert!(whole("op", "w", f64::NAN).is_err());
    }
}
