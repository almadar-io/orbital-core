#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::{
    DensityTokens, DiagramTokens, ElevationTokens, EnterAnimation, GeometryTokens, IconFamily,
    IconographyTokens, MotionDurationKey, MotionEasingKey, MotionIntent, MotionTokens, SceneTokens,
    SpacingScale, SurfaceTokens, TypeIntent, TypeScale, TypeScaleEntry, TypeScaleTokens,
    TypeSizeKey, TypeWeight,
};

pub(crate) fn icon_family_str(f: &IconFamily) -> &'static str {
    match f {
        IconFamily::Lucide => "lucide",
        IconFamily::PhosphorOutline => "phosphor-outline",
        IconFamily::PhosphorFill => "phosphor-fill",
        IconFamily::PhosphorDuotone => "phosphor-duotone",
        IconFamily::Tabler => "tabler",
        IconFamily::FaSolid => "fa-solid",
    }
}

pub(crate) fn type_size_key_str(k: &TypeSizeKey) -> &'static str {
    match k {
        TypeSizeKey::Xs => "xs",
        TypeSizeKey::Sm => "sm",
        TypeSizeKey::Base => "base",
        TypeSizeKey::Lg => "lg",
        TypeSizeKey::Xl => "xl",
        TypeSizeKey::TwoXl => "2xl",
        TypeSizeKey::ThreeXl => "3xl",
        TypeSizeKey::FourXl => "4xl",
        TypeSizeKey::Display1 => "display-1",
        TypeSizeKey::Display2 => "display-2",
    }
}

pub(crate) fn type_weight_str(w: &TypeWeight) -> &'static str {
    match w {
        TypeWeight::Normal => "normal",
        TypeWeight::Medium => "medium",
        TypeWeight::Bold => "bold",
    }
}

pub(crate) fn motion_duration_key_str(k: &MotionDurationKey) -> &'static str {
    match k {
        MotionDurationKey::Instant => "instant",
        MotionDurationKey::Fast => "fast",
        MotionDurationKey::Normal => "normal",
        MotionDurationKey::Slow => "slow",
        MotionDurationKey::Dramatic => "dramatic",
    }
}

pub(crate) fn motion_easing_key_str(k: &MotionEasingKey) -> &'static str {
    match k {
        MotionEasingKey::Linear => "linear",
        MotionEasingKey::Standard => "standard",
        MotionEasingKey::Emphasized => "emphasized",
        MotionEasingKey::Spring => "spring",
    }
}

pub(crate) fn emit_spacing_scale(spacing: Option<&SpacingScale>, indent: &str) -> String {
    let mut out = String::new();
    let Some(s) = spacing else {
        return out;
    };
    let pairs: [(&str, &Option<String>); 13] = [
        ("0", &s.space0),
        ("1", &s.space1),
        ("2", &s.space2),
        ("3", &s.space3),
        ("4", &s.space4),
        ("5", &s.space5),
        ("6", &s.space6),
        ("7", &s.space7),
        ("8", &s.space8),
        ("9", &s.space9),
        ("10", &s.space10),
        ("11", &s.space11),
        ("12", &s.space12),
    ];
    for (k, v) in pairs {
        if let Some(val) = v {
            out.push_str(&format!("{}--space-{}: {};\n", indent, k, val));
        }
    }
    out
}

pub(crate) fn emit_density(density: Option<&DensityTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(d) = density else {
        return out;
    };
    out.push_str(&format!("{}/* Density */\n", indent));
    out.push_str(&emit_spacing_scale(d.spacing.as_ref(), indent));
    let pairs: [(&str, &Option<String>); 14] = [
        ("button-height-sm", &d.button_height_sm),
        ("button-height-md", &d.button_height_md),
        ("button-height-lg", &d.button_height_lg),
        ("input-height-sm", &d.input_height_sm),
        ("input-height-md", &d.input_height_md),
        ("input-height-lg", &d.input_height_lg),
        ("row-height-compact", &d.row_height_compact),
        ("row-height-normal", &d.row_height_normal),
        ("row-height-spacious", &d.row_height_spacious),
        ("card-padding-sm", &d.card_padding_sm),
        ("card-padding-md", &d.card_padding_md),
        ("card-padding-lg", &d.card_padding_lg),
        ("dialog-padding", &d.dialog_padding),
        ("section-gap", &d.section_gap),
    ];
    for (k, v) in pairs {
        if let Some(val) = v {
            out.push_str(&format!("{}--{}: {};\n", indent, k, val));
        }
    }
    out.push('\n');
    out
}

pub(crate) fn emit_type_scale_entries(scale: Option<&TypeScale>, indent: &str) -> String {
    let mut out = String::new();
    let Some(s) = scale else {
        return out;
    };
    let pairs: [(&str, &Option<TypeScaleEntry>); 10] = [
        ("xs", &s.xs),
        ("sm", &s.sm),
        ("base", &s.base),
        ("lg", &s.lg),
        ("xl", &s.xl),
        ("2xl", &s.two_xl),
        ("3xl", &s.three_xl),
        ("4xl", &s.four_xl),
        ("display-1", &s.display_1),
        ("display-2", &s.display_2),
    ];
    for (k, v) in pairs {
        if let Some(entry) = v {
            out.push_str(&format!("{}--text-{}: {};\n", indent, k, entry.size));
            out.push_str(&format!(
                "{}--leading-{}: {};\n",
                indent, k, entry.line_height
            ));
        }
    }
    out
}

pub(crate) fn emit_type_intent(name: &str, intent: &TypeIntent, indent: &str) -> String {
    let mut out = String::new();
    let size_key = type_size_key_str(&intent.size);
    let weight = type_weight_str(&intent.weight);
    out.push_str(&format!(
        "{}--intent-{}-size: var(--text-{});\n",
        indent, name, size_key
    ));
    out.push_str(&format!(
        "{}--intent-{}-weight: var(--font-weight-{});\n",
        indent, name, weight
    ));
    out.push_str(&format!(
        "{}--intent-{}-leading: var(--leading-{});\n",
        indent, name, size_key
    ));
    out
}

pub(crate) fn emit_type_scale(ts: Option<&TypeScaleTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(t) = ts else {
        return out;
    };
    out.push_str(&format!("{}/* Type */\n", indent));
    if let Some(f) = &t.display_family {
        out.push_str(&format!("{}--font-family-display: {};\n", indent, f));
    }
    if let Some(f) = &t.body_family {
        out.push_str(&format!("{}--font-family-body: {};\n", indent, f));
    }
    if let Some(f) = &t.mono_family {
        out.push_str(&format!("{}--font-family-mono: {};\n", indent, f));
    }
    if let Some(v) = &t.heading_weight {
        out.push_str(&format!("{}--heading-weight: {};\n", indent, v));
    }
    if let Some(v) = &t.heading_transform {
        out.push_str(&format!("{}--heading-transform: {};\n", indent, v));
    }
    if let Some(v) = &t.heading_tracking {
        out.push_str(&format!("{}--heading-tracking: {};\n", indent, v));
    }
    if let Some(v) = &t.heading_style {
        out.push_str(&format!("{}--heading-style: {};\n", indent, v));
    }
    if let Some(v) = &t.heading_shadow {
        out.push_str(&format!("{}--heading-shadow: {};\n", indent, v));
    }
    out.push_str(&emit_type_scale_entries(t.scale.as_ref(), indent));
    if let Some(intents) = &t.intents {
        if let Some(i) = &intents.heading_major {
            out.push_str(&emit_type_intent("heading-major", i, indent));
        }
        if let Some(i) = &intents.heading_minor {
            out.push_str(&emit_type_intent("heading-minor", i, indent));
        }
        if let Some(i) = &intents.body_emphasis {
            out.push_str(&emit_type_intent("body-emphasis", i, indent));
        }
        if let Some(i) = &intents.body_default {
            out.push_str(&emit_type_intent("body-default", i, indent));
        }
        if let Some(i) = &intents.body_quiet {
            out.push_str(&emit_type_intent("body-quiet", i, indent));
        }
        if let Some(i) = &intents.caption {
            out.push_str(&emit_type_intent("caption", i, indent));
        }
        if let Some(i) = &intents.numeric {
            out.push_str(&emit_type_intent("numeric", i, indent));
        }
    }
    out.push('\n');
    out
}

pub(crate) fn emit_motion_intent(name: &str, intent: &MotionIntent, indent: &str) -> String {
    let mut out = String::new();
    let dur = motion_duration_key_str(&intent.duration);
    let ease = motion_easing_key_str(&intent.easing);
    out.push_str(&format!(
        "{}--intent-{}-duration: var(--duration-{});\n",
        indent, name, dur
    ));
    out.push_str(&format!(
        "{}--intent-{}-easing: var(--easing-{});\n",
        indent, name, ease
    ));
    out
}

pub(crate) fn emit_motion(m: Option<&MotionTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(motion) = m else {
        return out;
    };
    out.push_str(&format!("{}/* Motion */\n", indent));
    if let Some(d) = &motion.durations {
        let pairs: [(&str, &Option<String>); 5] = [
            ("instant", &d.instant),
            ("fast", &d.fast),
            ("normal", &d.normal),
            ("slow", &d.slow),
            ("dramatic", &d.dramatic),
        ];
        for (k, v) in pairs {
            if let Some(val) = v {
                out.push_str(&format!("{}--duration-{}: {};\n", indent, k, val));
            }
        }
    }
    if let Some(e) = &motion.easings {
        let pairs: [(&str, &Option<String>); 4] = [
            ("linear", &e.linear),
            ("standard", &e.standard),
            ("emphasized", &e.emphasized),
            ("spring", &e.spring),
        ];
        for (k, v) in pairs {
            if let Some(val) = v {
                out.push_str(&format!("{}--easing-{}: {};\n", indent, k, val));
            }
        }
    }
    if let Some(intents) = &motion.intents {
        if let Some(i) = &intents.enter {
            out.push_str(&emit_motion_intent("enter", i, indent));
        }
        if let Some(i) = &intents.exit {
            out.push_str(&emit_motion_intent("exit", i, indent));
        }
        if let Some(i) = &intents.hover {
            out.push_str(&emit_motion_intent("hover", i, indent));
        }
        if let Some(i) = &intents.press {
            out.push_str(&emit_motion_intent("press", i, indent));
        }
        if let Some(i) = &intents.expand {
            out.push_str(&emit_motion_intent("expand", i, indent));
        }
        if let Some(i) = &intents.transition {
            out.push_str(&emit_motion_intent("transition", i, indent));
        }
    }
    if let Some(sh) = &motion.shapes {
        let pairs: [(&str, &Option<String>); 10] = [
            ("modal-enter-from", &sh.modal_enter),
            ("modal-exit-to", &sh.modal_exit),
            ("popover-enter-from", &sh.popover_enter),
            ("popover-exit-to", &sh.popover_exit),
            ("toast-enter-from", &sh.toast_enter),
            ("toast-exit-to", &sh.toast_exit),
            ("page-enter-from", &sh.page_enter),
            ("enter-rise-from", &sh.enter_rise),
            ("enter-scale-from", &sh.enter_scale),
            ("enter-slide-from", &sh.enter_slide),
        ];
        for (k, v) in pairs {
            if let Some(val) = v {
                out.push_str(&format!("{}--motion-{}-transform: {};\n", indent, k, val));
            }
        }
    }
    if let Some(entry) = &motion.entry {
        if let Some(default) = entry.default {
            let keyframe = match default {
                EnterAnimation::None => "none",
                EnterAnimation::Fade => "almadar-enter-fade",
                EnterAnimation::Rise => "almadar-enter-rise",
                EnterAnimation::Scale => "almadar-enter-scale",
                EnterAnimation::Slide => "almadar-enter-slide",
            };
            out.push_str(&format!(
                "{}--motion-enter-default: {};\n",
                indent, keyframe
            ));
        }
        if let Some(stagger) = &entry.stagger {
            out.push_str(&format!("{}--motion-enter-stagger: {};\n", indent, stagger));
        }
    }
    if let Some(delay) = &motion.busy_delay {
        out.push_str(&format!("{}--motion-busy-delay: {};\n", indent, delay));
    }
    if let Some(skeleton) = motion.awaiting_skeleton {
        let display = if skeleton { "block" } else { "none" };
        out.push_str(&format!(
            "{}--awaiting-skeleton-display: {};\n",
            indent, display
        ));
    }
    out.push('\n');
    out
}

pub(crate) fn emit_iconography(i: Option<&IconographyTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(icon) = i else {
        return out;
    };
    out.push_str(&format!("{}/* Iconography */\n", indent));
    if let Some(f) = &icon.family {
        out.push_str(&format!(
            "{}--icon-family: {};\n",
            indent,
            icon_family_str(f)
        ));
    }
    if let Some(s) = &icon.stroke_width {
        out.push_str(&format!("{}--icon-stroke-width: {};\n", indent, s));
    }
    if let Some(d) = &icon.default_size {
        out.push_str(&format!("{}--icon-default-size: {};\n", indent, d));
    }
    out.push('\n');
    out
}

pub(crate) fn emit_elevation(e: Option<&ElevationTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(elev) = e else {
        return out;
    };
    out.push_str(&format!("{}/* Elevation */\n", indent));
    if let Some(v) = &elev.card_elevation {
        out.push_str(&format!("{}--elevation-card: {};\n", indent, v));
    }
    if let Some(v) = &elev.popover_elevation {
        out.push_str(&format!("{}--elevation-popover: {};\n", indent, v));
    }
    if let Some(v) = &elev.dialog_elevation {
        out.push_str(&format!("{}--elevation-dialog: {};\n", indent, v));
    }
    if let Some(v) = &elev.toast_elevation {
        out.push_str(&format!("{}--elevation-toast: {};\n", indent, v));
    }
    if let Some(v) = &elev.interactive_elevation {
        out.push_str(&format!("{}--elevation-interactive: {};\n", indent, v));
    }
    if let Some(v) = &elev.pressed_elevation {
        out.push_str(&format!("{}--elevation-pressed: {};\n", indent, v));
    }
    out.push('\n');
    out
}

pub(crate) fn emit_geometry(g: Option<&GeometryTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(geo) = g else {
        return out;
    };
    out.push_str(&format!("{}/* Geometry */\n", indent));
    if let Some(v) = &geo.radius_container {
        out.push_str(&format!("{}--radius-container: {};\n", indent, v));
    }
    if let Some(v) = &geo.radius_interactive {
        out.push_str(&format!("{}--radius-interactive: {};\n", indent, v));
    }
    if let Some(v) = &geo.radius_pill {
        out.push_str(&format!("{}--radius-pill: {};\n", indent, v));
    }
    if let Some(v) = &geo.border_hairline {
        out.push_str(&format!("{}--border-hairline: {};\n", indent, v));
    }
    if let Some(v) = &geo.border_standard {
        out.push_str(&format!("{}--border-standard: {};\n", indent, v));
    }
    if let Some(v) = &geo.border_heavy {
        out.push_str(&format!("{}--border-heavy: {};\n", indent, v));
    }
    if let Some(v) = &geo.corner_shape {
        out.push_str(&format!("{}--corner-shape: {};\n", indent, v));
    }
    if let Some(v) = &geo.corner_shape_pill {
        out.push_str(&format!("{}--corner-shape-pill: {};\n", indent, v));
    }
    if let Some(v) = &geo.corner_shape_interactive {
        out.push_str(&format!("{}--corner-shape-interactive: {};\n", indent, v));
    }
    if let Some(v) = &geo.border_style {
        out.push_str(&format!("{}--border-style: {};\n", indent, v));
    }
    if let Some(v) = &geo.border_style_interactive {
        out.push_str(&format!("{}--border-style-interactive: {};\n", indent, v));
    }
    out.push('\n');
    out
}

pub(crate) fn emit_surface(sf: Option<&SurfaceTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(surface) = sf else {
        return out;
    };
    out.push_str(&format!("{}/* Surface */\n", indent));
    if let Some(v) = &surface.backdrop {
        out.push_str(&format!("{}--surface-backdrop: {};\n", indent, v));
    }
    if let Some(v) = &surface.card_image {
        out.push_str(&format!("{}--surface-card-image: {};\n", indent, v));
    }
    if let Some(v) = &surface.page_image {
        out.push_str(&format!("{}--surface-page-image: {};\n", indent, v));
    }
    if let Some(v) = &surface.page_image_size {
        out.push_str(&format!("{}--surface-page-image-size: {};\n", indent, v));
    }
    if let Some(v) = &surface.diagram {
        out.push_str(&format!("{}--surface-diagram: {};\n", indent, v));
    }
    for (name, value) in [
        ("--surface-edge-mask", &surface.edge_mask),
        ("--surface-edge-height", &surface.edge_height),
        ("--surface-texture-mask", &surface.texture_mask),
        ("--surface-texture-size", &surface.texture_size),
        ("--surface-accent-image", &surface.accent_image),
        ("--surface-scrim", &surface.scrim),
    ] {
        if let Some(v) = value {
            out.push_str(&format!("{}{}: {};\n", indent, name, v));
        }
    }
    out.push('\n');
    out
}

fn push_var(out: &mut String, indent: &str, name: &str, value: Option<&str>) {
    if let Some(v) = value {
        out.push_str(&format!("{}{}: {};\n", indent, name, v));
    }
}

pub(crate) fn emit_diagram(dg: Option<&DiagramTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(d) = dg else {
        return out;
    };
    out.push_str(&format!("{}/* Diagram */\n", indent));
    push_var(
        &mut out,
        indent,
        "--diagram-stroke-thin",
        d.stroke_thin.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-stroke-normal",
        d.stroke_normal.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-stroke-bold",
        d.stroke_bold.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-line-cap",
        d.line_cap.map(|v| v.as_str()),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-line-join",
        d.line_join.map(|v| v.as_str()),
    );
    push_var(&mut out, indent, "--diagram-dash", d.dash.as_deref());
    push_var(&mut out, indent, "--diagram-dot", d.dot.as_deref());
    push_var(
        &mut out,
        indent,
        "--diagram-fill-style",
        d.fill_style.map(|v| v.as_str()),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-fill-opacity",
        d.fill_opacity.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-roughness",
        d.roughness.as_deref(),
    );
    push_var(&mut out, indent, "--diagram-glow", d.glow.as_deref());
    push_var(&mut out, indent, "--diagram-shadow", d.shadow.as_deref());
    push_var(
        &mut out,
        indent,
        "--diagram-marker",
        d.marker.map(|v| v.as_str()),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-label-font",
        d.label_font.map(|v| v.as_str()),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-label-size",
        d.label_size.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-label-weight",
        d.label_weight.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--diagram-label-case",
        d.label_case.map(|v| v.as_str()),
    );
    push_var(&mut out, indent, "--diagram-corner", d.corner.as_deref());
    out.push('\n');
    out
}

pub(crate) fn emit_scene(sc: Option<&SceneTokens>, indent: &str) -> String {
    let mut out = String::new();
    let Some(s) = sc else {
        return out;
    };
    out.push_str(&format!("{}/* Scene */\n", indent));
    push_var(
        &mut out,
        indent,
        "--material-roughness",
        s.material_roughness.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--material-metalness",
        s.material_metalness.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--material-flat",
        s.material_flat.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--material-outline",
        s.material_outline.as_deref(),
    );
    push_var(
        &mut out,
        indent,
        "--light-ambient",
        s.light_ambient.as_deref(),
    );
    push_var(&mut out, indent, "--light-key", s.light_key.as_deref());
    push_var(
        &mut out,
        indent,
        "--light-key-color",
        s.light_key_color.as_deref(),
    );
    push_var(&mut out, indent, "--scene-fog", s.fog.as_deref());
    out.push('\n');
    out
}

#[cfg(test)]
mod motion_tests {
    use super::emit_motion;
    use crate::schema::MotionTokens;

    /// The compiled app emits every motion var `@almadar/ui` `themeTokens.ts` `emitMotion` does.
    #[test]
    fn motion_emits_entry_shapes_busy_delay_and_awaiting_skeleton_like_the_runtime() {
        let tokens: MotionTokens = serde_json::from_value(serde_json::json!({
            "shapes": { "modalEnter": "scale(0.9)", "enterRise": "translateY(20px)", "enterScale": "scale(0.9)", "enterSlide": "translateX(8px)" },
            "entry": { "default": "rise", "stagger": "60ms" },
            "busyDelay": "450ms",
            "awaitingSkeleton": true
        }))
        .unwrap();
        let css = emit_motion(Some(&tokens), "");
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
        let none: MotionTokens = serde_json::from_value(
            serde_json::json!({ "entry": { "default": "none" }, "awaitingSkeleton": false }),
        )
        .unwrap();
        let css = emit_motion(Some(&none), "");
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
