//! Theme definitions: the skin axes and every token family.

use super::*;

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
pub type ThemeMap = crate::collections::IndexMap<String, String>;

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
    pub variants: Option<crate::collections::IndexMap<String, ThemeVariant>>,
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
