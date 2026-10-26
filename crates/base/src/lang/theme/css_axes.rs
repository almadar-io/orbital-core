#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use super::CssVars;
use crate::schema::types::{DensityTokens, DiagramTokens, ElevationTokens, EnterAnimation, GeometryTokens, IconFamily, IconographyTokens, IllustrationTokens, MotionDurationKey, MotionEasingKey, MotionIntent, MotionTokens, SceneTokens, SurfaceTokens, TypeIntent, TypeScaleTokens, TypeSizeKey, TypeWeight};

fn put(vars: &mut CssVars, name: &str, value: Option<&str>) {
    if let Some(v) = value {
        vars.insert(name.to_string(), v.to_string());
    }
}

fn icon_family(f: &IconFamily) -> &'static str {
    match f {
        IconFamily::Lucide => "lucide",
        IconFamily::PhosphorOutline => "phosphor-outline",
        IconFamily::PhosphorFill => "phosphor-fill",
        IconFamily::PhosphorDuotone => "phosphor-duotone",
        IconFamily::Tabler => "tabler",
        IconFamily::FaSolid => "fa-solid",
    }
}

fn size_key(k: &TypeSizeKey) -> &'static str {
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

fn weight(w: &TypeWeight) -> &'static str {
    match w {
        TypeWeight::Normal => "normal",
        TypeWeight::Medium => "medium",
        TypeWeight::Bold => "bold",
    }
}

fn duration_key(k: &MotionDurationKey) -> &'static str {
    match k {
        MotionDurationKey::Instant => "instant",
        MotionDurationKey::Fast => "fast",
        MotionDurationKey::Normal => "normal",
        MotionDurationKey::Slow => "slow",
        MotionDurationKey::Dramatic => "dramatic",
    }
}

fn easing_key(k: &MotionEasingKey) -> &'static str {
    match k {
        MotionEasingKey::Linear => "linear",
        MotionEasingKey::Standard => "standard",
        MotionEasingKey::Emphasized => "emphasized",
        MotionEasingKey::Spring => "spring",
    }
}

pub fn density(d: Option<&DensityTokens>, vars: &mut CssVars) {
    let Some(d) = d else { return };
    if let Some(s) = &d.spacing {
        let steps = [
            ("--space-0", &s.space0),
            ("--space-1", &s.space1),
            ("--space-2", &s.space2),
            ("--space-3", &s.space3),
            ("--space-4", &s.space4),
            ("--space-5", &s.space5),
            ("--space-6", &s.space6),
            ("--space-7", &s.space7),
            ("--space-8", &s.space8),
            ("--space-9", &s.space9),
            ("--space-10", &s.space10),
            ("--space-11", &s.space11),
            ("--space-12", &s.space12),
        ];
        for (n, v) in steps {
            put(vars, n, v.as_deref());
        }
    }
    let elements = [
        ("--button-height-sm", &d.button_height_sm),
        ("--button-height-md", &d.button_height_md),
        ("--button-height-lg", &d.button_height_lg),
        ("--input-height-sm", &d.input_height_sm),
        ("--input-height-md", &d.input_height_md),
        ("--input-height-lg", &d.input_height_lg),
        ("--row-height-compact", &d.row_height_compact),
        ("--row-height-normal", &d.row_height_normal),
        ("--row-height-spacious", &d.row_height_spacious),
        ("--card-padding-sm", &d.card_padding_sm),
        ("--card-padding-md", &d.card_padding_md),
        ("--card-padding-lg", &d.card_padding_lg),
        ("--dialog-padding", &d.dialog_padding),
        ("--section-gap", &d.section_gap),
    ];
    for (n, v) in elements {
        put(vars, n, v.as_deref());
    }
}

fn type_intent(name: &str, i: &TypeIntent, vars: &mut CssVars) {
    let size = size_key(&i.size);
    put(
        vars,
        &format!("--intent-{name}-size"),
        Some(&format!("var(--text-{size})")),
    );
    put(
        vars,
        &format!("--intent-{name}-weight"),
        Some(&format!("var(--font-weight-{})", weight(&i.weight))),
    );
    put(
        vars,
        &format!("--intent-{name}-leading"),
        Some(&format!("var(--leading-{size})")),
    );
}

pub fn type_scale(t: Option<&TypeScaleTokens>, vars: &mut CssVars) {
    let Some(t) = t else { return };
    let flat = [
        ("--font-family-display", &t.display_family),
        ("--font-family-body", &t.body_family),
        ("--font-family-mono", &t.mono_family),
        ("--heading-weight", &t.heading_weight),
        ("--heading-transform", &t.heading_transform),
        ("--heading-tracking", &t.heading_tracking),
        ("--heading-style", &t.heading_style),
        ("--heading-shadow", &t.heading_shadow),
    ];
    for (n, v) in flat {
        put(vars, n, v.as_deref());
    }
    if let Some(s) = &t.scale {
        let entries = [
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
        for (k, e) in entries {
            if let Some(e) = e {
                put(vars, &format!("--text-{k}"), Some(&e.size));
                put(vars, &format!("--leading-{k}"), Some(&e.line_height));
            }
        }
    }
    if let Some(i) = &t.intents {
        let intents = [
            ("heading-major", &i.heading_major),
            ("heading-minor", &i.heading_minor),
            ("body-emphasis", &i.body_emphasis),
            ("body-default", &i.body_default),
            ("body-quiet", &i.body_quiet),
            ("caption", &i.caption),
            ("numeric", &i.numeric),
        ];
        for (n, intent) in intents {
            if let Some(intent) = intent {
                type_intent(n, intent, vars);
            }
        }
    }
}

fn motion_intent(name: &str, i: &MotionIntent, vars: &mut CssVars) {
    put(
        vars,
        &format!("--intent-{name}-duration"),
        Some(&format!("var(--duration-{})", duration_key(&i.duration))),
    );
    put(
        vars,
        &format!("--intent-{name}-easing"),
        Some(&format!("var(--easing-{})", easing_key(&i.easing))),
    );
}

pub fn motion(m: Option<&MotionTokens>, vars: &mut CssVars) {
    let Some(m) = m else { return };
    if let Some(d) = &m.durations {
        let pairs = [
            ("--duration-instant", &d.instant),
            ("--duration-fast", &d.fast),
            ("--duration-normal", &d.normal),
            ("--duration-slow", &d.slow),
            ("--duration-dramatic", &d.dramatic),
        ];
        for (n, v) in pairs {
            put(vars, n, v.as_deref());
        }
    }
    if let Some(e) = &m.easings {
        let pairs = [
            ("--easing-linear", &e.linear),
            ("--easing-standard", &e.standard),
            ("--easing-emphasized", &e.emphasized),
            ("--easing-spring", &e.spring),
        ];
        for (n, v) in pairs {
            put(vars, n, v.as_deref());
        }
    }
    if let Some(i) = &m.intents {
        let intents = [
            ("enter", &i.enter),
            ("exit", &i.exit),
            ("hover", &i.hover),
            ("press", &i.press),
            ("expand", &i.expand),
            ("transition", &i.transition),
        ];
        for (n, intent) in intents {
            if let Some(intent) = intent {
                motion_intent(n, intent, vars);
            }
        }
    }
    if let Some(sh) = &m.shapes {
        let pairs = [
            ("--motion-modal-enter-from-transform", &sh.modal_enter),
            ("--motion-modal-exit-to-transform", &sh.modal_exit),
            ("--motion-popover-enter-from-transform", &sh.popover_enter),
            ("--motion-popover-exit-to-transform", &sh.popover_exit),
            ("--motion-toast-enter-from-transform", &sh.toast_enter),
            ("--motion-toast-exit-to-transform", &sh.toast_exit),
            ("--motion-page-enter-from-transform", &sh.page_enter),
            ("--motion-enter-rise-from-transform", &sh.enter_rise),
            ("--motion-enter-scale-from-transform", &sh.enter_scale),
            ("--motion-enter-slide-from-transform", &sh.enter_slide),
        ];
        for (n, v) in pairs {
            put(vars, n, v.as_deref());
        }
    }
    if let Some(entry) = &m.entry {
        if let Some(default) = entry.default {
            let keyframe = match default {
                EnterAnimation::None => "none",
                EnterAnimation::Fade => "almadar-enter-fade",
                EnterAnimation::Rise => "almadar-enter-rise",
                EnterAnimation::Scale => "almadar-enter-scale",
                EnterAnimation::Slide => "almadar-enter-slide",
            };
            put(vars, "--motion-enter-default", Some(keyframe));
        }
        put(vars, "--motion-enter-stagger", entry.stagger.as_deref());
    }
    put(vars, "--motion-busy-delay", m.busy_delay.as_deref());
    if let Some(skeleton) = m.awaiting_skeleton {
        put(
            vars,
            "--awaiting-skeleton-display",
            Some(if skeleton { "block" } else { "none" }),
        );
    }
}

pub fn iconography(i: Option<&IconographyTokens>, vars: &mut CssVars) {
    let Some(i) = i else { return };
    put(vars, "--icon-family", i.family.as_ref().map(icon_family));
    put(vars, "--icon-stroke-width", i.stroke_width.as_deref());
    put(vars, "--icon-default-size", i.default_size.as_deref());
}

pub fn elevation(e: Option<&ElevationTokens>, vars: &mut CssVars) {
    let Some(e) = e else { return };
    let pairs = [
        ("--elevation-card", &e.card_elevation),
        ("--elevation-popover", &e.popover_elevation),
        ("--elevation-dialog", &e.dialog_elevation),
        ("--elevation-toast", &e.toast_elevation),
        ("--elevation-interactive", &e.interactive_elevation),
        ("--elevation-pressed", &e.pressed_elevation),
    ];
    for (n, v) in pairs {
        put(vars, n, v.as_deref());
    }
}

pub fn geometry(g: Option<&GeometryTokens>, vars: &mut CssVars) {
    let Some(g) = g else { return };
    let pairs = [
        ("--radius-container", &g.radius_container),
        ("--radius-interactive", &g.radius_interactive),
        ("--radius-pill", &g.radius_pill),
        ("--border-hairline", &g.border_hairline),
        ("--border-standard", &g.border_standard),
        ("--border-heavy", &g.border_heavy),
        ("--corner-shape", &g.corner_shape),
        ("--corner-shape-pill", &g.corner_shape_pill),
        ("--corner-shape-interactive", &g.corner_shape_interactive),
        ("--border-style", &g.border_style),
        ("--border-style-interactive", &g.border_style_interactive),
    ];
    for (n, v) in pairs {
        put(vars, n, v.as_deref());
    }
}

pub fn illustration(i: Option<&IllustrationTokens>, vars: &mut CssVars) {
    let Some(asset) = i.and_then(|i| i.loading_asset.as_deref()) else {
        return;
    };
    if asset.is_empty() {
        return;
    }
    put(
        vars,
        "--illustration-loading",
        Some(&format!("url(\"{asset}\")")),
    );
    put(vars, "--loader-mark-display", Some("inline-block"));
    put(vars, "--loader-icon-display", Some("none"));
}

pub fn surface(s: Option<&SurfaceTokens>, vars: &mut CssVars) {
    let Some(s) = s else { return };
    let pairs = [
        ("--surface-backdrop", &s.backdrop),
        ("--surface-card-image", &s.card_image),
        ("--surface-page-image", &s.page_image),
        ("--surface-page-image-size", &s.page_image_size),
        ("--surface-diagram", &s.diagram),
        ("--surface-edge-mask", &s.edge_mask),
        ("--surface-edge-height", &s.edge_height),
        ("--surface-texture-mask", &s.texture_mask),
        ("--surface-texture-size", &s.texture_size),
        ("--surface-accent-image", &s.accent_image),
        ("--surface-scrim", &s.scrim),
    ];
    for (n, v) in pairs {
        put(vars, n, v.as_deref());
    }
}

pub fn diagram(d: Option<&DiagramTokens>, vars: &mut CssVars) {
    let Some(d) = d else { return };
    put(vars, "--diagram-stroke-thin", d.stroke_thin.as_deref());
    put(vars, "--diagram-stroke-normal", d.stroke_normal.as_deref());
    put(vars, "--diagram-stroke-bold", d.stroke_bold.as_deref());
    put(vars, "--diagram-line-cap", d.line_cap.map(|v| v.as_str()));
    put(vars, "--diagram-line-join", d.line_join.map(|v| v.as_str()));
    put(vars, "--diagram-dash", d.dash.as_deref());
    put(vars, "--diagram-dot", d.dot.as_deref());
    put(
        vars,
        "--diagram-fill-style",
        d.fill_style.map(|v| v.as_str()),
    );
    put(vars, "--diagram-fill-opacity", d.fill_opacity.as_deref());
    put(vars, "--diagram-roughness", d.roughness.as_deref());
    put(vars, "--diagram-glow", d.glow.as_deref());
    put(vars, "--diagram-shadow", d.shadow.as_deref());
    put(vars, "--diagram-marker", d.marker.map(|v| v.as_str()));
    put(
        vars,
        "--diagram-label-font",
        d.label_font.map(|v| v.as_str()),
    );
    put(vars, "--diagram-label-size", d.label_size.as_deref());
    put(vars, "--diagram-label-weight", d.label_weight.as_deref());
    put(
        vars,
        "--diagram-label-case",
        d.label_case.map(|v| v.as_str()),
    );
    put(vars, "--diagram-corner", d.corner.as_deref());
}

pub fn scene(s: Option<&SceneTokens>, vars: &mut CssVars) {
    let Some(s) = s else { return };
    let pairs = [
        ("--material-roughness", &s.material_roughness),
        ("--material-metalness", &s.material_metalness),
        ("--material-flat", &s.material_flat),
        ("--material-outline", &s.material_outline),
        ("--light-ambient", &s.light_ambient),
        ("--light-key", &s.light_key),
        ("--light-key-color", &s.light_key_color),
        ("--scene-fog", &s.fog),
    ];
    for (n, v) in pairs {
        put(vars, n, v.as_deref());
    }
}
