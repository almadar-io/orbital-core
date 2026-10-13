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
    base: Option<&crate::schema::ThemeMap>,
    variant: Option<&crate::schema::ThemeMap>,
) -> Option<crate::schema::ThemeMap> {
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

// ---------------------------------------------------------------------------
// Tokens -> CSS custom properties (`themeTokensToCssVars` in @almadar/ui)
// ---------------------------------------------------------------------------

/// CSS custom properties keyed by variable name, in emission order.
pub type CssVars = indexmap::IndexMap<String, String>;

mod css_axes {
    use super::CssVars;
    use crate::schema::{
        DensityTokens, DiagramTokens, ElevationTokens, EnterAnimation, GeometryTokens, IconFamily,
        IconographyTokens, IllustrationTokens, MotionDurationKey, MotionEasingKey, MotionIntent,
        MotionTokens, SceneTokens, SurfaceTokens, TypeIntent, TypeScaleTokens, TypeSizeKey,
        TypeWeight,
    };

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

    pub(super) fn density(d: Option<&DensityTokens>, vars: &mut CssVars) {
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

    pub(super) fn type_scale(t: Option<&TypeScaleTokens>, vars: &mut CssVars) {
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

    pub(super) fn motion(m: Option<&MotionTokens>, vars: &mut CssVars) {
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

    pub(super) fn iconography(i: Option<&IconographyTokens>, vars: &mut CssVars) {
        let Some(i) = i else { return };
        put(vars, "--icon-family", i.family.as_ref().map(icon_family));
        put(vars, "--icon-stroke-width", i.stroke_width.as_deref());
        put(vars, "--icon-default-size", i.default_size.as_deref());
    }

    pub(super) fn elevation(e: Option<&ElevationTokens>, vars: &mut CssVars) {
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

    pub(super) fn geometry(g: Option<&GeometryTokens>, vars: &mut CssVars) {
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

    pub(super) fn illustration(i: Option<&IllustrationTokens>, vars: &mut CssVars) {
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

    pub(super) fn surface(s: Option<&SurfaceTokens>, vars: &mut CssVars) {
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

    pub(super) fn diagram(d: Option<&DiagramTokens>, vars: &mut CssVars) {
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

    pub(super) fn scene(s: Option<&SceneTokens>, vars: &mut CssVars) {
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
}

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
    let mut vars = CssVars::new();
    let dark = if mode == ThemeMode::Dark {
        dark_variant
    } else {
        None
    };

    fn map_vars(vars: &mut CssVars, prefix: &str, map: Option<&crate::schema::ThemeMap>) {
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

#[cfg(test)]
mod css_vars_tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    fn run(tokens: Value, mode: ThemeMode, variant: Option<Value>) -> BTreeMap<String, String> {
        let tokens: ThemeTokens = serde_json::from_value(tokens).unwrap();
        let variant: Option<ThemeVariant> = variant.map(|v| serde_json::from_value(v).unwrap());
        css_vars(&tokens, mode, variant.as_ref())
            .into_iter()
            .collect()
    }

    fn light(tokens: Value) -> BTreeMap<String, String> {
        run(tokens, ThemeMode::Light, None)
    }

    fn expect(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // themeTokens.test.ts
    #[test]
    fn legacy_maps_use_their_prefixes() {
        assert_eq!(
            light(json!({"colors": {"primary": "#14b8a6", "primary-foreground": "#ffffff"}})),
            expect(&[
                ("--color-primary", "#14b8a6"),
                ("--color-primary-foreground", "#ffffff")
            ])
        );
        assert_eq!(
            light(json!({"radii": {"sm": "4px", "full": "9999px"}})),
            expect(&[("--radius-sm", "4px"), ("--radius-full", "9999px")])
        );
        assert_eq!(
            light(json!({"spacing": {"1": "4px"}})),
            expect(&[("--space-1", "4px")])
        );
        assert_eq!(
            light(json!({"typography": {"font-family": "Inter", "line-height-base": "1.5"}})),
            expect(&[("--font-family", "Inter"), ("--line-height-base", "1.5")])
        );
        assert_eq!(
            light(json!({"shadows": {"sm": "0 1px 2px #000", "main": "0 4px 8px #000"}})),
            expect(&[
                ("--shadow-sm", "0 1px 2px #000"),
                ("--shadow-main", "0 4px 8px #000")
            ])
        );
    }

    #[test]
    fn empty_tokens_emit_nothing_and_only_set_keys_appear() {
        assert!(light(json!({})).is_empty());
        let v = css_vars(
            &serde_json::from_value(json!({"colors": {"primary": "#ff0000"}})).unwrap(),
            ThemeMode::Light,
            None,
        );
        assert_eq!(v.keys().collect::<Vec<_>>(), vec!["--color-primary"]);
    }

    #[test]
    fn dark_variant_layers_over_base_colors() {
        let got = run(
            json!({"colors": {"primary": "#14b8a6", "background": "#ffffff"}}),
            ThemeMode::Dark,
            Some(json!({"colors": {"background": "#000000"}})),
        );
        assert_eq!(
            got,
            expect(&[
                ("--color-background", "#000000"),
                ("--color-primary", "#14b8a6")
            ])
        );
    }

    #[test]
    fn dark_mode_uses_base_radii_when_variant_omits_them() {
        let got = run(
            json!({"radii": {"md": "8px"}}),
            ThemeMode::Dark,
            Some(json!({"colors": {"background": "#000"}})),
        );
        assert_eq!(
            got,
            expect(&[("--color-background", "#000"), ("--radius-md", "8px")])
        );
    }

    #[test]
    fn control_light_mode_ignores_the_dark_variant() {
        let got = run(
            json!({"colors": {"background": "#fff"}}),
            ThemeMode::Light,
            Some(json!({"colors": {"background": "#000"}})),
        );
        assert_eq!(got, expect(&[("--color-background", "#fff")]));
    }

    // themeTokens.skin-axes.test.ts
    #[test]
    fn density_axis() {
        assert_eq!(
            light(
                json!({"density": {"spacing": {"space0": "0px", "space3": "12px", "space12": "48px"}}})
            ),
            expect(&[
                ("--space-0", "0px"),
                ("--space-3", "12px"),
                ("--space-12", "48px")
            ])
        );
        assert_eq!(
            light(
                json!({"density": {"buttonHeightMd": "36px", "inputHeightLg": "44px", "rowHeightCompact": "32px"}})
            ),
            expect(&[
                ("--button-height-md", "36px"),
                ("--input-height-lg", "44px"),
                ("--row-height-compact", "32px")
            ])
        );
        assert_eq!(
            light(
                json!({"density": {"cardPaddingMd": "16px", "dialogPadding": "24px", "sectionGap": "32px"}})
            ),
            expect(&[
                ("--card-padding-md", "16px"),
                ("--dialog-padding", "24px"),
                ("--section-gap", "32px")
            ])
        );
        let only = css_vars(
            &serde_json::from_value(json!({"density": {"spacing": {"space3": "12px"}}})).unwrap(),
            ThemeMode::Light,
            None,
        );
        assert_eq!(only.keys().collect::<Vec<_>>(), vec!["--space-3"]);
    }

    #[test]
    fn type_scale_families_scale_and_intents() {
        assert_eq!(
            light(
                json!({"typeScale": {"displayFamily": "\"Fraunces\", serif", "bodyFamily": "\"Inter\", sans-serif", "monoFamily": "mono"}})
            ),
            expect(&[
                ("--font-family-display", "\"Fraunces\", serif"),
                ("--font-family-body", "\"Inter\", sans-serif"),
                ("--font-family-mono", "mono"),
            ])
        );
        assert_eq!(
            light(json!({"typeScale": {"scale": {
                "sm": {"size": "14px", "lineHeight": "20px"},
                "2xl": {"size": "24px", "lineHeight": "32px"},
                "display-1": {"size": "48px", "lineHeight": "52px"}}}})),
            expect(&[
                ("--text-sm", "14px"),
                ("--leading-sm", "20px"),
                ("--text-2xl", "24px"),
                ("--leading-2xl", "32px"),
                ("--text-display-1", "48px"),
                ("--leading-display-1", "52px"),
            ])
        );
        assert_eq!(
            light(json!({"typeScale": {"intents": {
                "headingMajor": {"slot": "display", "size": "2xl", "weight": "bold"},
                "caption": {"slot": "body", "size": "xs", "weight": "normal"}}}})),
            expect(&[
                ("--intent-heading-major-size", "var(--text-2xl)"),
                ("--intent-heading-major-weight", "var(--font-weight-bold)"),
                ("--intent-heading-major-leading", "var(--leading-2xl)"),
                ("--intent-caption-size", "var(--text-xs)"),
                ("--intent-caption-weight", "var(--font-weight-normal)"),
                ("--intent-caption-leading", "var(--leading-xs)"),
            ])
        );
    }

    #[test]
    fn motion_palettes_and_intents() {
        assert_eq!(
            light(
                json!({"motion": {"durations": {"fast": "100ms", "normal": "200ms", "dramatic": "600ms"}}})
            ),
            expect(&[
                ("--duration-fast", "100ms"),
                ("--duration-normal", "200ms"),
                ("--duration-dramatic", "600ms")
            ])
        );
        assert_eq!(
            light(
                json!({"motion": {"easings": {"linear": "linear", "spring": "cubic-bezier(0.34, 1.56, 0.64, 1)"}}})
            ),
            expect(&[
                ("--easing-linear", "linear"),
                ("--easing-spring", "cubic-bezier(0.34, 1.56, 0.64, 1)")
            ])
        );
        assert_eq!(
            light(json!({"motion": {"intents": {
                "enter": {"duration": "fast", "easing": "emphasized"},
                "press": {"duration": "instant", "easing": "linear"}}}})),
            expect(&[
                ("--intent-enter-duration", "var(--duration-fast)"),
                ("--intent-enter-easing", "var(--easing-emphasized)"),
                ("--intent-press-duration", "var(--duration-instant)"),
                ("--intent-press-easing", "var(--easing-linear)"),
            ])
        );
    }

    #[test]
    fn iconography_every_family_passes_through() {
        assert_eq!(
            light(
                json!({"iconography": {"family": "phosphor-duotone", "strokeWidth": "1.5", "defaultSize": "20px"}})
            ),
            expect(&[
                ("--icon-family", "phosphor-duotone"),
                ("--icon-stroke-width", "1.5"),
                ("--icon-default-size", "20px")
            ])
        );
        for f in [
            "lucide",
            "phosphor-outline",
            "phosphor-fill",
            "phosphor-duotone",
            "tabler",
            "fa-solid",
        ] {
            assert_eq!(
                light(json!({"iconography": {"family": f}})),
                expect(&[("--icon-family", f)])
            );
        }
    }

    #[test]
    fn elevation_and_geometry_axes() {
        assert_eq!(
            light(
                json!({"elevation": {"cardElevation": "var(--shadow-sm)", "popoverElevation": "var(--shadow-main)", "dialogElevation": "var(--shadow-lg)", "toastElevation": "none"}})
            ),
            expect(&[
                ("--elevation-card", "var(--shadow-sm)"),
                ("--elevation-popover", "var(--shadow-main)"),
                ("--elevation-dialog", "var(--shadow-lg)"),
                ("--elevation-toast", "none"),
            ])
        );
        assert_eq!(
            light(
                json!({"geometry": {"radiusContainer": "12px", "radiusInteractive": "9999px", "radiusPill": "9999px", "borderHairline": "1px", "borderStandard": "2px", "borderHeavy": "3px"}})
            ),
            expect(&[
                ("--radius-container", "12px"),
                ("--radius-interactive", "9999px"),
                ("--radius-pill", "9999px"),
                ("--border-hairline", "1px"),
                ("--border-standard", "2px"),
                ("--border-heavy", "3px"),
            ])
        );
    }

    #[test]
    fn dark_cascade_per_axis_and_unset_axes() {
        let got = run(
            json!({"density": {"buttonHeightMd": "36px"}, "iconography": {"family": "lucide", "defaultSize": "16px"}}),
            ThemeMode::Dark,
            Some(json!({"density": {"buttonHeightMd": "40px"}})),
        );
        assert_eq!(
            got,
            expect(&[
                ("--button-height-md", "40px"),
                ("--icon-family", "lucide"),
                ("--icon-default-size", "16px")
            ])
        );
        assert!(light(json!({})).is_empty());
    }

    #[test]
    fn combined_axis_fixture() {
        let got = light(json!({
            "density": {"spacing": {"space3": "12px", "space6": "24px"}, "buttonHeightMd": "36px"},
            "typeScale": {"bodyFamily": "\"Inter\", sans-serif",
                "scale": {"base": {"size": "16px", "lineHeight": "24px"}},
                "intents": {"bodyDefault": {"slot": "body", "size": "base", "weight": "normal"}}},
            "motion": {"durations": {"normal": "200ms"}, "intents": {"hover": {"duration": "fast", "easing": "standard"}}},
            "iconography": {"family": "lucide"},
            "elevation": {"cardElevation": "var(--shadow-sm)"},
            "geometry": {"radiusContainer": "8px", "borderHairline": "1px"}
        }));
        assert_eq!(
            got,
            expect(&[
                ("--space-3", "12px"),
                ("--space-6", "24px"),
                ("--button-height-md", "36px"),
                ("--font-family-body", "\"Inter\", sans-serif"),
                ("--text-base", "16px"),
                ("--leading-base", "24px"),
                ("--intent-body-default-size", "var(--text-base)"),
                ("--intent-body-default-weight", "var(--font-weight-normal)"),
                ("--intent-body-default-leading", "var(--leading-base)"),
                ("--duration-normal", "200ms"),
                ("--intent-hover-duration", "var(--duration-fast)"),
                ("--intent-hover-easing", "var(--easing-standard)"),
                ("--icon-family", "lucide"),
                ("--elevation-card", "var(--shadow-sm)"),
                ("--radius-container", "8px"),
                ("--border-hairline", "1px"),
            ])
        );
    }

    // themeTokens.range-axes.test.ts
    #[test]
    fn range_axes() {
        assert_eq!(
            light(json!({"typeScale": {
                "displayFamily": "\"Limelight\", serif",
                "fontImport": "https://fonts.googleapis.com/css2?family=Limelight&display=swap",
                "headingWeight": "400", "headingTransform": "uppercase", "headingTracking": "0.2em",
                "headingStyle": "italic", "headingShadow": "0 0 6px currentColor"}})),
            expect(&[
                ("--font-family-display", "\"Limelight\", serif"),
                ("--heading-weight", "400"),
                ("--heading-transform", "uppercase"),
                ("--heading-tracking", "0.2em"),
                ("--heading-style", "italic"),
                ("--heading-shadow", "0 0 6px currentColor"),
            ])
        );
        assert_eq!(
            light(
                json!({"geometry": {"cornerShape": "bevel", "cornerShapePill": "round", "cornerShapeInteractive": "round", "borderStyle": "double", "borderStyleInteractive": "outset"}})
            ),
            expect(&[
                ("--corner-shape", "bevel"),
                ("--corner-shape-pill", "round"),
                ("--corner-shape-interactive", "round"),
                ("--border-style", "double"),
                ("--border-style-interactive", "outset"),
            ])
        );
        assert_eq!(
            light(
                json!({"elevation": {"interactiveElevation": "2px 2px 0 #000", "pressedElevation": "inset 2px 2px 0 #000"}})
            ),
            expect(&[
                ("--elevation-interactive", "2px 2px 0 #000"),
                ("--elevation-pressed", "inset 2px 2px 0 #000")
            ])
        );
        let surface = json!({"backdrop": "blur(18px) saturate(1.6)", "cardImage": "linear-gradient(#fff6, #fff0)",
            "pageImage": "radial-gradient(#0002 1px, transparent 1px)", "pageImageSize": "8px 8px"});
        assert_eq!(
            light(json!({"surface": surface})),
            expect(&[
                ("--surface-backdrop", "blur(18px) saturate(1.6)"),
                ("--surface-card-image", "linear-gradient(#fff6, #fff0)"),
                (
                    "--surface-page-image",
                    "radial-gradient(#0002 1px, transparent 1px)"
                ),
                ("--surface-page-image-size", "8px 8px"),
            ])
        );
    }

    #[test]
    fn dark_surface_replaces_base_and_absent_dark_surface_falls_back() {
        let tokens = json!({"surface": {"backdrop": "blur(18px)", "cardImage": "x"}});
        assert_eq!(
            run(
                tokens.clone(),
                ThemeMode::Dark,
                Some(json!({"surface": {"backdrop": "blur(4px)"}}))
            ),
            expect(&[("--surface-backdrop", "blur(4px)")])
        );
        assert_eq!(
            run(tokens.clone(), ThemeMode::Dark, Some(json!({}))),
            light(tokens)
        );
    }

    #[test]
    fn control_no_range_axes_and_empty_string_passes_through() {
        let got = light(json!({"geometry": {"radiusContainer": "8px"}}));
        assert!(got.keys().all(|k| !k.contains("corner")
            && !k.contains("border-style")
            && !k.contains("surface")));
        assert_eq!(
            light(json!({"geometry": {"borderStyle": ""}})),
            expect(&[("--border-style", "")])
        );
    }

    #[test]
    fn motion_shapes_entry_busy_delay_and_skeleton() {
        assert_eq!(
            light(
                json!({"motion": {"shapes": {"modalEnter": "scale(0.9)", "pageEnter": "none", "toastEnter": "translateX(24px)"}}})
            ),
            expect(&[
                ("--motion-modal-enter-from-transform", "scale(0.9)"),
                ("--motion-toast-enter-from-transform", "translateX(24px)"),
                ("--motion-page-enter-from-transform", "none"),
            ])
        );
        assert!(light(json!({"motion": {"durations": {"normal": "200ms"}}}))
            .keys()
            .all(|k| !k.starts_with("--motion-")));
        let v = light(
            json!({"motion": {"entry": {"default": "rise", "stagger": "60ms"}, "shapes": {"enterRise": "translateY(24px)"}}}),
        );
        assert_eq!(v["--motion-enter-default"], "almadar-enter-rise");
        assert_eq!(v["--motion-enter-stagger"], "60ms");
        assert_eq!(v["--motion-enter-rise-from-transform"], "translateY(24px)");
        assert_eq!(
            light(json!({"motion": {"busyDelay": "500ms"}}))["--motion-busy-delay"],
            "500ms"
        );
        assert_eq!(
            light(json!({"motion": {"entry": {"default": "none"}}}))["--motion-enter-default"],
            "none"
        );
        let skeleton = light(json!({"motion": {"awaitingSkeleton": true}}));
        assert_eq!(skeleton["--awaiting-skeleton-display"], "block");
        assert_eq!(
            light(json!({"motion": {"awaitingSkeleton": false}}))["--awaiting-skeleton-display"],
            "none"
        );
        let none = light(json!({"motion": {"durations": {"fast": "100ms"}}}));
        assert!(
            !none.contains_key("--motion-enter-default")
                && !none.contains_key("--motion-enter-stagger")
        );
    }

    // themeTokens.diagram-axes.test.ts
    #[test]
    fn diagram_scene_and_canvas_ground() {
        assert_eq!(
            light(json!({"diagram": {
                "strokeThin": "1px", "strokeNormal": "2px", "strokeBold": "4px", "lineCap": "square", "lineJoin": "miter",
                "dash": "8 4", "dot": "1 4", "fillStyle": "outline", "fillOpacity": "0.25", "roughness": "1.5", "glow": "6",
                "shadow": "2px 2px 0 #000", "marker": "open", "labelFont": "mono", "labelSize": "sm", "labelWeight": "600",
                "labelCase": "uppercase", "corner": "0px"}})),
            expect(&[
                ("--diagram-stroke-thin", "1px"),
                ("--diagram-stroke-normal", "2px"),
                ("--diagram-stroke-bold", "4px"),
                ("--diagram-line-cap", "square"),
                ("--diagram-line-join", "miter"),
                ("--diagram-dash", "8 4"),
                ("--diagram-dot", "1 4"),
                ("--diagram-fill-style", "outline"),
                ("--diagram-fill-opacity", "0.25"),
                ("--diagram-roughness", "1.5"),
                ("--diagram-glow", "6"),
                ("--diagram-shadow", "2px 2px 0 #000"),
                ("--diagram-marker", "open"),
                ("--diagram-label-font", "mono"),
                ("--diagram-label-size", "sm"),
                ("--diagram-label-weight", "600"),
                ("--diagram-label-case", "uppercase"),
                ("--diagram-corner", "0px"),
            ])
        );
        assert_eq!(
            light(json!({"scene": {
                "materialRoughness": "0.8", "materialMetalness": "0.1", "materialFlat": "1", "materialOutline": "2",
                "lightAmbient": "0.6", "lightKey": "1.2", "lightKeyColor": "#fff4e0", "fog": "0.2"}})),
            expect(&[
                ("--material-roughness", "0.8"),
                ("--material-metalness", "0.1"),
                ("--material-flat", "1"),
                ("--material-outline", "2"),
                ("--light-ambient", "0.6"),
                ("--light-key", "1.2"),
                ("--light-key-color", "#fff4e0"),
                ("--scene-fog", "0.2"),
            ])
        );
        assert_eq!(
            light(json!({"surface": {"diagram": "var(--color-card)", "backdrop": "blur(4px)"}})),
            expect(&[
                ("--surface-backdrop", "blur(4px)"),
                ("--surface-diagram", "var(--color-card)")
            ])
        );
        assert_eq!(
            light(json!({"diagram": {"glow": "8"}})),
            expect(&[("--diagram-glow", "8")])
        );
        let control = light(json!({"geometry": {"cornerShape": "round"}}));
        assert!(control.keys().all(|k| !k.starts_with("--diagram-")
            && !k.starts_with("--material-")
            && !k.starts_with("--light-")
            && k != "--scene-fog"
            && k != "--surface-diagram"));
    }

    #[test]
    fn dark_variant_replaces_the_whole_drawing_axis() {
        let tokens = json!({
            "diagram": {"glow": "6", "strokeNormal": "2px"},
            "scene": {"lightAmbient": "0.6", "materialRoughness": "0.8"},
            "surface": {"diagram": "var(--color-card)"}
        });
        let got = run(
            tokens,
            ThemeMode::Dark,
            Some(json!({"diagram": {"glow": "10"}, "scene": {"lightAmbient": "0.3"}})),
        );
        assert_eq!(got["--diagram-glow"], "10");
        assert!(!got.contains_key("--diagram-stroke-normal"));
        assert_eq!(got["--light-ambient"], "0.3");
        assert!(!got.contains_key("--material-roughness"));
        assert_eq!(got["--surface-diagram"], "var(--color-card)");
    }

    #[test]
    fn series_and_diagram_colors_ride_the_colors_map() {
        let got = light(json!({"colors": {"series-1": "#123456", "diagram-ink": "#000"}}));
        assert_eq!(got["--color-series-1"], "#123456");
        assert_eq!(got["--color-diagram-ink"], "#000");
    }

    // themeTokens.band-axes.test.ts
    #[test]
    fn surface_band_axes() {
        let got = light(json!({"surface": {
            "edgeMask": "url(\"a.svg\")", "edgeHeight": "40px", "textureMask": "url(\"b.svg\")",
            "textureSize": "24px 24px", "accentImage": "linear-gradient(red, blue)", "scrim": "linear-gradient(black, transparent)"}}));
        assert_eq!(got["--surface-edge-mask"], "url(\"a.svg\")");
        assert_eq!(got["--surface-edge-height"], "40px");
        assert_eq!(got["--surface-texture-mask"], "url(\"b.svg\")");
        assert_eq!(got["--surface-texture-size"], "24px 24px");
        assert_eq!(got["--surface-accent-image"], "linear-gradient(red, blue)");
        assert_eq!(
            got["--surface-scrim"],
            "linear-gradient(black, transparent)"
        );
        let bare = light(json!({"surface": {"backdrop": "none"}}));
        assert!(
            !bare.contains_key("--surface-edge-mask")
                && !bare.contains_key("--surface-accent-image")
        );
        let dark = run(
            json!({"surface": {"edgeMask": "url(\"light.svg\")"}}),
            ThemeMode::Dark,
            Some(json!({"surface": {"edgeMask": "url(\"dark.svg\")"}})),
        );
        assert_eq!(dark["--surface-edge-mask"], "url(\"dark.svg\")");
    }

    // themeTokens.illustration-axis.test.ts
    #[test]
    fn illustration_loading_artwork() {
        let art = "data:image/svg+xml;utf8,%3Csvg%3E%3C/svg%3E";
        let got = light(json!({"illustration": {"loadingAsset": art}}));
        assert_eq!(got["--illustration-loading"], format!("url(\"{art}\")"));
        assert_eq!(got["--loader-mark-display"], "inline-block");
        assert_eq!(got["--loader-icon-display"], "none");
        let dark_art = "data:image/svg+xml;utf8,%3Csvg%20id=%22d%22%3E%3C/svg%3E";
        let dark = run(
            json!({"illustration": {"loadingAsset": art}}),
            ThemeMode::Dark,
            Some(json!({"illustration": {"loadingAsset": dark_art}})),
        );
        assert_eq!(
            dark["--illustration-loading"],
            format!("url(\"{dark_art}\")")
        );
        let none = light(json!({"illustration": {"style": "minimal"}}));
        assert!(
            !none.contains_key("--illustration-loading")
                && !none.contains_key("--loader-icon-display")
        );
    }
}
