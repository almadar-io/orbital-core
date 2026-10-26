//! `@currentTheme` resolution — the render-resolved theme key + merged tokens
//! for a schema/orbital pair.
//!
//! Mirrors the JS runtime's precedence exactly (`packages/almadar-runtime/src/
//! OrbitalServerRuntime.ts` `themeDataKey`/`sigilTheme`, `docs/Almadar_LOLO.md`
//! §3): orbital `theme` → app-level `OrbitalSchema.theme` → the baseline
//! default (`minimalist-light`). A declared theme is either an inline
//! `ThemeDefinition` or a preset key string (`"<base>-<light|dark>"`, the
//! `data-theme` value) resolved against [`ThemePresets`].
//!
//! The kernel has no filesystem access at runtime, so a program carries the
//! presets it references (`themeDefinitions`, written by `orb` from the
//! installed `@almadar/core` — [`carry_theme_definitions`]); the compiler reads
//! the installed set ([`ThemePresets::installed`]).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::types::{OrbitalSchema, ThemeDefinition, ThemeRef, ThemeTokens, ThemeVariant};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Resolved color mode. Mirrors `@almadar/ui`'s `ThemeMode` (`'light' | 'dark'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
}

impl ThemeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }
}

/// The baseline theme `@currentTheme` falls back to when neither the orbital
/// nor the app declares a `theme`. Mirrors the JS runtime's `DEFAULT_THEME_KEY`
/// and the compiler's `resolve.rs::DEFAULT_THEME_KEY`.
pub const DEFAULT_THEME_BASE: &str = "minimalist";
pub const DEFAULT_THEME_KEY: &str = "minimalist-light";

/// A schema/orbital pair's resolved `@currentTheme`: the `data-theme` key,
/// its base/mode split, and the base tokens with the dark variant merged in
/// when `mode` is [`ThemeMode::Dark`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTheme {
    /// `"<base>-<light|dark>"` — the `data-theme` selector value.
    pub key: String,
    pub base: String,
    pub mode: ThemeMode,
    /// Base tokens with `variants.dark` merged in per axis when `mode` is Dark.
    pub tokens: ThemeTokens,
}

/// Errors from parsing a theme preset document.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("failed to parse theme preset JSON: {0}")]
    Parse(#[from] serde_json::Error),
}

/// A registry of theme presets keyed by base name (`"clay"`, not
/// `"clay-light"`) — the shape of `packages/almadar-core/themes/<name>.json`,
/// one `ThemeDefinition` per file.
#[derive(Debug, Clone, Default)]
pub struct ThemePresets {
    definitions: HashMap<String, ThemeDefinition>,
}

impl ThemePresets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a single preset document (one `ThemeDefinition`, e.g. the
    /// contents of `clay.json`) into a registry containing just that theme,
    /// keyed by its declared `name`. No filesystem access — the kernel reads
    /// no directory at runtime; callers hand it a JSON string (baked in via
    /// `include_str!`, see [`Self::baked`]).
    pub fn from_json(json: &str) -> Result<Self, ThemeError> {
        let def: ThemeDefinition = serde_json::from_str(json)?;
        let mut definitions = HashMap::new();
        definitions.insert(def.name.clone(), def);
        Ok(Self { definitions })
    }

    /// Merge another registry's presets into this one (later insert wins on
    /// a name collision).
    pub fn merge(&mut self, other: Self) {
        self.definitions.extend(other.definitions);
    }

    pub fn get(&self, base: &str) -> Option<&ThemeDefinition> {
        self.definitions.get(base)
    }

    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// Every `[data-theme]` key the presets define ([`theme_keys`]), sorted.
    pub fn theme_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.definitions.values().flat_map(theme_keys).collect();
        keys.sort();
        keys
    }

    /// A registry of the given definitions, keyed by name.
    pub fn from_definitions<'a>(
        definitions: impl IntoIterator<Item = &'a ThemeDefinition>,
    ) -> Self {
        Self {
            definitions: definitions
                .into_iter()
                .map(|d| (d.name.clone(), d.clone()))
                .collect(),
        }
    }

    /// The presets of the installed `@almadar/core` (`orbLanguage.themes`),
    /// parsed once. Hosts that compile programs read these; a kernel reads the
    /// definitions each program carries ([`carry_theme_definitions`]).
    pub fn installed() -> &'static Self {
        static PRESETS: crate::sync::OnceLock<ThemePresets> = crate::sync::OnceLock::new();
        PRESETS.get_or_init(|| {
            let mut presets = Self::new();
            for json in &crate::language_data::language_data_after_preflight().themes {
                let parsed = Self::from_json(json).unwrap_or_else(|e| {
                    panic!("installed @almadar/core theme preset failed to parse: {e}")
                });
                presets.merge(parsed);
            }
            presets
        })
    }
}

/// The `[data-theme]` keys a preset defines, as `pattern-sync themes` writes
/// its CSS: `<name>-light` when its base tokens declare anything, `<name>-dark`
/// when it declares a `dark` variant (the `game-*` presets are dark-only).
pub fn theme_keys(definition: &ThemeDefinition) -> Vec<String> {
    let mut keys = Vec::new();
    let declares_tokens = serde_json::to_value(&definition.tokens)
        .ok()
        .and_then(|v| v.as_object().map(|o| !o.is_empty()))
        .unwrap_or(false);
    if declares_tokens {
        keys.push(format!("{}-light", definition.name));
    }
    if definition
        .variants
        .as_ref()
        .is_some_and(|v| v.contains_key("dark"))
    {
        keys.push(format!("{}-dark", definition.name));
    }
    keys
}

/// Writes into `schema.themeDefinitions` the presets the program references
/// (app and orbital `theme` keys, the app's `themes`) plus the runtime default, from `presets`, so
/// a kernel resolves `@currentTheme` from the program alone. An inline theme
/// needs no entry; an unknown key carries nothing (it resolves as before).
pub fn carry_theme_definitions(schema: &mut OrbitalSchema, presets: &ThemePresets) {
    let mut bases: Vec<String> = schema
        .theme
        .iter()
        .chain(schema.orbitals.iter().filter_map(|o| o.theme.as_ref()))
        .filter_map(|t| match t {
            ThemeRef::Reference(key) if !key.is_empty() => Some(split_theme_key(key).0),
            _ => None,
        })
        .chain(schema.themes.iter().flatten().map(|key| split_theme_key(key).0))
        .chain(std::iter::once(DEFAULT_THEME_BASE.to_string()))
        .collect();
    bases.sort();
    bases.dedup();
    let carried: Vec<ThemeDefinition> = bases
        .iter()
        .filter_map(|base| presets.get(base).cloned())
        .collect();
    schema.theme_definitions = (!carried.is_empty()).then_some(carried);
}

/// Resolve `@currentTheme` for `orbital` (by name) inside `schema`, applying
/// the exact JS precedence: orbital `theme` → app-level `OrbitalSchema.theme`
/// → the baseline default. `requested_mode`, when given, overrides the mode
/// encoded in the winning theme's key/name (e.g. a kernel-tracked dark-mode
/// toggle independent of what the author declared); when `None`, the mode is
/// derived from the winning reference itself (a `"<base>-dark"` preset key,
/// defaulting to light otherwise).
pub fn resolve_theme(
    schema: &OrbitalSchema,
    orbital: Option<&str>,
    presets: &ThemePresets,
    requested_mode: Option<ThemeMode>,
) -> ResolvedTheme {
    let orbital_theme = orbital
        .and_then(|name| schema.orbitals.iter().find(|o| o.name == name))
        .and_then(|o| o.theme.as_ref());

    match orbital_theme.or(schema.theme.as_ref()) {
        Some(theme_ref) => resolve_theme_ref(theme_ref, presets, requested_mode),
        None => resolve_theme_default(presets, requested_mode.unwrap_or_default()),
    }
}

fn resolve_theme_ref(
    theme_ref: &ThemeRef,
    presets: &ThemePresets,
    requested_mode: Option<ThemeMode>,
) -> ResolvedTheme {
    match theme_ref {
        ThemeRef::Inline(def) => {
            build_resolved(&def.name, requested_mode.unwrap_or_default(), def, presets)
        }
        ThemeRef::Reference(key) if key.is_empty() => {
            resolve_theme_default(presets, requested_mode.unwrap_or_default())
        }
        ThemeRef::Reference(key) => {
            let (base, key_mode) = split_theme_key(key);
            let mode = requested_mode.unwrap_or(key_mode);
            match presets.get(&base) {
                Some(def) => build_resolved(&def.name, mode, def, presets),
                None => ResolvedTheme {
                    key: format!("{base}-{}", mode.as_str()),
                    base,
                    mode,
                    tokens: empty_tokens(),
                },
            }
        }
    }
}

/// Resolve a preset key (`"<base>-<light|dark>"`) against `presets`.
pub fn resolve_preset_key(presets: &ThemePresets, key: &str) -> ResolvedTheme {
    resolve_theme_ref(&ThemeRef::Reference(key.to_string()), presets, None)
}

pub fn resolve_theme_default(presets: &ThemePresets, mode: ThemeMode) -> ResolvedTheme {
    match presets.get(DEFAULT_THEME_BASE) {
        Some(def) => build_resolved(&def.name, mode, def, presets),
        None => ResolvedTheme {
            key: DEFAULT_THEME_KEY.to_string(),
            base: DEFAULT_THEME_BASE.to_string(),
            mode,
            tokens: empty_tokens(),
        },
    }
}

/// Split a full `data-theme` key (`"clay-dark"`) into its base (`"clay"`) and
/// mode. A key carrying neither suffix is treated as an already-bare base
/// name, defaulting to light (permissive — the author-facing contract is the
/// full `<base>-<light|dark>` key, but a bare base should still resolve).
fn split_theme_key(key: &str) -> (String, ThemeMode) {
    if let Some(base) = key.strip_suffix("-dark") {
        (base.to_string(), ThemeMode::Dark)
    } else if let Some(base) = key.strip_suffix("-light") {
        (base.to_string(), ThemeMode::Light)
    } else {
        (key.to_string(), ThemeMode::Light)
    }
}

fn build_resolved(
    base: &str,
    mode: ThemeMode,
    def: &ThemeDefinition,
    _presets: &ThemePresets,
) -> ResolvedTheme {
    let variant = if mode == ThemeMode::Dark {
        def.variants.as_ref().and_then(|v| v.get("dark"))
    } else {
        None
    };
    ResolvedTheme {
        key: format!("{base}-{}", mode.as_str()),
        base: base.to_string(),
        mode,
        tokens: merge_theme_tokens(&def.tokens, variant),
    }
}

/// Merge `variant` ("dark") onto `base` tokens per axis: prefer the variant's
/// axis wholesale when present, else fall back to the base axis — the same
/// rule `@almadar/ui`'s `themeTokensToCssVars` applies (`packages/almadar-ui/
/// lib/themeTokens.ts`). `colors` (the legacy free-form map) gets the one
/// deeper merge that function also does: when the variant supplies *some*
/// colors but not all, base colors fill the gaps rather than disappearing.
fn merge_theme_tokens(base: &ThemeTokens, variant: Option<&ThemeVariant>) -> ThemeTokens {
    let Some(variant) = variant else {
        return base.clone();
    };
    ThemeTokens {
        color: variant.color.clone().or_else(|| base.color.clone()),
        colors: merge_colors_map(base.colors.as_ref(), variant.colors.as_ref()),
        radii: variant.radii.clone().or_else(|| base.radii.clone()),
        spacing: variant.spacing.clone().or_else(|| base.spacing.clone()),
        typography: variant
            .typography
            .clone()
            .or_else(|| base.typography.clone()),
        shadows: variant.shadows.clone().or_else(|| base.shadows.clone()),
        density: variant.density.clone().or_else(|| base.density.clone()),
        type_scale: variant
            .type_scale
            .clone()
            .or_else(|| base.type_scale.clone()),
        motion: variant.motion.clone().or_else(|| base.motion.clone()),
        iconography: variant
            .iconography
            .clone()
            .or_else(|| base.iconography.clone()),
        elevation: variant.elevation.clone().or_else(|| base.elevation.clone()),
        geometry: variant.geometry.clone().or_else(|| base.geometry.clone()),
        illustration: variant
            .illustration
            .clone()
            .or_else(|| base.illustration.clone()),
        surface: variant.surface.clone().or_else(|| base.surface.clone()),
        diagram: variant.diagram.clone().or_else(|| base.diagram.clone()),
        scene: variant.scene.clone().or_else(|| base.scene.clone()),
    }
}

fn merge_colors_map(
    base: Option<&crate::schema::types::ThemeMap>,
    variant: Option<&crate::schema::types::ThemeMap>,
) -> Option<crate::schema::types::ThemeMap> {
    match variant {
        Some(v) => {
            let mut merged = v.clone();
            if let Some(b) = base {
                for (k, val) in b {
                    merged.entry(k.clone()).or_insert_with(|| val.clone());
                }
            }
            Some(merged)
        }
        None => base.cloned(),
    }
}

fn empty_tokens() -> ThemeTokens {
    ThemeTokens {
        color: None,
        colors: None,
        radii: None,
        spacing: None,
        typography: None,
        shadows: None,
        density: None,
        type_scale: None,
        motion: None,
        iconography: None,
        elevation: None,
        geometry: None,
        illustration: None,
        diagram: None,
        scene: None,
        surface: None,
    }
}

mod css;
pub use css::{css_vars, CssVars};

mod css_axes;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod css_vars_tests;
