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
