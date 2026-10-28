//! G6 Game (Wave 1b): coherent-noise operators (`noise/*`).
//!
//! Perlin-derived value-coherent noise. The permutation table is copied
//! verbatim from the frozen spec so Rust and JS produce byte-identical output.
//! All ops are pure and return a number in `[-1, 1]`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

/// Frozen permutation table (256 entries) — must stay byte-identical to the JS impl.
const PERM: [i64; 256] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69,
    142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148, 247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219,
    203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175,
    74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230,
    220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76,
    132, 187, 208, 89, 18, 169, 200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173,
    186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212, 207, 206,
    59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163,
    70, 221, 153, 101, 155, 167, 43, 172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232,
    178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162,
    241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204,
    176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93, 222, 114, 67, 29, 24, 72, 243, 141,
    128, 195, 78, 66, 215, 61, 156, 180,
];

#[inline]
fn p(idx: i64) -> i64 {
    PERM[(idx & 255) as usize]
}

#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + t * (b - a)
}

#[inline]
fn grad2(hash: i64, x: f64, y: f64) -> f64 {
    let h = hash & 7;
    let u = if h < 4 { x } else { y };
    let v = if h < 4 { y } else { x };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}

/// 2D Perlin noise (the shared kernel reused by simplex + fbm).
fn perlin(x: f64, y: f64, seed: f64) -> f64 {
    let s = (seed.floor() as i64) & 255;
    let xi = ((x.floor() as i64) + s) & 255;
    let yi = ((y.floor() as i64) + s) & 255;
    let xf = x - x.floor();
    let yf = y - y.floor();
    let u = fade(xf);
    let v = fade(yf);
    let aa = p(p(xi) + yi);
    let ab = p(p(xi) + yi + 1);
    let ba = p(p(xi + 1) + yi);
    let bb = p(p(xi + 1) + yi + 1);
    let x1 = lerp(grad2(aa, xf, yf), grad2(ba, xf - 1.0, yf), u);
    let x2 = lerp(grad2(ab, xf, yf - 1.0), grad2(bb, xf - 1.0, yf - 1.0), u);
    lerp(x1, x2, v)
}

fn arg_number(args: &[SExpression], i: usize, ctx: &BindingContext) -> EvalResult2 {
    Ok(eval_expr(&args[i], ctx)?.number()?)
}

fn seed_arg(args: &[SExpression], i: usize, ctx: &BindingContext) -> EvalResult2 {
    match args.get(i) {
        Some(_) => arg_number(args, i, ctx),
        None => Ok(0.0),
    }
}

type EvalResult2 = Result<f64, OrbitalError>;

pub struct NoisePerlinOp;
impl Operator for NoisePerlinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "noise/perlin", >= 2);
        let x = arg_number(args, 0, ctx)?;
        let y = arg_number(args, 1, ctx)?;
        let seed = seed_arg(args, 2, ctx)?;
        Ok(Value::Number(perlin(x, y, seed)))
    }
}

pub struct NoiseSimplexOp;
impl Operator for NoiseSimplexOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "noise/simplex", >= 2);
        let x = arg_number(args, 0, ctx)?;
        let y = arg_number(args, 1, ctx)?;
        let seed = seed_arg(args, 2, ctx)?;
        let v = (perlin(x, y, seed)
            + perlin(x * 2.0 + 5.2, y * 2.0 + 1.3, seed) * 0.5
            + perlin(x * 4.0 + 9.1, y * 4.0 + 7.7, seed) * 0.25)
            / 1.75;
        Ok(Value::Number(v))
    }
}

pub struct NoiseFbmOp;
impl Operator for NoiseFbmOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "noise/fbm", >= 3);
        let x = arg_number(args, 0, ctx)?;
        let y = arg_number(args, 1, ctx)?;
        let octaves_raw = arg_number(args, 2, ctx)?;
        let seed = seed_arg(args, 3, ctx)?;
        let octaves = (octaves_raw.floor() as i64).clamp(1, 8);
        let mut total = 0.0;
        let mut freq = 1.0;
        let mut amp = 1.0;
        let mut max = 0.0;
        for _ in 0..octaves {
            total += perlin(x * freq, y * freq, seed) * amp;
            max += amp;
            freq *= 2.0;
            amp *= 0.5;
        }
        Ok(Value::Number(total / max))
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
    fn declared_arity_and_strict_numbers() {
        let v = eval(r#"["noise/perlin", 1.5, 2.5]"#)
            .unwrap()
            .number()
            .unwrap();
        assert_eq!(
            Value::Number(v),
            eval(r#"["noise/perlin", 1.5, 2.5, 0]"#).unwrap()
        );
        assert!(matches!(
            eval(r#"["noise/perlin", 1.5]"#),
            Err(OrbitalError::Eval(EvalError::ArityMismatch { .. }))
        ));
        assert!(eval(r#"["noise/fbm", 1.5, 2.5, 4]"#).is_ok());
        assert!(error_text(r#"["noise/perlin", null, 2.5]"#).contains("number"));
        assert!(error_text(r#"["noise/simplex", 1, 2, null]"#).contains("number"));
        assert!(error_text(r#"["noise/fbm", 1, 2, null]"#).contains("number"));
    }
}
