//! `color/*`: WCAG 2.2 relative luminance and contrast ratio of CSS colours. Pure; the colour reader is
//! `orbital_core_base::color`, shared with the canvas.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::color::parse_css_color;

fn read_color(op: &str, value: &Value) -> Result<[f32; 4], OrbitalError> {
    let text = value.text()?;
    parse_css_color(text).ok_or_else(|| {
        EvalError::InvalidArgument(format!("{op}: {text:?} is not a CSS colour")).into()
    })
}

fn relative_luminance(color: [f32; 4]) -> f64 {
    let linear = |channel: f32| {
        let c = f64::from(channel).clamp(0.0, 1.0);
        if c <= 0.04045 {
            c / 12.92
        } else {
            libm::pow((c + 0.055) / 1.055, 2.4)
        }
    };
    0.2126 * linear(color[0]) + 0.7152 * linear(color[1]) + 0.0722 * linear(color[2])
}

/// Reads `top` and an opaque `backdrop`, returning `top` painted over it (opaque).
fn read_over(
    op: &str,
    top: &Value,
    backdrop: &Value,
) -> Result<([f32; 4], [f32; 4]), OrbitalError> {
    let top = read_color(op, top)?;
    let backdrop = read_color(op, backdrop)?;
    if backdrop[3] < 1.0 {
        return Err(
            EvalError::InvalidArgument(format!("{op}: the backdrop must be opaque")).into(),
        );
    }
    let top = if top[3] < 1.0 {
        composite_over(top, backdrop)
    } else {
        top
    };
    Ok((top, backdrop))
}

/// `top` painted over `backdrop`, per channel in sRGB space as browsers composite.
fn composite_over(top: [f32; 4], backdrop: [f32; 4]) -> [f32; 4] {
    let alpha = top[3].clamp(0.0, 1.0);
    let mix = |t: f32, b: f32| t * alpha + b * (1.0 - alpha);
    [
        mix(top[0], backdrop[0]),
        mix(top[1], backdrop[1]),
        mix(top[2], backdrop[2]),
        1.0,
    ]
}

/// color/luminance - WCAG 2.2 relative luminance, 0 (black) to 1 (white)
pub struct ColorLuminanceOp;
impl StrictOperator for ColorLuminanceOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "color/luminance", 1);
        Ok(Value::Number(relative_luminance(read_color(
            "color/luminance",
            args[0],
        )?)))
    }
}

/// color/contrast - WCAG 2.2 contrast ratio; a translucent first colour is composited over the second
pub struct ColorContrastOp;
impl StrictOperator for ColorContrastOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "color/contrast", >= 2);
        let (top, backdrop) = read_over("color/contrast", args[0], args[1])?;
        let (a, b) = (relative_luminance(top), relative_luminance(backdrop));
        let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
        Ok(Value::Number((lighter + 0.05) / (darker + 0.05)))
    }
}

/// color/composite - a CSS colour painted over an opaque backdrop, as `#rrggbb` (channels rounded half up)
pub struct ColorCompositeOp;
impl StrictOperator for ColorCompositeOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "color/composite", == 2);
        let (top, _) = read_over("color/composite", args[0], args[1])?;
        let byte = |channel: f32| (channel.clamp(0.0, 1.0) * 255.0).round() as u8;
        Ok(Value::String(
            format!(
                "#{:02x}{:02x}{:02x}",
                byte(top[0]),
                byte(top[1]),
                byte(top[2])
            )
            .into(),
        ))
    }
}
