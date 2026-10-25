//! English -> native rendering: the forward direction of `i18n`.
//!
//! `lang::Lang::canon_*` goes native -> English so a program written in
//! Arabic or Slovenian COMPILES. This module is the mirror — it takes the
//! canonical English word and returns the target language's spelling — and it
//! is what `orb i18n translate` and the `.lolo` formatter's language mode are
//! built on.
//!
//! ## The round-trip guard
//!
//! A translation is only emitted when canonicalizing it lands back on the
//! same English word. Two English words can share one native spelling across
//! sections: Arabic has no letter case, so the `event` keyword and the
//! `Event` shape are both `حدث`, as are `entity`/`Entity`,
//! `orbital`/`Orbital`, `page`/`Page` and `trait`/`Trait`. The coverage gate's
//! collision rule is per-section, so these pass it. At such a word the parser
//! picks one meaning by its fixed resolution order, and emitting the other
//! would produce a program that no longer says what the English one said.
//!
//! When the guard rejects a translation the English word is kept, which is
//! always legal: every `canon_*` call site matches the English spelling
//! before consulting the table, so English words parse in every language.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::lang::Lang;
use serde_json::Value;
use std::collections::BTreeMap;

/// Look `english` up in `section`, keeping the result only when `canon`
/// reads it back as `english`.
fn guarded<'a>(
    section: &'a BTreeMap<String, String>,
    english: &str,
    canon: impl Fn(&str) -> Option<&'static str>,
) -> Option<&'a str> {
    let native = section.get(english)?.as_str();
    if canon(native) == Some(english) {
        Some(native)
    } else {
        None
    }
}

macro_rules! localize_fn {
    ($name:ident, $field:ident, $canon:ident) => {
        /// English -> native for this section, subject to the round-trip
        /// guard; `None` means "keep the English word".
        pub fn $name(&self, english: &str) -> Option<&str> {
            guarded(&self.def.$field, english, |native| self.$canon(native))
        }
    };
}

impl Lang {
    // Identifier positions resolve through `canon_any`, so the guard for a
    // keyword / shape / tag / category / capability must use it too.
    localize_fn!(localize_keyword, keywords, canon_any);
    localize_fn!(localize_shape, shapes, canon_any);
    localize_fn!(localize_tag, tags, canon_any);
    localize_fn!(localize_category, categories, canon_any);
    localize_fn!(localize_capability, capabilities, canon_any);
    // These have their own dedicated call-site lookups.
    localize_fn!(localize_type, types, canon_type);
    localize_fn!(localize_unit, units, canon_unit);
    localize_fn!(localize_literal, literals, canon_literal);
    localize_fn!(localize_reserved_event, reserved_events, canon_reserved_event);
    localize_fn!(localize_orb_key, orb, canon_orb_key);

    /// An s-expression head — an effect verb or an operator, guarded through
    /// `canon_head` (effects first, then operators), matching the parser.
    pub fn localize_head(&self, english: &str) -> Option<&str> {
        guarded(&self.def.effects, english, |n| self.canon_head(n))
            .or_else(|| guarded(&self.def.operators, english, |n| self.canon_head(n)))
    }

    /// A `@root` sigil or annotation, guarded through
    /// `canon_sigil_or_annotation`.
    pub fn localize_sigil_or_annotation(&self, english: &str) -> Option<&str> {
        guarded(&self.def.sigils, english, |n| {
            self.canon_sigil_or_annotation(n)
        })
        .or_else(|| {
            guarded(&self.def.annotations, english, |n| {
                self.canon_sigil_or_annotation(n)
            })
        })
    }

    /// An event name: only the reserved lifecycle event has a translation;
    /// every other event name is the author's and passes through.
    pub fn localize_event_name(&self, english: &str) -> Option<&str> {
        self.localize_reserved_event(english)
    }
}

/// The inverse of `translate::translate_orb_value` — rewrite an English
/// `.orb` document's structural positions into `lang`: every object key, a
/// `type` value, an array's leading string (an s-expression / effect head),
/// and the root of any `@sigil` string. Every other string is untouched.
pub fn localize_orb_value(value: &mut Value, lang: &Lang) {
    localize_walk(value, lang, false);
}

/// Mirror of `translate::walk`'s positional rule — see its doc comment for why
/// `type` cannot be resolved by name alone.
fn localize_walk(value: &mut Value, lang: &Lang, in_expression: bool) {
    match value {
        Value::Object(map) => {
            let entries = std::mem::take(map);
            for (key, mut v) in entries {
                if let Value::String(s) = &v {
                    let replacement = if key == "type" && !in_expression {
                        lang.localize_type(s)
                    } else if key == "persistence" {
                        lang.localize_tag(s)
                    } else {
                        None
                    };
                    if let Some(native) = replacement {
                        let native = native.to_string();
                        v = Value::String(native);
                    }
                }
                localize_walk(&mut v, lang, in_expression);
                let new_key = lang.localize_orb_key(&key).unwrap_or(&key).to_string();
                map.insert(new_key, v);
            }
        }
        Value::Array(arr) => {
            let head_native = match arr.first() {
                Some(Value::String(head)) => lang.localize_head(head).map(str::to_string),
                _ => None,
            };
            let is_expression = head_native.is_some();
            if let Some(native) = head_native {
                if let Some(Value::String(head)) = arr.first_mut() {
                    *head = native;
                }
            }
            for (index, item) in arr.iter_mut().enumerate() {
                if index == 0 && is_expression {
                    continue;
                }
                localize_walk(item, lang, in_expression || is_expression);
            }
        }
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix('@') {
                let (root, tail) = match rest.split_once('.') {
                    Some((r, t)) => (r, Some(t)),
                    None => (rest, None),
                };
                if let Some(native) = lang.localize_sigil_or_annotation(root) {
                    *s = match tail {
                        Some(t) => format!("@{native}.{t}"),
                        None => format!("@{native}"),
                    };
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::lang;

    fn ar() -> &'static Lang {
        lang::all().iter().find(|l| l.code == "ar").expect("ar")
    }
    fn sl() -> &'static Lang {
        lang::all().iter().find(|l| l.code == "sl").expect("sl")
    }

    #[test]
    fn keywords_localize_and_canon_back() {
        for l in [ar(), sl()] {
            for english in l.def.keywords.keys() {
                if let Some(native) = l.localize_keyword(english) {
                    assert_eq!(
                        l.canon_any(native),
                        Some(english.as_str()),
                        "{}: {english} -> {native} did not canon back",
                        l.code
                    );
                }
            }
        }
    }

    #[test]
    fn every_localizable_word_round_trips() {
        // The guard's contract: whatever this module emits, the parser reads
        // back as the same English word. Checked across every section.
        for l in [ar(), sl()] {
            for english in l.def.types.keys() {
                if let Some(native) = l.localize_type(english) {
                    assert_eq!(l.canon_type(native), Some(english.as_str()));
                }
            }
            for english in l.def.effects.keys() {
                if let Some(native) = l.localize_head(english) {
                    assert_eq!(l.canon_head(native), Some(english.as_str()));
                }
            }
            for english in l.def.operators.keys() {
                if let Some(native) = l.localize_head(english) {
                    assert_eq!(l.canon_head(native), Some(english.as_str()));
                }
            }
            for english in l.def.sigils.keys() {
                if let Some(native) = l.localize_sigil_or_annotation(english) {
                    assert_eq!(l.canon_sigil_or_annotation(native), Some(english.as_str()));
                }
            }
        }
    }

    #[test]
    fn arabic_case_collisions_keep_the_english_shape_word() {
        // `event`/`Event` are both `حدث`; the keyword wins in `canon_any`, so
        // the SHAPE must not be emitted or `type X = Event {…}` would parse
        // as the `event` keyword.
        let ar = ar();
        assert_eq!(ar.localize_keyword("event"), Some("حدث"));
        assert_eq!(ar.localize_shape("Event"), None);
        assert_eq!(ar.localize_shape("Entity"), None);
    }

    #[test]
    fn slovenian_shapes_are_case_distinct_and_survive() {
        let sl = sl();
        assert!(sl.localize_shape("Event").is_some());
    }

    /// `type` is overloaded: a field type on an entity field, a PATTERN name
    /// inside a `render-ui` props object. Exactly one pattern name collides
    /// with a field-type word (`icon`). Both walkers used to apply `types` at
    /// both positions, which renamed the pattern — and because the defect was
    /// symmetric, the `.orb` round-trip still matched and the corpus gate
    /// could not see it. Hence these assertions.
    #[test]
    fn a_pattern_name_inside_an_expression_is_left_alone() {
        for l in [ar(), sl()] {
            let mut v: Value = serde_json::json!({
                "effects": [["render-ui", "main", { "type": "icon", "name": "cart" }]]
            });
            localize_orb_value(&mut v, l);
            let text = v.to_string();
            assert!(text.contains("\"icon\""), "{}: pattern name was translated: {text}", l.code);
        }
    }

    #[test]
    fn a_field_type_outside_an_expression_is_still_translated() {
        for l in [ar(), sl()] {
            let mut v: Value =
                serde_json::json!({ "entity": { "fields": [{ "name": "avatar", "type": "icon" }] } });
            localize_orb_value(&mut v, l);
            let native = l.localize_type("icon").expect("icon translates");
            assert!(v.to_string().contains(native), "{}: field type not translated", l.code);
        }
    }

    #[test]
    fn a_plain_string_list_is_not_mistaken_for_an_expression() {
        for l in [ar(), sl()] {
            let mut v: Value = serde_json::json!({ "values": ["title", "completed"] });
            localize_orb_value(&mut v, l);
            assert!(v.to_string().contains("\"title\""), "{}: value list altered", l.code);
        }
    }

    #[test]
    fn persistence_values_are_translated_and_canon_back() {
        for l in [ar(), sl()] {
            let mut v: Value = serde_json::json!({ "entity": { "persistence": "persistent" } });
            localize_orb_value(&mut v, l);
            let native = l.localize_tag("persistent").expect("persistent translates");
            assert!(v.to_string().contains(native), "{}: persistence not translated", l.code);
            super::super::translate::translate_orb_value(&mut v, l);
            assert!(v.to_string().contains("persistent"), "{}: persistence did not canon back", l.code);
        }
    }

    /// Every field type core accepts must be spellable in every language.
    #[test]
    fn every_field_type_word_has_a_translation() {
        for l in [ar(), sl()] {
            for t in ["array", "enum", "relation", "node", "union"] {
                assert!(
                    l.def.types.contains_key(t),
                    "{}: types table is missing the field type `{t}`",
                    l.code
                );
                let native = l.localize_type(t).unwrap_or_else(|| panic!("{}: {t} withheld", l.code));
                assert_eq!(l.canon_type(native), Some(t), "{}: {t} -> {native} did not canon back", l.code);
            }
        }
    }

    #[test]
    fn orb_value_round_trips_through_canonicalization() {
        for l in [ar(), sl()] {
            let mut v: Value = serde_json::json!({
                "orbitals": [{
                    "name": "Demo",
                    "entity": { "name": "Task", "fields": [{ "name": "title", "type": "string" }] },
                    "effects": [["set", "@entity.title", "x"]]
                }]
            });
            let original = v.clone();
            localize_orb_value(&mut v, l);
            super::super::translate::translate_orb_value(&mut v, l);
            assert_eq!(v, original, "{} did not round-trip", l.code);
        }
    }
}
