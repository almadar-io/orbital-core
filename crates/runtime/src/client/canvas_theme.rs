//! The drawing axes a native canvas paints with, resolved from `@currentTheme`'s
//! tokens. Rust twin of `@almadar/ui` `resolveCanvasTheme` evaluated over
//! `themeTokensToCssVars` + `themes/_base.css` (the chain `OrbitalThemeProvider`
//! runs on the web), limited to the fields the native renderer reads. Parity is
//! pinned by `tests/fixtures/canvas-theme/*.json`, generated in Chromium by
//! `tools/almadar-pattern-sync/scripts/canvas-theme-fixture.mjs`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::schema::types::ThemeTokens;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `@almadar/core` `DIAGRAM_TONE_COLOR_KEYS`: tone → the `--color-<key>` it reads.
pub const DIAGRAM_TONE_COLOR_KEYS: [(&str, &str); 22] = [
    ("ink", "diagram-ink"),
    ("label", "diagram-label"),
    ("guide", "diagram-guide"),
    ("axis", "diagram-axis"),
    ("grid", "diagram-grid"),
    ("highlight", "diagram-highlight"),
    ("fill", "diagram-fill"),
    ("series-1", "series-1"),
    ("series-2", "series-2"),
    ("series-3", "series-3"),
    ("series-4", "series-4"),
    ("series-5", "series-5"),
    ("series-6", "series-6"),
    ("series-7", "series-7"),
    ("series-8", "series-8"),
    ("primary", "primary"),
    ("accent", "accent"),
    ("success", "success"),
    ("warning", "warning"),
    ("error", "error"),
    ("info", "info"),
    ("muted", "muted-foreground"),
];

/// `_base.css`: the color each diagram key aliases when the theme sets none.
const DIAGRAM_COLOR_DEFAULTS: [(&str, &str); 7] = [
    ("diagram-ink", "foreground"),
    ("diagram-label", "muted-foreground"),
    ("diagram-guide", "muted-foreground"),
    ("diagram-axis", "foreground"),
    ("diagram-grid", "border"),
    ("diagram-highlight", "primary"),
    ("diagram-fill", "muted"),
];

const TEXT_SIZES: [(&str, f64); 6] = [
    ("xs", 12.0),
    ("sm", 14.0),
    ("base", 16.0),
    ("lg", 20.0),
    ("xl", 24.0),
    ("2xl", 32.0),
];

const UNRESOLVED_COLOR: &str = "currentColor";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasStrokes {
    pub thin: f64,
    pub normal: f64,
    pub bold: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSurfaces {
    pub muted: String,
    pub card: String,
    pub on_primary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasLabel {
    pub size: String,
    pub weight: String,
    pub uppercase: bool,
}

/// The subset of `@almadar/core` `CanvasTheme` the native renderer consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasTheme {
    pub ground: String,
    pub surfaces: CanvasSurfaces,
    pub tones: BTreeMap<String, String>,
    pub series: Vec<String>,
    pub strokes: CanvasStrokes,
    pub line_cap: String,
    pub line_join: String,
    pub dash: Vec<f64>,
    pub dot: Vec<f64>,
    pub fill_style: String,
    pub fill_opacity: f64,
    pub marker: String,
    pub label: CanvasLabel,
    pub text: BTreeMap<String, f64>,
    pub corner: f64,
}

/// Resolve the canvas theme from `@currentTheme`'s merged tokens.
pub fn canvas_theme(tokens: &ThemeTokens) -> CanvasTheme {
    let colors = tokens.colors.clone().unwrap_or_default();
    let color_of = |key: &str| -> Option<Rgba> { colors.get(key).and_then(|v| parse_color(v)) };
    let diagram_key = |key: &str| -> Option<Rgba> {
        color_of(key).or_else(|| {
            DIAGRAM_COLOR_DEFAULTS
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, alias)| color_of(alias))
        })
    };
    let series_key = |n: usize| -> Option<Rgba> {
        color_of(&format!("series-{n}")).or_else(|| {
            let primary = color_of("primary")?;
            Some(if n == 1 {
                primary
            } else {
                rotate_series(primary, 45.0 * (n - 1) as f64)
            })
        })
    };

    let mut tones = BTreeMap::new();
    for (tone, key) in DIAGRAM_TONE_COLOR_KEYS {
        let resolved = if let Some(n) = key.strip_prefix("series-") {
            n.parse().ok().and_then(series_key)
        } else if key.starts_with("diagram-") {
            diagram_key(key)
        } else {
            color_of(key)
        };
        tones.insert(
            tone.to_string(),
            resolved.map_or_else(|| UNRESOLVED_COLOR.to_string(), |c| c.css()),
        );
    }
    let series = (1..=8)
        .map(|n| tones[&format!("series-{n}")].clone())
        .collect();

    let surface_diagram = tokens.surface.as_ref().and_then(|s| s.diagram.as_deref());
    let ground = surface_diagram
        .and_then(parse_color)
        .or_else(|| color_of("card"))
        .map_or_else(|| "transparent".to_string(), |c| c.css());

    let surface = |key: &str| color_of(key).map_or_else(|| "transparent".to_string(), |c| c.css());
    let surfaces = CanvasSurfaces {
        muted: surface("muted"),
        card: surface("card"),
        on_primary: surface("primary-foreground"),
    };

    let diagram = tokens.diagram.clone().unwrap_or_default();
    let geometry = tokens.geometry.clone().unwrap_or_default();
    let typography = tokens.typography.clone().unwrap_or_default();
    let typo_px = |key: &str| typography.get(key).and_then(|v| parse_px(v));
    let border = |geo: &Option<String>, typo_key: &str, fallback: f64| {
        geo.as_deref()
            .and_then(parse_px)
            .or_else(|| typo_px(typo_key))
            .unwrap_or(fallback)
    };
    let strokes = CanvasStrokes {
        thin: diagram
            .stroke_thin
            .as_deref()
            .and_then(parse_px)
            .unwrap_or_else(|| border(&geometry.border_hairline, "border-width-thin", 1.0)),
        normal: diagram
            .stroke_normal
            .as_deref()
            .and_then(parse_px)
            .unwrap_or_else(|| border(&geometry.border_standard, "border-width", 1.0) * 1.5),
        bold: diagram
            .stroke_bold
            .as_deref()
            .and_then(parse_px)
            .unwrap_or_else(|| border(&geometry.border_heavy, "border-width-thick", 2.0) * 1.5),
    };

    let scale = tokens.type_scale.as_ref().and_then(|t| t.scale.as_ref());
    let mut text = BTreeMap::new();
    for (key, fallback) in TEXT_SIZES {
        let entry = scale.and_then(|s| match key {
            "xs" => s.xs.as_ref(),
            "sm" => s.sm.as_ref(),
            "base" => s.base.as_ref(),
            "lg" => s.lg.as_ref(),
            "xl" => s.xl.as_ref(),
            _ => s.two_xl.as_ref(),
        });
        text.insert(
            key.to_string(),
            entry.and_then(|e| parse_px(&e.size)).unwrap_or(fallback),
        );
    }

    let radii = tokens.radii.clone().unwrap_or_default();
    CanvasTheme {
        ground,
        surfaces,
        tones,
        series,
        strokes,
        line_cap: diagram.line_cap.map_or("round", |v| v.as_str()).to_string(),
        line_join: diagram
            .line_join
            .map_or("round", |v| v.as_str())
            .to_string(),
        dash: diagram
            .dash
            .as_deref()
            .and_then(parse_list)
            .unwrap_or_else(|| vec![6.0, 4.0]),
        dot: diagram
            .dot
            .as_deref()
            .and_then(parse_list)
            .unwrap_or_else(|| vec![1.0, 4.0]),
        fill_style: diagram
            .fill_style
            .map_or("tint", |v| v.as_str())
            .to_string(),
        fill_opacity: diagram
            .fill_opacity
            .as_deref()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0.18),
        marker: diagram
            .marker
            .map_or("triangle", |v| v.as_str())
            .to_string(),
        label: CanvasLabel {
            size: diagram
                .label_size
                .clone()
                .unwrap_or_else(|| "sm".to_string()),
            weight: diagram
                .label_weight
                .clone()
                .or_else(|| typography.get("font-weight-medium").cloned())
                .unwrap_or_else(|| "500".to_string()),
            uppercase: diagram.label_case.map(|c| c.as_str()) == Some("uppercase"),
        },
        text,
        corner: diagram
            .corner
            .as_deref()
            .and_then(parse_px)
            .or_else(|| radii.get("sm").and_then(|v| parse_px(v)))
            .unwrap_or(4.0),
    }
}

fn parse_px(raw: &str) -> Option<f64> {
    let t = raw.trim();
    t.strip_suffix("px").unwrap_or(t).trim().parse().ok()
}

fn parse_list(raw: &str) -> Option<Vec<f64>> {
    let parts: Option<Vec<f64>> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<f64>().ok().filter(|n| *n >= 0.0))
        .collect();
    parts.filter(|p| !p.is_empty())
}

/// An sRGB color, channels 0–255, alpha 0–1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    /// Chromium's computed serialization: `rgb(r, g, b)` / `rgba(r, g, b, a)`.
    pub fn css(&self) -> String {
        let ch = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        if self.a >= 1.0 {
            format!("rgb({}, {}, {})", ch(self.r), ch(self.g), ch(self.b))
        } else {
            format!(
                "rgba({}, {}, {}, {})",
                ch(self.r),
                ch(self.g),
                ch(self.b),
                self.a
            )
        }
    }
}

/// The color forms theme tokens carry: `#rgb[a]`, `#rrggbb[aa]`, `rgb()`/`rgba()`.
pub fn parse_color(raw: &str) -> Option<Rgba> {
    let t = raw.trim();
    if let Some(hex) = t.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        let expand = |d: &[u8]| -> Vec<f64> { d.iter().map(|v| f64::from(v * 17)).collect() };
        let pairs =
            |d: &[u8]| -> Vec<f64> { d.chunks(2).map(|p| f64::from(p[0] * 16 + p[1])).collect() };
        let ch = match digits.len() {
            3 | 4 => expand(&digits),
            6 | 8 => pairs(&digits),
            _ => return None,
        };
        return Some(Rgba {
            r: ch[0],
            g: ch[1],
            b: ch[2],
            a: ch.get(3).map_or(1.0, |a| a / 255.0),
        });
    }
    let inner = t
        .strip_prefix("rgba(")
        .or_else(|| t.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let nums: Vec<f64> = inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(|p| match p.strip_suffix('%') {
            Some(pct) => pct.parse::<f64>().ok().map(|v| v / 100.0),
            None => p.parse::<f64>().ok(),
        })
        .collect::<Option<_>>()?;
    match nums.as_slice() {
        [r, g, b] => Some(Rgba {
            r: *r,
            g: *g,
            b: *b,
            a: 1.0,
        }),
        [r, g, b, a] => Some(Rgba {
            r: *r,
            g: *g,
            b: *b,
            a: *a,
        }),
        _ => None,
    }
}

/// `_base.css` `--color-series-N`: `oklch(from var(--color-primary) clamp(0.45, l, 0.72)
/// max(c, 0.12) calc(h + deg))`, clipped into sRGB as Chromium computes it.
fn rotate_series(primary: Rgba, degrees: f64) -> Rgba {
    let (l, c, h) = srgb_to_oklch(primary);
    let l = l.clamp(0.45, 0.72);
    let c = c.max(0.12);
    let h = (h + degrees).rem_euclid(360.0);
    let mut out = oklch_to_srgb(l, c, h);
    out.a = primary.a;
    out
}

fn to_linear(v: f64) -> f64 {
    let v = v / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(v: f64) -> f64 {
    let v = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).clamp(0.0, 255.0)
}

fn srgb_to_oklch(c: Rgba) -> (f64, f64, f64) {
    let (r, g, b) = (to_linear(c.r), to_linear(c.g), to_linear(c.b));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let ok_l = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
    let ok_a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
    let ok_b = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
    let chroma = (ok_a * ok_a + ok_b * ok_b).sqrt();
    let hue = if chroma < 1e-6 {
        0.0
    } else {
        ok_b.atan2(ok_a).to_degrees().rem_euclid(360.0)
    };
    (ok_l, chroma, hue)
}

fn oklch_to_srgb(l: f64, c: f64, h: f64) -> Rgba {
    let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
    let l_ = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m_ = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s_ = (l - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    Rgba {
        r: from_linear(4.076_741_662_1 * l_ - 3.307_711_591_3 * m_ + 0.230_969_929_2 * s_),
        g: from_linear(-1.268_438_004_6 * l_ + 2.609_757_401_1 * m_ - 0.341_319_396_5 * s_),
        b: from_linear(-0.004_196_086_3 * l_ - 0.703_418_614_7 * m_ + 1.707_614_701 * s_),
        a: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::theme::{resolve_preset_key, ThemePresets};
    use serde_json::Value;

    fn fixtures() -> Vec<(String, Value)> {
        let dir = concat!(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."), "/tests/fixtures/canvas-theme");
        let mut out: Vec<(String, Value)> = std::fs::read_dir(dir)
            .expect("canvas-theme fixture dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .map(|p| {
                let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                (
                    name,
                    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap(),
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn same_color(want: &str, got: &str) -> bool {
        match (parse_color(want), parse_color(got)) {
            (Some(w), Some(g)) => {
                (w.r - g.r).abs() <= 1.0
                    && (w.g - g.g).abs() <= 1.0
                    && (w.b - g.b).abs() <= 1.0
                    && (w.a - g.a).abs() <= 0.01
            }
            _ => want == got,
        }
    }

    fn same_value(w: &Value, g: &Value) -> bool {
        match (w, g) {
            (Value::String(ws), Value::String(gs)) => same_color(ws, gs),
            (Value::Number(wn), Value::Number(gn)) => {
                (wn.as_f64().unwrap() - gn.as_f64().unwrap()).abs() < 1e-6
            }
            _ => w == g,
        }
    }

    fn mismatches(name: &str, want: &Value, got: &CanvasTheme) -> Vec<String> {
        let got = serde_json::to_value(got).unwrap();
        let mut pairs: Vec<(String, Value, Value)> = Vec::new();
        for f in [
            "ground",
            "lineCap",
            "lineJoin",
            "fillStyle",
            "fillOpacity",
            "marker",
            "corner",
        ] {
            pairs.push((f.to_string(), want[f].clone(), got[f].clone()));
        }
        for f in ["dash", "dot", "series"] {
            let (w, g) = (want[f].as_array().unwrap(), got[f].as_array().unwrap());
            if w.len() == g.len() {
                for (i, (wi, gi)) in w.iter().zip(g).enumerate() {
                    pairs.push((format!("{f}[{i}]"), wi.clone(), gi.clone()));
                }
            } else {
                pairs.push((f.to_string(), want[f].clone(), got[f].clone()));
            }
        }
        for group in ["tones", "strokes", "text", "surfaces"] {
            for (k, w) in want[group].as_object().unwrap() {
                pairs.push((format!("{group}.{k}"), w.clone(), got[group][k].clone()));
            }
        }
        for k in ["size", "weight", "uppercase"] {
            pairs.push((
                format!("label.{k}"),
                want["label"][k].clone(),
                got["label"][k].clone(),
            ));
        }
        pairs
            .into_iter()
            .filter(|(_, w, g)| !same_value(w, g))
            .map(|(path, w, g)| format!("{name} {path}: web {w} native {g}"))
            .collect()
    }

    #[test]
    fn every_preset_matches_the_web_resolution_in_light_and_dark() {
        let cases = fixtures();
        assert!(cases.len() >= 2, "fixture present");
        let mut bad = Vec::new();
        for (name, fx) in &cases {
            let presets = ThemePresets::from_json(&fx["preset"].to_string()).unwrap();
            let key = format!(
                "{}-{}",
                fx["theme"].as_str().unwrap(),
                fx["mode"].as_str().unwrap()
            );
            let resolved = resolve_preset_key(&presets, &key);
            bad.extend(mismatches(
                name,
                &fx["canvasTheme"],
                &canvas_theme(&resolved.tokens),
            ));
        }
        assert!(
            bad.is_empty(),
            "{} mismatches:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    #[test]
    fn control_the_parity_check_catches_a_changed_tone_and_a_changed_stroke() {
        let (name, fx) = fixtures()
            .into_iter()
            .find(|(n, _)| n == "fixture-derived-defaults-light")
            .unwrap();
        let presets = ThemePresets::from_json(&fx["preset"].to_string()).unwrap();
        let resolved = resolve_preset_key(&presets, "fixture-derived-defaults-light");
        let mut theme = canvas_theme(&resolved.tokens);
        assert!(mismatches(&name, &fx["canvasTheme"], &theme).is_empty());
        theme.tones.insert("series-3".into(), "rgb(1, 2, 3)".into());
        theme.strokes.bold += 0.5;
        assert_eq!(mismatches(&name, &fx["canvasTheme"], &theme).len(), 2);
    }

    #[test]
    fn a_theme_with_only_primary_derives_the_series_by_rotating_hue() {
        let tokens: ThemeTokens =
            serde_json::from_value(serde_json::json!({ "colors": { "primary": "#c2410c" } }))
                .unwrap();
        let theme = canvas_theme(&tokens);
        assert!(same_color("rgb(194, 65, 12)", &theme.series[0]));
        assert!(!same_color(&theme.series[0], &theme.series[1]));
    }

    #[test]
    fn control_a_theme_with_no_colors_leaves_tones_unresolved_like_the_web() {
        let tokens: ThemeTokens = serde_json::from_value(serde_json::json!({})).unwrap();
        let theme = canvas_theme(&tokens);
        assert_eq!(theme.tones["ink"], UNRESOLVED_COLOR);
        assert_eq!(theme.ground, "transparent");
        assert_eq!(theme.surfaces.muted, "transparent");
        assert_eq!(theme.surfaces.card, "transparent");
        assert_eq!(theme.surfaces.on_primary, "transparent");
    }

    #[test]
    fn parse_color_reads_the_token_forms() {
        assert_eq!(parse_color("#fff").unwrap().r, 255.0);
        assert_eq!(parse_color("#0b618e").unwrap().b, 142.0);
        assert!((parse_color("rgba(0, 0, 0, 0.6)").unwrap().a - 0.6).abs() < 1e-9);
        assert!(parse_color("currentColor").is_none());
    }
}
