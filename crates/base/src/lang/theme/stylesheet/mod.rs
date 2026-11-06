//! Theme Code Generation
//!
//! Generates CSS custom properties from theme definitions.
//!
//! # Theme Architecture
//!
//! Hosts share a unified theming system with a single data attribute:
//!   data-theme="themename-light" | data-theme="themename-dark"
//!
//! Generated themes define both light and dark modes:
//! - Light mode: `[data-theme="name-light"]`
//! - Dark mode: `[data-theme="name-dark"]`

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

mod config;
mod css;
mod defaults;
mod emitters;
mod tokens;

pub use config::{client_themes, ClientTheme};
pub use css::{generate_theme_css, program_themes, program_themes_css, theme_font_imports};

#[cfg(test)]
mod tests {
    use super::tokens::emit_skin_axes;
    use super::*;
    use crate::schema::{
        DensityTokens, ElevationTokens, GeometryTokens, IconFamily, IconographyTokens,
        MotionDurationKey, MotionDurationPalette, MotionEasingKey, MotionIntent, MotionIntentMap,
        MotionTokens, SpacingScale, ThemeDefinition, ThemeTokens, ThemeVariant, TypeIntent,
        TypeIntentMap, TypeScale, TypeScaleEntry, TypeScaleTokens, TypeSizeKey, TypeSlot,
        TypeWeight,
    };

    fn create_test_theme() -> ThemeDefinition {
        let mut colors = crate::schema::ThemeMap::new();
        colors.insert("primary".to_string(), "#0ea5e9".to_string());
        colors.insert("background".to_string(), "#ffffff".to_string());

        let mut radii = crate::schema::ThemeMap::new();
        radii.insert("md".to_string(), "8px".to_string());

        let mut dark_colors = crate::schema::ThemeMap::new();
        dark_colors.insert("background".to_string(), "#0c4a6e".to_string());

        let mut variants = crate::collections::IndexMap::new();
        variants.insert(
            "dark".to_string(),
            ThemeVariant {
                color: None,
                colors: Some(dark_colors),
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
            },
        );

        ThemeDefinition {
            name: "ocean".to_string(),
            display_name: None,
            tokens: ThemeTokens {
                color: None,
                colors: Some(colors),
                radii: Some(radii),
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
            },
            variants: Some(variants),
        }
    }

    #[test]
    fn test_generate_theme_css() {
        let theme = create_test_theme();
        let css = generate_theme_css(&theme);

        assert!(css.contains("Theme: ocean"));
        // New format uses data-theme="name-light" and data-theme="name-dark"
        assert!(css.contains("[data-theme=\"ocean-light\"]"));
        assert!(css.contains("[data-theme=\"ocean-dark\"]"));
        assert!(css.contains("--color-primary: #0ea5e9"));
        assert!(css.contains("--radius-md: 8px"));
        // Should include all default color tokens
        assert!(css.contains("--color-foreground:"));
        assert!(css.contains("--color-muted:"));
        assert!(css.contains("--color-border:"));
    }

    /// Skin-axis emission parity anchor.
    ///
    /// This fixture mirrors `packages/almadar-ui/test/themeTokens.skin-axes.test.ts`
    /// "combined-axis fixture (round-trip parity anchor)". The Rust output and
    /// the JS runtime mirror's output MUST produce the same CSS-variable set
    /// for the same input. If this Rust test passes AND the TS test passes,
    /// the byte-identical contract documented in
    /// `packages/almadar-ui/context/themeTokens.ts:11` holds.
    #[test]
    fn test_emit_skin_axes_parity() {
        let mut spacing = SpacingScale {
            space0: None,
            space1: None,
            space2: None,
            space3: Some("12px".to_string()),
            space4: None,
            space5: None,
            space6: Some("24px".to_string()),
            space7: None,
            space8: None,
            space9: None,
            space10: None,
            space11: None,
            space12: None,
        };
        let density = DensityTokens {
            spacing: Some(spacing.clone()),
            button_height_sm: None,
            button_height_md: Some("36px".to_string()),
            button_height_lg: None,
            input_height_sm: None,
            input_height_md: None,
            input_height_lg: None,
            row_height_compact: None,
            row_height_normal: None,
            row_height_spacious: None,
            card_padding_sm: None,
            card_padding_md: None,
            card_padding_lg: None,
            dialog_padding: None,
            section_gap: None,
        };
        // keep spacing referenced
        let _ = &mut spacing;

        let scale = TypeScale {
            xs: None,
            sm: None,
            base: Some(TypeScaleEntry {
                size: "16px".to_string(),
                line_height: "24px".to_string(),
            }),
            lg: None,
            xl: None,
            two_xl: None,
            three_xl: None,
            four_xl: None,
            display_1: None,
            display_2: None,
        };
        let intents = TypeIntentMap {
            heading_major: None,
            heading_minor: None,
            body_emphasis: None,
            body_default: Some(TypeIntent {
                slot: TypeSlot::Body,
                size: TypeSizeKey::Base,
                weight: TypeWeight::Normal,
            }),
            body_quiet: None,
            caption: None,
            numeric: None,
        };
        let type_scale = TypeScaleTokens {
            display_family: None,
            body_family: Some("\"Inter\", sans-serif".to_string()),
            mono_family: None,
            scale: Some(scale),
            intents: Some(intents),
            ..Default::default()
        };

        let durations = MotionDurationPalette {
            instant: None,
            fast: None,
            normal: Some("200ms".to_string()),
            slow: None,
            dramatic: None,
        };
        let motion_intents = MotionIntentMap {
            enter: None,
            exit: None,
            hover: Some(MotionIntent {
                duration: MotionDurationKey::Fast,
                easing: MotionEasingKey::Standard,
            }),
            press: None,
            expand: None,
            transition: None,
        };
        let motion = MotionTokens {
            durations: Some(durations),
            easings: None,
            intents: Some(motion_intents),
            shapes: None,
            entry: None,
            busy_delay: None,
            awaiting_skeleton: None,
        };

        let iconography = IconographyTokens {
            family: Some(IconFamily::Lucide),
            stroke_width: None,
            default_size: None,
        };

        let elevation = ElevationTokens {
            card_elevation: Some("var(--shadow-sm)".to_string()),
            popover_elevation: None,
            dialog_elevation: None,
            toast_elevation: None,
            ..Default::default()
        };

        let geometry = GeometryTokens {
            radius_container: Some("8px".to_string()),
            radius_interactive: None,
            radius_pill: None,
            border_hairline: Some("1px".to_string()),
            border_standard: None,
            border_heavy: None,
            ..Default::default()
        };

        let tokens = ThemeTokens {
            color: None,
            colors: None,
            radii: None,
            spacing: None,
            typography: None,
            shadows: None,
            density: Some(density),
            type_scale: Some(type_scale),
            motion: Some(motion),
            iconography: Some(iconography),
            elevation: Some(elevation),
            geometry: Some(geometry),
            illustration: None,
            diagram: None,
            scene: None,
            surface: None,
        };

        // Use emit_skin_axes directly so we're not coupled to color/shadow defaults.
        let out = emit_skin_axes(&tokens, "");

        // Same expected set as packages/almadar-ui/test/themeTokens.skin-axes.test.ts.
        let expected_lines: &[&str] = &[
            "--space-3: 12px;",
            "--space-6: 24px;",
            "--button-height-md: 36px;",
            "--font-family-body: \"Inter\", sans-serif;",
            "--text-base: 16px;",
            "--leading-base: 24px;",
            "--intent-body-default-size: var(--text-base);",
            "--intent-body-default-weight: var(--font-weight-normal);",
            "--intent-body-default-leading: var(--leading-base);",
            "--duration-normal: 200ms;",
            "--intent-hover-duration: var(--duration-fast);",
            "--intent-hover-easing: var(--easing-standard);",
            "--icon-family: lucide;",
            "--elevation-card: var(--shadow-sm);",
            "--radius-container: 8px;",
            "--border-hairline: 1px;",
        ];
        for line in expected_lines {
            assert!(
                out.contains(line),
                "Rust skin-axis output missing expected line `{}`. Full output:\n{}",
                line,
                out
            );
        }
    }

    fn range_fixture() -> ThemeTokens {
        let json = serde_json::json!({
            "typeScale": {
                "displayFamily": "\"Limelight\", serif",
                "fontImport": "https://fonts.googleapis.com/css2?family=Limelight&display=swap",
                "headingWeight": "400",
                "headingTransform": "uppercase",
                "headingTracking": "0.2em",
                "headingStyle": "italic",
                "headingShadow": "0 0 6px currentColor"
            },
            "geometry": {
                "cornerShape": "bevel",
                "cornerShapePill": "round",
                "cornerShapeInteractive": "round",
                "borderStyle": "double",
                "borderStyleInteractive": "outset"
            },
            "elevation": {
                "interactiveElevation": "2px 2px 0 #000",
                "pressedElevation": "inset 2px 2px 0 #000"
            },
            "surface": {
                "backdrop": "blur(18px) saturate(1.6)",
                "cardImage": "linear-gradient(#fff6, #fff0)",
                "pageImage": "radial-gradient(#0002 1px, transparent 1px)",
                "pageImageSize": "8px 8px"
            }
        });
        serde_json::from_value(json).expect("range fixture deserializes")
    }

    /// Twin of `packages/almadar-ui/test/themeTokens.range-axes.test.ts`.
    #[test]
    fn range_axes_emit_the_same_vars_as_the_runtime_mirror() {
        let css = emit_skin_axes(&range_fixture(), "");
        for line in [
            "--font-family-display: \"Limelight\", serif;",
            "--heading-weight: 400;",
            "--heading-transform: uppercase;",
            "--heading-tracking: 0.2em;",
            "--heading-style: italic;",
            "--heading-shadow: 0 0 6px currentColor;",
            "--corner-shape: bevel;",
            "--corner-shape-pill: round;",
            "--corner-shape-interactive: round;",
            "--border-style: double;",
            "--border-style-interactive: outset;",
            "--elevation-interactive: 2px 2px 0 #000;",
            "--elevation-pressed: inset 2px 2px 0 #000;",
            "--surface-backdrop: blur(18px) saturate(1.6);",
            "--surface-card-image: linear-gradient(#fff6, #fff0);",
            "--surface-page-image: radial-gradient(#0002 1px, transparent 1px);",
            "--surface-page-image-size: 8px 8px;",
        ] {
            assert!(css.contains(line), "missing `{line}` in:\n{css}");
        }
        assert!(!css.contains("fontImport") && !css.contains("--font-import"));
    }

    #[test]
    fn range_axes_control_theme_without_them_emits_none() {
        let css = emit_skin_axes(&create_test_theme().tokens, "");
        for needle in [
            "--heading-",
            "--corner-shape",
            "--border-style",
            "--surface-",
            "--elevation-interactive",
        ] {
            assert!(!css.contains(needle), "unexpected `{needle}`");
        }
    }

    #[test]
    fn range_axes_font_imports_are_collected_deduplicated_and_kept_out_of_theme_css() {
        let mut theme = create_test_theme();
        theme.tokens = range_fixture();
        let dark = theme.variants.as_mut().unwrap().get_mut("dark").unwrap();
        dark.type_scale = theme.tokens.type_scale.clone();
        let url = "https://fonts.googleapis.com/css2?family=Limelight&display=swap";
        assert_eq!(
            theme_font_imports(std::slice::from_ref(&theme)),
            vec![url.to_string()]
        );
        assert!(!generate_theme_css(&theme).contains("@import"));
        assert!(theme_font_imports(&[create_test_theme()]).is_empty());
    }

    #[test]
    fn range_axes_dark_variant_surface_replaces_base() {
        let base = range_fixture();
        let mut dark = create_test_theme()
            .variants
            .unwrap()
            .remove("dark")
            .unwrap();
        dark.surface =
            serde_json::from_value(serde_json::json!({ "backdrop": "blur(4px)" })).unwrap();
        let css = tokens::emit_skin_axes_dark(&base, Some(&dark), "");
        assert!(css.contains("--surface-backdrop: blur(4px);"));
        assert!(!css.contains("--surface-page-image"));
    }

    fn diagram_fixture() -> ThemeTokens {
        serde_json::from_value(serde_json::json!({
            "diagram": {
                "strokeThin": "1px", "strokeNormal": "2px", "strokeBold": "4px",
                "lineCap": "square", "lineJoin": "miter", "dash": "8 4", "dot": "1 4",
                "fillStyle": "outline", "fillOpacity": "0.25", "roughness": "1.5", "glow": "6",
                "shadow": "2px 2px 0 #000", "marker": "open", "labelFont": "mono",
                "labelSize": "sm", "labelWeight": "600", "labelCase": "uppercase", "corner": "0px"
            },
            "scene": {
                "materialRoughness": "0.8", "materialMetalness": "0.1", "materialFlat": "1",
                "materialOutline": "2", "lightAmbient": "0.6", "lightKey": "1.2",
                "lightKeyColor": "#fff4e0", "fog": "0.2"
            },
            "surface": { "diagram": "var(--color-card)" }
        }))
        .unwrap()
    }

    #[test]
    fn diagram_axes_emit_the_same_vars_as_the_runtime_mirror() {
        let css = emit_skin_axes(&diagram_fixture(), "");
        for line in [
            "--diagram-stroke-thin: 1px;",
            "--diagram-stroke-normal: 2px;",
            "--diagram-stroke-bold: 4px;",
            "--diagram-line-cap: square;",
            "--diagram-line-join: miter;",
            "--diagram-dash: 8 4;",
            "--diagram-dot: 1 4;",
            "--diagram-fill-style: outline;",
            "--diagram-fill-opacity: 0.25;",
            "--diagram-roughness: 1.5;",
            "--diagram-glow: 6;",
            "--diagram-shadow: 2px 2px 0 #000;",
            "--diagram-marker: open;",
            "--diagram-label-font: mono;",
            "--diagram-label-size: sm;",
            "--diagram-label-weight: 600;",
            "--diagram-label-case: uppercase;",
            "--diagram-corner: 0px;",
            "--material-roughness: 0.8;",
            "--material-metalness: 0.1;",
            "--material-flat: 1;",
            "--material-outline: 2;",
            "--light-ambient: 0.6;",
            "--light-key: 1.2;",
            "--light-key-color: #fff4e0;",
            "--scene-fog: 0.2;",
            "--surface-diagram: var(--color-card);",
        ] {
            assert!(css.contains(line), "missing `{line}` in:\n{css}");
        }
    }

    #[test]
    fn diagram_axes_control_theme_without_them_emits_none() {
        let css = emit_skin_axes(&create_test_theme().tokens, "");
        for needle in ["--diagram-", "--material-", "--light-", "--scene-fog", "--surface-diagram"] {
            assert!(!css.contains(needle), "unexpected `{needle}`");
        }
    }

    #[test]
    fn diagram_axes_partial_axis_emits_only_its_keys() {
        let tokens: ThemeTokens =
            serde_json::from_value(serde_json::json!({ "diagram": { "glow": "8" } })).unwrap();
        let css = emit_skin_axes(&tokens, "");
        assert!(css.contains("--diagram-glow: 8;"));
        assert!(!css.contains("--diagram-stroke"));
    }

    #[test]
    fn diagram_axes_dark_variant_replaces_the_whole_axis() {
        let base = diagram_fixture();
        let mut dark = create_test_theme().variants.unwrap().remove("dark").unwrap();
        dark.diagram = serde_json::from_value(serde_json::json!({ "glow": "10" })).unwrap();
        dark.scene = serde_json::from_value(serde_json::json!({ "lightAmbient": "0.3" })).unwrap();
        let css = tokens::emit_skin_axes_dark(&base, Some(&dark), "");
        assert!(css.contains("--diagram-glow: 10;"));
        assert!(!css.contains("--diagram-stroke-normal"));
        assert!(css.contains("--light-ambient: 0.3;"));
        assert!(!css.contains("--material-roughness"));
        assert!(css.contains("--surface-diagram: var(--color-card);"));
    }

    #[test]
    fn diagram_axes_reject_an_unknown_line_cap() {
        let bad: Result<ThemeTokens, _> =
            serde_json::from_value(serde_json::json!({ "diagram": { "lineCap": "wavy" } }));
        assert!(bad.is_err());
    }

    #[test]
    fn a_default_theme_key_sets_the_default_and_its_mode() {
        let mut theme = create_test_theme();
        theme.display_name = Some("Ocean's \"Deep\"".to_string());
        let (themes, default, mode) = client_themes(std::slice::from_ref(&theme), Some("ocean-dark"));
        assert_eq!(default.as_deref(), Some("ocean"));
        assert_eq!(mode, "dark");
        assert_eq!(themes[0].display_name, "Ocean's \"Deep\"");
    }

    #[test]
    fn without_a_default_key_the_first_theme_is_the_default_in_light_mode() {
        let (themes, default, mode) = client_themes(&[create_test_theme()], None);
        assert_eq!((default.as_deref(), mode), (Some("ocean"), "light"));
        assert_eq!(themes[0].display_name, "Ocean");
    }

    #[test]
    fn a_dark_only_preset_reports_no_light_mode_and_no_themes_mean_no_default() {
        let mut theme = create_test_theme();
        theme.tokens = serde_json::from_value(serde_json::json!({})).unwrap();
        let (themes, _, _) = client_themes(&[theme], Some("ocean-dark"));
        assert!(!themes[0].has_light_mode && themes[0].has_dark_mode);
        assert_eq!(client_themes(&[], None).1, None);
    }

    /// Twin of the `motion shapes` case in `themeTokens.range-axes.test.ts`.
    #[test]
    fn motion_shapes_emit_the_surface_transform_endpoints() {
        let tokens: ThemeTokens = serde_json::from_value(serde_json::json!({
            "motion": { "shapes": { "modalEnter": "scale(0.9)", "pageEnter": "none", "toastEnter": "translateX(24px)" } }
        }))
        .unwrap();
        let css = emit_skin_axes(&tokens, "");
        assert!(css.contains("--motion-modal-enter-from-transform: scale(0.9);"));
        assert!(css.contains("--motion-toast-enter-from-transform: translateX(24px);"));
        assert!(css.contains("--motion-page-enter-from-transform: none;"));
        assert!(
            !css.contains("--motion-popover"),
            "control: unset shapes emit nothing"
        );
    }
}
