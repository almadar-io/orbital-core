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

use crate::schema::{OrbitalSchema, ThemeDefinition, ThemeRef, ThemeTokens, ThemeVariant};
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
        static PRESETS: std::sync::OnceLock<ThemePresets> = std::sync::OnceLock::new();
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
/// (app and orbital `theme` keys) plus the runtime default, from `presets`, so
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

pub(crate) fn resolve_theme_default(presets: &ThemePresets, mode: ThemeMode) -> ResolvedTheme {
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
    base: Option<&HashMap<String, String>>,
    variant: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(json: &str) -> ThemeDefinition {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn theme_keys_follow_the_generated_css_selectors() {
        let both = preset(
            r##"{"name":"clay","tokens":{"colors":{"primary":"#000"}},"variants":{"dark":{}}}"##,
        );
        assert_eq!(theme_keys(&both), vec!["clay-light", "clay-dark"]);
        let light = preset(r##"{"name":"paper","tokens":{"colors":{"primary":"#fff"}}}"##);
        assert_eq!(theme_keys(&light), vec!["paper-light"]);
    }

    #[test]
    fn a_dark_only_preset_has_no_light_key() {
        let dark = preset(r##"{"name":"game-rpg","tokens":{},"variants":{"dark":{}}}"##);
        assert_eq!(theme_keys(&dark), vec!["game-rpg-dark"]);
    }

    #[test]
    fn control_a_non_dark_variant_adds_no_key() {
        let hc = preset(
            r##"{"name":"clay","tokens":{"colors":{"primary":"#000"}},"variants":{"high-contrast":{}}}"##,
        );
        assert_eq!(theme_keys(&hc), vec!["clay-light"]);
    }

    #[test]
    fn preset_keys_are_sorted_across_presets() {
        let presets = ThemePresets::from_definitions(&[
            preset(r##"{"name":"zen","tokens":{"colors":{"a":"b"}}}"##),
            preset(r##"{"name":"aqua","tokens":{"colors":{"a":"b"}},"variants":{"dark":{}}}"##),
        ]);
        assert_eq!(
            presets.theme_keys(),
            vec!["aqua-dark", "aqua-light", "zen-light"]
        );
    }
    use crate::schema::{EntityRef, Orbital, TraitDefinition, TraitReference};

    fn minimal_orbital(name: &str, theme: Option<ThemeRef>) -> Orbital {
        Orbital {
            name: name.to_string(),
            id: None,
            uses: None,
            expects: None,
            theme,
            services: None,
            entity: EntityRef::Reference(format!("{name}Entity")),
            auxiliary_entities: None,
            traits: vec![TraitReference::Inline(minimal_trait_def(&format!(
                "{name}Trait"
            )))],
            pages: None,
            emits: None,
            listens: None,
            exposes: None,
            reference: None,
            config: None,
            domain_context: None,
            design: None,
            types: None,
        }
    }

    fn minimal_trait_def(name: &str) -> TraitDefinition {
        // `TraitDefinition` has many fields; `Default` isn't derived, so tests
        // build through `serde_json` instead of hand-listing every field —
        // only `name` + `stateMachine` (states + transitions, both required)
        // are non-`Option`.
        serde_json::from_value(serde_json::json!({
            "name": name,
            "stateMachine": {
                "states": [{ "name": "idle" }],
                "transitions": [],
            },
        }))
        .expect("minimal trait definition parses")
    }

    fn minimal_schema(app_theme: Option<ThemeRef>, orbitals: Vec<Orbital>) -> OrbitalSchema {
        OrbitalSchema {
            theme_definitions: None,
            site: None,
            public_dir: None,
            locales: None,
            lazy_pages: None,
            name: "TestApp".to_string(),
            version: None,
            description: None,
            theme: app_theme,
            config: None,
            orbitals,
            metadata: None,
            schema_version: None,
            ledger: None,
        }
    }

    fn presets() -> ThemePresets {
        ThemePresets::installed().clone()
    }

    #[test]
    fn resolve_theme_precedence_orbital_over_app_over_default() {
        let presets = presets();

        // Neither declares a theme → the baseline default.
        let schema = minimal_schema(None, vec![minimal_orbital("A", None)]);
        let resolved = resolve_theme(&schema, Some("A"), &presets, None);
        assert_eq!(resolved.key, DEFAULT_THEME_KEY);

        // App declares one, orbital doesn't → app wins.
        let schema = minimal_schema(
            Some(ThemeRef::Reference("linear-clean-light".to_string())),
            vec![minimal_orbital("A", None)],
        );
        let resolved = resolve_theme(&schema, Some("A"), &presets, None);
        assert_eq!(resolved.key, "linear-clean-light");

        // Orbital declares its own → orbital wins over the app-level one.
        let schema = minimal_schema(
            Some(ThemeRef::Reference("linear-clean-light".to_string())),
            vec![minimal_orbital(
                "A",
                Some(ThemeRef::Reference("clay-light".to_string())),
            )],
        );
        let resolved = resolve_theme(&schema, Some("A"), &presets, None);
        assert_eq!(resolved.key, "clay-light");
        assert_eq!(resolved.base, "clay");
        assert_eq!(resolved.mode, ThemeMode::Light);
    }

    #[test]
    fn resolve_theme_dark_merges_variant() {
        let presets = presets();
        let schema = minimal_schema(
            Some(ThemeRef::Reference("clay-dark".to_string())),
            vec![minimal_orbital("A", None)],
        );
        let resolved = resolve_theme(&schema, Some("A"), &presets, None);
        assert_eq!(resolved.key, "clay-dark");
        assert_eq!(resolved.mode, ThemeMode::Dark);

        let light = resolve_theme(
            &minimal_schema(
                Some(ThemeRef::Reference("clay-light".to_string())),
                vec![minimal_orbital("A", None)],
            ),
            Some("A"),
            &presets,
            None,
        );
        // Dark colors differ from light colors — the variant actually merged in,
        // not just a base-tokens passthrough with the key relabeled.
        assert_ne!(
            resolved
                .tokens
                .colors
                .as_ref()
                .and_then(|c| c.get("primary")),
            light.tokens.colors.as_ref().and_then(|c| c.get("primary")),
        );
        // A dark-only value (present in the fixture's `variants.dark`) surfaces.
        assert!(resolved
            .tokens
            .colors
            .as_ref()
            .unwrap()
            .contains_key("background"));

        // requested_mode overrides the declared key's own suffix.
        let forced_dark = resolve_theme(
            &minimal_schema(
                Some(ThemeRef::Reference("clay-light".to_string())),
                vec![minimal_orbital("A", None)],
            ),
            Some("A"),
            &presets,
            Some(ThemeMode::Dark),
        );
        assert_eq!(forced_dark.key, "clay-dark");
        assert_eq!(
            forced_dark
                .tokens
                .colors
                .as_ref()
                .and_then(|c| c.get("primary")),
            resolved
                .tokens
                .colors
                .as_ref()
                .and_then(|c| c.get("primary")),
        );
    }

    #[test]
    fn resolve_theme_preset_key_lookup() {
        let presets = presets();
        let schema = minimal_schema(
            Some(ThemeRef::Reference("clay-light".to_string())),
            vec![minimal_orbital("A", None)],
        );
        let resolved = resolve_theme(&schema, Some("A"), &presets, None);
        // `clay.json`'s declared `tokens.colors.primary` — looked up through the
        // preset registry, not a literal passthrough of the reference string.
        assert_eq!(
            resolved
                .tokens
                .colors
                .as_ref()
                .and_then(|c| c.get("primary"))
                .cloned(),
            Some("#b97fd8".to_string())
        );
    }

    #[test]
    fn installed_presets_cover_every_installed_file() {
        let files = crate::language_data::language_data_after_preflight()
            .themes
            .len();
        assert!(files > 0);
        assert_eq!(
            ThemePresets::installed().len(),
            files,
            "one preset per file, names unique"
        );
    }

    fn carried(schema: &OrbitalSchema) -> Vec<String> {
        schema
            .theme_definitions
            .iter()
            .flatten()
            .map(|d| d.name.clone())
            .collect()
    }

    #[test]
    fn carries_referenced_presets_and_the_default_once() {
        let mut schema = minimal_schema(
            Some(ThemeRef::Reference("clay-dark".to_string())),
            vec![
                minimal_orbital("A", Some(ThemeRef::Reference("clay-light".to_string()))),
                minimal_orbital("B", Some(ThemeRef::Reference("aqua".to_string()))),
            ],
        );
        carry_theme_definitions(&mut schema, &presets());
        assert_eq!(carried(&schema), vec!["aqua", "clay", DEFAULT_THEME_BASE]);
    }

    #[test]
    fn control_no_declared_theme_carries_only_the_default() {
        let mut schema = minimal_schema(None, vec![minimal_orbital("A", None)]);
        carry_theme_definitions(&mut schema, &presets());
        assert_eq!(carried(&schema), vec![DEFAULT_THEME_BASE]);
    }

    #[test]
    fn edge_an_unknown_key_carries_nothing_for_it_and_resolves_as_before() {
        let mut schema = minimal_schema(
            Some(ThemeRef::Reference("zz-unknown-dark".to_string())),
            vec![minimal_orbital("A", None)],
        );
        let before = resolve_theme(&schema, Some("A"), &presets(), None);
        carry_theme_definitions(&mut schema, &presets());
        assert_eq!(carried(&schema), vec![DEFAULT_THEME_BASE]);
        let from_program =
            ThemePresets::from_definitions(schema.theme_definitions.iter().flatten());
        let after = resolve_theme(&schema, Some("A"), &from_program, None);
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(&after).unwrap()
        );
    }

    #[test]
    fn the_program_alone_resolves_like_the_installed_presets() {
        let mut schema = minimal_schema(
            Some(ThemeRef::Reference("clay-dark".to_string())),
            vec![
                minimal_orbital("A", None),
                minimal_orbital("B", Some(ThemeRef::Reference("aqua-light".to_string()))),
            ],
        );
        carry_theme_definitions(&mut schema, &presets());
        let from_program =
            ThemePresets::from_definitions(schema.theme_definitions.iter().flatten());
        for orbital in ["A", "B"] {
            let installed = resolve_theme(&schema, Some(orbital), &presets(), None);
            let carried = resolve_theme(&schema, Some(orbital), &from_program, None);
            assert_eq!(
                serde_json::to_value(&installed).unwrap(),
                serde_json::to_value(&carried).unwrap(),
                "{orbital}"
            );
        }
    }

    #[test]
    fn resolved_theme_serde_round_trip() {
        let presets = presets();
        let schema = minimal_schema(
            Some(ThemeRef::Reference("clay-dark".to_string())),
            vec![minimal_orbital("A", None)],
        );
        let resolved = resolve_theme(&schema, Some("A"), &presets, None);
        let json = serde_json::to_value(&resolved).expect("serializes");
        let back: ResolvedTheme = serde_json::from_value(json.clone()).expect("deserializes");
        assert_eq!(serde_json::to_value(&back).unwrap(), json);
    }
}
