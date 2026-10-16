//! Behaviors as first-class values: the Rust twin of `@almadar/core`
//! `types/behavior-value.ts`.
//!
//! A trait value is the object `{"behavior": "<spec>", "trait": "<Pascal>", ...overrides}`;
//! its literal spelling is `<spec>.traits.<Pascal>`. An orbital value is
//! `{"behavior": "<spec>", "orbital": "<Pascal>", ...§8b import body}`, spelled
//! `<spec>.orbitals.<Pascal>`. A local embed (`@trait.X`) is a different, string
//! form and never a behavior value.

use crate::value::ValueMap;
use crate::value::Value;

const TRAITS_SEGMENT: &str = ".traits.";
const ORBITALS_SEGMENT: &str = ".orbitals.";

/// Override keys on the TraitReference wire form that merge key-wise (later wins).
const MERGE_KEYS: [&str; 3] = ["config", "events", "fields"];
/// Override keys that replace wholesale.
const REPLACE_KEYS: [&str; 3] = ["listens", "emitsScope", "linkedEntity"];
/// §8b import-body keys that merge key-wise on an orbital value (later wins).
const ORBITAL_MAP_KEYS: [&str; 10] = [
    "app",
    "config",
    "events",
    "fields",
    "pages",
    "pageModifiers",
    "roles",
    "entities",
    "mounts",
    "mock",
];
/// §8b field lists merged by field name.
const ORBITAL_FIELD_LIST_KEYS: [&str; 2] = ["extend", "retype"];
/// §8b keys that replace wholesale (`name` is the orbital's name in the importer).
const ORBITAL_REPLACE_KEYS: [&str; 11] = [
    "name",
    "entity",
    "persistence",
    "collection",
    "only",
    "omit",
    "listens",
    "shared",
    "identity",
    "local",
    "seedMock",
];

/// The identity of a trait value, without overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitValueRef {
    pub behavior: String,
    pub trait_name: String,
}

fn is_name_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_pascal(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

fn is_path_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_relative_specifier(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("./") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    parts.len() >= 2
        && parts.iter().all(|p| is_path_segment(p))
        && parts
            .last()
            .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_alphanumeric()))
}

/// `<prefix>/<name>` (canonical), the `uses` std spelling `std/behaviors/<name>`,
/// or a workspace-relative `./<dir>/<name>` (no `../`, no absolute paths).
pub fn is_behavior_specifier(s: &str) -> bool {
    if is_relative_specifier(s) {
        return true;
    }
    let parts: Vec<&str> = s.split('/').collect();
    match parts.as_slice() {
        [prefix, name] => is_name_segment(prefix) && is_name_segment(name),
        [prefix, "behaviors", name] => is_name_segment(prefix) && is_name_segment(name),
        _ => false,
    }
}

impl TraitValueRef {
    /// Parse `<prefix>/<name>.traits.<Trait>`; `None` for anything else (a local `@trait.X` included).
    pub fn parse(raw: &str) -> Option<Self> {
        let at = raw.rfind(TRAITS_SEGMENT)?;
        if at == 0 {
            return None;
        }
        let behavior = &raw[..at];
        let trait_name = &raw[at + TRAITS_SEGMENT.len()..];
        (is_behavior_specifier(behavior) && is_pascal(trait_name)).then(|| Self {
            behavior: behavior.to_string(),
            trait_name: trait_name.to_string(),
        })
    }

    /// Inverse of [`TraitValueRef::parse`].
    pub fn format(&self) -> String {
        format!("{}{}{}", self.behavior, TRAITS_SEGMENT, self.trait_name)
    }

    /// The wire object `{"behavior", "trait"}`.
    pub fn to_value(&self) -> Value {
        Value::Object(ValueMap::from([
            ("behavior".to_string(), Value::String(self.behavior.clone())),
            ("trait".to_string(), Value::String(self.trait_name.clone())),
        ]))
    }

    /// The identity of `value` when it is a trait value.
    pub fn from_value(value: &Value) -> Option<Self> {
        let Value::Object(map) = value else {
            return None;
        };
        let (Some(Value::String(behavior)), Some(Value::String(trait_name))) =
            (map.get("behavior"), map.get("trait"))
        else {
            return None;
        };
        (is_behavior_specifier(behavior) && is_pascal(trait_name)).then(|| Self {
            behavior: behavior.clone(),
            trait_name: trait_name.clone(),
        })
    }
}

/// Parse a trait-value literal straight to its wire object.
pub fn parse_trait_value_literal(raw: &str) -> Option<Value> {
    TraitValueRef::parse(raw).map(|r| r.to_value())
}

/// Structural guard for a trait value carried as data.
pub fn is_trait_value(value: &Value) -> bool {
    TraitValueRef::from_value(value).is_some()
}

fn merge_object_key(out: &mut ValueMap, key: &str, new: &Value) {
    match (new, out.get(key)) {
        (Value::Object(new), Some(Value::Object(old))) => {
            let mut merged = old.clone();
            merged.extend(new.iter().map(|(k, v)| (k.clone(), v.clone())));
            out.insert(key.to_string(), Value::Object(merged));
        }
        _ => {
            out.insert(key.to_string(), new.clone());
        }
    }
}

fn merge_trait_overrides(
    base: &ValueMap,
    over: &ValueMap,
) -> Option<ValueMap> {
    if over
        .keys()
        .any(|k| !MERGE_KEYS.contains(&k.as_str()) && !REPLACE_KEYS.contains(&k.as_str()))
    {
        return None;
    }
    let mut out = base.clone();
    for key in MERGE_KEYS {
        if let Some(new) = over.get(key) {
            if !matches!(new, Value::Object(_)) {
                return None;
            }
            merge_object_key(&mut out, key, new);
        }
    }
    for key in REPLACE_KEYS {
        if let Some(new) = over.get(key) {
            out.insert(key.to_string(), new.clone());
        }
    }
    Some(out)
}

fn field_name(field: &Value) -> Option<&str> {
    match field {
        Value::Object(map) => match map.get("name") {
            Some(Value::String(name)) => Some(name.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn merge_fields_by_name(base: Option<&Value>, next: &[Value]) -> Vec<Value> {
    let mut out = match base {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    for field in next {
        let at =
            field_name(field).and_then(|name| out.iter().position(|f| field_name(f) == Some(name)));
        match at {
            Some(i) => out[i] = field.clone(),
            None => out.push(field.clone()),
        }
    }
    out
}

/// `behavior/apply` on a trait value: `config`, `events` and `fields` merge
/// key-wise with the later value winning; `listens`, `emitsScope` and
/// `linkedEntity` replace. Pure; `None` when `value` is not a trait value or
/// `overrides` is not an object of override keys (an unknown key is refused).
pub fn apply_trait_overrides(value: &Value, overrides: &Value) -> Option<Value> {
    if !is_trait_value(value) {
        return None;
    }
    let (Value::Object(base), Value::Object(over)) = (value, overrides) else {
        return None;
    };
    merge_trait_overrides(base, over).map(Value::Object)
}

/// `behavior/apply` on an orbital value — the §8b merge table: the map keys
/// merge key-wise, `traits` per trait with the trait rules, `extend`/`retype`
/// by field name, the rest replaces. `None` on a non-orbital value, non-object
/// overrides, or an unknown key.
pub fn apply_orbital_overrides(value: &Value, overrides: &Value) -> Option<Value> {
    if !is_orbital_value(value) {
        return None;
    }
    let (Value::Object(base), Value::Object(over)) = (value, overrides) else {
        return None;
    };
    let known = |k: &str| {
        k == "traits"
            || ORBITAL_MAP_KEYS.contains(&k)
            || ORBITAL_FIELD_LIST_KEYS.contains(&k)
            || ORBITAL_REPLACE_KEYS.contains(&k)
    };
    if over.keys().any(|k| !known(k.as_str())) {
        return None;
    }
    let mut out = base.clone();
    for key in ORBITAL_MAP_KEYS {
        if let Some(new) = over.get(key) {
            if !matches!(new, Value::Object(_)) {
                return None;
            }
            merge_object_key(&mut out, key, new);
        }
    }
    for key in ORBITAL_FIELD_LIST_KEYS {
        if let Some(new) = over.get(key) {
            let Value::Array(next) = new else {
                return None;
            };
            out.insert(
                key.to_string(),
                Value::Array(merge_fields_by_name(base.get(key), next)),
            );
        }
    }
    for key in ORBITAL_REPLACE_KEYS {
        if let Some(new) = over.get(key) {
            out.insert(key.to_string(), new.clone());
        }
    }
    if let Some(new) = over.get("traits") {
        let Value::Object(new_traits) = new else {
            return None;
        };
        let mut traits = match base.get("traits") {
            Some(Value::Object(old)) => old.clone(),
            _ => ValueMap::new(),
        };
        for (name, entry) in new_traits {
            let Value::Object(entry) = entry else {
                return None;
            };
            let prior = match traits.get(name) {
                Some(Value::Object(old)) => old.clone(),
                _ => ValueMap::new(),
            };
            traits.insert(
                name.clone(),
                Value::Object(merge_trait_overrides(&prior, entry)?),
            );
        }
        out.insert("traits".to_string(), Value::Object(traits));
    }
    Some(Value::Object(out))
}

/// `behavior/apply` for every kind: a trait value, an orbital value or a whole behavior value.
pub fn apply_behavior_overrides(value: &Value, overrides: &Value) -> Option<Value> {
    if is_behavior_ref_value(value) {
        apply_behavior_ref_overrides(value, overrides)
    } else if is_orbital_value(value) {
        apply_orbital_overrides(value, overrides)
    } else {
        apply_trait_overrides(value, overrides)
    }
}

/// The identity of an orbital value, without its recorded body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbitalValueRef {
    pub behavior: String,
    pub orbital: String,
}

impl OrbitalValueRef {
    /// Parse `<prefix>/<name>.orbitals.<Orbital>`; `None` for anything else.
    pub fn parse(raw: &str) -> Option<Self> {
        let at = raw.rfind(ORBITALS_SEGMENT)?;
        if at == 0 {
            return None;
        }
        let behavior = &raw[..at];
        let orbital = &raw[at + ORBITALS_SEGMENT.len()..];
        (is_behavior_specifier(behavior) && is_pascal(orbital)).then(|| Self {
            behavior: behavior.to_string(),
            orbital: orbital.to_string(),
        })
    }

    /// Inverse of [`OrbitalValueRef::parse`].
    pub fn format(&self) -> String {
        format!("{}{}{}", self.behavior, ORBITALS_SEGMENT, self.orbital)
    }

    /// The wire object `{"behavior", "orbital"}`.
    pub fn to_value(&self) -> Value {
        Value::Object(ValueMap::from([
            ("behavior".to_string(), Value::String(self.behavior.clone())),
            ("orbital".to_string(), Value::String(self.orbital.clone())),
        ]))
    }

    /// The identity of `value` when it is an orbital value.
    pub fn from_value(value: &Value) -> Option<Self> {
        let Value::Object(map) = value else {
            return None;
        };
        let (Some(Value::String(behavior)), Some(Value::String(orbital))) =
            (map.get("behavior"), map.get("orbital"))
        else {
            return None;
        };
        (is_behavior_specifier(behavior) && is_pascal(orbital)).then(|| Self {
            behavior: behavior.clone(),
            orbital: orbital.clone(),
        })
    }
}

/// Parse an orbital-value literal straight to its wire object.
pub fn parse_orbital_value_literal(raw: &str) -> Option<Value> {
    OrbitalValueRef::parse(raw).map(|r| r.to_value())
}

/// Structural guard for an orbital value carried as data.
pub fn is_orbital_value(value: &Value) -> bool {
    OrbitalValueRef::from_value(value).is_some()
}

/// A whole behavior as a value (`.lolo` type `behavior`): `{"behavior"}` for a
/// valid specifier, `None` otherwise. Twin of `@almadar/core` `behaviorRef`.
pub fn behavior_ref(specifier: &str) -> Option<Value> {
    is_behavior_specifier(specifier).then(|| {
        Value::Object(ValueMap::from([(
            "behavior".to_string(),
            Value::String(specifier.to_string()),
        )]))
    })
}

/// Structural guard for a behavior value: a specifier and, optionally, its
/// declared `bridge` (a trait or orbital value also names a member).
pub fn is_behavior_ref_value(value: &Value) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    let spec_ok =
        matches!(map.get("behavior"), Some(Value::String(spec)) if is_behavior_specifier(spec));
    match map.len() {
        1 => spec_ok,
        2 => spec_ok && map.get("bridge").is_some_and(is_bridge),
        _ => false,
    }
}

/// A behavior value's bridge: `{ listens: [event], emits: [event] }`, nothing else.
fn is_bridge(value: &Value) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    let events = |key: &str| matches!(map.get(key), Some(Value::Array(items)) if items.iter().all(|e| matches!(e, Value::String(s) if !s.is_empty())));
    map.len() == 2 && events("listens") && events("emits")
}

/// `behavior/apply` on a whole behavior value: records (replaces) its bridge.
/// Twin of `@almadar/core` `applyBehaviorRefOverrides`.
pub fn apply_behavior_ref_overrides(value: &Value, overrides: &Value) -> Option<Value> {
    let Value::Object(value_map) = value else {
        return None;
    };
    let Value::Object(over) = overrides else {
        return None;
    };
    let bridge = over.get("bridge")?;
    if over.len() != 1 || !is_bridge(bridge) {
        return None;
    }
    Some(Value::Object(ValueMap::from([
        ("behavior".to_string(), value_map.get("behavior")?.clone()),
        ("bridge".to_string(), bridge.clone()),
    ])))
}

/// Parse either literal form.
pub fn parse_behavior_value_literal(raw: &str) -> Option<Value> {
    parse_trait_value_literal(raw).or_else(|| parse_orbital_value_literal(raw))
}
