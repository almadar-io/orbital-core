//! The theme's motion shapes and element entry, resolved once for the native shells (the twin of
//! [`super::canvas_theme`]): the web reads `--motion-*-transform` / `--motion-enter-*` straight
//! from CSS, while iOS/Android animate the numbers this resolves into, so no shell parses a CSS
//! transform itself. A token outside the declared transform vocabulary keeps the base shape and
//! is logged, never guessed.

use crate::schema::{css_time_ms, EnterAnimation, MotionShapeTokens, ThemeTokens};
use serde::Serialize;

/// One surface's from/to transform: uniform `scale` about the element's centre, then a translate
/// in points. `inline_signed` flips `translate_x` under right-to-left layout
/// (`var(--motion-enter-slide-sign)`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionShape {
    pub scale: f64,
    pub translate_x: f64,
    pub translate_y: f64,
    pub inline_signed: bool,
}

impl MotionShape {
    pub const IDENTITY: MotionShape = MotionShape {
        scale: 1.0,
        translate_x: 0.0,
        translate_y: 0.0,
        inline_signed: false,
    };
}

/// Every surface's shape (`MotionShapeTokens` keys).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionShapes {
    pub modal_enter: MotionShape,
    pub modal_exit: MotionShape,
    pub popover_enter: MotionShape,
    pub popover_exit: MotionShape,
    pub toast_enter: MotionShape,
    pub toast_exit: MotionShape,
    pub page_enter: MotionShape,
    pub enter_rise: MotionShape,
    pub enter_scale: MotionShape,
    pub enter_slide: MotionShape,
}

/// The resolved motion a native shell animates (kernel snapshot `motion`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedMotion {
    /// The entry slot content plays when it changes (`--motion-enter-default`).
    pub entry_default: EnterAnimation,
    /// One `enterDelay` step (`--motion-enter-stagger`), in milliseconds.
    pub stagger_ms: u64,
    pub shapes: MotionShapes,
}

/// `@almadar/ui` `themes/_base.css` — what a theme declaring no motion token plays.
pub mod base {
    use super::MotionShape;
    use crate::schema::EnterAnimation;

    const fn shape(
        scale: f64,
        translate_x: f64,
        translate_y: f64,
        inline_signed: bool,
    ) -> MotionShape {
        MotionShape {
            scale,
            translate_x,
            translate_y,
            inline_signed,
        }
    }

    pub const MODAL_ENTER: MotionShape = shape(0.96, 0.0, 0.96 * 8.0, false);
    pub const MODAL_EXIT: MotionShape = shape(0.92, 0.0, 0.92 * 16.0, false);
    pub const POPOVER_ENTER: MotionShape = shape(0.95, 0.0, 0.0, false);
    pub const POPOVER_EXIT: MotionShape = shape(0.95, 0.0, 0.0, false);
    pub const TOAST_ENTER: MotionShape = shape(1.0, 0.0, 16.0, false);
    pub const TOAST_EXIT: MotionShape = shape(1.0, 0.0, 16.0, false);
    pub const PAGE_ENTER: MotionShape = shape(1.0, 0.0, 8.0, false);
    pub const ENTER_RISE: MotionShape = shape(1.0, 0.0, 12.0, false);
    pub const ENTER_SCALE: MotionShape = shape(0.96, 0.0, 0.0, false);
    pub const ENTER_SLIDE: MotionShape = shape(1.0, 16.0, 0.0, true);
    pub const ENTRY_DEFAULT: EnterAnimation = EnterAnimation::Fade;
    pub const STAGGER_MS: u64 = 40;
}

/// Why a transform token was refused.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformError(pub String);

/// Parse a CSS transform list into one [`MotionShape`], composing left to right as CSS does
/// (`scale(0.96) translateY(8px)` moves the scaled box 0.96 × 8). Accepted: `none`,
/// `translateX/Y(<len>)`, `translate(<len>[, <len>])`, `scale(<n>[, <n>])` with equal factors,
/// and `translateZ(<len>)` (no depth on a 2D surface). `<len>` is `0`, `<n>px`, or
/// `calc(<n>px * var(--motion-enter-slide-sign))` on the x axis.
pub fn parse_transform(raw: &str) -> Result<MotionShape, TransformError> {
    let text = raw.trim();
    let mut shape = MotionShape::IDENTITY;
    if text == "none" {
        return Ok(shape);
    }
    let mut rest = text;
    while !rest.is_empty() {
        let open = rest
            .find('(')
            .ok_or_else(|| TransformError(format!("`{rest}` is not a transform function")))?;
        let name = rest[..open].trim();
        let close = matching_paren(rest, open)
            .ok_or_else(|| TransformError(format!("unbalanced parentheses in `{text}`")))?;
        let args: Vec<&str> = split_args(&rest[open + 1..close]);
        match (name, args.as_slice()) {
            ("translateX", [x]) => translate(&mut shape, length(x, true)?, (0.0, false))?,
            ("translateY", [y]) => translate(&mut shape, (0.0, false), length(y, false)?)?,
            ("translate", [x]) => translate(&mut shape, length(x, true)?, (0.0, false))?,
            ("translate", [x, y]) => translate(&mut shape, length(x, true)?, length(y, false)?)?,
            ("translateZ", [z]) => {
                length(z, false)?;
            }
            ("scale", [k]) => shape.scale *= number(k)?,
            ("scale", [kx, ky]) => {
                let (kx, ky) = (number(kx)?, number(ky)?);
                if kx != ky {
                    return Err(TransformError(format!("non-uniform `scale({kx}, {ky})`")));
                }
                shape.scale *= kx;
            }
            _ => {
                return Err(TransformError(format!(
                    "`{name}({})` is not a motion transform",
                    args.join(", ")
                )))
            }
        }
        rest = rest[close + 1..].trim_start();
    }
    Ok(shape)
}

fn translate(
    shape: &mut MotionShape,
    x: (f64, bool),
    y: (f64, bool),
) -> Result<(), TransformError> {
    if x.1 && shape.translate_x != 0.0 && !shape.inline_signed {
        return Err(TransformError(
            "a signed and an unsigned x translate in one transform".into(),
        ));
    }
    if x.1 {
        shape.inline_signed = true;
    } else if x.0 != 0.0 && shape.inline_signed {
        return Err(TransformError(
            "a signed and an unsigned x translate in one transform".into(),
        ));
    }
    shape.translate_x += shape.scale * x.0;
    shape.translate_y += shape.scale * y.0;
    Ok(())
}

/// A length in points, and whether it carries the inline (RTL) sign.
fn length(raw: &str, x_axis: bool) -> Result<(f64, bool), TransformError> {
    let raw = raw.trim();
    if raw == "0" {
        return Ok((0.0, false));
    }
    if let Some(px) = raw.strip_suffix("px") {
        return Ok((number(px)?, false));
    }
    if let Some(inner) = raw.strip_prefix("calc(").and_then(|r| r.strip_suffix(')')) {
        if let Some((len, sign)) = inner.split_once('*') {
            if x_axis && sign.trim() == "var(--motion-enter-slide-sign)" {
                let (n, _) = length(len, false)?;
                return Ok((n, true));
            }
        }
    }
    Err(TransformError(format!("`{raw}` is not a px length")))
}

fn number(raw: &str) -> Result<f64, TransformError> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(|| TransformError(format!("`{}` is not a number", raw.trim())))
}

fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices().skip_while(|(i, _)| *i < open) {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_args(inner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(inner[start..].trim());
    out
}

fn shape_or_base(name: &str, token: Option<&String>, fallback: MotionShape) -> MotionShape {
    let Some(raw) = token else {
        return fallback;
    };
    parse_transform(raw).unwrap_or_else(|TransformError(why)| {
        tracing::warn!(target: "almadar::theme", shape = %name, token = %raw, "motion shape kept at base: {why}");
        fallback
    })
}

/// Resolve `tokens`' motion axis against the base (the kernel snapshot's `motion`).
pub fn resolve_motion(tokens: &ThemeTokens) -> ResolvedMotion {
    let motion = tokens.motion.as_ref();
    let empty = MotionShapeTokens::default();
    let s = motion.and_then(|m| m.shapes.as_ref()).unwrap_or(&empty);
    let entry = motion.and_then(|m| m.entry.as_ref());
    let stagger_ms = match entry.and_then(|e| e.stagger.as_deref()) {
        None => base::STAGGER_MS,
        Some(raw) => css_time_ms(raw).unwrap_or_else(|| {
            tracing::warn!(target: "almadar::theme", token = %raw, "motion stagger kept at base: not a CSS time");
            base::STAGGER_MS
        }),
    };
    ResolvedMotion {
        entry_default: entry.and_then(|e| e.default).unwrap_or(base::ENTRY_DEFAULT),
        stagger_ms,
        shapes: MotionShapes {
            modal_enter: shape_or_base("modalEnter", s.modal_enter.as_ref(), base::MODAL_ENTER),
            modal_exit: shape_or_base("modalExit", s.modal_exit.as_ref(), base::MODAL_EXIT),
            popover_enter: shape_or_base(
                "popoverEnter",
                s.popover_enter.as_ref(),
                base::POPOVER_ENTER,
            ),
            popover_exit: shape_or_base("popoverExit", s.popover_exit.as_ref(), base::POPOVER_EXIT),
            toast_enter: shape_or_base("toastEnter", s.toast_enter.as_ref(), base::TOAST_ENTER),
            toast_exit: shape_or_base("toastExit", s.toast_exit.as_ref(), base::TOAST_EXIT),
            page_enter: shape_or_base("pageEnter", s.page_enter.as_ref(), base::PAGE_ENTER),
            enter_rise: shape_or_base("enterRise", s.enter_rise.as_ref(), base::ENTER_RISE),
            enter_scale: shape_or_base("enterScale", s.enter_scale.as_ref(), base::ENTER_SCALE),
            enter_slide: shape_or_base("enterSlide", s.enter_slide.as_ref(), base::ENTER_SLIDE),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn base_css() -> serde_json::Map<String, Value> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/motion/base-css.json"
        );
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        fixture["vars"].as_object().unwrap().clone()
    }

    fn close(a: MotionShape, b: MotionShape) -> bool {
        (a.scale - b.scale).abs() < 1e-9
            && (a.translate_x - b.translate_x).abs() < 1e-9
            && (a.translate_y - b.translate_y).abs() < 1e-9
            && a.inline_signed == b.inline_signed
    }

    #[test]
    fn the_base_shapes_are_base_css_parsed() {
        let css = base_css();
        let pairs = [
            ("--motion-modal-enter-from-transform", base::MODAL_ENTER),
            ("--motion-modal-exit-to-transform", base::MODAL_EXIT),
            ("--motion-popover-enter-from-transform", base::POPOVER_ENTER),
            ("--motion-popover-exit-to-transform", base::POPOVER_EXIT),
            ("--motion-toast-enter-from-transform", base::TOAST_ENTER),
            ("--motion-toast-exit-to-transform", base::TOAST_EXIT),
            ("--motion-page-enter-from-transform", base::PAGE_ENTER),
            ("--motion-enter-rise-from-transform", base::ENTER_RISE),
            ("--motion-enter-scale-from-transform", base::ENTER_SCALE),
            ("--motion-enter-slide-from-transform", base::ENTER_SLIDE),
        ];
        for (var, want) in pairs {
            let got = parse_transform(css[var].as_str().unwrap()).unwrap();
            assert!(close(got, want), "{var}: base.css {got:?} vs base {want:?}");
        }
        assert_eq!(css["--motion-enter-default"], "almadar-enter-fade");
        assert_eq!(base::ENTRY_DEFAULT, EnterAnimation::Fade);
        assert_eq!(
            css_time_ms(css["--motion-enter-stagger"].as_str().unwrap()),
            Some(base::STAGGER_MS)
        );
    }

    #[test]
    fn transforms_compose_left_to_right_like_css() {
        let scaled_then_moved = parse_transform("scale(0.5) translateY(10px)").unwrap();
        assert_eq!(scaled_then_moved.translate_y, 5.0);
        let moved_then_scaled = parse_transform("translateY(10px) scale(0.5)").unwrap();
        assert_eq!(
            moved_then_scaled.translate_y, 10.0,
            "control: a translate before the scale is not scaled"
        );
        assert_eq!(moved_then_scaled.scale, 0.5);
        assert!(close(
            parse_transform("translateZ(0)").unwrap(),
            MotionShape::IDENTITY
        ));
        assert!(close(
            parse_transform("none").unwrap(),
            MotionShape::IDENTITY
        ));
        let both = parse_transform("translate(4px, -2px) scale(1.1, 1.1)").unwrap();
        assert_eq!(
            (both.translate_x, both.translate_y, both.scale),
            (4.0, -2.0, 1.1)
        );
    }

    #[test]
    fn the_slide_sign_marks_the_x_translate_inline() {
        let slide =
            parse_transform("translateX(calc(24px * var(--motion-enter-slide-sign)))").unwrap();
        assert_eq!((slide.translate_x, slide.inline_signed), (24.0, true));
        assert!(
            !parse_transform("translateX(24px)").unwrap().inline_signed,
            "control: a plain px is unsigned"
        );
        assert!(
            parse_transform("translateY(calc(8px * var(--motion-enter-slide-sign)))").is_err(),
            "the sign is an x-axis var"
        );
    }

    #[test]
    fn a_transform_outside_the_vocabulary_is_refused() {
        for bad in [
            "rotate(10deg)",
            "translateX(10%)",
            "scale(0.9, 1)",
            "skew(4deg)",
            "translateY(8em)",
            "scale(",
            "matrix(1,0,0,1,0,0)",
        ] {
            assert!(parse_transform(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_theme_overrides_shapes_and_entry_and_a_bad_token_keeps_the_base() {
        let tokens: ThemeTokens = serde_json::from_value(serde_json::json!({
            "motion": {
                "shapes": { "modalEnter": "none", "toastEnter": "rotate(3deg)", "enterSlide": "translateX(calc(32px * var(--motion-enter-slide-sign)))" },
                "entry": { "default": "rise", "stagger": "0.06s" }
            }
        }))
        .unwrap();
        let resolved = resolve_motion(&tokens);
        assert!(close(resolved.shapes.modal_enter, MotionShape::IDENTITY));
        assert!(
            close(resolved.shapes.toast_enter, base::TOAST_ENTER),
            "the refused token keeps the base"
        );
        assert_eq!(resolved.shapes.enter_slide.translate_x, 32.0);
        assert!(
            close(resolved.shapes.page_enter, base::PAGE_ENTER),
            "control: an undeclared shape is the base"
        );
        assert_eq!(resolved.entry_default, EnterAnimation::Rise);
        assert_eq!(resolved.stagger_ms, 60);
    }

    #[test]
    fn a_theme_with_no_motion_resolves_to_the_base() {
        let tokens: ThemeTokens = serde_json::from_value(serde_json::json!({})).unwrap();
        let resolved = resolve_motion(&tokens);
        assert_eq!(resolved.entry_default, base::ENTRY_DEFAULT);
        assert_eq!(resolved.stagger_ms, base::STAGGER_MS);
        assert!(close(resolved.shapes.enter_rise, base::ENTER_RISE));
        let wire = serde_json::to_value(&resolved).unwrap();
        assert_eq!(wire["entryDefault"], "fade");
        assert_eq!(wire["shapes"]["enterSlide"]["inlineSigned"], true);
    }
}
