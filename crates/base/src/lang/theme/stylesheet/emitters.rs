//! A theme's skin axes as stylesheet lines, written from the one runtime var table (`css_axes`), so the served
//! stylesheet and the runtime var map cannot say different things.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::types::{
    DensityTokens, DiagramTokens, ElevationTokens, GeometryTokens, IconographyTokens, MotionTokens,
    SceneTokens, SurfaceTokens, TypeScaleTokens,
};
use crate::theme::{css_axes, CssVars};

/// The skin axes one stylesheet block writes.
#[derive(Clone, Copy, Default)]
pub(crate) struct SkinAxes<'a> {
    pub density: Option<&'a DensityTokens>,
    pub type_scale: Option<&'a TypeScaleTokens>,
    pub motion: Option<&'a MotionTokens>,
    pub iconography: Option<&'a IconographyTokens>,
    pub elevation: Option<&'a ElevationTokens>,
    pub geometry: Option<&'a GeometryTokens>,
    pub surface: Option<&'a SurfaceTokens>,
    pub diagram: Option<&'a DiagramTokens>,
    pub scene: Option<&'a SceneTokens>,
}

/// The axes' CSS custom properties, one `name: value;` line each at `indent`.
pub(crate) fn emit_axes(axes: SkinAxes<'_>, indent: &str) -> String {
    let mut vars = CssVars::default();
    css_axes::density(axes.density, &mut vars);
    css_axes::type_scale(axes.type_scale, &mut vars);
    css_axes::motion(axes.motion, &mut vars);
    css_axes::iconography(axes.iconography, &mut vars);
    css_axes::elevation(axes.elevation, &mut vars);
    css_axes::geometry(axes.geometry, &mut vars);
    css_axes::surface(axes.surface, &mut vars);
    css_axes::diagram(axes.diagram, &mut vars);
    css_axes::scene(axes.scene, &mut vars);
    vars.iter()
        .map(|(name, value)| format!("{indent}{name}: {value};\n"))
        .collect()
}

#[cfg(test)]
mod motion_tests {
    use super::{emit_axes, SkinAxes};
    use crate::json_value::from_json_value;
    use crate::schema::MotionTokens;

    fn motion_css(tokens: &MotionTokens) -> String {
        emit_axes(
            SkinAxes {
                motion: Some(tokens),
                ..SkinAxes::default()
            },
            "",
        )
    }

    /// The compiled app emits every motion var `@almadar/ui` `themeTokens.ts` `emitMotion` does.
    #[test]
    fn motion_emits_entry_shapes_busy_delay_and_awaiting_skeleton_like_the_runtime() {
        let tokens: MotionTokens = from_json_value(&(serde_json::json!({
            "shapes": { "modalEnter": "scale(0.9)", "enterRise": "translateY(20px)", "enterScale": "scale(0.9)", "enterSlide": "translateX(8px)" },
            "entry": { "default": "rise", "stagger": "60ms" },
            "busyDelay": "450ms",
            "awaitingSkeleton": true
        })))
        .unwrap();
        let css = motion_css(&tokens);
        for line in [
            "--motion-modal-enter-from-transform: scale(0.9);",
            "--motion-enter-rise-from-transform: translateY(20px);",
            "--motion-enter-scale-from-transform: scale(0.9);",
            "--motion-enter-slide-from-transform: translateX(8px);",
            "--motion-enter-default: almadar-enter-rise;",
            "--motion-enter-stagger: 60ms;",
            "--motion-busy-delay: 450ms;",
            "--awaiting-skeleton-display: block;",
        ] {
            assert!(css.contains(line), "missing `{line}` in:\n{css}");
        }
        let none: MotionTokens = from_json_value(
            &(serde_json::json!({ "entry": { "default": "none" }, "awaitingSkeleton": false })),
        )
        .unwrap();
        let css = motion_css(&none);
        assert!(
            css.contains("--motion-enter-default: none;"),
            "`none` stays `none`"
        );
        assert!(css.contains("--awaiting-skeleton-display: none;"));
        assert!(
            !css.contains("--motion-busy-delay"),
            "control: an undeclared token emits nothing"
        );
    }
}
