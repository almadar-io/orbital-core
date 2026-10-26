//! Theme tokens turned into the CSS custom properties the runtime applies.

use super::*;

// ---------------------------------------------------------------------------
// Tokens -> CSS custom properties (`themeTokensToCssVars` in @almadar/ui)
// ---------------------------------------------------------------------------

/// CSS custom properties keyed by variable name, in emission order.
pub type CssVars = crate::collections::IndexMap<String, String>;

/// Convert theme tokens (plus the optional dark variant) into the CSS
/// custom-property map the runtime applies. Only keys the tokens set are
/// emitted, so unset ones cascade from the parent. In dark mode each category
/// takes the variant's value when the variant sets it, else the base; the
/// colors map additionally layers base colors the variant does not name.
pub fn css_vars(
    tokens: &ThemeTokens,
    mode: ThemeMode,
    dark_variant: Option<&ThemeVariant>,
) -> CssVars {
    let mut vars = CssVars::default();
    let dark = if mode == ThemeMode::Dark {
        dark_variant
    } else {
        None
    };

    fn map_vars(vars: &mut CssVars, prefix: &str, map: Option<&crate::schema::types::ThemeMap>) {
        if let Some(map) = map {
            for (k, v) in map {
                vars.insert(format!("{prefix}{k}"), v.clone());
            }
        }
    }

    let colors = dark
        .and_then(|v| v.colors.as_ref())
        .or(tokens.colors.as_ref());
    map_vars(&mut vars, "--color-", colors);
    if dark.is_some_and(|v| v.colors.is_some()) {
        if let Some(base) = &tokens.colors {
            for (k, v) in base {
                vars.entry(format!("--color-{k}"))
                    .or_insert_with(|| v.clone());
            }
        }
    }
    map_vars(
        &mut vars,
        "--radius-",
        dark.and_then(|v| v.radii.as_ref())
            .or(tokens.radii.as_ref()),
    );
    map_vars(
        &mut vars,
        "--space-",
        dark.and_then(|v| v.spacing.as_ref())
            .or(tokens.spacing.as_ref()),
    );
    map_vars(
        &mut vars,
        "--",
        dark.and_then(|v| v.typography.as_ref())
            .or(tokens.typography.as_ref()),
    );
    map_vars(
        &mut vars,
        "--shadow-",
        dark.and_then(|v| v.shadows.as_ref())
            .or(tokens.shadows.as_ref()),
    );

    css_axes::density(
        dark.and_then(|v| v.density.as_ref())
            .or(tokens.density.as_ref()),
        &mut vars,
    );
    css_axes::type_scale(
        dark.and_then(|v| v.type_scale.as_ref())
            .or(tokens.type_scale.as_ref()),
        &mut vars,
    );
    css_axes::motion(
        dark.and_then(|v| v.motion.as_ref())
            .or(tokens.motion.as_ref()),
        &mut vars,
    );
    css_axes::iconography(
        dark.and_then(|v| v.iconography.as_ref())
            .or(tokens.iconography.as_ref()),
        &mut vars,
    );
    css_axes::elevation(
        dark.and_then(|v| v.elevation.as_ref())
            .or(tokens.elevation.as_ref()),
        &mut vars,
    );
    css_axes::geometry(
        dark.and_then(|v| v.geometry.as_ref())
            .or(tokens.geometry.as_ref()),
        &mut vars,
    );
    css_axes::illustration(
        dark.and_then(|v| v.illustration.as_ref())
            .or(tokens.illustration.as_ref()),
        &mut vars,
    );
    css_axes::surface(
        dark.and_then(|v| v.surface.as_ref())
            .or(tokens.surface.as_ref()),
        &mut vars,
    );
    css_axes::diagram(
        dark.and_then(|v| v.diagram.as_ref())
            .or(tokens.diagram.as_ref()),
        &mut vars,
    );
    css_axes::scene(
        dark.and_then(|v| v.scene.as_ref())
            .or(tokens.scene.as_ref()),
        &mut vars,
    );
    vars
}
