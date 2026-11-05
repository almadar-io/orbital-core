#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::defaults::{get_default_colors_dark, get_default_colors_light, get_default_radii};
use super::emitters::{
    emit_density, emit_diagram, emit_elevation, emit_geometry, emit_iconography, emit_motion,
    emit_scene, emit_surface, emit_type_scale,
};
use crate::schema::{ThemeTokens, ThemeVariant};
use std::collections::BTreeMap;

/// Pick the right Option for each axis: prefer dark_variant.<axis> if Some, else base_tokens.<axis>.
/// Matches the same category-level cascade used for colors / shadows in this file.
pub(crate) fn emit_skin_axes(tokens: &ThemeTokens, indent: &str) -> String {
    let mut out = String::new();
    out.push_str(&emit_density(tokens.density.as_ref(), indent));
    out.push_str(&emit_type_scale(tokens.type_scale.as_ref(), indent));
    out.push_str(&emit_motion(tokens.motion.as_ref(), indent));
    out.push_str(&emit_iconography(tokens.iconography.as_ref(), indent));
    out.push_str(&emit_elevation(tokens.elevation.as_ref(), indent));
    out.push_str(&emit_geometry(tokens.geometry.as_ref(), indent));
    out.push_str(&emit_surface(tokens.surface.as_ref(), indent));
    out.push_str(&emit_diagram(tokens.diagram.as_ref(), indent));
    out.push_str(&emit_scene(tokens.scene.as_ref(), indent));
    out
}

pub(crate) fn emit_skin_axes_dark(
    base_tokens: &ThemeTokens,
    dark_variant: Option<&ThemeVariant>,
    indent: &str,
) -> String {
    let mut out = String::new();
    let density = dark_variant
        .and_then(|v| v.density.as_ref())
        .or(base_tokens.density.as_ref());
    let type_scale = dark_variant
        .and_then(|v| v.type_scale.as_ref())
        .or(base_tokens.type_scale.as_ref());
    let motion = dark_variant
        .and_then(|v| v.motion.as_ref())
        .or(base_tokens.motion.as_ref());
    let iconography = dark_variant
        .and_then(|v| v.iconography.as_ref())
        .or(base_tokens.iconography.as_ref());
    let elevation = dark_variant
        .and_then(|v| v.elevation.as_ref())
        .or(base_tokens.elevation.as_ref());
    let geometry = dark_variant
        .and_then(|v| v.geometry.as_ref())
        .or(base_tokens.geometry.as_ref());
    out.push_str(&emit_density(density, indent));
    out.push_str(&emit_type_scale(type_scale, indent));
    out.push_str(&emit_motion(motion, indent));
    out.push_str(&emit_iconography(iconography, indent));
    out.push_str(&emit_elevation(elevation, indent));
    out.push_str(&emit_geometry(geometry, indent));
    let surface = dark_variant
        .and_then(|v| v.surface.as_ref())
        .or(base_tokens.surface.as_ref());
    out.push_str(&emit_surface(surface, indent));
    let diagram = dark_variant
        .and_then(|v| v.diagram.as_ref())
        .or(base_tokens.diagram.as_ref());
    out.push_str(&emit_diagram(diagram, indent));
    let scene = dark_variant
        .and_then(|v| v.scene.as_ref())
        .or(base_tokens.scene.as_ref());
    out.push_str(&emit_scene(scene, indent));
    out
}

/// Generate CSS custom properties from theme tokens with defaults for missing values
pub(crate) fn generate_tokens_css(
    tokens: &ThemeTokens,
    indent: &str,
    defaults: &BTreeMap<&str, &str>,
    _is_dark: bool,
) -> String {
    let mut css = String::new();

    // Colors - merge schema tokens with required defaults
    css.push_str(&format!("{}/* Colors */\n", indent));

    let default_colors = get_default_colors_light();
    let colors = tokens.colors.as_ref();

    for (key, default_value) in &default_colors {
        let value = colors
            .and_then(|c| c.get(*key))
            .map(|v| v.as_str())
            .unwrap_or(default_value);
        css.push_str(&format!("{}--color-{}: {};\n", indent, key, value));
    }

    // Also include any custom colors from schema not in defaults
    if let Some(schema_colors) = colors {
        for (key, value) in schema_colors {
            if !default_colors.contains_key(key.as_str()) {
                css.push_str(&format!("{}--color-{}: {};\n", indent, key, value));
            }
        }
    }
    css.push('\n');

    // Border Radius
    css.push_str(&format!("{}/* Border Radius */\n", indent));
    let default_radii = get_default_radii();
    let radii = tokens.radii.as_ref();

    for (key, default_value) in &default_radii {
        let value = radii
            .and_then(|r| r.get(*key))
            .map(|v| v.as_str())
            .unwrap_or(default_value);
        css.push_str(&format!("{}--radius-{}: {};\n", indent, key, value));
    }
    if let Some(schema_radii) = radii {
        for (key, value) in schema_radii {
            if !default_radii.contains_key(key.as_str()) {
                css.push_str(&format!("{}--radius-{}: {};\n", indent, key, value));
            }
        }
    }
    css.push('\n');

    // Border width
    css.push_str(&format!("{}/* Border Width */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("border-width") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Typography
    css.push_str(&format!("{}/* Typography */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("font-") || key.starts_with("letter-") || key.starts_with("line-") {
            let value = tokens
                .typography
                .as_ref()
                .and_then(|t| t.get(*key))
                .map(|v| v.as_str())
                .unwrap_or(default_value);
            css.push_str(&format!("{}--{}: {};\n", indent, key, value));
        }
    }
    css.push('\n');

    // Shadows
    css.push_str(&format!("{}/* Shadows */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("shadow-") {
            // Check if schema provides this shadow (without "shadow-" prefix in schema)
            let schema_key = key.strip_prefix("shadow-").unwrap_or(key);
            let value = tokens
                .shadows
                .as_ref()
                .and_then(|s| s.get(schema_key))
                .map(|v| v.as_str())
                .unwrap_or(default_value);
            css.push_str(&format!("{}--{}: {};\n", indent, key, value));
        }
    }
    css.push('\n');

    // Transitions
    css.push_str(&format!("{}/* Transitions */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("transition-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Transforms (hover/active states)
    css.push_str(&format!("{}/* Transforms */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("hover-") || key.starts_with("active-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Focus
    css.push_str(&format!("{}/* Focus */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("focus-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Icon
    css.push_str(&format!("{}/* Icon */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("icon-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Skin axes (Layer 1 — density / type / motion / iconography / elevation / geometry)
    css.push_str(&emit_skin_axes(tokens, indent));

    css
}

/// Generate CSS for dark mode - complete set of variables
pub(crate) fn generate_tokens_css_dark(
    base_tokens: &ThemeTokens,
    dark_variant: Option<&ThemeVariant>,
    indent: &str,
    defaults: &BTreeMap<&str, &str>,
) -> String {
    let mut css = String::new();

    // Colors - start with dark mode defaults, overlay schema dark variant, then any base schema colors
    css.push_str(&format!("{}/* Colors */\n", indent));
    let dark_defaults = get_default_colors_dark();
    let dark_colors = dark_variant.and_then(|v| v.colors.as_ref());
    let _base_colors = base_tokens.colors.as_ref();

    for (key, default_value) in &dark_defaults {
        // Priority: dark variant > dark defaults
        let value = dark_colors
            .and_then(|c| c.get(*key))
            .map(|v| v.as_str())
            .unwrap_or(default_value);
        css.push_str(&format!("{}--color-{}: {};\n", indent, key, value));
    }

    // Also include any custom colors from dark variant not in defaults
    if let Some(colors) = dark_colors {
        for (key, value) in colors {
            if !dark_defaults.contains_key(key.as_str()) {
                css.push_str(&format!("{}--color-{}: {};\n", indent, key, value));
            }
        }
    }
    css.push('\n');

    // Border Radius - same as light mode (from base tokens)
    css.push_str(&format!("{}/* Border Radius */\n", indent));
    let default_radii = get_default_radii();
    let radii = base_tokens.radii.as_ref();

    for (key, default_value) in &default_radii {
        let value = radii
            .and_then(|r| r.get(*key))
            .map(|v| v.as_str())
            .unwrap_or(default_value);
        css.push_str(&format!("{}--radius-{}: {};\n", indent, key, value));
    }
    if let Some(schema_radii) = radii {
        for (key, value) in schema_radii {
            if !default_radii.contains_key(key.as_str()) {
                css.push_str(&format!("{}--radius-{}: {};\n", indent, key, value));
            }
        }
    }
    css.push('\n');

    // Border width
    css.push_str(&format!("{}/* Border Width */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("border-width") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Typography - same as light mode
    css.push_str(&format!("{}/* Typography */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("font-") || key.starts_with("letter-") || key.starts_with("line-") {
            let value = base_tokens
                .typography
                .as_ref()
                .and_then(|t| t.get(*key))
                .map(|v| v.as_str())
                .unwrap_or(default_value);
            css.push_str(&format!("{}--{}: {};\n", indent, key, value));
        }
    }
    css.push('\n');

    // Shadows - may be different in dark mode
    css.push_str(&format!("{}/* Shadows */\n", indent));
    let dark_shadows = dark_variant.and_then(|v| v.shadows.as_ref());
    for (key, default_value) in defaults {
        if key.starts_with("shadow-") {
            let schema_key = key.strip_prefix("shadow-").unwrap_or(key);
            // Check dark variant first, then base tokens, then defaults
            let value = dark_shadows
                .and_then(|s| s.get(schema_key))
                .or_else(|| base_tokens.shadows.as_ref().and_then(|s| s.get(schema_key)))
                .map(|v| v.as_str())
                .unwrap_or(default_value);
            css.push_str(&format!("{}--{}: {};\n", indent, key, value));
        }
    }
    css.push('\n');

    // Transitions - same as light mode
    css.push_str(&format!("{}/* Transitions */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("transition-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Transforms - same as light mode
    css.push_str(&format!("{}/* Transforms */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("hover-") || key.starts_with("active-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Focus - same as light mode
    css.push_str(&format!("{}/* Focus */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("focus-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Icon - same as light mode
    css.push_str(&format!("{}/* Icon */\n", indent));
    for (key, default_value) in defaults {
        if key.starts_with("icon-") {
            css.push_str(&format!("{}--{}: {};\n", indent, key, default_value));
        }
    }
    css.push('\n');

    // Skin axes (Layer 1 — dark variant overrides cascade per category, same as colors / shadows above)
    css.push_str(&emit_skin_axes_dark(base_tokens, dark_variant, indent));

    css
}
