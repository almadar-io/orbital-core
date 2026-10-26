//! Pattern vocabulary tables for the validator, derived once at first use from the
//! INSTALLED `@almadar/core` patterns registry (`crate::language_data`,
//! `docs/Almadar_Behavior_Packages.md` §14) — no longer a pattern-sync-generated
//! copy, so a new pattern or prop needs no compiler rebuild.
//!
//! The table names and accessor signatures are unchanged from the generated file
//! they replace; the derivation rules are the ones `tools/almadar-pattern-sync/
//! rust-codegen.ts` applied at generation time.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::{HashMap, HashSet};
use crate::sync::LazyLock;

use serde_json::{Map, Value};

/// Item-action props the validator always accepts; each pattern's declared
/// `itemActions` item properties are added to these.
const BASE_ITEM_ACTION_PROPS: [&str; 9] = [
    "label",
    "event",
    "icon",
    "variant",
    "disabled",
    "hidden",
    "confirm",
    "payload",
    "navigatesTo",
];

static REGISTRY: LazyLock<Value> = LazyLock::new(|| {
    let data = crate::language_data::language_data_after_preflight();
    serde_json::from_str(&data.patterns).unwrap_or_else(|e| {
        panic!(
            "ORB_LANGUAGE_DATA_INVALID: {}: patterns registry is not valid JSON: {e}",
            data.root_text()
        )
    })
});

fn patterns_map() -> &'static Map<String, Value> {
    match REGISTRY.get("patterns").and_then(Value::as_object) {
        Some(map) => map,
        None => panic!("ORB_LANGUAGE_DATA_INVALID: patterns registry has no `patterns` object"),
    }
}

fn props_schema(def: &'static Value) -> Option<&'static Map<String, Value>> {
    def.get("propsSchema").and_then(Value::as_object)
}

fn str_list(value: Option<&'static Value>) -> Vec<&'static str> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn leak(list: Vec<&'static str>) -> &'static [&'static str] {
    Box::leak(list.into_boxed_slice())
}

fn category_of(def: &'static Value) -> &'static str {
    def.get("category")
        .and_then(Value::as_str)
        .unwrap_or("component")
}

/// All valid pattern types
pub static PATTERN_TYPES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| patterns_map().keys().map(String::as_str).collect());

/// Pattern categories
pub static PATTERN_CATEGORIES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| patterns_map().values().map(category_of).collect());

/// Pattern to category mapping
pub static PATTERN_TO_CATEGORY: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| {
        patterns_map()
            .iter()
            .map(|(name, def)| (name.as_str(), category_of(def)))
            .collect()
    });

/// Valid itemAction props (to detect hallucinated props from LLMs)
pub static VALID_ITEM_ACTION_PROPS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut out: HashSet<&'static str> = BASE_ITEM_ACTION_PROPS.into_iter().collect();
    for def in patterns_map().values() {
        let declared = props_schema(def)
            .and_then(|schema| schema.get("itemActions"))
            .and_then(|prop| prop.get("items"))
            .and_then(|items| items.get("properties"))
            .and_then(Value::as_object);
        if let Some(properties) = declared {
            out.extend(properties.keys().map(String::as_str));
        }
    }
    out
});

/// Required props for patterns that need them
pub static PATTERN_REQUIRED_PROPS: LazyLock<HashMap<&'static str, Vec<&'static str>>> =
    LazyLock::new(|| {
        patterns_map()
            .iter()
            .filter_map(|(name, def)| {
                let required: Vec<&'static str> = props_schema(def)?
                    .iter()
                    .filter(|(_, prop)| prop.get("required").and_then(Value::as_bool) == Some(true))
                    .map(|(prop, _)| prop.as_str())
                    .collect();
                (!required.is_empty()).then_some((name.as_str(), required))
            })
            .collect()
    });

/// Valid props per pattern (from registry propsSchema)
pub static PATTERN_PROPS_SCHEMA: LazyLock<HashMap<&'static str, HashSet<&'static str>>> =
    LazyLock::new(|| {
        patterns_map()
            .iter()
            .filter_map(|(name, def)| {
                let schema = props_schema(def)?;
                (!schema.is_empty())
                    .then(|| (name.as_str(), schema.keys().map(String::as_str).collect()))
            })
            .collect()
    });

/// Prop types per pattern (pattern_type -> prop_name -> allowed_types).
/// Only includes props with concrete types (not "unknown").
pub static PATTERN_PROP_TYPES: LazyLock<
    HashMap<&'static str, HashMap<&'static str, &'static [&'static str]>>,
> = LazyLock::new(|| {
    patterns_map()
        .iter()
        .filter_map(|(name, def)| {
            let props: HashMap<&'static str, &'static [&'static str]> = props_schema(def)?
                .iter()
                .filter_map(|(prop, prop_def)| {
                    let concrete: Vec<&'static str> = str_list(prop_def.get("types"))
                        .into_iter()
                        .filter(|t| *t != "unknown")
                        .collect();
                    (!concrete.is_empty()).then(|| (prop.as_str(), leak(concrete)))
                })
                .collect();
            (!props.is_empty()).then_some((name.as_str(), props))
        })
        .collect()
});

/// Allowed string-literal values per pattern prop (pattern_type -> prop_name
/// -> allowed_values). Only present when the prop's declared TypeScript type
/// is a bounded union of string literals (e.g. StackJustify, AlertVariant).
/// The validator rejects any literal value that isn't in this set.
pub static PATTERN_PROP_ENUM_VALUES: LazyLock<
    HashMap<&'static str, HashMap<&'static str, &'static [&'static str]>>,
> = LazyLock::new(|| {
    patterns_map()
        .iter()
        .filter_map(|(name, def)| {
            let props: HashMap<&'static str, &'static [&'static str]> = props_schema(def)?
                .iter()
                .filter_map(|(prop, prop_def)| {
                    let values = str_list(prop_def.get("enumValues"));
                    (!values.is_empty()).then(|| (prop.as_str(), leak(values)))
                })
                .collect();
            (!props.is_empty()).then_some((name.as_str(), props))
        })
        .collect()
});

/// Event-valued prop names per pattern (pattern_type -> set of prop names whose
/// registry `kind` is "event", i.e. typed `EventKey` in `@almadar/core`). The
/// inline phase routes these props' string values through the trait-ref events
/// rename map. Type-derived by pattern-sync — no name heuristics.
pub static PATTERN_EVENT_PROPS: LazyLock<HashMap<&'static str, HashSet<&'static str>>> =
    LazyLock::new(|| {
        patterns_map()
            .iter()
            .filter_map(|(name, def)| {
                let props: HashSet<&'static str> = props_schema(def)?
                    .iter()
                    .filter(|(_, prop)| prop.get("kind").and_then(Value::as_str) == Some("event"))
                    .map(|(prop, _)| prop.as_str())
                    .collect();
                (!props.is_empty()).then_some((name.as_str(), props))
            })
            .collect()
    });

/// Every prop that names a bus event, per pattern (pattern_type -> prop_name -> (registry kind, eventField)): the
/// kinds `event`, `event-ref`, `event-listen`, `event-list` (its `eventField`, default `event`) and `event-map` (its
/// values). Type-derived by pattern-sync, no name heuristics.
pub static PATTERN_EVENT_NAME_PROPS: LazyLock<
    HashMap<&'static str, HashMap<&'static str, (&'static str, &'static str)>>,
> = LazyLock::new(|| {
    const KINDS: [&str; 5] = ["event", "event-ref", "event-listen", "event-list", "event-map"];
    patterns_map()
        .iter()
        .filter_map(|(name, def)| {
            let props: HashMap<&'static str, (&'static str, &'static str)> = props_schema(def)?
                .iter()
                .filter_map(|(prop, prop_def)| {
                    let kind = prop_def.get("kind").and_then(Value::as_str)?;
                    let kind = KINDS.iter().copied().find(|k| *k == kind)?;
                    let field = prop_def.get("eventField").and_then(Value::as_str).unwrap_or("event");
                    Some((prop.as_str(), (kind, field)))
                })
                .collect();
            (!props.is_empty()).then_some((name.as_str(), props))
        })
        .collect()
});

/// Event-list props per pattern (pattern_type -> prop_name -> eventField). The
/// inline phase walks the array and rewrites each item's eventField-named value.
/// The default eventField is "event".
pub static PATTERN_EVENT_LIST_PROPS: LazyLock<
    HashMap<&'static str, HashMap<&'static str, &'static str>>,
> = LazyLock::new(|| {
    patterns_map()
        .iter()
        .filter_map(|(name, def)| {
            let props: HashMap<&'static str, &'static str> = props_schema(def)?
                .iter()
                .filter(|(_, prop)| prop.get("kind").and_then(Value::as_str) == Some("event-list"))
                .map(|(prop, prop_def)| {
                    let field = prop_def
                        .get("eventField")
                        .and_then(Value::as_str)
                        .unwrap_or("event");
                    (prop.as_str(), field)
                })
                .collect();
            (!props.is_empty()).then_some((name.as_str(), props))
        })
        .collect()
});

/// Patterns that require action/event props
pub static ACTION_REQUIRED_PATTERNS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["button", "icon-button", "link"].into_iter().collect());

// Canvas draw-host + neutral-drawable capability sets live in orbital-core
// (`crate::drawable`) — shared by the validator here and the
// orbital-client render_probe. Re-exported so `sync::patterns::*` imports resolve.
pub use crate::drawable::{is_draw_host, is_drawable, PATTERN_DRAWABLE, PATTERN_DRAW_HOST};

/// Check if a pattern type is valid
pub fn is_valid_pattern(pattern_type: &str) -> bool {
    PATTERN_TYPES.contains(pattern_type)
}

/// Get the category for a pattern
pub fn get_pattern_category(pattern_type: &str) -> Option<&'static str> {
    PATTERN_TO_CATEGORY.get(pattern_type).copied()
}

/// Get required props for a pattern
pub fn get_pattern_props(pattern_type: &str) -> Option<&'static Vec<&'static str>> {
    PATTERN_REQUIRED_PROPS.get(pattern_type)
}

/// Check if a pattern requires action prop
pub fn requires_action(pattern_type: &str) -> bool {
    ACTION_REQUIRED_PATTERNS.contains(pattern_type)
}

/// Check if an itemAction prop is valid
pub fn is_valid_item_action_prop(prop: &str) -> bool {
    VALID_ITEM_ACTION_PROPS.contains(prop)
}

/// Get valid props for a pattern (from PATTERN_PROPS_SCHEMA)
pub fn get_pattern_valid_props(pattern_type: &str) -> Option<&'static HashSet<&'static str>> {
    PATTERN_PROPS_SCHEMA.get(pattern_type)
}

/// Get expected types for a specific prop on a pattern.
/// Returns None if the pattern or prop is not in the registry, or if types are unknown.
pub fn get_prop_types(pattern_type: &str, prop_name: &str) -> Option<&'static [&'static str]> {
    PATTERN_PROP_TYPES
        .get(pattern_type)
        .and_then(|props| props.get(prop_name))
        .copied()
}

/// Get the allowed string-literal enum values for a prop on a pattern.
/// Returns Some(&["start","center",...]) when the prop's TS type is a
/// bounded union of string literals; None when the prop is unbounded or the
/// pattern/prop is unknown.
pub fn get_prop_enum_values(
    pattern_type: &str,
    prop_name: &str,
) -> Option<&'static [&'static str]> {
    PATTERN_PROP_ENUM_VALUES
        .get(pattern_type)
        .and_then(|props| props.get(prop_name))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_types() {
        assert!(is_valid_pattern("table-view"));
        assert!(is_valid_pattern("form"));
        assert!(is_valid_pattern("canvas-2d"));
        assert!(!is_valid_pattern("nonexistent-pattern"));
    }

    #[test]
    fn test_pattern_category() {
        assert_eq!(get_pattern_category("table-view"), Some("display"));
        assert_eq!(get_pattern_category("form"), Some("form"));
        assert_eq!(get_pattern_category("canvas-2d"), Some("game"));
        assert_eq!(get_pattern_category("nonexistent"), None);
    }

    #[test]
    fn test_item_action_props() {
        assert!(is_valid_item_action_prop("label"));
        assert!(is_valid_item_action_prop("event"));
        assert!(!is_valid_item_action_prop("condition"));
    }

    #[test]
    fn test_all_patterns_have_category() {
        for pattern in PATTERN_TYPES.iter() {
            assert!(
                PATTERN_TO_CATEGORY.contains_key(pattern),
                "Pattern '{}' missing from PATTERN_TO_CATEGORY",
                pattern
            );
        }
    }

    #[test]
    fn test_all_categorized_patterns_are_valid() {
        for (pattern, _) in PATTERN_TO_CATEGORY.iter() {
            assert!(
                PATTERN_TYPES.contains(pattern),
                "Pattern '{}' in PATTERN_TO_CATEGORY but not in PATTERN_TYPES",
                pattern
            );
        }
    }
}
