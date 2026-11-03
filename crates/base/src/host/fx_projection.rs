//! Pure world FX presentation projection shared by native hosts and the web WASM host.

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde_json::{json, Map, Value};
use std::f64::consts::TAU;

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number_or(value: &Value, key: &str, default: f64) -> f64 {
    number(value, key).unwrap_or(default)
}

pub fn resolve_view(item: &Value, presets: Option<&[Value]>) -> Value {
    let Some(row) = presets.and_then(|rows| {
        rows.iter()
            .find(|row| text(row, "type") == text(item, "type"))
    }) else {
        return item.clone();
    };
    let Some(mut merged) = item.as_object().cloned() else {
        return item.clone();
    };
    for key in [
        "space",
        "effect",
        "color",
        "size",
        "vx",
        "vy",
        "vz",
        "particleCount",
        "animation",
    ] {
        if merged.get(key).is_none_or(Value::is_null) {
            if let Some(value) = row.get(key) {
                merged.insert(key.into(), value.clone());
            }
        }
    }
    for key in ["shape", "count", "glow", "gravity", "color2"] {
        if let Some(value) = row.get(key) {
            merged.insert(key.into(), value.clone());
        }
    }
    Value::Object(merged)
}

fn lifecycle(view: &Value, epoch_now_ms: f64, tick_ms: f64) -> (f64, f64, f64, f64) {
    let ttl = number_or(view, "ttl", 0.0);
    let max_ttl = number_or(view, "maxTtl", ttl).max(ttl).max(1.0);
    let life_ms = max_ttl * tick_ms;
    let age_ms = match number(view, "bornAt") {
        Some(born_at) if epoch_now_ms > 0.0 => (epoch_now_ms - born_at).max(0.0),
        _ => (1.0 - ttl / max_ttl) * life_ms,
    };
    let progress = (age_ms / life_ms).clamp(0.0, 1.0);
    (life_ms, age_ms, progress, 1.0 - progress)
}

pub fn lifecycle_view(view: &Value, epoch_now_ms: f64, tick_ms: f64) -> Value {
    let (life_ms, age_ms, progress, fade) = lifecycle(view, epoch_now_ms, tick_ms);
    json!({ "lifeMs": life_ms, "ageMs": age_ms,
        "progress": progress, "fade": fade })
}

/// The web FX seed is FNV-1a over JavaScript UTF-16 code units, then one avalanche.
pub fn fx_hash01(seed: &str, salt: u32) -> f64 {
    let mut hash = 2_166_136_261u32 ^ salt;
    for unit in seed.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(1_274_126_177);
    hash ^= hash >> 16;
    f64::from(hash) / 4_294_967_296.0
}

fn position(view: &Value, dimension: &str, age_sec: f64) -> Option<Value> {
    let gravity = number_or(view, "gravity", 0.0);
    let fall = 0.5 * gravity * age_sec * age_sec;
    if dimension == "2d" {
        let base = view.get("position");
        let x = base
            .and_then(|p| number(p, "x"))
            .or_else(|| number(view, "x"))?;
        let y = base
            .and_then(|p| number(p, "y"))
            .or_else(|| number(view, "z"))
            .or_else(|| number(view, "y"))
            .unwrap_or(0.0);
        let x = x + number_or(view, "vx", 0.0) * age_sec;
        let y = y + number_or(view, "vz", 0.0) * age_sec + fall;
        (x.is_finite() && y.is_finite()).then(|| json!({ "x": x, "y": y }))
    } else {
        let base = view.get("position");
        let x = base
            .and_then(|p| number(p, "x"))
            .or_else(|| number(view, "x"))?;
        let row = base
            .and_then(|p| number(p, "y"))
            .or_else(|| number(view, "z"))
            .unwrap_or(0.0);
        let height = base
            .and_then(|p| number(p, "z"))
            .or_else(|| number(view, "y"));
        let x = x + number_or(view, "vx", 0.0) * age_sec;
        let row = row + number_or(view, "vz", 0.0) * age_sec;
        if !(x.is_finite() && row.is_finite()) {
            return None;
        }
        let mut pos = json!({ "x": x, "y": row });
        if height.is_some() || number(view, "vy").is_some() || gravity != 0.0 {
            pos["z"] = json!(height.unwrap_or(0.0) + number_or(view, "vy", 0.0) * age_sec - fall);
        }
        Some(pos)
    }
}

fn offset_position(pos: &Value, dx: f64, dy: f64) -> Value {
    let mut next = pos.clone();
    next["x"] = json!(number_or(pos, "x", 0.0) + dx);
    next["y"] = json!(number_or(pos, "y", 0.0) + dy);
    next
}

fn ease_out(progress: f64) -> f64 {
    1.0 - (1.0 - progress) * (1.0 - progress)
}

fn shadow(view: &Value, color: &str) -> Option<Value> {
    number(view, "glow")
        .filter(|v| *v != 0.0)
        .map(|blur| json!({ "color": color, "blur": blur }))
}

fn shape_node(mut fields: Map<String, Value>, view: &Value, color: &str) -> Value {
    if let Some(shadow) = shadow(view, color) {
        fields.insert("shadow".into(), shadow);
    }
    Value::Object(fields)
}

fn procedural(view: &Value, pos: &Value, fade: f64, progress: f64) -> Vec<Value> {
    let size = number_or(view, "size", 0.5);
    let color = text(view, "color").unwrap_or("#ffffff");
    let id = text(view, "id").unwrap_or("");
    match text(view, "shape").unwrap_or("spark") {
        "ring" => {
            let stroke = text(view, "color2").unwrap_or(color);
            let mut fields = Map::new();
            fields.insert("type".into(), json!("draw-shape"));
            fields.insert("shape".into(), json!("ellipse"));
            fields.insert("position".into(), pos.clone());
            fields.insert("anchor".into(), json!("center"));
            fields.insert("radiusX".into(), json!(size * ease_out(progress)));
            fields.insert("stroke".into(), json!(stroke));
            fields.insert("strokeWidth".into(), json!(2));
            fields.insert("blendMode".into(), json!("lighter"));
            fields.insert("opacity".into(), json!(fade));
            vec![shape_node(fields, view, color)]
        }
        "puff" => {
            let count = number_or(view, "count", 3.0).max(0.0) as usize;
            (0..count)
                .map(|i| {
                    let salt = i as u32 * 5;
                    let angle = fx_hash01(id, salt) * TAU;
                    let drift = size * 0.3 * fx_hash01(id, salt + 1) * ease_out(progress);
                    let p = offset_position(pos, angle.cos() * drift, angle.sin() * drift);
                    let fill = if i == 0 {
                        text(view, "color2").unwrap_or(color)
                    } else {
                        color
                    };
                    let radius =
                        size * (0.15 + 0.35 * progress) * (0.7 + 0.6 * fx_hash01(id, salt + 2));
                    let mut fields = Map::new();
                    fields.insert("type".into(), json!("draw-shape"));
                    fields.insert("shape".into(), json!("ellipse"));
                    fields.insert("position".into(), p);
                    fields.insert("anchor".into(), json!("center"));
                    fields.insert("radiusX".into(), json!(radius));
                    fields.insert("fill".into(), json!(fill));
                    fields.insert("opacity".into(), json!(fade * 0.6));
                    vec![shape_node(fields, view, color)]
                })
                .flatten()
                .collect()
        }
        "streak" => {
            let count = number_or(view, "count", 5.0).max(0.0) as usize;
            (0..count)
                .map(|i| {
                    let salt = i as u32 * 3;
                    let angle = fx_hash01(id, salt) * TAU;
                    let inner = size
                        * (0.2 + 0.8 * ease_out(progress))
                        * (0.6 + 0.4 * fx_hash01(id, salt + 1));
                    let length = size * 0.35;
                    let mut fields = Map::new();
                    fields.insert("type".into(), json!("draw-shape"));
                    fields.insert("shape".into(), json!("ellipse"));
                    fields.insert("position".into(), pos.clone());
                    fields.insert("anchor".into(), json!("center"));
                    fields.insert("offsetX".into(), json!(inner + length / 2.0));
                    fields.insert("offsetY".into(), json!(0));
                    fields.insert("radiusX".into(), json!(length / 2.0));
                    fields.insert("radiusY".into(), json!(size * 0.04));
                    fields.insert("rotate".into(), json!(angle));
                    fields.insert("fill".into(), json!(color));
                    fields.insert("blendMode".into(), json!("lighter"));
                    fields.insert("opacity".into(), json!(fade));
                    vec![shape_node(fields, view, color)]
                })
                .flatten()
                .collect()
        }
        _ => {
            let count = number_or(view, "count", 6.0).max(0.0) as usize;
            (0..count)
                .map(|i| {
                    let salt = i as u32 * 3;
                    let angle = fx_hash01(id, salt) * TAU;
                    let dist = size * (0.4 + 0.6 * fx_hash01(id, salt + 1)) * ease_out(progress);
                    let radius = size * (0.06 + 0.06 * fx_hash01(id, salt + 2));
                    let p = offset_position(pos, angle.cos() * dist, angle.sin() * dist);
                    let mut fields = Map::new();
                    fields.insert("type".into(), json!("draw-shape"));
                    fields.insert("shape".into(), json!("ellipse"));
                    fields.insert("position".into(), p);
                    fields.insert("anchor".into(), json!("center"));
                    fields.insert("radiusX".into(), json!(radius));
                    fields.insert("fill".into(), json!(color));
                    fields.insert("blendMode".into(), json!("lighter"));
                    fields.insert("opacity".into(), json!(fade));
                    vec![shape_node(fields, view, color)]
                })
                .flatten()
                .collect()
        }
    }
}

/// Expand one `draw-fx-layer` descriptor at the host's paint clock instant.
/// `std-fx-particles` still owns ttl decay and removal; this only derives pixels.
pub fn expand_layer(
    layer: &Value,
    epoch_now_ms: f64,
    dimension: &str,
    tile_width: Option<f64>,
    font_family: Option<&str>,
) -> Vec<Value> {
    let Some(items) = layer.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    let presets = layer
        .get("presets")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let mut out = Vec::new();
    for item in items {
        if text(item, "id").is_none() {
            continue;
        }
        let view = resolve_view(item, presets);
        if text(&view, "space") == Some("screen") {
            continue;
        }
        let tick_ms = number_or(layer, "tickMs", 500.0);
        let (_, age_ms, progress, fade) = lifecycle(&view, epoch_now_ms, tick_ms);
        if progress >= 1.0 {
            continue;
        }
        let Some(pos) = position(&view, dimension, age_ms / 1000.0) else {
            continue;
        };
        let kind = text(&view, "type").unwrap_or("");
        let art = layer
            .get("art")
            .and_then(|art| art.get(kind))
            .and_then(|art| art.get("idle"));
        let sprite = layer.get("sprites").and_then(|sprites| sprites.get(kind));
        if let Some(art) = art.and_then(Value::as_array) {
            let mut group =
                json!({ "type": "draw-group", "position": pos, "opacity": fade, "items": art });
            if let Some(size) = number(&view, "size") {
                group["scale"] = json!(size);
            }
            out.push(group);
        } else if sprite.and_then(|asset| text(asset, "url")).is_some() {
            let mut node = json!({
                "type": "draw-sprite", "position": pos, "asset": sprite,
                "anchor": "center", "width": number_or(&view, "size", 0.6),
                "height": number_or(&view, "size", 0.6), "opacity": fade,
                "clockMs": age_ms,
            });
            if let Some(animation) = view.get("animation").filter(|v| !v.is_null()).or_else(|| {
                sprite
                    .and_then(|s| s.get("animations"))
                    .and_then(Value::as_array)
                    .and_then(|v| v.first())
            }) {
                node["animation"] = animation.clone();
            }
            out.push(node);
        } else {
            out.extend(procedural(&view, &pos, fade, progress));
        }
        if let Some(message) = text(&view, "message").filter(|message| !message.is_empty()) {
            let px_size = tile_width
                .and_then(|tw| number(&view, "size").map(|size| (size * tw).round().max(10.0)));
            let family = font_family.unwrap_or("system-ui, sans-serif");
            out.push(json!({
                "type": "draw-text", "text": message, "position": pos,
                "offsetY": -(0.15 + 0.55 * progress),
                "color": text(&view, "color").or_else(|| text(layer, "textColor")).unwrap_or("#ffe066"),
                "opacity": fade, "font": format!("bold {}px {family}", px_size.unwrap_or(14.0)),
            }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fx_projection_uses_preset_under_item_and_smooth_birth_clock() {
        let layer = json!({
            "items": [{ "id": "hit-1", "type": "hit", "x": 2, "z": 3,
                "ttl": 2, "maxTtl": 4, "bornAt": 1000, "message": "-2", "color": "#ff0000" }],
            "presets": [{ "type": "hit", "shape": "ring", "size": 0.5,
                "color": "#00ff00", "vx": 1 }], "tickMs": 500
        });
        let out = expand_layer(&layer, 1500.0, "2d", Some(32.0), None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["shape"], "ellipse");
        assert_eq!(out[0]["stroke"], "#ff0000");
        assert_eq!(out[0]["position"]["x"], 2.5);
        assert_eq!(out[0]["opacity"], 0.75);
        assert_eq!(out[1]["font"], "bold 16px system-ui, sans-serif");
    }

    #[test]
    fn fx_projection_skips_screen_and_expired_entries() {
        let layer = json!({ "items": [
            {"id":"a","type":"spark","x":0,"ttl":1,"space":"screen"},
            {"id":"b","type":"spark","x":0,"ttl":0,"maxTtl":1,"bornAt":1000}
        ] });
        assert!(expand_layer(&layer, 2000.0, "2d", None, None).is_empty());
    }

    #[test]
    fn fx_hash_matches_utf16_seed_contract() {
        assert_eq!(fx_hash01("", 0), 0.24220648407936096);
        assert!(fx_hash01("💥", 2) >= 0.0 && fx_hash01("💥", 2) < 1.0);
    }
}
