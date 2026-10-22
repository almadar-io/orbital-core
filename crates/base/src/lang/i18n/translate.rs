//! Structural `.orb` JSON translation: native-language keys/heads/types/
//! sigils -> canonical English. Never touches any other string value (a
//! user's entity name, a label, free text, …) — only the four positions the
//! `.orb` schema itself gives grammatical meaning to.

use super::lang::{self, Lang};
use serde_json::Value;

/// Walk `value` in place, rewriting:
/// - every object key, via `canon_orb_key`;
/// - the string VALUE of a (now-English) `"type"` key, via `canon_type`;
/// - the first element of an array, when it is a string, via `canon_head`
///   (an s-expression / effect-array head — `["set", …]`, `["array/push",
///   …]`);
/// - any string starting with `@`, translating the sigil root (the text
///   between `@` and the first `.`, or the whole rest when there is no `.`)
///   via `canon_sigil`.
///
/// Every other string is left untouched.
pub fn translate_orb_value(value: &mut Value, lang: &Lang) {
    walk(value, lang, false);
}

/// `in_expression` is true once we are inside an s-expression. `type` is
/// overloaded and only POSITION separates its two meanings: on an entity field
/// it names a field type, inside a `render-ui` props object it names a PATTERN,
/// which is opaque by the pattern-vocabulary ruling. Exactly one of the 279
/// pattern names collides with a field-type word (`icon`), and canonicalizing
/// it there renamed the pattern.
///
/// The same defect existed in both directions, so `orb emit orb` matched and
/// the round-trip corpus gate could not see it — a symmetric error is invisible
/// to a symmetric gate.
fn walk(value: &mut Value, lang: &Lang, in_expression: bool) {
    match value {
        Value::Object(map) => {
            let entries = std::mem::take(map);
            for (key, mut v) in entries {
                walk(&mut v, lang, in_expression);
                let new_key = lang.canon_orb_key(&key).unwrap_or(&key).to_string();
                if let Value::String(s) = &v {
                    let replacement = if new_key == "type" && !in_expression {
                        lang.canon_type(s)
                    } else if new_key == "persistence" {
                        // The word is `tags` vocabulary and has always had a
                        // translation; only this walker never applied it.
                        lang.canon_tag(s)
                    } else {
                        None
                    };
                    if let Some(canon) = replacement {
                        v = Value::String(canon.to_string());
                    }
                }
                map.insert(new_key, v);
            }
        }
        Value::Array(arr) => {
            // An array is an s-expression only when its head resolves as one;
            // a plain value list (`["title", "completed"]`) is not.
            let head_canon = match arr.first() {
                Some(Value::String(head)) => lang.canon_head(head),
                _ => None,
            };
            let is_expression = head_canon.is_some();
            if let Some(canon) = head_canon {
                let canon = canon.to_string();
                if let Some(Value::String(head)) = arr.first_mut() {
                    *head = canon;
                }
            }
            for (index, item) in arr.iter_mut().enumerate() {
                if index == 0 && is_expression {
                    continue;
                }
                walk(item, lang, in_expression || is_expression);
            }
        }
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix('@') {
                let (root, tail) = match rest.split_once('.') {
                    Some((r, t)) => (r, Some(t)),
                    None => (rest, None),
                };
                if let Some(canon) = lang.canon_sigil(root) {
                    *s = match tail {
                        Some(t) => format!("@{canon}.{t}"),
                        None => format!("@{canon}"),
                    };
                }
            }
        }
        _ => {}
    }
}

/// Parse `content` as JSON, detect its language from the top-level keys
/// (`Lang::for_orb_top_level_keys`), translate it to English when a
/// non-English language is detected, and return the pretty-printed result
/// plus the detected language (`None` = already English / undetected).
pub fn detect_and_translate(
    content: &str,
) -> Result<(String, Option<&'static Lang>), serde_json::Error> {
    let mut value: Value = serde_json::from_str(content)?;
    let detected = match &value {
        Value::Object(map) => lang::for_orb_top_level_keys(map.keys().map(String::as_str)),
        _ => None,
    };
    if let Some(l) = detected {
        translate_orb_value(&mut value, l);
    }
    let pretty = serde_json::to_string_pretty(&value)?;
    Ok((pretty, detected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build the Arabic rendering of a small English `.orb` value using the
    /// baked EN->native maps directly (the forward direction — this is the
    /// TS generator's translation direction, exercised here to construct a
    /// fixture), then translate it back with `translate_orb_value` and
    /// assert we recover the original. Some `ar.json` entries are still
    /// empty pending the parallel translation wave; skip (rather than
    /// fail) when a required entry isn't translated yet, so this test
    /// passes automatically once the tables are complete.
    #[test]
    fn arabic_orb_value_round_trips_to_english() {
        let ar = lang::all().iter().find(|l| l.code == "ar").unwrap();
        let need = [
            ("orb.orbitals", ar.def.orb.get("orbitals")),
            ("orb.name", ar.def.orb.get("name")),
            ("orb.type", ar.def.orb.get("type")),
            ("types.string", ar.def.types.get("string")),
            ("effects.set", ar.def.effects.get("set")),
            ("sigils.entity", ar.def.sigils.get("entity")),
        ];
        let missing: Vec<&str> = need
            .iter()
            .filter(|(_, v)| v.map(|s| s.is_empty()).unwrap_or(true))
            .map(|(k, _)| *k)
            .collect();
        if !missing.is_empty() {
            eprintln!(
                "arabic_orb_value_round_trips_to_english: skipped, ar.json missing: {:?}",
                missing
            );
            return;
        }
        let orbitals = ar.def.orb["orbitals"].clone();
        let name_key = ar.def.orb["name"].clone();
        let type_key = ar.def.orb["type"].clone();
        let string_type = ar.def.types["string"].clone();
        let set_head = ar.def.effects["set"].clone();
        let entity_sigil = ar.def.sigils["entity"].clone();

        let mut native = json!({
            orbitals: [
                {
                    name_key: "Widget",
                    type_key: string_type,
                    "default": [set_head, format!("@{entity_sigil}.count"), 0],
                }
            ]
        });

        translate_orb_value(&mut native, ar);

        let expected = json!({
            "orbitals": [
                {
                    "name": "Widget",
                    "type": "string",
                    "default": ["set", "@entity.count", 0],
                }
            ]
        });
        assert_eq!(native, expected);
    }
}
