//! Core schema types for Orbital
//!
//! These types match the TypeScript OrbitalSchema definitions and can parse `.orb` files.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};

use super::SExpression;
use crate::effects::EffectRow;

/// `ConfigField.default`'s deserializer: distinguishes an ABSENT `default`
/// key (→ `None`, via `#[serde(default)]` on the field, which short-circuits
/// before this ever runs) from a key PRESENT with JSON value `null` (→
/// `Some(SExpression::Null)`). Without this, deriving `Option<SExpression>`
/// directly hands `null` to `Option`'s own `Deserialize` impl, which treats
/// ANY `null` as `None` regardless of `SExpression`'s own `Null` variant —
/// collapsing "no default was ever declared" and "the default IS the
/// unbound sentinel" (a `trait`-typed knob's `none`, owner ruling
/// 2026-09-13) into the same on-disk shape. Every consumer that reads a
/// `.orb` registry file (a downstream `uses` load, `orb validate`'s
/// re-inline, the runtime) needs the distinction preserved through the
/// round trip, not just within one process's in-memory compile.
fn deserialize_present_default<'de, D>(deserializer: D) -> Result<Option<SExpression>, D::Error>
where
    D: Deserializer<'de>,
{
    SExpression::deserialize(deserializer).map(Some)
}

// =============================================================================
// Root Schema
// =============================================================================

/// The root OrbitalSchema type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbitalSchema {
    /// Application name
    pub name: String,

    /// Schema version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Application description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// App-level theme — the skin every orbital's `@currentTheme` seeds from
    /// unless the orbital declares its own `theme` (precedence: orbital →
    /// app → `DEFAULT_THEME_KEY`). Authored as `theme "<key>"` in the
    /// `.lolo` app header; a full `<base>-<light|dark>` registry key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeRef>,

    /// The theme presets this program references plus the runtime default,
    /// written by `orb` when it compiles the program for a runtime, so a kernel
    /// with no installed `@almadar/core` (wasm, mobile) resolves `@currentTheme`
    /// from the program alone. Never authored.
    #[serde(
        rename = "themeDefinitions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub theme_definitions: Option<Vec<ThemeDefinition>>,

    /// Organism-wide declared config knobs (Almadar_Orbital_Import.md §4.5). Same
    /// `ConfigField` value type as `TraitDefinition.config` and `Orbital.config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, ConfigField>>,

    /// List of orbitals (feature units)
    pub orbitals: Vec<Orbital>,

    /// `locales [en, ar, sl]` — the locales this program ships. Every
    /// `(i18n/t …)` key must exist in the catalog of each one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locales: Option<Vec<String>>,

    /// `public "./public"` — a folder beside the program copied verbatim to
    /// the built site's root (logos, favicons, downloads).
    #[serde(rename = "publicDir", default, skip_serializing_if = "Option::is_none")]
    pub public_dir: Option<String>,

    /// Site modifiers (`origin`, `icon`, `siteName`, `signIn`) from the app bracket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<SiteMeta>,

    /// Pages served by `uses lazy` behaviors, each loaded from its own `.orb`
    /// when opened.
    #[serde(rename = "lazyPages", default, skip_serializing_if = "Option::is_none")]
    pub lazy_pages: Option<Vec<LazyPage>>,

    /// Schema metadata
    #[serde(rename = "_metadata", skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SchemaMetadata>,

    /// V4 IR schema version — optional until the Phase-7 flip.
    #[serde(
        rename = "schemaVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub schema_version: Option<u32>,

    /// V4 identity ledger slice (id → name history) — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<crate::identity::IdentityLedger>,
}

impl OrbitalSchema {
    /// An orbital's inline trait with no `linkedEntity` binds its host orbital's
    /// entity (name + id) — the default `@almadar/runtime`'s `buildTraitIndex`
    /// applies, settled once so the kernel, every shell and every validator
    /// read one declared binding.
    pub fn bind_unlinked_traits_to_host_entity(&mut self) {
        for orbital in &mut self.orbitals {
            let EntityRef::Inline(host) = &orbital.entity else {
                continue;
            };
            let (name, id) = (host.name.clone(), host.id.clone());
            for t in &mut orbital.traits {
                if let TraitReference::Inline(td) = t {
                    if td.linked_entity.is_none() {
                        td.linked_entity = Some(name.clone());
                        td.linked_entity_id = id.clone();
                    }
                }
            }
        }
    }
}

/// Schema metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchemaMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,

    /// Config→knob provenance for `@config.X` bindings that the inline phase
    /// substituted into render-ui props, keyed by orbital name. After
    /// substitution the `@config.X` string is gone, so a consumer (Studio
    /// contextual edit) reads this to trace a resolved render-ui prop back to
    /// the config knob it came from. See [`ConfigProvenanceRecord`].
    #[serde(rename = "configProvenance", skip_serializing_if = "Option::is_none")]
    pub config_provenance: Option<HashMap<String, Vec<ConfigProvenanceRecord>>>,

    /// For a program that declares `locales`: each loaded behavior's name →
    /// the folder of its source file, relative to the program's own folder.
    /// Its message catalogs live at `<folder>/locales/<locale>/<name>.json`.
    #[serde(
        rename = "messageSources",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub message_sources: Option<std::collections::BTreeMap<String, String>>,
}

/// One `@config.<knob>` → render-ui prop substitution recorded at inline time.
/// Emitted under [`SchemaMetadata::config_provenance`] so a resolved literal
/// can be traced back to its originating config knob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProvenanceRecord {
    /// Resolved trait name (post call-site rename) owning the render-ui effect.
    #[serde(rename = "trait")]
    pub trait_name: String,
    /// Pattern path within the render-ui tree (e.g. `root.children.0`), matching
    /// the runtime `UISlotRenderer` `${parentPath}.children.${index}` convention.
    #[serde(rename = "patternPath", skip_serializing_if = "Option::is_none")]
    pub pattern_path: Option<String>,
    /// The pattern-node prop key whose value was a `@config.X` binding.
    pub prop: String,
    /// The config knob name (`X` in `@config.X`).
    pub knob: String,
}

// =============================================================================
// Theme Definition — Skin axes (Layer 1 visual variation)
//
// Mirrors `@almadar/core`'s ThemeTokens extension. See
// `docs/Almadar_Std_Variations.md` §2 (Skin layer) and
// `packages/almadar-ui/themes/_contract.md` for the CSS variable contract.
//
// JSON casing: ThemeTokens / ThemeVariant accept camelCase keys for new
// fields (`density`, `typeScale`, `motion`, `iconography`, `elevation`,
// `geometry`) and the nested sub-structs use camelCase for their fields
// (`buttonHeightMd`, `headingMajor`, etc.). Rust fields stay snake_case
// and rely on serde renames.
// =============================================================================

/// Spacing scale — each step pairs with a CSS `--space-N` variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpacingScale {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space0: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space2: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space3: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space6: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space7: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space8: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space9: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space10: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space11: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space12: Option<String>,
}

/// Density axis — spacing rhythm + per-element heights and paddings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DensityTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing: Option<SpacingScale>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button_height_sm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button_height_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button_height_lg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_height_sm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_height_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_height_lg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_height_compact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_height_normal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_height_spacious: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_padding_sm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_padding_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_padding_lg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialog_padding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_gap: Option<String>,
}

/// Single entry in the type scale — size + matching line-height.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeScaleEntry {
    pub size: String,
    pub line_height: String,
}

/// Type slot (which family the intent renders in).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypeSlot {
    Display,
    Body,
    Mono,
}

/// Type size key from the scale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeSizeKey {
    #[serde(rename = "xs")]
    Xs,
    #[serde(rename = "sm")]
    Sm,
    #[serde(rename = "base")]
    Base,
    #[serde(rename = "lg")]
    Lg,
    #[serde(rename = "xl")]
    Xl,
    #[serde(rename = "2xl")]
    TwoXl,
    #[serde(rename = "3xl")]
    ThreeXl,
    #[serde(rename = "4xl")]
    FourXl,
    #[serde(rename = "display-1")]
    Display1,
    #[serde(rename = "display-2")]
    Display2,
}

/// Type weight key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypeWeight {
    Normal,
    Medium,
    Bold,
}

/// Intent → (slot, size, weight) mapping. Authors write intent in render-ui; skin resolves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeIntent {
    pub slot: TypeSlot,
    pub size: TypeSizeKey,
    pub weight: TypeWeight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeScale {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xs: Option<TypeScaleEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sm: Option<TypeScaleEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<TypeScaleEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lg: Option<TypeScaleEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xl: Option<TypeScaleEntry>,
    #[serde(rename = "2xl", skip_serializing_if = "Option::is_none")]
    pub two_xl: Option<TypeScaleEntry>,
    #[serde(rename = "3xl", skip_serializing_if = "Option::is_none")]
    pub three_xl: Option<TypeScaleEntry>,
    #[serde(rename = "4xl", skip_serializing_if = "Option::is_none")]
    pub four_xl: Option<TypeScaleEntry>,
    #[serde(rename = "display-1", skip_serializing_if = "Option::is_none")]
    pub display_1: Option<TypeScaleEntry>,
    #[serde(rename = "display-2", skip_serializing_if = "Option::is_none")]
    pub display_2: Option<TypeScaleEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeIntentMap {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_major: Option<TypeIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_minor: Option<TypeIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_emphasis: Option<TypeIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_default: Option<TypeIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_quiet: Option<TypeIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<TypeIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numeric: Option<TypeIntent>,
}

/// Type axis — family triplet, size scale, intent mapping.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TypeScaleTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono_family: Option<String>,
    /// Stylesheet URL loading the theme's web fonts; emitted as `@import`, never a var.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_import: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_weight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_transform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_tracking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_shadow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<TypeScale>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intents: Option<TypeIntentMap>,
}

/// Duration palette key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MotionDurationKey {
    Instant,
    Fast,
    Normal,
    Slow,
    Dramatic,
}

/// Easing palette key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MotionEasingKey {
    Linear,
    Standard,
    Emphasized,
    Spring,
}

/// Per-intent motion specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionIntent {
    pub duration: MotionDurationKey,
    pub easing: MotionEasingKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionDurationPalette {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dramatic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionEasingPalette {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emphasized: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spring: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionIntentMap {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enter: Option<MotionIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<MotionIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover: Option<MotionIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub press: Option<MotionIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expand: Option<MotionIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition: Option<MotionIntent>,
}

/// Motion axis — duration palette + easing palette + per-intent mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durations: Option<MotionDurationPalette>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub easings: Option<MotionEasingPalette>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intents: Option<MotionIntentMap>,
    /// Per-surface motion shapes (modal, popover, toast, page).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shapes: Option<MotionShapeTokens>,
    /// Element entry: the default for slot content and the stagger step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<MotionEntryTokens>,
    /// How long an action runs before busy affordances show (`--motion-busy-delay`, e.g. `300ms`).
    #[serde(rename = "busyDelay", skip_serializing_if = "Option::is_none")]
    pub busy_delay: Option<String>,
    /// Whether an empty slot shows the predicted skeleton while its trait awaits the server.
    #[serde(rename = "awaitingSkeleton", skip_serializing_if = "Option::is_none")]
    pub awaiting_skeleton: Option<bool>,
}

/// `--motion-busy-delay` when a theme declares none (`@almadar/ui` `themes/_base.css`).
pub const BASE_BUSY_DELAY_MS: u64 = 300;

impl MotionTokens {
    /// `busyDelay` in milliseconds (`450ms`, `0.3s`); `None` when unset or not a plain CSS time.
    pub fn busy_delay_ms(&self) -> Option<u64> {
        css_time_ms(self.busy_delay.as_deref()?)
    }
}

/// A plain CSS time (`450ms`, `0.3s`) in milliseconds; `None` for anything else.
pub fn css_time_ms(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (number, scale) = if let Some(ms) = raw.strip_suffix("ms") {
        (ms, 1.0)
    } else {
        (raw.strip_suffix('s')?, 1000.0)
    };
    let value: f64 = number.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| (value * scale).round() as u64)
}

/// How a rendered element animates in when it mounts (`@almadar/core` `ENTER_ANIMATIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnterAnimation {
    None,
    Fade,
    Rise,
    Scale,
    Slide,
}

/// Theme-level element entry (`@almadar/core` `MotionEntryTokens`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MotionEntryTokens {
    /// Entry every slot's content plays when it changes (`--motion-enter-default`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<EnterAnimation>,
    /// Delay between consecutive staggered entries (`--motion-enter-stagger`), e.g. `40ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stagger: Option<String>,
}

/// Motion shapes — each surface's enter-from / exit-to transform. Mirrors
/// `@almadar/core`'s `MotionShapeTokens`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MotionShapeTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modal_enter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modal_exit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub popover_enter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub popover_exit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toast_enter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toast_exit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_enter: Option<String>,
    /// `rise` element-entry enter-from transform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enter_rise: Option<String>,
    /// `scale` element-entry enter-from transform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enter_scale: Option<String>,
    /// `slide` element-entry enter-from transform (inline-start side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enter_slide: Option<String>,
}

/// Icon family selector.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IconFamily {
    Lucide,
    PhosphorOutline,
    PhosphorFill,
    PhosphorDuotone,
    Tabler,
    FaSolid,
}

/// Iconography axis — icon set + stroke + default size.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IconographyTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<IconFamily>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_size: Option<String>,
}

/// Elevation axis — per-layer shadow mapping.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ElevationTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_elevation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub popover_elevation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialog_elevation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toast_elevation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_elevation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressed_elevation: Option<String>,
}

/// Geometry axis — radius rhythm + border-width rhythm with intent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeometryTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius_container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius_interactive: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius_pill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_hairline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_standard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_heavy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corner_shape: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corner_shape_pill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corner_shape_interactive: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_style_interactive: Option<String>,
}

/// Surface axis — panel/page material (glass backdrop, gloss/paper card image,
/// page pattern). Mirrors `@almadar/core`'s `SurfaceTokens`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_image_size: Option<String>,
    /// Ground a canvas, chart or diagram draws on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagram: Option<String>,
    /// Mask image of a band's curved edge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_mask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_height: Option<String>,
    /// Mask image tiled as a band texture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture_mask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture_size: Option<String>,
    /// Background image of an accent band.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent_image: Option<String>,
    /// Overlay laid over a photo so text stays readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrim: Option<String>,
}

macro_rules! token_enum {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "kebab-case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
    };
}

token_enum!(
    /// Mirrors `@almadar/core`'s `DiagramLineCap`.
    DiagramLineCap { Butt => "butt", Round => "round", Square => "square" }
);
token_enum!(
    /// Mirrors `@almadar/core`'s `DiagramLineJoin`.
    DiagramLineJoin { Miter => "miter", Round => "round", Bevel => "bevel" }
);
token_enum!(
    /// Mirrors `@almadar/core`'s `DiagramFillStyle`.
    DiagramFillStyle { Solid => "solid", Tint => "tint", Outline => "outline", Hatch => "hatch" }
);
token_enum!(
    /// Mirrors `@almadar/core`'s `DiagramMarker`.
    DiagramMarker { Triangle => "triangle", Open => "open", Line => "line", Dot => "dot" }
);
token_enum!(
    /// Mirrors `@almadar/core`'s `DiagramLabelFont`.
    DiagramLabelFont { Body => "body", Display => "display", Mono => "mono" }
);
token_enum!(
    /// Mirrors `@almadar/core`'s `DiagramLabelCase`.
    DiagramLabelCase { None => "none", Uppercase => "uppercase" }
);

/// Diagram axis — how marks are drawn on every drawing surface. Mirrors `@almadar/core`'s
/// `DiagramTokens`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiagramTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke_thin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke_normal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke_bold: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_cap: Option<DiagramLineCap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_join: Option<DiagramLineJoin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_style: Option<DiagramFillStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_opacity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<DiagramMarker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_font: Option<DiagramLabelFont>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_weight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_case: Option<DiagramLabelCase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corner: Option<String>,
}

/// Scene axis — 3D material and lighting. Mirrors `@almadar/core`'s `SceneTokens`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SceneTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material_roughness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material_metalness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material_flat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material_outline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light_ambient: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light_key_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fog: Option<String>,
}

/// Color axis — semantic + surface + feedback color tokens as a typed bundle.
///
/// Mirrors `@almadar/core`'s `ColorTokens` (`src/types/domain.ts`). Replaces the
/// free-form `colors` map on `ThemeTokens` for authors targeting the typed axis;
/// all fields are optional so a partial slice can layer onto a default.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_hover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_hover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info_foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_border: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_row_hover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_hover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_hover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Backdrop dimming behind modals, drawers and lightboxes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrim: Option<String>,
}

/// Illustration style preset for empty/loading/error/onboarding states.
///
/// Mirrors `@almadar/core`'s `IllustrationStyle`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IllustrationStyle {
    Minimal,
    Illustrated,
    Photo,
    TextOnly,
    Mascot,
}

/// Illustration axis — empty/loading/error/onboarding imagery style.
///
/// Mirrors `@almadar/core`'s `IllustrationTokens`. `style` selects the preset;
/// the per-state `*_asset` fields are optional overrides for skins that want
/// to ship custom artwork.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IllustrationTokens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<IllustrationStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_asset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loading_asset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_asset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onboarding_asset: Option<String>,
}

/// Theme token entries in authored order, so the emitted CSS is deterministic.
pub type ThemeMap = indexmap::IndexMap<String, String>;

/// Theme tokens - design system values
///
/// Layer 1 visual variation: the original `colors`/`radii`/`spacing`/`typography`/`shadows`
/// fields are kept for backward compatibility. New typed axes (`color`, `density`, `type_scale`,
/// `motion`, `iconography`, `elevation`, `geometry`, `illustration`) carry the structured token
/// surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeTokens {
    /// Color axis — typed semantic + surface + feedback colors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorTokens>,

    /// Color tokens (e.g., primary, background, foreground)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<ThemeMap>,

    /// Border radius tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radii: Option<ThemeMap>,

    /// Spacing tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing: Option<ThemeMap>,

    /// Typography tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typography: Option<ThemeMap>,

    /// Shadow tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadows: Option<ThemeMap>,

    /// Density axis — spacing rhythm, per-element heights and paddings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<DensityTokens>,

    /// Type axis — family triplet, scale, intent mapping
    #[serde(rename = "typeScale", skip_serializing_if = "Option::is_none")]
    pub type_scale: Option<TypeScaleTokens>,

    /// Motion axis — duration palette, easing palette, intent mapping
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<MotionTokens>,

    /// Iconography axis — icon family, stroke, default size
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iconography: Option<IconographyTokens>,

    /// Elevation axis — per-layer shadow mapping
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevation: Option<ElevationTokens>,

    /// Geometry axis — radius rhythm, border-width rhythm with intent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geometry: Option<GeometryTokens>,

    /// Illustration axis — empty/loading/error/onboarding imagery style
    #[serde(skip_serializing_if = "Option::is_none")]
    pub illustration: Option<IllustrationTokens>,

    /// Surface axis — panel/page material
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceTokens>,

    /// Diagram axis — how marks are drawn on every drawing surface
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagram: Option<DiagramTokens>,

    /// Scene axis — 3D material and lighting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneTokens>,
}

/// Theme variant override
///
/// Mirrors ThemeTokens shape with the same backward-compat + new-axis structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeVariant {
    /// Color axis overrides (typed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorTokens>,

    /// Color overrides for this variant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<ThemeMap>,

    /// Radius overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radii: Option<ThemeMap>,

    /// Spacing overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing: Option<ThemeMap>,

    /// Typography overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typography: Option<ThemeMap>,

    /// Shadow overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadows: Option<ThemeMap>,

    /// Density axis overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<DensityTokens>,

    /// Type axis overrides
    #[serde(rename = "typeScale", skip_serializing_if = "Option::is_none")]
    pub type_scale: Option<TypeScaleTokens>,

    /// Motion axis overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<MotionTokens>,

    /// Iconography axis overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iconography: Option<IconographyTokens>,

    /// Elevation axis overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevation: Option<ElevationTokens>,

    /// Geometry axis overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geometry: Option<GeometryTokens>,

    /// Illustration axis overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub illustration: Option<IllustrationTokens>,

    /// Surface axis — panel/page material
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceTokens>,

    /// Diagram axis — how marks are drawn on every drawing surface
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagram: Option<DiagramTokens>,

    /// Scene axis — 3D material and lighting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneTokens>,
}

/// Theme definition - design system for an orbital
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeDefinition {
    /// Theme name
    pub name: String,

    /// Human-readable name for theme pickers
    #[serde(
        rename = "displayName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,

    /// Base tokens
    pub tokens: ThemeTokens,

    /// Named variants (e.g., "dark", "high-contrast")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<HashMap<String, ThemeVariant>>,
}

/// ThemeRef - Theme can be inline definition OR reference to imported theme.
///
/// Reference format: "Alias.theme"
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)] // boxing Inline would change untagged-match sites across the tree
pub enum ThemeRef {
    /// Inline theme definition
    Inline(ThemeDefinition),
    /// Reference to imported theme: "Alias.theme"
    Reference(String),
}

impl ThemeRef {
    /// Check if this is a reference
    pub fn is_reference(&self) -> bool {
        matches!(self, ThemeRef::Reference(_))
    }

    /// Get the reference string if this is a reference
    pub fn as_reference(&self) -> Option<&str> {
        match self {
            ThemeRef::Reference(s) => Some(s),
            _ => None,
        }
    }

    /// Get the inline theme if this is inline
    pub fn as_inline(&self) -> Option<&ThemeDefinition> {
        match self {
            ThemeRef::Inline(t) => Some(t),
            _ => None,
        }
    }
}

// =============================================================================
// Service Definition
// =============================================================================

/// REST authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestAuthConfig {
    /// Authentication type
    #[serde(rename = "type")]
    pub auth_type: RestAuthType,

    /// For api-key: the query parameter or header name
    #[serde(rename = "keyName", skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,

    /// For api-key: where to place the key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<AuthKeyLocation>,

    /// Environment variable name containing the secret
    #[serde(rename = "secretEnv", skip_serializing_if = "Option::is_none")]
    pub secret_env: Option<String>,
}

/// REST authentication type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum RestAuthType {
    ApiKey,
    Bearer,
    Basic,
    OAuth2,
}

/// Where to place the API key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthKeyLocation {
    Query,
    Header,
}

/// REST service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestServiceDef {
    /// Service name
    pub name: String,

    /// Service type (must be "rest")
    #[serde(rename = "type")]
    pub service_type: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Base URL for the API
    #[serde(rename = "baseUrl")]
    pub base_url: String,

    /// Default headers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,

    /// Authentication configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<RestAuthConfig>,

    /// Timeout in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
}

/// Socket event definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketEvents {
    /// Events received from server
    pub inbound: Vec<String>,

    /// Events sent to server
    pub outbound: Vec<String>,
}

/// Socket reconnection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketReconnect {
    /// Enable automatic reconnection
    pub enabled: bool,

    /// Maximum reconnection attempts
    #[serde(rename = "maxAttempts", skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,

    /// Delay between attempts in ms
    #[serde(rename = "delayMs", skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u32>,
}

/// Socket service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketServiceDef {
    /// Service name
    pub name: String,

    /// Service type (must be "socket")
    #[serde(rename = "type")]
    pub service_type: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// WebSocket URL
    pub url: String,

    /// Event definitions
    pub events: SocketEvents,

    /// Reconnection configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<SocketReconnect>,
}

/// MCP service definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServiceDef {
    /// Service name
    pub name: String,

    /// Service type (must be "mcp")
    #[serde(rename = "type")]
    pub service_type: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Path to MCP server executable
    #[serde(rename = "serverPath")]
    pub server_path: String,

    /// Capabilities exposed by the MCP server
    pub capabilities: Vec<String>,

    /// Environment variables to pass to the MCP server
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Service definition - discriminated union by type field
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServiceDefinition {
    #[serde(rename = "rest")]
    Rest(RestServiceDefInner),
    #[serde(rename = "socket")]
    Socket(SocketServiceDefInner),
    #[serde(rename = "mcp")]
    Mcp(McpServiceDefInner),
}

/// Inner REST service definition (without type field, used in tagged enum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestServiceDefInner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<RestAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
}

/// Inner Socket service definition (without type field, used in tagged enum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketServiceDefInner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub url: String,
    pub events: SocketEvents,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<SocketReconnect>,
}

/// Inner MCP service definition (without type field, used in tagged enum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServiceDefInner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "serverPath")]
    pub server_path: String,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

impl ServiceDefinition {
    /// Get the service name
    pub fn name(&self) -> &str {
        match self {
            ServiceDefinition::Rest(s) => &s.name,
            ServiceDefinition::Socket(s) => &s.name,
            ServiceDefinition::Mcp(s) => &s.name,
        }
    }

    /// Get the service type as string
    pub fn service_type(&self) -> &str {
        match self {
            ServiceDefinition::Rest(_) => "rest",
            ServiceDefinition::Socket(_) => "socket",
            ServiceDefinition::Mcp(_) => "mcp",
        }
    }
}

/// ServiceRefObject - Reference to imported service with optional overrides.
///
/// Mirrors `PageRefObject` for services. The override fields are a curated
/// subset of what `ServiceDefinition` exposes — only fields that make sense
/// to customize at the reference site (per-environment endpoints, header
/// tweaks, descriptions). No config-merging logic; each override directly
/// replaces the corresponding field on the resolved inline service.
///
/// The overrides are variant-aware: `base_url` and `headers` only apply to
/// `ServiceDefinition::Rest`, `url` only to `Socket`, `server_path` only to
/// `Mcp`. `description` applies to all three. Overrides that don't match
/// the resolved service variant are silently ignored (mirroring how
/// `PageRefObject::path` only applies when the resolved page is inline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRefObject {
    /// Reference to imported service: "Alias.services.ServiceName"
    #[serde(rename = "ref")]
    pub reference: String,

    /// Optional description override (applies to Rest, Socket, Mcp).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional REST `baseUrl` override.
    #[serde(rename = "baseUrl", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Optional REST `headers` override (full replacement, not merge).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,

    /// Optional Socket `url` override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Optional MCP `serverPath` override.
    #[serde(rename = "serverPath", skip_serializing_if = "Option::is_none")]
    pub server_path: Option<String>,
}

/// ServiceRef - Service can be inline definition OR reference to imported service.
///
/// Reference formats:
/// - "Alias.services.ServiceName" - Simple reference (uses upstream defaults)
/// - { ref: "Alias.services.ServiceName", baseUrl: "https://prod.api" } - Reference with overrides
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServiceRef {
    /// Inline service definition (must be FIRST — most specific pattern)
    Inline(ServiceDefinition),
    /// Reference object with optional override fields
    Object(ServiceRefObject),
    /// Simple reference string: "Alias.services.ServiceName"
    Reference(String),
}

impl ServiceRef {
    /// Check if this is a reference (string or object)
    pub fn is_reference(&self) -> bool {
        !matches!(self, ServiceRef::Inline(_))
    }

    /// Get the reference string (works for both Reference and Object forms)
    pub fn as_reference(&self) -> Option<&str> {
        match self {
            ServiceRef::Reference(s) => Some(s),
            ServiceRef::Object(o) => Some(&o.reference),
            _ => None,
        }
    }

    /// Get the inline service if this is inline
    pub fn as_inline(&self) -> Option<&ServiceDefinition> {
        match self {
            ServiceRef::Inline(s) => Some(s),
            _ => None,
        }
    }
}

// =============================================================================
// Use Declaration (Import System)
// =============================================================================

/// UseDeclaration - Import an external Orbital as a namespace.
///
/// After importing, access components via:
/// - Alias.entity - The entity definition
/// - Alias.traits.TraitName - A specific trait
/// - Alias.pages.PageName - A specific page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UseDeclaration {
    /// Import source path:
    /// - "std/behaviors/game-core" - Standard library
    /// - "./shared/health.orb" - Local .orb file (relative path)
    /// - "../common/physics.orb" - Parent directory .orb file
    /// - "@game-lib/enemies.orb" - Scoped package (configured base)
    pub from: String,

    /// Alias for accessing imported components.
    /// Used as namespace: Alias.entity, Alias.traits.X, Alias.pages.X
    #[serde(rename = "as")]
    pub alias: String,

    /// `uses A from "…" { config { … } }` — call-site override of the imported
    /// organism's app-level declared config knobs (Almadar_Orbital_Import.md §4.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, ConfigField>>,

    /// `uses lazy A from "…"` — the imported behavior is compiled to its own
    /// `.orb` and loaded when one of its pages is opened (see [`LazyPage`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy: Option<bool>,
}

/// A page served by a lazily-used behavior. The importer lists the page so
/// routing and prerender know it; the behavior lives in the `.orb` at
/// `orb_ref`, relative to the importer's `.orb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LazyPage {
    pub path: String,
    pub orbital: String,
    #[serde(rename = "orbRef")]
    pub orb_ref: String,
    /// The link and navigate targets inside this page's resolved behavior, recorded at split time so
    /// page reachability follows links between lazy pages. Never serialized: not part of the `.orb`.
    #[serde(skip)]
    pub links: Vec<String>,
}

// =============================================================================
// Expect Declaration (Consumer-Side Requirements)
// =============================================================================

/// ExpectDeclaration — a consumer-side requirement: "some sibling orbital
/// provides this, and here is the minimal typed slice of it I rely on."
///
/// The dual of a provider: the entity `[identity]` tag declares "I am the
/// roster"; `expects identity` declares "I read the roster". Standalone
/// validation trusts the declaration and uses its shape as the local type
/// environment; composed-program validation link-checks it against the real
/// provider. The shape reuses the canonical `FieldDefinition` so all type
/// compatibility flows through `type_compat`. See §3-§5 of
/// docs/Almadar_LOLO_Expects_Proposal.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ExpectDeclaration {
    /// `expects identity Customer { role : "admin" | "member" }` — the `@user`
    /// fields this orbital reads, typed. Bare `expects identity` (no name, no
    /// shape) is the hand-authoring escape hatch: existence-only, open shape.
    Identity {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape: Option<Vec<FieldDefinition>>,
    },
    /// `expects entity OrderRecord { totalAmount : number }` — a sibling
    /// entity this orbital touches (relation field target, persist/fetch
    /// effect target, linkedEntity rebind). Shape omitted = opaque reference.
    Entity {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape: Option<Vec<FieldDefinition>>,
    },
    /// `expects event Checkout.ORDER_PLACED` — a sibling trait produces this
    /// event. Phase 2: feeds escape analysis / listens-audit.
    Event {
        #[serde(rename = "traitName")]
        trait_name: String,
        event: String,
    },
    /// `expects page "/articles/:slug"` — a route this orbital navigates to
    /// but does not declare. The one cross-boundary edge family a `navigate`
    /// effect cannot state for itself: the effect carries a static PREFIX
    /// (`/articles/`, all `extract_static_path_prefix` can recover from
    /// `(str/concat "/articles/" @payload.data.slug)`), while the provider
    /// declares a PATTERN (`/articles/:slug`). The parameterisation is real
    /// information the usage does not contain, which is why this is a
    /// declaration rather than a restatement of the effect.
    ///
    /// Derived, not hand-authored, in the normal path: `deriveExpectations`
    /// resolves each navigate target against the whole organism's pages and
    /// emits the matched pattern when it belongs to a sibling. A target that
    /// matches no page derives nothing, so a genuine typo stays an error.
    Page { path: String },
}

// =============================================================================
// Entity Reference (Inline OR Reference)
// =============================================================================

/// EntityRefExtends — Inherit an imported entity, optionally renamed,
/// with additional fields appended.
///
/// Mirrors the pattern of `PageRefObject` and `ServiceRefObject`: a
/// dedicated struct that carries a `reference` (the upstream alias.entity
/// path) plus the override fields. The `extends` JSON key is what
/// disambiguates this variant from `EntityRef::Inline` under serde's
/// untagged enum dispatch.
///
/// Semantic: "resolve `reference` to an entity, clone it, optionally rename
/// it via `name`, append `fields` (deduplicate by name where the user's
/// `fields` win on conflict), and apply optional `persistence` and
/// `collection` overrides. The result is the molecule's entity, with the
/// inherited fields available."
///
/// After the inline phase processes this variant, it is rewritten to a
/// regular `EntityRef::Inline(merged_entity)` so downstream phases see no
/// surprises.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRefExtends {
    /// Reference to imported entity: "Alias.entity"
    #[serde(rename = "extends")]
    pub reference: String,

    /// V4 dual-carry id sibling of `extends` — optional until the Phase-7 flip.
    #[serde(rename = "extendsId", default, skip_serializing_if = "Option::is_none")]
    pub extends_id: Option<String>,
    /// Optional rename for the resulting entity. When omitted the merged
    /// entity keeps the upstream entity's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Additional fields to append. Merged after inherited fields and
    /// dedup'd by `name`: when an extra field shares a name with an
    /// inherited field, the extra field replaces the inherited one (the
    /// molecule wins).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldDefinition>>,
    /// Optional persistence override that replaces the upstream value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence: Option<Persistence>,
    /// Optional collection override that replaces the upstream value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
}

/// EntityRef - Entity can be inline definition OR reference to imported entity.
///
/// Reference formats:
/// - extends form `{ extends: "Alias.entity", name?, fields?, ... }`
/// - inline `EntityDefinition` (a literal `name`/`fields`/...)
/// - simple string reference `"Alias.entity"`
///
/// Variant ordering matters because of `serde(untagged)`: serde tries the
/// variants in order and uses the first that successfully deserializes,
/// and `EntityDefinition` does NOT carry `#[serde(deny_unknown_fields)]`,
/// so a JSON object with the discriminating `extends` key would otherwise
/// be silently swallowed by `Inline` (which would treat `extends` as an
/// unknown field and discard it). The ordering below is therefore:
///
/// 1. `Extends` FIRST — disambiguated by the required `extends` key on
///    `EntityRefExtends::reference`. An inline entity (no `extends` key)
///    fails this variant immediately because `reference` is `String`, not
///    `Option<String>`, so serde rejects the missing-key shape.
/// 2. `Inline` second — the literal `EntityDefinition` shape. Matches any
///    object with `name` + `fields` + (optional persistence/collection).
/// 3. `Reference(String)` last — the bare string form. Cannot match a JSON
///    object, so it only fires when the JSON is a literal string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntityRef {
    /// Extends form: inherit an imported entity and append fields
    Extends(EntityRefExtends),
    /// Inline entity definition
    Inline(EntityDefinition),
    /// Reference to imported entity: "Alias.entity"
    Reference(String),
}

impl EntityRef {
    /// Check if this is a reference (string or extends form)
    pub fn is_reference(&self) -> bool {
        !matches!(self, EntityRef::Inline(_))
    }

    /// Get the reference string if this is a reference. For the extends
    /// form returns the upstream `extends` value; for `Reference` returns
    /// the bare string.
    pub fn as_reference(&self) -> Option<&str> {
        match self {
            EntityRef::Reference(s) => Some(s),
            EntityRef::Extends(e) => Some(&e.reference),
            _ => None,
        }
    }

    /// Get the inline entity if this is inline
    pub fn as_inline(&self) -> Option<&EntityDefinition> {
        match self {
            EntityRef::Inline(e) => Some(e),
            _ => None,
        }
    }
}

// =============================================================================
// Page Reference (Inline OR Reference)
// =============================================================================

/// PageRefObject - Reference to imported page with optional override fields.
///
/// Phase F: Carries `linked_entity` and `traits` overrides in addition to `path`.
/// The inline phase applies these by rewriting identifiers in the imported page
/// (linked_entity) and replacing its trait list (traits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRefObject {
    /// Reference to imported page: "Alias.pages.PageName"
    #[serde(rename = "ref")]
    pub reference: String,

    /// V4 dual-carry id sibling of `ref` — optional until the Phase-7 flip.
    #[serde(rename = "refId", default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,

    /// Optional path override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Phase F: rebind the page's entity to the caller-supplied entity.
    /// The identifier-substitution pass rewrites references to the original
    /// entity inside the imported page.
    #[serde(
        rename = "linkedEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity: Option<String>,

    /// V4 dual-carry id sibling of `linkedEntity` — optional until the Phase-7 flip.
    #[serde(
        rename = "linkedEntityId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity_id: Option<String>,

    /// Phase F: override the page's trait set with the caller-supplied list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<TraitReference>>,

    /// V4 dual-carry id siblings of `traits` — optional until the Phase-7 flip.
    #[serde(
        rename = "traitRefIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub trait_ref_ids: Option<Vec<String>>,

    /// Explicit page-modifier overrides on the reference form. Absent fields
    /// inherit the upstream page's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PageAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<PageIndexing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<PageMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<PageMeta>,
    #[serde(
        rename = "translationOf",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub translation_of: Option<String>,
    /// The page's nav entry (`@label`/`@icon`/`@roles` on the reference form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
}

/// PageRef - Page can be inline definition OR reference to imported page.
///
/// Reference formats:
/// - "Alias.pages.PageName" - Simple reference (keeps original path)
/// - { ref: "Alias.pages.PageName", path: "/custom" } - Reference with path override
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PageRef {
    /// Inline page definition (must be first - more specific pattern)
    Inline(PageDefinition),
    /// Reference object with optional path override
    Object(PageRefObject),
    /// Simple reference string: "Alias.pages.PageName"
    Reference(String),
}

impl PageRef {
    /// Check if this is a reference (string or object)
    pub fn is_reference(&self) -> bool {
        !matches!(self, PageRef::Inline(_))
    }

    /// Get the reference string
    pub fn reference_string(&self) -> Option<&str> {
        match self {
            PageRef::Reference(s) => Some(s),
            PageRef::Object(o) => Some(&o.reference),
            _ => None,
        }
    }

    /// Get the inline page if this is inline
    pub fn as_inline(&self) -> Option<&PageDefinition> {
        match self {
            PageRef::Inline(p) => Some(p),
            _ => None,
        }
    }

    /// Get path override (only for PageRefObject)
    pub fn path_override(&self) -> Option<&str> {
        match self {
            PageRef::Object(o) => o.path.as_deref(),
            _ => None,
        }
    }
}

// =============================================================================
// Orbital Reference (Orbital Import)
// =============================================================================

/// OrbitalRefObject - Reference to an imported orbital with optional override fields.
///
/// Field-for-field mirror of `PageRefObject`'s serde style (Almadar_Orbital_Import.md §4).
/// Transient: set by lowering on an unresolved `orbital X = Alias.orbitals.Y { … }`
/// declaration, cleared by the inline phase's orbital-reference resolution — must be
/// `None` in any schema reaching validation, codegen, or a committed `.orb`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrbitalRefObject {
    /// Reference to imported orbital: "Alias.orbitals.UpstreamName"
    #[serde(rename = "ref")]
    pub reference: String,

    /// V4 dual-carry id sibling of `ref` — optional until the Phase-7 flip.
    #[serde(rename = "refId", default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,

    /// Rebind the primary entity to a caller-supplied bare name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,

    /// Primary-entity field renames: upstream field name → local field name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<HashMap<String, String>>,

    /// Route path remap: upstream path → local path. An unmapped path keeps
    /// its upstream value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<HashMap<String, String>>,

    /// Page modifiers a `pages {}` remap entry declares, keyed by the
    /// UPSTREAM path: they replace the upstream page's, absent ones inherit.
    #[serde(
        rename = "pageModifiers",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub page_modifiers: Option<std::collections::BTreeMap<String, PageModifiers>>,

    /// Trait names to drop from the imported set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit: Option<Vec<String>>,

    /// Trait names to keep from the imported set (all others dropped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,

    /// Call-site overrides of the upstream orbital's declared config knobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, ConfigField>>,

    /// Orbital-scope event rename: upstream event key → local event key,
    /// applied to every trait in the imported set (emits and listen keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<HashMap<String, String>>,

    /// Role-vocabulary remap (Stage B): upstream role name → one or more
    /// consumer roster role names. A single target lowers to a one-element
    /// Vec; the formatter prints the single form back when len == 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<HashMap<String, Vec<String>>>,

    /// Out-of-orbital entity remap (Stage B): upstream entity name →
    /// consumer entity name, applied to relation targets, `@Entity` tokens,
    /// fetch/persist targets, policy comparisons, and page
    /// `primaryEntity`/`sourceEntityDefinition`. Identity (`X: X`) is legal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<HashMap<String, String>>,

    /// Sibling-trait page mounts (Stage B): upstream page path → the
    /// sibling trait names (declared in this reference's body) to mount on
    /// that page, in addition to the traits it already imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounts: Option<HashMap<String, Vec<String>>>,

    /// `extend { <field-decl>+ }` (Stage C): full entity-field declarations
    /// appended to the materialized primary entity, AFTER `fields {}`
    /// renames apply — unlike `fields {}` (rename-only), each entry is a
    /// whole new field the upstream entity never declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend: Option<Vec<FieldDefinition>>,

    /// `mock { field: "a, b" }` — sample data for fields of the materialized
    /// primary entity (named after `fields {}` renames), replacing the
    /// upstream field's `@mock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<HashMap<String, String>>,

    /// `retype { name : Type = default }`: replaces the type/default of fields the
    /// upstream primary entity already declares (after `fields {}` renames). Unlike
    /// `extend {}`, every name MUST already exist (`ORB_O_RETYPE_UNKNOWN_FIELD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retype: Option<Vec<FieldDefinition>>,

    /// `entity Name [persistent: tbl]`: overrides the materialized primary entity's
    /// persistence mode. Refused when the upstream entity is not `persistent`
    /// (`ORB_O_ENTITY_PERSISTENCE_LOCKED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence: Option<Persistence>,

    /// `entity Name [persistent: tbl]`: overrides the primary entity's collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,

    /// Entity-header flags of the rebind line (`[shared]`, `[identity]`, `[local]`,
    /// `[mock]`); each one switches the flag ON for the materialized primary entity.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identity: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    #[serde(
        rename = "seedMock",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub seed_mock: bool,

    /// `traits { UpstreamTrait { ... } }`: per-imported-trait call-site overrides,
    /// keyed by the trait's UPSTREAM name. Same surface as a trait reference's body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<HashMap<String, OrbitalRefTraitOverride>>,

    /// Declared routes INTO the imported set: each entry appends one listen
    /// to the named imported trait (upstream name). The source is a host
    /// sibling or another imported trait; the imported set's own wiring stays
    /// upstream-owned. Events reach another trait only through `listens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listens: Option<Vec<OrbitalRefListen>>,
}

/// One `traits { UpstreamTrait { ... } }` entry of an orbital import: the §8
/// trait-reference override surface, applied to a single imported trait.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrbitalRefTraitOverride {
    /// `-> Entity` on the entry: rebinds the trait's linked entity.
    #[serde(
        rename = "linkedEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, ConfigField>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<HashMap<String, String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<HashMap<String, String>>,

    /// Replaces the trait's whole `listens` array when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listens: Option<Vec<ListenDefinition>>,

    #[serde(
        rename = "emitsScope",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub emits_scope: Option<EventScope>,
}

/// One `listens { Source.EVENT -> Target.TRIGGER }` entry of an orbital
/// import: `trait` names the imported trait (upstream name) that gains the
/// route; the rest is an ordinary listen definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbitalRefListen {
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[serde(flatten)]
    pub listen: ListenDefinition,
}

// =============================================================================
// Orbital (Feature Unit)
// =============================================================================

/// An Orbital is a self-contained feature unit: Entity + Traits + Pages
///
/// Use `uses` to import other orbitals and access their components via:
/// - Alias.entity - The entity definition
/// - Alias.traits.TraitName - A specific trait
/// - Alias.pages.PageName - A specific page
/// - Alias.theme - The theme definition
/// - Alias.services.ServiceName - A specific service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Orbital {
    /// Orbital name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Import declarations for external orbitals
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses: Option<Vec<UseDeclaration>>,

    /// Consumer-side requirement declarations (`.lolo` `expects ...`).
    /// Promises about sibling orbitals this unit relies on: trusted as the
    /// local type environment when the orbital is validated standalone,
    /// link-checked against real providers at compose. Compile-time only —
    /// runtime semantics are unchanged. See docs/Almadar_LOLO_Expects_Proposal.md.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expects: Option<Vec<ExpectDeclaration>>,

    /// Theme definition (inline OR reference via "Alias.theme")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeRef>,

    /// Service definitions (inline OR reference via "Alias.services.ServiceName")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<ServiceRef>>,

    /// Entity definition (inline OR reference via "Alias.entity")
    ///
    /// Entity sharing depends on persistence type:
    /// - persistent: Shared (same DB collection)
    /// - runtime: Isolated (each orbital gets own instances)
    /// - singleton: Shared (single global instance)
    pub entity: EntityRef,

    /// Imported atom entities surfaced into this orbital's resolution
    /// scope WITHOUT modifying the primary entity. Populated by the
    /// inline phase when a trait reference imports an atom and OMITS
    /// the `-> Entity` linkedEntity rebind: the atom keeps its own
    /// entity (e.g. `SearchResult` from std-search), and that entity
    /// is registered here so validators / codegen / runtime can
    /// resolve `linkedEntity = "SearchResult"` lookups, register
    /// persistence for it, and emit its TypeScript type alongside
    /// the primary entity. The orbital's per-trait entity arity stays
    /// 1:1 — this is purely a NAME-RESOLUTION side channel for cross
    /// import linkedEntity references.
    #[serde(rename = "auxiliaryEntities", skip_serializing_if = "Option::is_none")]
    pub auxiliary_entities: Option<Vec<EntityRef>>,

    /// Traits attached to this orbital (local or via "Alias.traits.TraitName")
    pub traits: Vec<TraitReference>,

    /// Pages for this orbital (inline OR reference)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<PageRef>>,

    /// Events this orbital emits (for cross-orbital communication)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emits: Option<Vec<String>>,

    /// Events this orbital listens to (for cross-orbital communication)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listens: Option<Vec<ListenDefinition>>,

    /// Filter which trait events are exposed externally.
    /// If omitted, all external-scoped trait events are exposed.
    /// Format: ["TraitName.EVENT_NAME", ...]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposes: Option<Vec<String>>,

    /// Present ONLY on an unresolved `orbital X = Alias.orbitals.Y { … }` declaration.
    /// Set by lowering, cleared by the inline phase's orbital-reference resolution; must be
    /// `None` in any schema reaching validation, codegen, or a committed `.orb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<OrbitalRefObject>,

    /// This orbital's own declared config knobs (§4.5) — mirrors `TraitDefinition.config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, ConfigField>>,

    /// Domain context metadata
    #[serde(rename = "domainContext", skip_serializing_if = "Option::is_none")]
    pub domain_context: Option<DomainContext>,

    /// Design hints
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design: Option<DesignHints>,

    /// The orbital's own named STRUCT aliases (`type X = { ... }`), each
    /// flattened to the same `PayloadField` tree emit/listen payloads use.
    /// `.orb`'s only record of a struct alias post-L1 — a Type-kind trait
    /// type-param arg (`:: p SomeStructAlias`) resolves against this map.
    /// Enum/primitive/entity aliases are NOT carried here (not legal `::`
    /// args). `BTreeMap` for deterministic serialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<std::collections::BTreeMap<String, Vec<PayloadField>>>,
}

// =============================================================================
// Entity Definition
// =============================================================================

/// Row columns the store mints itself on `persist create` — never authored
/// input, so never a candidate REQUIRED column regardless of a `required:
/// true` declaration. The single owner of this list: before this constant,
/// `runtime/seed.rs`, `orbital-shell-typescript`'s `backend/shared.rs`, and
/// the JS interpreter (`packages/almadar-core/src/mock/sampleValue.ts`,
/// `packages/almadar-runtime/src/{EffectExecutor,MockPersistenceAdapter}.ts`)
/// each carried their own literal `["id", "createdAt", "updatedAt"]` copy —
/// consume this one instead of adding another.
pub const FRAMEWORK_STAMPED_FIELDS: &[&str] = &["id", "createdAt", "updatedAt"];

/// Entity definition - the data schema for an orbital
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityDefinition {
    /// Entity name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Database collection name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,

    /// Persistence type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence: Option<Persistence>,

    /// Field definitions
    pub fields: Vec<FieldDefinition>,

    /// Pre-authored instances (seed data / static reference data)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<Vec<crate::schema::SObject>>,

    /// Shared across all bound traits (vs per-trait copy); orthogonal to persistence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,

    /// Types the ambient `@user` viewer. Orthogonal to both persistence and
    /// `shared`: `[persistent: people, identity]` is an app-owned user
    /// directory, `[runtime, identity]` the provider-supplied current viewer.
    /// At most one per composed program.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identity: bool,

    /// `[persistent: x, local]` — the rows live in the browser (IndexedDB), so
    /// data effects on this entity run client-side. Twin of `@almadar/core`
    /// `OrbitalEntity.local`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,

    /// `[persistent: x, local, mock]` — an empty browser store is seeded from the
    /// shared mock seeder on first open (the other seed source is `instances`).
    #[serde(
        default,
        rename = "seedMock",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub seed_mock: bool,

    /// A browser-stored entity's first rows per declared locale (`instances ar [ … ]`);
    /// a locale with no block takes `instances`.
    #[serde(
        default,
        rename = "localeInstances",
        skip_serializing_if = "Option::is_none"
    )]
    pub locale_instances:
        Option<std::collections::BTreeMap<String, Vec<crate::schema::SObject>>>,

    /// `@read` — a per-row predicate (`@entity` = candidate row, `@user` =
    /// viewer) ANDed with any call-site `filter:`. The un-bypassable twin of
    /// `filter:`, enforced once for every trait that fetches this entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_policy: Option<SExpression>,

    /// `@create` — `@entity` binds the incoming data about to become a row
    /// (no row exists yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_policy: Option<SExpression>,

    /// `@update` — `@entity` binds the existing row before mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_policy: Option<SExpression>,

    /// `@delete` — same binding shape as `@update`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_policy: Option<SExpression>,

    /// `@<op> none "<reason>"` — the omission of a directive, DECLARED.
    ///
    /// Runtime behaviour is identical to simply leaving the directive out
    /// (an absent policy is allow-all). The point is to record that the author
    /// weighed it and concluded no per-row predicate can express the rule —
    /// typically because visibility depends on a DIFFERENT entity, which a
    /// per-row predicate cannot join across. Without this, a lint has no way to
    /// tell a considered omission from a forgotten one, so it must flag both;
    /// flagging correct code forever is how a signal gets ignored.
    ///
    /// Never a way to silence a policy that IS expressible — the reason is
    /// mandatory and gets read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_waivers: Option<AccessWaivers>,
}

impl EntityDefinition {
    /// The rows the entity starts with for a viewer's locale: its locale block, else
    /// `instances`. Twin of `@almadar/core` `instancesForLocale`.
    pub fn instances_for_locale(&self, locale: Option<&str>) -> &[crate::schema::SObject] {
        locale
            .and_then(|l| self.locale_instances.as_ref()?.get(l))
            .or(self.instances.as_ref())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Rows live in the client: `[runtime]` (in memory) or `[persistent: x, local]`
    /// (the browser's IndexedDB). Twin of `@almadar/core` `isClientResident`.
    pub fn is_client_resident(&self) -> bool {
        self.local || matches!(self.persistence, Some(Persistence::Runtime))
    }
}

/// Why a declared access directive is deliberately absent, per operation.
/// Each value is the author's stated reason; `None` means "not waived".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AccessWaivers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
}

/// Persistence type for entities
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Persistence {
    /// Stored in database
    #[default]
    Persistent,
    /// In-memory only
    Runtime,
}

/// Field definition within an entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    /// Field name (optional for nested items/properties)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Field type
    #[serde(rename = "type")]
    pub field_type: FieldType,

    /// Whether the field is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,

    /// Whether this is a primary key
    #[serde(rename = "primaryKey", skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<bool>,

    /// Whether this field must be unique
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,

    /// Default value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<SExpression>,

    /// Enum values (for enum type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,

    /// Relation definition (for relation type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<RelationDefinition>,

    /// Array item type (for array type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<FieldDefinition>>,

    /// Object properties (for object type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, FieldDefinition>>,

    /// Field-level rebindability designation. `Some(true)` marks the field as
    /// INTRINSIC machinery/view-state the owning trait computes itself (e.g. a
    /// step-flow's `currentStepIndex`, a board's `boards`/`currentId`). Intrinsic
    /// fields travel WITH the trait onto whatever entity it binds and are NOT
    /// required to pre-exist on a rebind target. Absent / `None` = DATA field:
    /// the consumer supplies it; a rebind target must satisfy it (present, or
    /// remapped via the `fields` override). Authored in `.lolo` as a trailing
    /// `@intrinsic` annotation on the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intrinsic: Option<bool>,

    /// Human/semantic description — authored in `.lolo` as a trailing
    /// `@description "..."` annotation. Authoring/build-time metadata that flows
    /// to the factory-signature catalog, embeddings, and the curation field
    /// matcher; the runtime ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (free text) — authored in `.lolo` as a trailing
    /// `@synonyms "..."` annotation. Feeds catalog search / curation matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Declared mock value(s) — authored in `.lolo` as a trailing
    /// `@mock "..."` annotation, used LITERALLY by the seeder (`seed.rs`,
    /// `sampleValue.ts`), never mapped through a domain-name lookup table —
    /// the generator has no built-in idea what "person-name" or any other
    /// word means, only the author does. Comma-separated is an explicit
    /// candidate list rotated by row index (`@mock "Acme Corp, Globex Inc"`);
    /// no comma is a single literal value, index-suffixed per row
    /// (`@mock "person-name"` -> "person-name-1", "person-name-2", ...).
    /// Adding support for a new kind of mock data is never a generator change
    /// on either side — the author just writes different literal values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<String>,

    /// Name of the imported entity this field was copied FROM by the inline
    /// phase's rebind merge (`merge_imported_entity_fields`), when this field
    /// was added to the host rather than authored by it. `None` for every
    /// field the host itself declares, even if an atom also declares a field
    /// of the same name (the host's own declaration always wins on collision
    /// and is never merged). A merged field's `required: true` is the
    /// IMPORTED atom's own write contract, never the host writer's — see
    /// `required_create_field_names`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "mergedFrom"
    )]
    pub merged_from: Option<String>,

    /// The declared field this field's type was projected from — `.lolo`
    /// `T.f` in any type position (`tagId : NoteTagRef.id`). The field
    /// carries `T.f`'s declared type itself; this records where it came from,
    /// so a value of it can be drawn from `T`'s own `f` (the mock seeder draws
    /// from `T`'s seeded rows). `None` for a field typed any other way.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectedFrom"
    )]
    pub projected_from: Option<FieldProjection>,
}

impl FieldDefinition {
    /// A `tuple` field's slots, in slot order (`properties` keyed `"0"`, `"1"`, …).
    pub fn tuple_slots(&self) -> Vec<&FieldDefinition> {
        tuple_slots_of(self.properties.as_ref())
    }
}

/// Tuple slots carried in a `properties` map keyed by slot index, in slot order — the
/// one reader for a tuple field and a tuple-typed config knob alike.
pub fn tuple_slots_of(
    properties: Option<&HashMap<String, FieldDefinition>>,
) -> Vec<&FieldDefinition> {
    let mut slots: Vec<(usize, &FieldDefinition)> = properties
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v)))
        .collect();
    slots.sort_by_key(|(i, _)| *i);
    slots.into_iter().map(|(_, v)| v).collect()
}

/// `T.f` — a field `f` of the declared type `T` (an entity or a struct type).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldProjection {
    #[serde(rename = "type")]
    pub type_name: String,
    pub field: String,
}

/// Field type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Number,
    Boolean,
    Date,
    Datetime,
    Timestamp,
    /// Semantic string domains. `.lolo` already types `date`/`datetime` rather
    /// than annotating them, so a field's domain belongs in the type system for
    /// the same reason. These replaced the TS-only `format` property, which had
    /// no Rust counterpart at all and therefore no way to reach `.lolo`.
    Email,
    Url,
    Phone,
    Uuid,
    Image,
    /// Monetary amount. Numeric at rest (plain number); the type — not the
    /// field name — drives currency-formatted display and currency inputs.
    /// Was a grammar token that silently collapsed to `String` (the schema
    /// lied about the value's shape); promoted 2026-08-20.
    Money,
    /// Uploaded file/attachment. A STRUCT value at rest —
    /// `{ name, url, mimeType, sizeBytes }` — not a validated string like
    /// the semantic domains above: it names a stored artifact. `url` is a
    /// `data:` URI in mock/playground mode (size-ceilinged) or an
    /// object-storage URL in production.
    File,
    Array,
    Object,
    Enum,
    Relation,
    /// `@trait.X` reference. Allowed only on config fields. Runtime + compiled
    /// codegen substitute the value with embedded trait UI at render time.
    Trait,
    /// An orbital value held as data (language trio): `<spec>.orbitals.<Orbital>`
    /// plus a recorded §8b import body, or `none` (unbound).
    Orbital,
    /// A whole behavior held as data: `{ behavior }` (`behavior/ref`), or `none`.
    /// Embedding one in a render tree runs the program in place.
    Behavior,
    /// UI slot name. Validated against `VALID_SLOTS` at compile time.
    Slot,
    /// Pattern type name. Validated against `PATTERN_TYPES` at compile time.
    Pattern,
    /// Renderable UI content — a pattern node (or array of them), the same
    /// shape `render-ui` effects carry. First-class so view-fragment fields
    /// stop hiding behind erased `object`/`json` (type-integrity campaign).
    Node,
    /// Event-name reference — a string at rest, but the TYPE marks it as an
    /// event key rather than free text (Phase B / SCAN-EVENT-KNOB-2). Legal
    /// on struct-alias fields consumed by config knobs (e.g. `ItemAction.event`);
    /// forbidden directly on entity fields by the parser gate
    /// (`ELOLO_CONFIG_TYPE_FORBIDDEN_IN_ENTITY`). Acceptance is
    /// closed-circuit parity: own transitions ∪ orbital-wide transitions ∪
    /// own emits (`config_type.rs::check_field_def_shape`).
    Event,
    /// A value that ADDRESSES a declared external input — `Trait.EVENT` within
    /// the orbital, `Orbital.Trait.EVENT` across the program; never a bare name.
    /// The `@almadar/core` `EventAddress` contract (what `llm call-tools` reads).
    #[serde(rename = "EventAddress")]
    EventAddress,
    /// A fixed-length tuple (`.lolo` `[T1, T2, …]`). Its slots ride `properties`, keyed
    /// by slot index (`"0"`, `"1"`, …).
    Tuple,
    /// A single primitive VALUE of unknown-but-bounded kind:
    /// `string | number | boolean | null`. The honest type for a field-value
    /// bag whose keys are dynamic — `Map string scalar` — where `json` used to
    /// stand in.
    ///
    /// This is a real type, not a generic: it REJECTS objects and arrays,
    /// which `json` accepted. That is the whole point. A value that may nest
    /// is not a scalar, and a field declared `scalar` that receives an object
    /// is a shape mismatch, where `json` was silence.
    ///
    /// Introduced 2026-09-01 for the `json` burndown: `loadedRow` and friends
    /// are written through config-named keys
    /// (`(object/set @entity.loadedRow @config.contentField ?value)`) and read
    /// whole, so their KEYS are dynamic and their VALUES are field values.
    /// No struct can name them and no existing primitive admits all three.
    Scalar,
    /// An S-expression as data — an atom (string/number/boolean/null), a list
    /// of S-expressions, or an options object: what a transition's guard and
    /// effect arguments are. `SExpr`-typed struct members (`args : [SExpr]`)
    /// lowered to a shapeless `Object` before, which rejected every atom.
    #[serde(rename = "SExpr")]
    SExpr,
    /// A TAGGED union of struct variants: `type DrawItem = DrawShape | DrawText`.
    /// `values` carries the variant NAMES in declaration order; `properties`
    /// carries each variant's shape keyed by that name. The discriminator is
    /// the variant's own literal-typed tag field (`type : "draw-shape"`), so
    /// selection is data-driven, not positional.
    ///
    /// Distinct from a bare `Union` of names, which lowers to `String` +
    /// `values` (a string enum). That collapse is why `DrawShape | DrawText`
    /// used to declare a field whose only legal values were the literal words
    /// "DrawShape"/"DrawText" — it accepted the NAME and rejected the VALUE.
    ///
    /// Recursion (`DrawGroup.items : [DrawItem]`) is carried BY NAME: a
    /// variant that refers back to the union being lowered gets `values` with
    /// no `properties`, so the shape is a reference rather than an infinite
    /// inline expansion.
    Union,
}

/// Relation definition for entity references
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationDefinition {
    /// Target entity name
    pub entity: String,

    /// V4 dual-carry id sibling of `entity` — optional until the Phase-7 flip.
    #[serde(rename = "entityId", default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,

    /// Cardinality
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<Cardinality>,
}

impl RelationDefinition {
    pub fn is_to_many(&self) -> bool {
        matches!(
            self.cardinality,
            Some(Cardinality::Many | Cardinality::OneToMany | Cardinality::ManyToMany)
        )
    }
}

/// Relation cardinality
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    One,
    Many,
    #[serde(rename = "many-to-one")]
    ManyToOne,
    #[serde(rename = "one-to-many")]
    OneToMany,
    #[serde(rename = "many-to-many")]
    ManyToMany,
}

// =============================================================================
// Trait Definition
// =============================================================================

/// Increment 3-II (`docs/Almadar_LOLO.md`): one trait-level type parameter,
/// declared `(V : Type)` in `trait Name (V : Type, R : Type) -> Entity { ... }`.
/// `kind` is one of the same six kind names Increment 3-I's alias params
/// accept (`Type`, `Entity`, `Trait`, `Page`, `Orbital`, `Event`) — in
/// practice `Type` and `Entity` are the two meaningful kinds for a trait
/// param today. Carried on the DECLARING trait's `TraitDefinition` (never on
/// a `Reference`) so the compiler's inline phase can tell, for a surviving
/// `"$<name>"` payload sentinel, whether it's Entity-kind (defaults to the
/// trait's `linkedEntity` when no call-site arg was supplied — the 3-I
/// behavior) or Type-kind (no default; an unresolved sentinel is blocking).
///
/// Gap C: `default` carries an optional `(r : Type = <name>)` declaration
/// default — a primitive name (incl. `scalar`), an entity name, or a
/// struct-alias name declared in the atom's own file. Resolved via the same
/// vocabulary a call-site `::` arg uses, consulted only when no call-site
/// arg was supplied (and, for Entity-kind, only when `linkedEntity` also
/// gave no default) — see `resolve_type_param_sentinels`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraitTypeParamDef {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// Trait reference in an orbital
///
/// Both `Serialize` and `Deserialize` are hand-written so the wire form is FLAT
/// (the inner trait/reference fields at the top level), identical to the bytes
/// the old `#[serde(untagged)]` produced — NOT externally tagged. Deserialization
/// dispatches on the `stateMachine` discriminator (present only on an inline
/// `TraitDefinition`, never on a reference) rather than serde's untagged
/// trial-and-error: dispatching to ONE concrete variant means serde surfaces the
/// variant's real field-level error (`unknown field`, `missing field`, type
/// mismatch) instead of the opaque "did not match any variant of untagged enum".
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // boxing Inline would change match sites across the tree
pub enum TraitReference {
    /// Inline trait definition
    Inline(TraitDefinition),
    /// Reference to a named trait. Serde attributes for these fields live on the
    /// `TraitReferenceFields` (deserialize) and `TraitReferenceFieldsRef`
    /// (serialize) mirror structs below; the enum's `Serialize`/`Deserialize`
    /// are hand-written, so no derive consumes per-field `#[serde]` here.
    Reference {
        reference: Option<String>,
        /// V4 dual-carry id sibling of `reference` — optional until the Phase-7 flip.
        ref_id: Option<String>,
        /// V4 LOCAL DECLARATION id — the identity of this trait AS DECLARED in
        /// its orbital (`trait X = Alias.traits.Y` declares node X; `ref_id`
        /// carries Y's id). Minted by `stamp_identity` so page trait refs and
        /// `@trait.X` embeds resolve by id across `name:` renames — the
        /// composed-organism surface was the one declaration class without an
        /// id (battery 2026-07-20 FACTORY-PAGE-TRAIT-NAME-COUPLING).
        id: Option<String>,
        name: Option<String>,
        linked_entity: Option<String>,
        /// V4 dual-carry id sibling of `linked_entity` — optional until the Phase-7 flip.
        linked_entity_id: Option<String>,
        /// Call-site config overrides. Each entry may be a plain wiring
        /// (`ConfigField` with only `default` set, `field_type` = `"unknown"`)
        /// or a fully-annotated re-declaration (`field_type`, `label`,
        /// `description`, `synonyms`, `tier` populated). The inline phase
        /// folds both into the atom's declared config schema: the `default`
        /// is always applied; annotations are merged (organism wins) onto the
        /// inlined trait's config field so emitted artifacts carry the
        /// organism's wording.
        ///
        /// Serialization: a plain wiring value serializes as
        /// `{ "key": { "type": "unknown", "default": <SExpr> } }` — the
        /// same shape the atom's config block uses, so round-trip is clean.
        config: Option<HashMap<String, ConfigField>>,
        /// Phase F: caller-supplied event-key renames, e.g. {"OPEN": "ADD_ITEM"}.
        /// The inline phase rewrites every occurrence of the atom-declared event
        /// key (StateMachine.events[*].key, transitions[*].event, emits[*].event,
        /// listens[*].event/triggers, and any SExpression event references) to
        /// the caller-supplied value.
        events: Option<HashMap<String, String>>,
        /// V4 dual-carry id siblings of `events` keys (baked event name → id) —
        /// optional until the Phase-7 flip.
        event_ids: Option<HashMap<String, String>>,
        /// Per-key entity-field rename map, e.g. {"title": "dishName"}. Mirrors
        /// `events`: the inline phase rewrites every `@entity.<old>` read inside
        /// the resolved trait to `@entity.<new>` at the call site, so a DATA
        /// field the trait reads can be satisfied by a differently-named field
        /// on the rebind target. Keys are the trait's declared field names;
        /// values are the target entity's field names.
        fields: Option<HashMap<String, String>>,
        /// Phase F.7: replace the imported trait's `listens` array with the
        /// caller-supplied list. Empty array clears all upstream listens.
        /// Applied AFTER the substitution pass, overwriting whatever the
        /// upstream declared. Use sparingly: this drops external event wiring.
        listens: Option<Vec<ListenDefinition>>,
        /// Phase F.7: set every emit's `scope` to the caller-supplied value.
        /// `Internal` confines emits to the orbital's internal bus; `External`
        /// broadcasts them. Applied after substitution and listens replacement.
        emits_scope: Option<EventScope>,
        /// Increment 3-II: call-site type-parameter arguments, e.g.
        /// `{"V": "string"}` from `trait X :: V string = Alias.traits.Y { ... }`.
        /// Reinterpretation of the pre-existing `::` type-annotation surface —
        /// `orbital-lolo` populates this ONLY when the annotation's `type_name`
        /// does NOT resolve as a known local type alias (preserving the
        /// original alias-conformance meaning of `::` untouched whenever it
        /// does resolve). The inline phase consumes this against the
        /// resolved source trait's `type_params` to substitute each
        /// `"$<param>"` payload sentinel; unconsumed keys (source declares no
        /// matching param) are a harmless no-op.
        type_args: Option<HashMap<String, String>>,
        /// Call-site `[local]` (`trait X = Alias.traits.Y -> E [local]`): the
        /// resolved trait dispatches client-only (`hybridClientOnly`) whatever
        /// the atom declared. Resolved local = call-site OR atom.
        local: bool,
    },
}

impl TraitReference {
    /// Get the trait name (either from ref or inline name)
    pub fn trait_name(&self) -> Option<&str> {
        match self {
            TraitReference::Inline(def) => Some(&def.name),
            TraitReference::Reference {
                reference, name, ..
            } => reference.as_deref().or(name.as_deref()),
        }
    }

    /// V4 dual-carry id of the referenced trait (inline node id, or the
    /// reference's `refId`). `None` until the Phase-7 flip mints ids everywhere.
    pub fn trait_ref_id(&self) -> Option<&str> {
        match self {
            TraitReference::Inline(def) => def.id.as_deref(),
            TraitReference::Reference { ref_id, .. } => ref_id.as_deref(),
        }
    }

    /// V4 LOCAL DECLARATION id — the identity of this trait as declared in its
    /// orbital (for a `Reference` this is the declared node's own id, NOT the
    /// referenced target's `refId`).
    pub fn trait_decl_id(&self) -> Option<&str> {
        match self {
            TraitReference::Inline(def) => def.id.as_deref(),
            TraitReference::Reference { id, .. } => id.as_deref(),
        }
    }
}

/// Fields of the `Reference` variant, mirrored as a struct so the manual
/// `Deserialize` for `TraitReference` can deserialize the named-reference shape
/// in one concrete pass (producing real field-level errors).
#[derive(Deserialize)]
struct TraitReferenceFields {
    #[serde(rename = "ref", default)]
    reference: Option<String>,
    #[serde(rename = "refId", default)]
    ref_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "linkedEntity", default)]
    linked_entity: Option<String>,
    #[serde(rename = "linkedEntityId", default)]
    linked_entity_id: Option<String>,
    #[serde(default)]
    config: Option<HashMap<String, ConfigField>>,
    #[serde(default)]
    events: Option<HashMap<String, String>>,
    #[serde(rename = "eventIds", default)]
    event_ids: Option<HashMap<String, String>>,
    #[serde(default)]
    fields: Option<HashMap<String, String>>,
    #[serde(default)]
    listens: Option<Vec<ListenDefinition>>,
    #[serde(rename = "emitsScope", default)]
    emits_scope: Option<EventScope>,
    #[serde(rename = "typeArgs", default)]
    type_args: Option<HashMap<String, String>>,
    #[serde(default)]
    local: bool,
}

/// Borrowing mirror of the `Reference` variant used only by the hand-written
/// `Serialize`. Field names, `rename`s, and `skip_serializing_if` rules match
/// the variant exactly so the FLAT wire output is byte-identical to the old
/// `#[serde(untagged)]` derive (which serialized the reference's fields at the
/// top level, NOT under a `"Reference"` tag).
#[derive(Serialize)]
struct TraitReferenceFieldsRef<'a> {
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    reference: &'a Option<String>,
    #[serde(rename = "refId", skip_serializing_if = "Option::is_none")]
    ref_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: &'a Option<String>,
    #[serde(rename = "linkedEntity", skip_serializing_if = "Option::is_none")]
    linked_entity: &'a Option<String>,
    #[serde(rename = "linkedEntityId", skip_serializing_if = "Option::is_none")]
    linked_entity_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: &'a Option<HashMap<String, ConfigField>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    events: &'a Option<HashMap<String, String>>,
    #[serde(rename = "eventIds", skip_serializing_if = "Option::is_none")]
    event_ids: &'a Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: &'a Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    listens: &'a Option<Vec<ListenDefinition>>,
    #[serde(rename = "emitsScope", skip_serializing_if = "Option::is_none")]
    emits_scope: &'a Option<EventScope>,
    #[serde(rename = "typeArgs", skip_serializing_if = "Option::is_none")]
    type_args: &'a Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "is_false")]
    local: &'a bool,
}

fn is_false(b: &&bool) -> bool {
    !**b
}

impl Serialize for TraitReference {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // FLAT output (no external `"Inline"`/`"Reference"` tag): the inline
        // variant serializes as the bare `TraitDefinition`; the reference
        // variant serializes its fields at the top level via the borrowing
        // mirror. Matches the old untagged derive byte-for-byte.
        match self {
            TraitReference::Inline(t) => t.serialize(serializer),
            TraitReference::Reference {
                reference,
                ref_id,
                id,
                name,
                linked_entity,
                linked_entity_id,
                config,
                events,
                event_ids,
                fields,
                listens,
                emits_scope,
                type_args,
                local,
            } => TraitReferenceFieldsRef {
                reference,
                ref_id,
                id,
                name,
                linked_entity,
                linked_entity_id,
                config,
                events,
                event_ids,
                fields,
                listens,
                emits_scope,
                type_args,
                local,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for TraitReference {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        // Buffer the value, then dispatch on the discriminator. An inline trait
        // is the only shape that carries a `stateMachine` (it's a required field
        // on `TraitDefinition`); a named reference never does. Anything with a
        // `ref` key but no `stateMachine` is also a reference. Deserializing
        // exactly ONE concrete variant (not serde's untagged trial of both)
        // makes the resulting error name the offending field (`unknown field`,
        // `missing field`, type mismatch) rather than the opaque "did not match
        // any variant of untagged enum".
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value.as_object().ok_or_else(|| {
            D::Error::custom("trait reference must be a JSON object (inline trait or { ref: ... })")
        })?;

        let is_reference = obj.contains_key("ref") || !obj.contains_key("stateMachine");
        if is_reference {
            let f: TraitReferenceFields =
                serde_json::from_value(value).map_err(D::Error::custom)?;
            Ok(TraitReference::Reference {
                reference: f.reference,
                ref_id: f.ref_id,
                id: f.id,
                name: f.name,
                linked_entity: f.linked_entity,
                linked_entity_id: f.linked_entity_id,
                config: f.config,
                events: f.events,
                event_ids: f.event_ids,
                fields: f.fields,
                listens: f.listens,
                emits_scope: f.emits_scope,
                type_args: f.type_args,
                local: f.local,
            })
        } else {
            let td: TraitDefinition = serde_json::from_value(value).map_err(D::Error::custom)?;
            Ok(TraitReference::Inline(td))
        }
    }
}

/// Inferred field contract for a `@rebindable` trait binding. `requires`
/// = entity fields the trait READS via `@entity.<field>`; `provides` =
/// fields it WRITES via `(set @entity.<field> ...)`. A consumer that
/// rebinds the trait (`linkedEntity`) must point at an entity whose
/// fields ⊇ `requires`. Inferred by the lolo lowerer from the trait's own
/// SExpr usage; the L2 validator enforces it (`ORB_T_REBIND_MISSING_FIELDS`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntityFieldContract {
    // Always emit both arrays (no skip_serializing_if) — the @almadar/core
    // zod validator (EntityFieldContractSchema) requires BOTH `requires` and
    // `provides` keys. Skipping an empty array produces a partial object that
    // fails strict validation downstream even when the field is semantically
    // empty.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
}

/// Full trait definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraitDefinition {
    /// Trait name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Opt-in marker authored as `-> @rebindable Entity` in `.lolo`. When
    /// `Some(true)`, a consuming molecule/organism may rebind this trait's
    /// entity via `linkedEntity`; otherwise the binding is fixed and the
    /// validator rejects any rebind (`ORB_T_ENTITY_NOT_REBINDABLE`). Absent
    /// on traits that don't bind an entity.
    #[serde(
        rename = "entityRebindable",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub entity_rebindable: Option<bool>,

    /// Inferred field contract a rebind target must satisfy. Emitted only
    /// alongside `entity_rebindable: Some(true)`.
    #[serde(
        rename = "entityContract",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub entity_contract: Option<EntityFieldContract>,

    /// Inferred effect surface — every effect this trait produces, tagged with
    /// the resource it acts on and whether it sits in a transition or a `ticks`
    /// block. Derived by the lolo lowerer from the trait's own effects (never
    /// authored, the way `entity_contract` is never authored), then re-resolved
    /// post-inline once call-site config makes `@config.*` resources concrete.
    /// Absent on traits that produce no effects at all.
    #[serde(rename = "effectRow", default, skip_serializing_if = "Option::is_none")]
    pub effect_row: Option<EffectRow>,

    /// Trait category
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<TraitCategory>,

    /// Author-supplied capability tags lifted from the `.lolo` trait header
    /// bracket list (anything beyond the scope marker + the first category
    /// identifier). Free-form strings — no closed enum. Empty when none
    /// were declared. See `docs/Almadar_Domain_Language.md` Phase 3.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Linked entity name
    #[serde(rename = "linkedEntity", skip_serializing_if = "Option::is_none")]
    pub linked_entity: Option<String>,

    /// V4 dual-carry id sibling of `linkedEntity` — optional until the Phase-7 flip.
    #[serde(
        rename = "linkedEntityId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_entity_id: Option<String>,

    /// V4 leverage-ids — id side-map for `@entity.<Name>` binding tokens + positional
    /// entity args (`fetch`/`ref`/`persist`/`spawn`) appearing in this trait's guards /
    /// effects / ticks / listens. Maps the referenced entity NAME → its stable entity id,
    /// minted at stamp from the orbital's entity id map. Readers resolve entity refs by
    /// this id (else the name), so a rename of the entity declaration never strands these
    /// bare-string tokens — this is what lets the `rewriteEntityInInlineTrait` name-rewrite
    /// be deleted. Present only when the trait references an entity in an expression.
    #[serde(
        rename = "entityRefIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub entity_ref_ids: Option<HashMap<String, String>>,

    /// V4 leverage-ids — id side-map for `@trait.<Name>` embed tokens (render-ui embeds /
    /// effect-tree sibling references). Maps the embed token NAME → the embedded trait's
    /// stable id, minted at stamp from the orbital's trait id map. Readers splice embeds by
    /// this id (else the name), which is what lets the `@trait.` name-rewrite be deleted.
    /// Present only when the trait embeds a sibling render trait.
    #[serde(
        rename = "traitEmbedIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub trait_embed_ids: Option<HashMap<String, String>>,

    /// Events this trait emits
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emits: Option<Vec<EmitDefinition>>,

    /// Events this trait listens to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listens: Option<Vec<ListenDefinition>>,

    /// State machine definition
    #[serde(rename = "stateMachine")]
    pub state_machine: StateMachine,

    /// Scheduled ticks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticks: Option<Vec<TickDefinition>>,

    /// Configuration schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, ConfigField>>,

    /// Data entities created by this trait
    #[serde(rename = "dataEntities", skip_serializing_if = "Option::is_none")]
    pub data_entities: Option<Vec<EntityDefinition>>,

    /// Trait scope (V2): `instance` = one record at a time, `collection` =
    /// many-records view. Parsed from the `[instance]` / `[collection]` marker
    /// in a `.lolo` trait header. Optional during the V2 migration; Phase 6
    /// makes it required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<TraitScope>,

    /// `local` trait-header flag, parsed from the same `.lolo` bracket list
    /// as `scope`/`category`/capabilities. Declares client-only dispatch —
    /// a local trait's transitions never await a server round trip.
    /// Undeclared (`false`, the default) means server-gated dispatch;
    /// nothing about dispatch is inferred from effects. Only legal on a
    /// `[shared]` or `[runtime]` entity (Wave 2 enforces). See
    /// `docs/Almadar_LOLO.md` §7 and `OirTrait::dispatch_mode`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,

    /// Set by the inline phase when this trait was cloned from a `uses[]`
    /// import. Absent for traits authored directly on the orbital.
    /// Viewport-only metadata — runtime + codegen ignore it. Studio's L2
    /// canvas buckets cards by alias to give imported behaviors their own
    /// drill-down level. See `docs/Almadar_Studio_Gaps.md` STUDIO-1.
    #[serde(rename = "sourceBehavior", skip_serializing_if = "Option::is_none")]
    pub source_behavior: Option<SourceBehaviorMetadata>,

    /// Set by the inline phase alongside `source_behavior`. Carries the
    /// resolved `EntityDefinition` of the imported behavior so the
    /// client-side mock-data generator can produce rows for the trait's
    /// `linked_entity` without re-walking the import graph at runtime.
    /// See `docs/Almadar_Studio_Gaps.md` STUDIO-2.
    #[serde(
        rename = "sourceEntityDefinition",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_entity_definition: Option<EntityDefinition>,

    /// Increment 3-II: this trait's own declared type parameters —
    /// `(V : Type, R : Type)` in `trait Name (V : Type, R : Type) -> Entity
    /// { ... }`. Present only on an atom's DECLARING inline trait (never
    /// authored on a `Reference`); survives cloning into a consumer's
    /// composed schema so the inline phase can read each param's `kind`
    /// when deciding how to resolve a surviving `"$<name>"` sentinel.
    #[serde(
        rename = "typeParams",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub type_params: Option<Vec<TraitTypeParamDef>>,

    /// Increment 3-II: TRANSIENT carrier for the call-site type-parameter
    /// arguments a `Reference`'s `type_args` supplied, stashed here by the
    /// inline phase the moment the reference is resolved to this cloned
    /// `TraitDefinition` (`phases::inline::trait::inline_traits`) so they
    /// survive to the later per-orbital sentinel-resolution pass
    /// (`phases::inline::rewrite::resolve_orbital_type_param_sentinels`),
    /// which needs the orbital's FULLY composed entity set to resolve an
    /// entity-valued argument. That pass clears this field after consuming
    /// it — it never appears in a final emitted `.orb`.
    #[serde(rename = "typeArgs", default, skip_serializing_if = "Option::is_none")]
    pub type_args: Option<HashMap<String, String>>,
}

impl TraitDefinition {
    /// Client dispatch strategy for this trait — the schema-side entry point
    /// to the same [`crate::runtime::dispatch_mode::compute`] rule
    /// `ir::types::OirTrait::dispatch_mode` calls, so the compiled path and
    /// the runtime kernel can never disagree. `entity` is this trait's
    /// resolved `linked_entity`; `None` when unlinked/unresolved.
    pub fn dispatch_mode(
        &self,
        entity: Option<&EntityDefinition>,
        residence: &crate::effects::Residence,
    ) -> crate::runtime::dispatch_mode::DispatchMode {
        let entity_is_client = entity.is_some_and(EntityDefinition::is_client_resident);
        crate::runtime::dispatch_mode::compute(
            self.local,
            entity_is_client,
            self.touches_server(residence),
        )
    }

    /// Whether this trait's own effects reach the server: any transition, or
    /// any client tick (a `[background]` tick runs on the host, not a dispatch).
    /// A data effect on a client-resident entity (`residence`) is client work.
    pub fn touches_server(&self, residence: &crate::effects::Residence) -> bool {
        use crate::effects::sexpr_runs_server_effect;
        let server = |e: &SExpression| sexpr_runs_server_effect(e, residence);
        self.state_machine
            .transitions
            .iter()
            .filter(|t| !t.guard.as_ref().is_some_and(|g| g.is_statically_false()))
            .any(|t| t.effects.iter().flatten().any(server))
            || self
                .ticks
                .iter()
                .flatten()
                .filter(|t| !t.background)
                .any(|t| t.effects.iter().any(server))
    }

    /// Every entity field NAME this trait's own body reads or writes — the
    /// union of `@entity.<field>` refs and `set @entity.<field>` writers
    /// across every guard, transition effect, tick guard/effect, and
    /// config-field default. Reuses the single canonical walker
    /// `SExpression::collect_entity_field_refs` (the same one
    /// `phases/validation/entity_mutation.rs`'s `collect_entity_field_reads`
    /// and `binding.rs` use in the compiler) — no second S-expression
    /// walker. Entity-agnostic: this says which field NAMES the trait's own
    /// logic touches, not which entity they belong to — a caller matches the
    /// result against a specific `EntityDefinition`.
    pub fn referenced_entity_field_names(&self) -> HashSet<String> {
        let mut referenced: HashMap<String, String> = HashMap::new();
        let mut set_fields: HashSet<String> = HashSet::new();
        for transition in &self.state_machine.transitions {
            if let Some(guard) = &transition.guard {
                guard.collect_entity_field_refs(&mut referenced, &mut set_fields, "GUARD");
            }
            if let Some(effects) = &transition.effects {
                for effect in effects {
                    effect.collect_entity_field_refs(&mut referenced, &mut set_fields, "EFFECT");
                }
            }
        }
        if let Some(ticks) = &self.ticks {
            for tick in ticks {
                if let Some(guard) = &tick.guard {
                    guard.collect_entity_field_refs(&mut referenced, &mut set_fields, "TICK_GUARD");
                }
                for effect in &tick.effects {
                    effect.collect_entity_field_refs(&mut referenced, &mut set_fields, "TICK");
                }
            }
        }
        if let Some(config) = &self.config {
            for field in config.values() {
                if let Some(default) = &field.default {
                    default.collect_entity_field_refs(
                        &mut referenced,
                        &mut set_fields,
                        "CONFIG_DEFAULT",
                    );
                }
            }
        }
        let mut out: HashSet<String> = referenced.into_keys().collect();
        out.extend(set_fields);
        out
    }
}

/// Provenance metadata attached to a trait that was cloned from a `uses[]`
/// import during the inline phase. Serialised on the resolved `.orb` JSON
/// so Studio can group L2 cards by source behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBehaviorMetadata {
    /// Behavior name (e.g. `"std-stat-card"`) — the resolved `from:` value.
    pub behavior: String,

    /// Alias used at the import site (e.g. `"Stat"`). Multiple imports of
    /// the same atom under different aliases group separately.
    pub alias: String,

    /// Original trait name inside the imported behavior, before any
    /// call-site `name:` rename.
    pub original_name: String,
}

/// Trait category
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TraitCategory {
    Interaction,
    Integration,
    Lifecycle,
    GameCore,
    GameEntity,
    GameUi,
}

/// Trait scope (V2 entity migration). A trait operates over either a single
/// record (`instance`) or a group (`collection`). Drives payload inference
/// in the typed emit grammar and later phases of the V2 plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TraitScope {
    Instance,
    Collection,
}

/// Configuration field definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_default"
    )]
    pub default: Option<SExpression>,
    /// Human-readable question prompt for the studio's questionnaire.
    /// Authored in `.lolo` as `@label "..."` on the line below the
    /// config field declaration. When unset, the questionnaire falls
    /// back to a humanized version of the key name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Long-form description / help text shown under the label in the
    /// studio's questionnaire. Authored as `@description "..."` on the
    /// line below the config field declaration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// User-vocabulary synonyms — comma-separated free text authored
    /// in `.lolo` as `@synonyms "..."` on the line below the config
    /// field declaration. Threaded through the .orb pipeline so the
    /// agent's catalog-summary prompt + the knob-embeddings bake can
    /// widen recall on prompts that name the knob via synonym
    /// ("taller" → `height`, "highest to lowest" → `sortDir=desc`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,
    /// Visibility tier — authored in `.lolo` as `@tier "..."` on the
    /// line below the config field declaration. One of `essential`,
    /// `customization`, `advanced`, `internal`. Drives the studio
    /// Questionnaire's filter + disclosure: `internal` is hidden
    /// entirely, `advanced` collapses behind a "Show advanced" toggle.
    /// Validated by the L2 `ORB_F_TIER_INVALID` rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Literal-union variants for fields declared as `"a" | "b" | "c"` in
    /// the .lolo source. Mirrors the entity-side `FieldDefinition.values`
    /// shape — preserving the closed set of legal values through the .orb
    /// pipeline so downstream consumers (the std-functions regenerator,
    /// validators, agent tool schemas) can produce literal-union TS types
    /// instead of falling back to bare `string`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    /// Array item schema for `[T]` config slots — mirrors
    /// `FieldDefinition.items`. Populated by lowering named array types
    /// (e.g. `metrics : [MetricSpec]`) so consumers see the full
    /// item shape, not just the `"[MetricSpec]"` type tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<FieldDefinition>>,
    /// Object property schema for inline / named object config slots
    /// — mirrors `FieldDefinition.properties`. Populated when the config
    /// type resolves to a `TypeExpr::Object` (struct alias or literal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, FieldDefinition>>,
    /// `!` required marker — authored in `.lolo` as `fields : [FieldSpec]!`.
    /// Declares that call sites referencing this trait MUST supply this
    /// knob's key in their `config` block (any value form). Coexists with
    /// `default`: the default serves the atom's own demo page binding,
    /// which is exempt from the requirement. Validated by the L2
    /// call-site config rules (unknown key / missing required key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// V4-W5 dual-carry id — set by the stamp when `field_type` is a reference
    /// config type (`entity`/`trait`/`event`): the stable id of the node the
    /// field's `default` value names. Readers (compiler `identity_normalize` +
    /// runtime resolver) resolve the value to that node's CURRENT name by id, so
    /// a declaration-only rename flows through a config value. Absent otherwise.
    #[serde(rename = "refId", skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    /// The declared field this knob's type was projected from — `.lolo`
    /// `T.f`; see [`FieldDefinition::projected_from`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectedFrom"
    )]
    pub projected_from: Option<FieldProjection>,
    /// The original `@config.<knob>` forward token this field's `default`
    /// collapsed FROM, when the inline phase resolved a forward against the
    /// orbital/schema rung (`forwarded_sibling_config_from`,
    /// `orbital-compiler`'s `phases::inline`). Set only at the moment of
    /// collapse, only when the value truly came from a forward — never
    /// stamped on a field whose default was always concrete. Carries the
    /// provenance the RESOLVED `.orb`'s `default` alone can no longer show
    /// (the literal token is gone, replaced by its resolved value), so
    /// `ORB_O_CONFIG_DEAD_KNOB`'s forwarded-anywhere check — which runs on
    /// the resolved schema, in both the Rust and JS (`lintWiring`
    /// `orbital-config-knob-unforwarded`) compilers — can still see that the
    /// declaring knob was consumed. One owner, in the IR, so the provenance
    /// crosses the wire instead of living in a side channel per language.
    #[serde(rename = "forwardedFrom", skip_serializing_if = "Option::is_none")]
    pub forwarded_from: Option<String>,
    /// The `@config.<knob>` tokens nested INSIDE this field's value (an
    /// array element or object member, e.g. `metrics[].target`) that the
    /// inline phase resolved to their values — the list twin of
    /// `forwarded_from`, so `ORB_O_CONFIG_DEAD_KNOB` still sees those knobs
    /// as read once the tokens are gone. Empty when nothing nested resolved.
    #[serde(
        rename = "nestedForwardedFrom",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub nested_forwarded_from: Vec<String>,
    /// The definer knob name this field is a payload COMPANION of, when
    /// this field's declared type was `@payload(<knob>)` — set by
    /// `resolve_sigil_types` at the moment it rewrites `field_type` from
    /// the sentinel to the resolved payload type name (`GeneratePayload`),
    /// the same way `forwarded_from` preserves a provenance the resolved
    /// `default`/`field_type` can no longer show on their own. `None` for
    /// any config field that was never a `@payload(<knob>)` sentinel.
    #[serde(rename = "payloadFor", skip_serializing_if = "Option::is_none")]
    pub payload_for: Option<String>,
}

impl ConfigField {
    /// True when this entry is a fully-annotated DECLARATION (a NEW typed
    /// knob) rather than a plain call-site wiring/override of an already
    /// declared knob. Mirrors the `is_plain_wiring` structural check the
    /// lowerer applies when building a trait Reference's `config` map
    /// (`orbital-lolo/src/lower/trait.rs::lower_trait_reference`): a plain
    /// wiring entry always lowers to `field_type == "unknown"` with no
    /// label/description/synonyms/tier annotations; anything else — a real
    /// type tag or any annotation — only comes from a `.lolo` declaration
    /// (`key : Type = default @label "..."`), never from a plain override
    /// value.
    pub fn is_declaration_form(&self) -> bool {
        !(self.field_type == "unknown"
            && self.label.is_none()
            && self.description.is_none()
            && self.synonyms.is_none()
            && self.tier.is_none())
    }
}

// =============================================================================
// Event Contracts
// =============================================================================

/// Binding grammar for a call-site-captured event payload reference.
///
/// When an inline trait REFERENCE's config value binds `@payload.<field>` at a
/// call site inside a composing render effect (e.g. JSX `<Typography
/// content={?error} />` where the composing transition's event declares
/// `error`), the lolo lowerer rewrites that value to
/// `@callsitePayload.<field>`. This distinct, source-tagged form means:
/// "resolve against the COMPOSING effect's triggering event payload at the call
/// site, then snapshot the literal into the child's config". It is deliberately
/// NOT `@payload.` so the child's own INIT scope never evaluates it — the
/// capture belongs to the parent transition's scope, not the child's.
pub const CALLSITE_PAYLOAD_PREFIX: &str = "@callsitePayload.";
/// The ordinary event-payload binding grammar the capture is rewritten FROM.
pub const PAYLOAD_PREFIX: &str = "@payload.";

/// If `s` is a call-site payload capture (`@callsitePayload.<path>`), return the
/// dotted `<path>` (the event-payload field path).
pub fn callsite_payload_path(s: &str) -> Option<&str> {
    s.strip_prefix(CALLSITE_PAYLOAD_PREFIX)
        .filter(|p| !p.is_empty())
}

/// Rewrite every `@payload.<path>` string inside a call-site config VALUE to the
/// call-site-captured form `@callsitePayload.<path>`. Applied ONLY to inline
/// trait-reference config values (the call-site override bucket), where a
/// `@payload` reference is, by position, a capture from the COMPOSING effect's
/// event — never the child atom's own payload.
pub fn capture_callsite_payload(expr: &mut SExpression) {
    match expr {
        SExpression::String(s) => {
            if let Some(rest) = s.strip_prefix(PAYLOAD_PREFIX) {
                *s = format!("{CALLSITE_PAYLOAD_PREFIX}{rest}");
            }
        }
        SExpression::Array(items) => items.iter_mut().for_each(capture_callsite_payload),
        SExpression::Object(map) => map.values_mut().for_each(capture_callsite_payload),
        SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
    }
}

/// First path segment (the payload FIELD name) of a `@payload`/`@callsitePayload`
/// dotted path — `data` from `data`, `data[0]` from `data[0].x`, `row` from
/// `row.id`. The field is what an event's `payloadSchema` declares.
fn payload_field_head(path: &str) -> &str {
    path.split(['.', '[']).next().unwrap_or(path)
}

/// Re-derive the payload-capture tag on every payload string in a call-site
/// config VALUE from actual scope evidence, rather than blanket-tagging by
/// position (`capture_callsite_payload`). For each `@payload.<field>` /
/// `@callsitePayload.<field>`, if `<field>` is in `local_fields` — the payload
/// fields the CHILD's own transition that renders this knob declares — the
/// binding resolves in the child's scope, so it is the ordinary `@payload.<path>`;
/// otherwise the field must come from the COMPOSING event, so it is the capture
/// `@callsitePayload.<path>`. Idempotent and source-agnostic (accepts either
/// incoming form), so a lower-time eager tag or a baked/untagged `.orb` both
/// converge to the correct tag. Fixes the over-approximation where overriding a
/// render-ui body knob (e.g. `galleryBodyContent`, rendered under the child's
/// own data-load transition) mis-tagged its `@payload.data` as a call-site
/// capture and validated it against the wrapper's events.
pub fn retag_callsite_payload(
    expr: &mut SExpression,
    local_fields: &std::collections::HashSet<String>,
) {
    match expr {
        SExpression::String(s) => {
            let rest = s
                .strip_prefix(PAYLOAD_PREFIX)
                .or_else(|| s.strip_prefix(CALLSITE_PAYLOAD_PREFIX));
            if let Some(rest) = rest {
                let is_local = local_fields.contains(payload_field_head(rest));
                *s = if is_local {
                    format!("{PAYLOAD_PREFIX}{rest}")
                } else {
                    format!("{CALLSITE_PAYLOAD_PREFIX}{rest}")
                };
            }
        }
        SExpression::Array(items) => items
            .iter_mut()
            .for_each(|i| retag_callsite_payload(i, local_fields)),
        SExpression::Object(map) => map
            .values_mut()
            .for_each(|v| retag_callsite_payload(v, local_fields)),
        SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
    }
}

/// Collect the dotted field paths of every `@callsitePayload.<path>` string in
/// `expr`.
pub fn collect_callsite_payload_paths(expr: &SExpression, out: &mut Vec<String>) {
    match expr {
        SExpression::String(s) => {
            if let Some(p) = callsite_payload_path(s) {
                out.push(p.to_string());
            }
        }
        SExpression::Array(items) => items
            .iter()
            .for_each(|i| collect_callsite_payload_paths(i, out)),
        SExpression::Object(map) => map
            .values()
            .for_each(|v| collect_callsite_payload_paths(v, out)),
        SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
    }
}

/// True when a config value IS or CONTAINS a call-site payload capture. The
/// inline phase uses this to keep the bound knob UNSUBSTITUTED in the child
/// body (leaving `@config.<knob>`), so each engine resolves the capture against
/// the composing effect's payload at the call site and passes the literal into
/// the child's config.
pub fn contains_callsite_payload(expr: &SExpression) -> bool {
    let mut v = Vec::new();
    collect_callsite_payload_paths(expr, &mut v);
    !v.is_empty()
}

/// `@config.<knob>` is the only legal reference form for an emit/listen event
/// name (Option B — the declared event name resolves to a string-typed
/// config knob's value at inline time). Returns the knob identifier when
/// `event` matches that form; `None` for a plain literal event name.
pub fn config_ref_event_knob(event: &str) -> Option<&str> {
    let knob = event.strip_prefix("@config.")?;
    if knob.is_empty() || knob.contains('.') {
        return None;
    }
    Some(knob)
}

/// The set of a trait's config knobs wired in a DEFINER position — named by
/// `emits { @config.<knob> }`, a stateMachine `events[].key` sentinel, or a
/// transition `event` sentinel. A definer knob's VALUE CREATES the event
/// name (A-UNIFY); it is exempt from event-set membership checks, unlike a
/// reference knob whose value must name an already-declared event. Single
/// owner of this rule — `orbital-compiler`'s `trait_event_ctx` (definer half)
/// and `stamp_config_ref_ids` (refId-stamping skip) both call through here
/// rather than recomputing it, since `orbital-core` is the shared ancestor of
/// both call sites and this is trait-declaration-only (no cross-orbital
/// resolution).
pub fn definer_knobs_for_trait(trait_def: &TraitDefinition) -> std::collections::HashSet<String> {
    let mut definer_knobs: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(emits) = &trait_def.emits {
        for emit in emits {
            if let Some(knob) = config_ref_event_knob(&emit.event) {
                definer_knobs.insert(knob.to_string());
            }
        }
    }
    if let Some(events) = &trait_def.state_machine.events {
        for event in events {
            if let Some(knob) = config_ref_event_knob(&event.key) {
                definer_knobs.insert(knob.to_string());
            }
        }
    }
    for transition in &trait_def.state_machine.transitions {
        if let Some(knob) = config_ref_event_knob(&transition.event) {
            definer_knobs.insert(knob.to_string());
        }
    }
    definer_knobs
}

/// Why a `@config.<knob>` emit/listen event-name reference failed to
/// resolve, per the Option B contract: the knob must be a DECLARED
/// string-typed config field with a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRefEventError {
    UnknownKnob,
    NotString,
    NoDefault,
}

/// Resolve a `@config.<knob>` emit/listen event-name reference to its
/// concrete literal. `effective_config` is the call-site-override-over-
/// declared-default map (same precedence `apply_overrides_to_trait` already
/// builds for `@config.X` body substitution); `declared_config` is the
/// trait's own `config {}` schema, used to check the string/event+default
/// contract. The single owner of this resolution rule — both the Rust
/// inline phase and the JS runtime reference-resolver call through the
/// equivalent contract (`resolveConfigRefEventName` in `@almadar/core`).
/// A-UNIFY: definer knobs spell `: event` same as reference knobs — the
/// VALUE must still be a plain string literal (checked below), only the
/// declared field TYPE is now `"string" | "event"`.
pub fn resolve_config_ref_event_name(
    event: &str,
    effective_config: &crate::schema::SObject,
    declared_config: Option<&HashMap<String, ConfigField>>,
) -> Result<String, ConfigRefEventError> {
    let knob = config_ref_event_knob(event).ok_or(ConfigRefEventError::UnknownKnob)?;
    let field = declared_config
        .and_then(|d| d.get(knob))
        .ok_or(ConfigRefEventError::UnknownKnob)?;
    if field.field_type != "string" && field.field_type != "event" {
        return Err(ConfigRefEventError::NotString);
    }
    if field.default.is_none() {
        return Err(ConfigRefEventError::NoDefault);
    }
    match effective_config.get(knob) {
        Some(SExpression::String(s)) => Ok(s.clone()),
        _ => Err(ConfigRefEventError::NotString),
    }
}

/// Event emission definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmitDefinition {
    /// Event name
    pub event: String,

    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(rename = "eventId", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,

    /// Event description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (comma-separated) for the event matcher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Authoring tier (`essential`/`customization`/`advanced`/`internal`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,

    /// Event scope
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<EventScope>,

    /// Payload schema — declarative type info for the event's payload.
    /// JSON key: `payloadSchema`. Distinct from the runtime payload value
    /// (`@payload.X` bindings, `EventPayload`) which is unrelated.
    #[serde(rename = "payloadSchema", skip_serializing_if = "Option::is_none")]
    pub payload_schema: Option<Vec<PayloadField>>,

    /// The config knob whose VALUE produced `event`, when this emit was
    /// declared as `emits { @config.<knob> -> … }`. Set by the inline phase,
    /// which resolves the reference to a literal and — before this field —
    /// discarded the fact that it ever was one.
    ///
    /// Why it matters: an atom's definer-knob emit is an OFFER (a DataGrid
    /// exposes `SORT`/`LOAD_MORE`; an Alert exposes `DISMISS`), and a consumer
    /// declining to wire it is an unsorted grid, not a defect. Without this,
    /// an unwired offer is indistinguishable from an authored emit that lost
    /// its listener, so every offer had to be reported.
    ///
    /// `None` = the event name was authored literally, which IS an obligation.
    #[serde(
        rename = "definerKnob",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub definer_knob: Option<String>,

    /// `Some(true)` when `scope` was set by a CALL-SITE `emitsScope` override
    /// (the inline phase applies it), not by the source atom's own `->
    /// external` declaration. The distinction carries consequence: an atom
    /// declaring its emits external is an unexamined default (nearly every
    /// atom does), while a composition restating it is the consumer's own
    /// deferral — "my consumer wires this" — which the dead-control check
    /// honours as an exemption. See ORB-X-EXTERNAL-EMIT-EXEMPTION.
    #[serde(
        rename = "scopeOverridden",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub scope_overridden: Option<bool>,

    /// The entity this WHOLE payload was declared as (`type X = Event
    /// <EntityName>`, the bare/concrete form) — set by `orbital-lolo` at
    /// lowering, when the alias's payload resolves to a known entity's own
    /// field list flattened at the payload root (G4,
    /// `Almadar_Compiler_Gaps.md` §82). Distinct from `PayloadField.entity`
    /// (a single NESTED field's value is an entity instance): this marks
    /// the payload schema ITSELF as a 1:1 mirror of that entity's CURRENT
    /// fields. Consumed by `resolve_type_param_sentinels`
    /// (`phases/inline/rewrite.rs`) to re-derive `payload_schema` from the
    /// entity's post-`fields{}`/`extend{}` shape once every orbital import
    /// or trait-reference override has materialized; kept in sync across
    /// entity renames by `rewrite_payload_entity_refs`, the same one owner
    /// `PayloadField.entity` already uses. `None` for any other payload
    /// shape (struct/primitive/sentinel).
    #[serde(
        rename = "payloadEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_entity: Option<String>,
}

/// Event listener definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenDefinition {
    /// Event to listen for (bare event name once `source` is populated;
    /// legacy concatenated form `"Source.EVENT"` when older .orb files are loaded)
    pub event: String,

    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(rename = "eventId", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,

    /// Internal event to trigger (defaults to same as event if not specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggers: Option<String>,

    /// V4 dual-carry id sibling of `triggers` — optional until the Phase-7 flip.
    #[serde(
        rename = "triggersId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub triggers_id: Option<String>,

    /// Event scope
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<EventScope>,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (comma-separated) for the event matcher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Authoring tier (`essential`/`customization`/`advanced`/`internal`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,

    /// Payload mapping
    #[serde(rename = "payloadMapping", skip_serializing_if = "Option::is_none")]
    pub payload_mapping: Option<crate::schema::SObject>,

    /// Guard condition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<SExpression>,

    /// Source-scope descriptor parsed from the `.lolo` authoring layer.
    /// See the runtime-side `ListenSourceDescriptor` for the matching shape.
    /// `None` means the listen entry is a local payload-declaration only
    /// (pure type metadata, no bus subscription).
    #[serde(rename = "source", skip_serializing_if = "Option::is_none")]
    pub source: Option<ListenSource>,
}

/// Source-scoping for a listen entry.
///
/// Mirrors the TypeScript `ListenSource` in `@almadar/core`. The authoring
/// forms are:
/// - `* EVENT -> TRIGGER`             → `ListenSource::Any`
/// - `TraitName EVENT -> TRIGGER`     → `ListenSource::Trait { trait }`
/// - `Orbital.Trait EVENT -> TRIGGER` → `ListenSource::Orbital { orbital, trait }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ListenSource {
    Any,
    Trait {
        #[serde(rename = "trait")]
        trait_name: String,
        /// V4 dual-carry id sibling of `trait` — optional until the Phase-7 flip.
        #[serde(rename = "traitId", default, skip_serializing_if = "Option::is_none")]
        trait_id: Option<String>,
    },
    Orbital {
        orbital: String,
        /// V4 dual-carry id sibling of `orbital` — optional until the Phase-7 flip.
        #[serde(rename = "orbitalId", default, skip_serializing_if = "Option::is_none")]
        orbital_id: Option<String>,
        #[serde(rename = "trait")]
        trait_name: String,
        /// V4 dual-carry id sibling of `trait` — optional until the Phase-7 flip.
        #[serde(rename = "traitId", default, skip_serializing_if = "Option::is_none")]
        trait_id: Option<String>,
    },
}

/// Split a `listens`-addressed event (`EVENT`, `Trait.EVENT`, `*.EVENT`,
/// `Orbital.Trait.EVENT`) into the bare event and its source. The one parser
/// of the address form, shared by `listens` lowering and event-typed values.
pub fn split_event_address(raw: &str) -> (String, Option<ListenSource>) {
    let parts: Vec<&str> = raw.split('.').collect();
    match parts.len() {
        0 | 1 => (raw.to_string(), None),
        2 => {
            let (src, ev) = (parts[0], parts[1]);
            if src == "*" {
                (ev.to_string(), Some(ListenSource::Any))
            } else {
                (
                    ev.to_string(),
                    Some(ListenSource::Trait {
                        trait_name: src.to_string(),
                        trait_id: None,
                    }),
                )
            }
        }
        n => (
            parts[n - 1].to_string(),
            Some(ListenSource::Orbital {
                orbital: parts[..n - 2].join("."),
                trait_name: parts[n - 2].to_string(),
                orbital_id: None,
                trait_id: None,
            }),
        ),
    }
}

/// Inverse of [`split_event_address`].
pub fn join_event_address(event: &str, source: Option<&ListenSource>) -> String {
    match source {
        None => event.to_string(),
        Some(ListenSource::Any) => format!("*.{event}"),
        Some(ListenSource::Trait { trait_name, .. }) => format!("{trait_name}.{event}"),
        Some(ListenSource::Orbital {
            orbital,
            trait_name,
            ..
        }) => format!("{orbital}.{trait_name}.{event}"),
    }
}

/// Event scope
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EventScope {
    Internal,
    External,
}

/// One condition of a payload field's `type_when` conditional list: "when
/// config knob `knob` resolves to the literal whose string spelling is
/// `equals`, this field's type is `ty`". Declared DATA — authored in
/// `.lolo` as `@typeWhen "<knob>=<literal>-><type>"` — never a compiler-side
/// predicate over a specific knob name. See `resolve_payload_type_when`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PayloadTypeWhen {
    pub knob: String,
    pub equals: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// Payload field definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PayloadField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    /// Closed value set (`"a" | "b"`), as on an entity field or a config knob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Nested object shape for an anonymous struct payload field (`field_type`
    /// = `"object"`). Set when the field is an inline `{ … }` object or a
    /// `type X = { … }` struct alias, so codegen emits a strict inline object
    /// type instead of the shapeless `EventPayload`. Entity-referencing fields
    /// keep the flat dotted-flatten convention and leave this `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<PayloadField>>,
    /// Declared knob-conditional type list (`@typeWhen` in `.lolo`). Each
    /// condition is checked in order against the trait's DECLARED config
    /// (resolved literal); the first match's `ty` is this field's real type
    /// at that call site. Resolved and CLEARED by
    /// `resolve_payload_type_when`'s callers once composed — the emitted
    /// `.orb` carries the resolved truth, not the conditional.
    #[serde(rename = "typeWhen", skip_serializing_if = "Option::is_none")]
    pub type_when: Option<Vec<PayloadTypeWhen>>,
    /// The declared entity this field's value IS an instance of — stamped by
    /// lowering when an entity-typed payload field (`row: Note`, scalar or
    /// `[Note]` array — the array form stamps the element entity) is
    /// flattened into `field_type: "object"`/`"[object]"` + `properties`.
    /// `None` for an anonymous struct/object field with no declared entity.
    /// Never inferred from shape or name downstream — this is the only
    /// source of entity identity for a payload field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// The declared field this payload field's value — each element's, for an
    /// array type — was projected from: `.lolo` `T.f` / `[T.f]`
    /// (`tagId : NoteTagRef.id`); see [`FieldDefinition::projected_from`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectedFrom"
    )]
    pub projected_from: Option<FieldProjection>,
}

/// The canonical string spelling of a resolved config literal, for comparison
/// against a `PayloadTypeWhen::equals` string. A `String` compares by its own
/// content; `Bool`/`Number` compare by their canonical Rust `Display`
/// spelling (`"true"`/`"false"`, integers without a trailing `.0`). Any other
/// shape (array/object/null) has no literal spelling and never matches.
fn config_literal_spelling(value: &SExpression) -> Option<String> {
    match value {
        SExpression::String(s) => Some(s.clone()),
        SExpression::Bool(b) => Some(b.to_string()),
        SExpression::Number(n) if n.fract() == 0.0 && n.abs() < i64::MAX as f64 => {
            Some((*n as i64).to_string())
        }
        SExpression::Number(n) => Some(n.to_string()),
        SExpression::Array(_) | SExpression::Object(_) | SExpression::Null => None,
    }
}

/// Resolve a payload field's `type_when` conditional list to a concrete type,
/// given the trait's DECLARED config (`config[knob].default` — by the time
/// either caller reads it, post-inline or post-validation-inline, this
/// already carries the call site's resolved override; see
/// `apply_overrides_to_trait`'s GAP-AG-VALUE-DRIFT fold). First condition
/// whose `knob` resolves to a literal spelled exactly `equals` wins. A
/// forwarded/unresolved knob (absent, non-literal, or not declared) matches
/// nothing — same safety semantics as the narrowing this replaces. Carries
/// ZERO knob-name or pattern-name knowledge: `conditions` is authored data,
/// this only evaluates it.
pub fn resolve_payload_type_when(
    conditions: &[PayloadTypeWhen],
    declared_config: Option<&HashMap<String, ConfigField>>,
) -> Option<String> {
    conditions.iter().find_map(|cond| {
        let resolved = declared_config?.get(&cond.knob)?.default.as_ref()?;
        (config_literal_spelling(resolved).as_deref() == Some(cond.equals.as_str()))
            .then(|| cond.ty.clone())
    })
}

// =============================================================================
// State Machine
// =============================================================================

/// State machine definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMachine {
    /// States in the machine
    pub states: Vec<StateDefinition>,

    /// Events (for documentation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<EventDefinition>>,

    /// Transitions
    pub transitions: Vec<TransitionDefinition>,
}

/// State definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateDefinition {
    /// State name
    pub name: String,

    /// Whether this is the initial state
    #[serde(rename = "isInitial", skip_serializing_if = "Option::is_none")]
    pub is_initial: Option<bool>,

    /// Whether this is a terminal state (no outgoing transitions expected)
    #[serde(rename = "isTerminal", skip_serializing_if = "Option::is_none")]
    pub is_terminal: Option<bool>,

    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Duration in milliseconds after which a synthesized/explicit TIMEOUT
    /// transition should fire, from `state X for Ns { ... }`. `None` for a
    /// state with no time bound.
    #[serde(rename = "timeBoundMs", skip_serializing_if = "Option::is_none")]
    pub time_bound_ms: Option<u64>,
}

/// Event definition (for documentation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDefinition {
    /// Event key
    pub key: String,

    /// V4 dual-carry id sibling of `key` — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Display name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Semantic description (authored `@description` on the emit/listen), the
    /// per-event analogue of a field/config `@description`. Feeds the matcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// User-vocabulary synonyms (comma-separated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,

    /// Authoring tier (`essential`/`customization`/`advanced`/`internal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,

    /// Payload schema — declarative type info for the event's payload.
    /// JSON key: `payloadSchema`. Distinct from the runtime payload value
    /// (`@payload.X` bindings, `EventPayload`) which is unrelated.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "payloadSchema"
    )]
    pub payload_schema: Option<Vec<PayloadField>>,

    /// Same marker as `EmitDefinition.payload_entity` — this transition's
    /// bare-event payload was declared `type X = Event <EntityName>` and is
    /// a flat mirror of that entity's fields. See that field's doc for the
    /// full contract (G4, `Almadar_Compiler_Gaps.md` §82).
    #[serde(
        rename = "payloadEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub payload_entity: Option<String>,

    /// A declared external input (`listens { EVENT -> external { … } }`): a
    /// client outside the program (an API, an agent, an MCP client) may send
    /// this event to the trait. Absent = not an input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<bool>,

    /// Declared in the trait's own `listens { EVENT … }` block (the local form,
    /// external or not): an input the author offers to the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listened: Option<bool>,
}

/// Transition definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionDefinition {
    /// Source state(s)
    pub from: StringOrArray,

    /// Target state
    pub to: String,

    /// Triggering event
    pub event: String,

    /// V4 dual-carry id sibling of `event` — optional until the Phase-7 flip.
    #[serde(rename = "eventId", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,

    /// Guard condition (S-expression)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<SExpression>,

    /// Effects to execute (S-expressions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<SExpression>>,
}

/// String or array of strings (for `from` field)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StringOrArray {
    Single(String),
    Multiple(Vec<String>),
}

impl std::fmt::Display for StringOrArray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StringOrArray::Single(s) => f.write_str(s),
            StringOrArray::Multiple(arr) => write!(f, "[{}]", arr.join(", ")),
        }
    }
}

impl StringOrArray {
    /// Check if a state matches
    pub fn matches(&self, state: &str) -> bool {
        match self {
            StringOrArray::Single(s) => s == state,
            StringOrArray::Multiple(arr) => arr.iter().any(|s| s == state),
        }
    }

    /// Get all states as a slice
    pub fn states(&self) -> Vec<&str> {
        match self {
            StringOrArray::Single(s) => vec![s.as_str()],
            StringOrArray::Multiple(arr) => arr.iter().map(|s| s.as_str()).collect(),
        }
    }
}

// =============================================================================
// Tick Definition
// =============================================================================

/// Scheduled tick definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickDefinition {
    /// Tick name
    pub name: String,

    /// Interval (milliseconds, "5s", "1m", or cron expression)
    pub interval: StringOrNumber,

    /// Guard condition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<SExpression>,

    /// Effects to execute
    pub effects: Vec<SExpression>,

    /// The trait states the tick runs in; absent or empty = every state.
    #[serde(rename = "appliesTo", skip_serializing_if = "Option::is_none")]
    pub applies_to: Option<Vec<String>>,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// `[background]`: runs with no client mounted; otherwise paused while no client has the trait mounted.
    #[serde(
        rename = "runsInBackground",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub background: bool,
}

impl TickDefinition {
    /// Resolve this tick's interval to milliseconds. `None` = per-frame
    /// (`"frame"`/`"every-frame"`), fired on every client frame. Mirrors the JS
    /// runtime's `parseIntervalString` (`5s`/`1m`/`1h`/`30000`); an unrecognized
    /// string falls back to 1000ms. The single canonical interval parser shared by
    /// the runtime kernel's tick path and the client tick scheduler.
    pub fn interval_ms(&self) -> Option<u64> {
        match &self.interval {
            StringOrNumber::Number(n) => Some(n.max(0.0) as u64),
            StringOrNumber::String(s) => {
                let s = s.trim();
                if s.eq_ignore_ascii_case("frame") || s.eq_ignore_ascii_case("every-frame") {
                    return None;
                }
                let (digits, unit): (String, String) = s.chars().partition(|c| c.is_ascii_digit());
                let value: u64 = digits.parse().unwrap_or(1000);
                match unit.trim() {
                    "ms" | "" => Some(value),
                    "s" => Some(value * 1000),
                    "m" => Some(value * 60 * 1000),
                    "h" => Some(value * 60 * 60 * 1000),
                    _ => Some(1000),
                }
            }
        }
    }
}

/// String or number (for intervals)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StringOrNumber {
    String(String),
    Number(f64),
}

// =============================================================================
// Page Definition
// =============================================================================

/// Page (route) definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageDefinition {
    /// Page name
    pub name: String,

    /// V4 dual-carry node id — optional until the Phase-7 flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Route path
    pub path: String,

    /// View type
    #[serde(rename = "viewType", skip_serializing_if = "Option::is_none")]
    pub view_type: Option<String>,

    /// Primary entity for this page
    #[serde(rename = "primaryEntity", skip_serializing_if = "Option::is_none")]
    pub primary_entity: Option<String>,

    /// Whether this is the initial page
    #[serde(rename = "isInitial", skip_serializing_if = "Option::is_none")]
    pub is_initial: Option<bool>,

    /// Traits active on this page
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<TraitReference>>,

    /// Page sections
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<SectionDefinition>>,

    /// Nav icon name — `@icon "..."` annotation on the page declaration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Nav label — `@label "..."` annotation. Only a page that declares one is
    /// a `@pages` nav entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Roles the nav entry shows for — `@roles "a, b"` annotation. Absent = everyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,

    /// `access:` page modifier. Absent = the app's authored behavior, no gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PageAccess>,

    /// `indexing:` page modifier — a search directive, never access control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<PageIndexing>,

    /// `title:` page modifier — the document title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<PageMeta>,

    /// `description:` page modifier — the document meta description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<PageMeta>,
    /// The upstream page this one was imported from (its id, kept through
    /// nested imports). Imports of one upstream page in different locales are
    /// language alternates of each other. Set by inlining, never authored.
    #[serde(
        rename = "sourcePage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_page: Option<String>,
    /// `translationOf:` page modifier — pages sharing it in different locales
    /// are language alternates (takes precedence over `sourcePage`).
    #[serde(
        rename = "translationOf",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub translation_of: Option<String>,
}

/// Who may open a page: `public` admits anonymous viewers; `authenticated`
/// mounts the page circuit only for a resolved signed-in viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageAccess {
    Public,
    Authenticated,
}

/// Search-engine directive for a page's document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageIndexing {
    Index,
    Noindex,
}

/// A page metadata value: a literal, or `(i18n/t "key")` resolved against the
/// route's locale. Nothing else may bind (no entity/config/viewer frame).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PageMeta {
    Literal(String),
    Message(PageMessage),
}

/// `["i18n/t", "<key>"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageMessage(pub I18nHead, pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum I18nHead {
    #[serde(rename = "i18n/t")]
    T,
}

impl PageMeta {
    pub fn message(key: impl Into<String>) -> Self {
        PageMeta::Message(PageMessage(I18nHead::T, key.into()))
    }

    /// The literal text, or the message resolved through `lookup`.
    pub fn resolve<'a>(&'a self, lookup: impl Fn(&str) -> Option<&'a str>) -> Option<&'a str> {
        match self {
            PageMeta::Literal(s) => Some(s),
            PageMeta::Message(PageMessage(_, key)) => lookup(key),
        }
    }
}

/// The page modifiers a `pages {}` remap entry may declare for one imported page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageModifiers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PageAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexing: Option<PageIndexing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<PageMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<PageMeta>,
    #[serde(
        rename = "translationOf",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub translation_of: Option<String>,
    /// The page's nav entry label (a `pages {}` remap entry's `label:`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The page's nav entry icon (a `pages {}` remap entry's `icon:`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The roles the page's nav entry shows for (a `pages {}` remap entry's `roles:`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
}

/// The sign-in route every host provides itself (the compiled shell's login form, the
/// runtime host's sign-in surface). `signIn:` may name it without a page, and no page
/// may be declared on it.
pub const HOST_SIGN_IN_ROUTE: &str = "/login";

/// Whether `path` is [`HOST_SIGN_IN_ROUTE`], ignoring a trailing slash.
pub fn is_host_sign_in_route(path: &str) -> bool {
    path.trim_end_matches('/') == HOST_SIGN_IN_ROUTE
}

/// App-level site modifiers from the `app … [ … ]` bracket.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SiteMeta {
    /// Canonical production origin, e.g. `https://orb.almadar.io`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Site icon path served from the public folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Site name (og:site_name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Route an `access: authenticated` page sends a non-admitted viewer to.
    #[serde(rename = "signIn", default, skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<String>,
}

impl SiteMeta {
    pub fn is_empty(&self) -> bool {
        *self == SiteMeta::default()
    }
}

/// Section definition within a page
/// DEPRECATED: Use render-ui effects instead
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionDefinition {
    /// Section slot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,

    /// Pattern type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<PatternDefinition>,

    /// Props
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props: Option<crate::schema::SObject>,
}

/// Pattern definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternDefinition {
    #[serde(rename = "type")]
    pub pattern_type: String,
}

// =============================================================================
// Domain & Design
// =============================================================================

/// Domain context metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocabulary: Option<HashMap<String, String>>,
    #[serde(rename = "requestFragment", skip_serializing_if = "Option::is_none")]
    pub request_fragment: Option<String>,
}

/// Design hints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignHints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(rename = "uxHints", skip_serializing_if = "Option::is_none")]
    pub ux_hints: Option<HashMap<String, String>>,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    #[test]
    fn busy_delay_reads_css_milliseconds_and_seconds() {
        let tokens = |delay: &str| super::MotionTokens {
            durations: None,
            easings: None,
            intents: None,
            shapes: None,
            entry: None,
            busy_delay: Some(delay.to_string()),
            awaiting_skeleton: None,
        };
        assert_eq!(tokens("450ms").busy_delay_ms(), Some(450));
        assert_eq!(tokens("0.3s").busy_delay_ms(), Some(300));
        assert_eq!(tokens(" 2s ").busy_delay_ms(), Some(2000));
        assert_eq!(
            tokens("var(--x)").busy_delay_ms(),
            None,
            "control: unreadable is None, never guessed"
        );
        assert_eq!(tokens("-5ms").busy_delay_ms(), None);
        let unset = super::MotionTokens {
            busy_delay: None,
            ..tokens("1ms")
        };
        assert_eq!(unset.busy_delay_ms(), None);
    }

    #[test]
    fn motion_tokens_keep_the_busy_delay_and_awaiting_skeleton_switch() {
        let tokens: super::MotionTokens = serde_json::from_value(
            serde_json::json!({ "busyDelay": "450ms", "awaitingSkeleton": true }),
        )
        .unwrap();
        assert_eq!(tokens.busy_delay.as_deref(), Some("450ms"));
        assert_eq!(tokens.awaiting_skeleton, Some(true));
        assert_eq!(
            serde_json::to_value(&tokens).unwrap(),
            serde_json::json!({ "busyDelay": "450ms", "awaitingSkeleton": true })
        );
        let bare: super::MotionTokens = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({}),
            "control: an undeclared switch stays absent"
        );
    }

    use super::*;

    #[test]
    fn field_type_event_serde_round_trips_as_lowercase_tag() {
        let json = serde_json::to_string(&FieldType::Event).expect("serializes");
        assert_eq!(json, "\"event\"");
        let back: FieldType = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, FieldType::Event);
    }

    /// `ConfigField.default: Option<SExpression>` must distinguish "no
    /// default was declared" (absent `default` key) from "the default IS
    /// `Null`" (a `trait`-typed knob's `none` sentinel, owner ruling
    /// 2026-09-13) through a full JSON round trip — the shape a `.orb`
    /// registry file actually persists and every downstream consumer
    /// (`uses` load, `orb validate`'s re-inline, the runtime) reads back.
    /// Without `deserialize_present_default`, `Option<T>`'s default serde
    /// impl collapses a present-but-null JSON value to `None`, same as an
    /// absent key — silently erasing the distinction on every round trip.
    #[test]
    fn config_field_default_null_round_trips_as_some_null_not_none() {
        let bound = ConfigField {
            field_type: "trait".to_string(),
            default: Some(SExpression::String("@trait.DefaultForm".to_string())),
            label: None,
            description: None,
            synonyms: None,
            tier: None,
            values: None,
            items: None,
            properties: None,
            required: None,
            ref_id: None,
            forwarded_from: None,
            nested_forwarded_from: Vec::new(),
            payload_for: None,
            projected_from: None,
        };
        let unbound = ConfigField {
            default: Some(SExpression::Null),
            ..bound.clone()
        };
        let no_default = ConfigField {
            default: None,
            ..bound.clone()
        };

        for (field, expected) in [
            (
                bound,
                Some(SExpression::String("@trait.DefaultForm".to_string())),
            ),
            (unbound, Some(SExpression::Null)),
            (no_default, None),
        ] {
            let json = serde_json::to_string(&field).expect("serializes");
            let back: ConfigField = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back.default, expected, "round trip of {json}");
        }

        // The on-disk shape itself: a present `"default": null` key, not an
        // absent one — pins the JSON representation, not just the Rust value.
        let json = serde_json::to_value(&ConfigField {
            default: Some(SExpression::Null),
            ..ConfigField {
                field_type: "trait".to_string(),
                default: None,
                label: None,
                description: None,
                synonyms: None,
                tier: None,
                values: None,
                items: None,
                properties: None,
                required: None,
                ref_id: None,
                forwarded_from: None,
                nested_forwarded_from: Vec::new(),
                payload_for: None,
                projected_from: None,
            }
        })
        .expect("serializes to value");
        assert_eq!(json.get("default"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn retag_callsite_payload_keeps_local_field_as_payload() {
        // `data` IS declared by the child's rendering transition → the binding
        // resolves locally → ordinary @payload. Incoming @callsitePayload (from
        // a lower-time eager tag) is corrected back to @payload.
        let local: std::collections::HashSet<String> = ["data".to_string()].into_iter().collect();
        let mut e = SExpression::Object(
            [(
                "entity".to_string(),
                SExpression::String("@callsitePayload.data".to_string()),
            )]
            .into_iter()
            .collect(),
        );
        retag_callsite_payload(&mut e, &local);
        if let SExpression::Object(m) = &e {
            assert_eq!(
                m["entity"],
                SExpression::String("@payload.data".to_string())
            );
        } else {
            panic!("shape changed");
        }
    }

    #[test]
    fn retag_callsite_payload_captures_nonlocal_field() {
        // `error` is NOT declared by the child's rendering transition (leaf under
        // INIT {}) → the field must come from the composing event → capture.
        // Preserves the legit `content={?error}` embed case.
        let local: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut e = SExpression::String("@payload.error".to_string());
        retag_callsite_payload(&mut e, &local);
        assert_eq!(e, SExpression::String("@callsitePayload.error".to_string()));
        // idempotent + source-agnostic: an already-captured token stays captured.
        retag_callsite_payload(&mut e, &local);
        assert_eq!(e, SExpression::String("@callsitePayload.error".to_string()));
    }

    #[test]
    fn retag_callsite_payload_uses_field_head_for_dotted_and_indexed_paths() {
        let local: std::collections::HashSet<String> = ["row".to_string()].into_iter().collect();
        let mut dotted = SExpression::String("@callsitePayload.row.name".to_string());
        retag_callsite_payload(&mut dotted, &local);
        assert_eq!(dotted, SExpression::String("@payload.row.name".to_string()));
        let mut indexed = SExpression::String("@payload.data[0].id".to_string());
        retag_callsite_payload(&mut indexed, &local); // data not local → capture
        assert_eq!(
            indexed,
            SExpression::String("@callsitePayload.data[0].id".to_string())
        );
    }

    #[test]
    fn test_parse_entity() {
        let json = r#"{
            "name": "User",
            "collection": "users",
            "fields": [
                { "name": "id", "type": "string", "required": true, "primaryKey": true },
                { "name": "email", "type": "string", "required": true, "unique": true },
                { "name": "status", "type": "enum", "values": ["pending", "active", "suspended"], "default": "pending" },
                { "name": "connectionSlots", "type": "number", "default": 10 }
            ]
        }"#;

        let entity: EntityDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(entity.name, "User");
        assert_eq!(entity.collection, Some("users".to_string()));
        assert_eq!(entity.fields.len(), 4);

        // Check enum field
        let status_field = &entity.fields[2];
        assert_eq!(status_field.name, Some("status".to_string()));
        assert_eq!(status_field.field_type, FieldType::Enum);
        assert_eq!(
            status_field.values,
            Some(vec![
                "pending".to_string(),
                "active".to_string(),
                "suspended".to_string()
            ])
        );
    }

    #[test]
    fn test_parse_relation() {
        let json = r#"{
            "name": "requesterId",
            "type": "relation",
            "relation": { "entity": "User", "cardinality": "one" },
            "required": true
        }"#;

        let field: FieldDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(field.field_type, FieldType::Relation);

        let relation = field.relation.unwrap();
        assert_eq!(relation.entity, "User");
        assert_eq!(relation.cardinality, Some(Cardinality::One));
    }

    #[test]
    fn test_parse_transition() {
        let json = r#"{
            "from": "browsing",
            "to": "registering",
            "event": "REGISTER",
            "effects": [
                ["render-ui", "main", { "type": "page-header", "title": "Register User" }]
            ]
        }"#;

        let transition: TransitionDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(
            transition.from,
            StringOrArray::Single("browsing".to_string())
        );
        assert_eq!(transition.to, "registering");
        assert_eq!(transition.event, "REGISTER");
        assert!(transition.effects.is_some());
    }

    #[test]
    fn test_parse_transition_with_guard() {
        let json = r#"{
            "from": "registering",
            "to": "browsing",
            "event": "SAVE",
            "guard": ["and", ["!=", "@payload.data.email", ""], ["!=", "@payload.data.name", ""]],
            "effects": [
                ["persist", "create", "User", "@payload.data"],
                ["emit", "USER_REGISTERED", { "userId": "@payload.data.id" }]
            ]
        }"#;

        let transition: TransitionDefinition = serde_json::from_str(json).unwrap();
        assert!(transition.guard.is_some());

        let guard = transition.guard.unwrap();
        assert_eq!(guard.operator(), Some("and"));
    }

    #[test]
    fn test_string_or_array() {
        // Single string
        let json = r#""browsing""#;
        let soa: StringOrArray = serde_json::from_str(json).unwrap();
        assert!(soa.matches("browsing"));
        assert!(!soa.matches("editing"));

        // Array
        let json = r#"["browsing", "viewing", "editing"]"#;
        let soa: StringOrArray = serde_json::from_str(json).unwrap();
        assert!(soa.matches("browsing"));
        assert!(soa.matches("viewing"));
        assert!(soa.matches("editing"));
        assert!(!soa.matches("other"));
    }

    #[test]
    fn test_parse_listen_definition() {
        let json = r#"{
            "event": "CONNECTION_ACCEPTED",
            "triggers": "CREATE_RELATIONSHIP",
            "scope": "external",
            "description": "Creates a new relationship health record",
            "payloadMapping": {
                "connectionId": "@payload.connectionId"
            }
        }"#;

        let listen: ListenDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(listen.event, "CONNECTION_ACCEPTED");
        assert_eq!(listen.triggers, Some("CREATE_RELATIONSHIP".to_string()));
        assert_eq!(listen.scope, Some(EventScope::External));
        assert!(listen.payload_mapping.is_some());
    }

    #[test]
    fn test_entity_shared_true_round_trips() {
        let json = r#"{ "name": "Session", "fields": [], "shared": true }"#;
        let entity: EntityDefinition = serde_json::from_str(json).unwrap();
        assert!(entity.shared);

        let reserialized = serde_json::to_string(&entity).unwrap();
        assert!(reserialized.contains("\"shared\":true"));
    }

    #[test]
    fn test_entity_shared_defaults_false_when_absent() {
        let json = r#"{ "name": "Task", "fields": [] }"#;
        let entity: EntityDefinition = serde_json::from_str(json).unwrap();
        assert!(!entity.shared);
    }

    #[test]
    fn test_entity_shared_false_omitted_from_serialization() {
        let entity = EntityDefinition {
            name: "Task".to_string(),
            id: None,
            collection: None,
            persistence: None,
            fields: vec![],
            instances: None,
            shared: false,
            identity: false,
            local: false,
            seed_mock: false,
            locale_instances: None,
            read_policy: None,
            create_policy: None,
            update_policy: None,
            delete_policy: None,
            access_waivers: None,
        };
        let serialized = serde_json::to_string(&entity).unwrap();
        assert!(!serialized.contains("shared"));
    }

    #[test]
    fn test_entity_identity_defaults_false_when_absent() {
        let json = r#"{ "name": "Task", "fields": [] }"#;
        let entity: EntityDefinition = serde_json::from_str(json).unwrap();
        assert!(!entity.identity);
    }

    /// The property that keeps all committed `.orb` files byte-identical after
    /// the `[identity]` tag lands: a non-identity entity must not gain a key.
    #[test]
    fn test_entity_identity_false_omitted_from_serialization() {
        let json = r#"{"name":"Task","fields":[]}"#;
        let entity: EntityDefinition = serde_json::from_str(json).unwrap();
        let serialized = serde_json::to_string(&entity).unwrap();
        assert!(!serialized.contains("identity"));
        assert_eq!(serialized, json);
    }

    #[test]
    fn test_entity_identity_round_trips_when_set() {
        let json = r#"{"name":"Person","fields":[],"identity":true}"#;
        let entity: EntityDefinition = serde_json::from_str(json).unwrap();
        assert!(entity.identity);
        assert!(serde_json::to_string(&entity)
            .unwrap()
            .contains("\"identity\":true"));
    }

    // =========================================================================
    // Orbital import wire contract (Almadar_Orbital_Import.md §4, W1-R)
    // =========================================================================

    #[test]
    fn orbital_ref_object_roundtrips_through_serde() {
        let mut fields = std::collections::HashMap::new();
        fields.insert("dueDate".to_string(), "deadline".to_string());

        let mut pages = std::collections::HashMap::new();
        pages.insert("/notes".to_string(), "/scratch-notes".to_string());

        let mut config = std::collections::HashMap::new();
        config.insert(
            "pageSize".to_string(),
            ConfigField {
                field_type: "number".to_string(),
                default: Some(SExpression::Number(50.0)),
                label: None,
                description: None,
                synonyms: None,
                tier: None,
                values: None,
                items: None,
                properties: None,
                required: None,
                ref_id: None,
                forwarded_from: None,
                nested_forwarded_from: Vec::new(),
                payload_for: None,
                projected_from: None,
            },
        );

        let mut events = std::collections::HashMap::new();
        events.insert("NOTE_SAVED".to_string(), "SCRATCH_NOTE_SAVED".to_string());

        let obj = OrbitalRefObject {
            reference: "Notes.orbitals.NoteOrbital".to_string(),
            ref_id: Some("orbital_note123".to_string()),
            entity: Some("ScratchNote".to_string()),
            fields: Some(fields),
            pages: Some(pages),
            omit: Some(vec!["NoteDelete".to_string()]),
            only: None,
            config: Some(config),
            events: Some(events),
            roles: None,
            entities: None,
            mounts: None,
            extend: None,
            mock: None,
            listens: None,
            ..Default::default()
        };

        let value = serde_json::to_value(&obj).expect("serializes");
        // Wire keys are `ref`/`refId`, never the Rust field names.
        assert!(value.get("ref").is_some());
        assert!(value.get("refId").is_some());
        assert!(value.get("reference").is_none());
        assert!(value.get("ref_id").is_none());
        assert_eq!(value["ref"], "Notes.orbitals.NoteOrbital");

        let back: OrbitalRefObject = serde_json::from_value(value.clone()).expect("deserializes");
        let round_tripped = serde_json::to_value(&back).expect("serializes again");
        assert_eq!(value, round_tripped);
    }

    #[test]
    fn orbital_config_defaults_to_none_on_missing_key() {
        let orbital_json = r#"{
            "name": "TestOrbital",
            "entity": { "name": "Thing", "fields": [] },
            "traits": []
        }"#;
        let orbital: Orbital = serde_json::from_str(orbital_json).unwrap();
        assert!(orbital.config.is_none());
        assert!(orbital.reference.is_none());

        let schema_json = r#"{
            "name": "TestApp",
            "orbitals": []
        }"#;
        let schema: OrbitalSchema = serde_json::from_str(schema_json).unwrap();
        assert!(schema.config.is_none());
    }

    /// `ColorTokens`/`IllustrationTokens` round-trip on their own — the two
    /// typed axes `ThemeTokens`/`ThemeVariant` gained to mirror
    /// `@almadar/core`'s `ColorTokens`/`IllustrationTokens`
    /// (`src/types/domain.ts`). Neither axis is emitted by
    /// `themeTokensToCssVars` yet (the migrated presets never populate
    /// them), so this is the only coverage of the fields themselves.
    #[test]
    fn color_and_illustration_tokens_round_trip() {
        let tokens = ThemeTokens {
            color: Some(ColorTokens {
                primary: Some("#14b8a6".to_string()),
                primary_hover: None,
                primary_foreground: Some("#ffffff".to_string()),
                secondary: None,
                secondary_hover: None,
                secondary_foreground: None,
                accent: None,
                accent_foreground: None,
                muted: None,
                muted_foreground: None,
                background: Some("#0b0b0f".to_string()),
                foreground: None,
                card: None,
                card_foreground: None,
                surface: None,
                border: None,
                input: None,
                ring: None,
                error: None,
                error_foreground: None,
                success: None,
                success_foreground: None,
                warning: None,
                warning_foreground: None,
                info: None,
                info_foreground: None,
                table_header: None,
                table_border: None,
                table_row_hover: None,
                surface_hover: None,
                border_hover: None,
                placeholder: None,
                scrim: None,
            }),
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
            illustration: Some(IllustrationTokens {
                style: Some(IllustrationStyle::Mascot),
                empty_asset: Some("empty.svg".to_string()),
                loading_asset: None,
                error_asset: None,
                onboarding_asset: None,
            }),
            surface: None,
            diagram: None,
            scene: None,
        };

        let json = serde_json::to_value(&tokens).expect("serializes");
        assert_eq!(json["color"]["primary"], "#14b8a6");
        assert_eq!(json["color"]["primaryHover"], serde_json::Value::Null);
        assert!(json["color"]
            .as_object()
            .unwrap()
            .get("primaryHover")
            .is_none());
        assert_eq!(json["illustration"]["style"], "mascot");

        let back: ThemeTokens = serde_json::from_value(json.clone()).expect("deserializes");
        let round_tripped = serde_json::to_value(&back).expect("serializes again");
        assert_eq!(json, round_tripped);
        assert_eq!(back.color.unwrap().primary, Some("#14b8a6".to_string()));
        assert_eq!(back.illustration.unwrap().style.is_some(), true);
    }

    /// `ThemeVariant` (the dark-mode override shape) carries the same
    /// `color`/`illustration` axes as `ThemeTokens` — assert the wire key
    /// is `typeScale` (camelCase), not `type_scale`, and that an absent
    /// `color`/`illustration` never appears in the serialized object.
    #[test]
    fn theme_variant_color_and_illustration_are_optional_and_omitted_when_absent() {
        let variant = ThemeVariant {
            color: None,
            colors: Some(ThemeMap::from([(
                "background".to_string(),
                "#111".to_string(),
            )])),
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
        };
        let json = serde_json::to_value(&variant).expect("serializes");
        let obj = json.as_object().expect("object");
        assert!(!obj.contains_key("color"));
        assert!(!obj.contains_key("illustration"));
        assert!(!obj.contains_key("typeScale"));
        assert_eq!(json["colors"]["background"], "#111");
    }

    /// Loads two of the migrated theme presets (`@almadar/core` `themes/*.json`,
    /// read from the installed core (`language_data`); produced by
    /// `almadar-core/scripts/migrate-css-themes.ts` from the hand-authored
    /// CSS — see plan `groovy-inventing-cloud.md` Program C, C1) through
    /// `ThemeDefinition` and asserts NO field is lost: reserializing must
    /// reproduce the exact same JSON value the TypeScript side wrote.
    #[test]
    fn migrated_theme_presets_round_trip_losslessly() {
        let installed = &crate::language_data::language_data_after_preflight().themes;
        let preset = |name: &str| -> &str {
            installed
                .iter()
                .find(|json| {
                    serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v["name"] == name)
                })
                .unwrap_or_else(|| panic!("installed core has no `{name}` preset"))
        };

        for (name, raw) in [
            ("clay", preset("clay")),
            ("minimalist", preset("minimalist")),
        ] {
            let original: serde_json::Value =
                serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}: invalid JSON: {e}"));
            let parsed: ThemeDefinition = serde_json::from_value(original.clone())
                .unwrap_or_else(|e| panic!("{name}: does not parse as ThemeDefinition: {e}"));
            let round_tripped = serde_json::to_value(&parsed).expect("serializes");
            assert_eq!(
                original, round_tripped,
                "{name}: lost or changed a field on round trip"
            );

            // Sanity: both presets declare colors, a dark variant, and the
            // typed skin axes the migration produced.
            assert!(
                parsed.tokens.colors.is_some(),
                "{name}: expected legacy colors map"
            );
            assert!(
                parsed.tokens.density.is_some(),
                "{name}: expected density axis"
            );
            assert!(
                parsed.tokens.type_scale.is_some(),
                "{name}: expected typeScale axis"
            );
            let dark = parsed.variants.as_ref().and_then(|v| v.get("dark"));
            assert!(dark.is_some(), "{name}: expected a dark variant");
            assert!(
                dark.unwrap().colors.is_some(),
                "{name}: expected dark colors override"
            );
        }
    }
}
