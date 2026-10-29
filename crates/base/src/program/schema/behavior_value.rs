//! Behaviors as first-class values: the Rust twin of `@almadar/core`
//! `types/behavior-value.ts`.
//!
//! A trait value is the object `{"behavior": "<spec>", "trait": "<Pascal>", ...overrides}`;
//! its literal spelling is `<spec>.traits.<Pascal>`. An orbital value is
//! `{"behavior": "<spec>", "orbital": "<Pascal>", ...§8b import body}`, spelled
//! `<spec>.orbitals.<Pascal>`. A local embed (`@trait.X`) is a different, string
//! form and never a behavior value.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::Value;
use crate::value::ValueMap;

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
const ORBITAL_REPLACE_KEYS: [&str; 10] = [
    "name",
    "entity",
    "persistence",
    "collection",
    "only",
    "omit",
    "listens",
    "shared",
    "identity",
    "browser",
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
        Value::object(ValueMap::from_iter([
            ("behavior".to_string(), Value::String(self.behavior.clone().into())),
            ("trait".to_string(), Value::String(self.trait_name.clone().into())),
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
            behavior: behavior.clone().into_inner(),
            trait_name: trait_name.clone().into_inner(),
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

fn merge_trait_overrides(base: &ValueMap, over: &ValueMap) -> Option<ValueMap> {
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
        _ => Vec::new().into(),
    };
    for field in next {
        let at =
            field_name(field).and_then(|name| out.iter().position(|f| field_name(f) == Some(name)));
        match at {
            Some(i) => out[i] = field.clone(),
            None => out.push(field.clone()),
        }
    }
    out.into_inner()
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
    merge_trait_overrides(base, over).map(Value::object)
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
                Value::array(merge_fields_by_name(base.get(key), next)),
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
            _ => ValueMap::default().into(),
        };
        for (name, entry) in new_traits {
            let Value::Object(entry) = entry else {
                return None;
            };
            let prior = match traits.get(name) {
                Some(Value::Object(old)) => old.clone(),
                _ => ValueMap::default().into(),
            };
            traits.insert(
                name.clone(),
                Value::object(merge_trait_overrides(&prior, entry)?),
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
        Value::object(ValueMap::from_iter([
            ("behavior".to_string(), Value::String(self.behavior.clone().into())),
            ("orbital".to_string(), Value::String(self.orbital.clone().into())),
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
            behavior: behavior.clone().into_inner(),
            orbital: orbital.clone().into_inner(),
        })
    }
}

/// The alias an orbital value's import names its source behavior by (`Source.orbitals.<Orbital>`).
pub const ORBITAL_VALUE_IMPORT_ALIAS: &str = "Source";

fn json_is_call_site_declaration(entry: &serde_json::Value) -> bool {
    entry.as_object().is_some_and(|o| {
        o.get("type").is_some_and(serde_json::Value::is_string) && o.contains_key("default")
    })
}

/// Call-site config values as declarations: a declaration stays, any other value becomes `{ type: "unknown", default }`.
fn json_config_declarations(config: &serde_json::Value) -> serde_json::Value {
    serde_json::Value::Object(
        config
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                let declared = if json_is_call_site_declaration(v) {
                    v.clone()
                } else {
                    serde_json::json!({ "type": "unknown", "default": v })
                };
                (k.clone(), declared)
            })
            .collect(),
    )
}

/// Structural guard for an orbital value held as JSON: a behavior specifier and a PascalCase orbital.
pub fn is_json_orbital_value(value: &serde_json::Value) -> bool {
    let (Some(behavior), Some(orbital)) = (
        value.get("behavior").and_then(serde_json::Value::as_str),
        value.get("orbital").and_then(serde_json::Value::as_str),
    ) else {
        return false;
    };
    value.is_object() && is_behavior_specifier(behavior) && is_pascal(orbital)
}

/// The import orbital an orbital value stands for (LOLO §8b): its source behavior used as `Source`, the value's
/// overrides as the import body, its siblings as the orbital's own traits. Twin of `@almadar/core`
/// `orbitalImportFromValue`.
pub fn orbital_import_from_value(value: &serde_json::Value) -> Result<serde_json::Value, String> {
    use serde_json::{json, Map, Value as J};
    let o = value.as_object().ok_or("an orbital value is an object")?;
    let behavior = o
        .get("behavior")
        .and_then(J::as_str)
        .ok_or("an orbital value names its behavior")?;
    let orbital = o
        .get("orbital")
        .and_then(J::as_str)
        .ok_or("an orbital value names its orbital")?;
    let reference_ref = format!("{ORBITAL_VALUE_IMPORT_ALIAS}.orbitals.{orbital}");
    let uses: Vec<J> = o
        .get("uses")
        .and_then(J::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(clash) = uses
        .iter()
        .find(|u| u.get("as").and_then(J::as_str) == Some(ORBITAL_VALUE_IMPORT_ALIAS))
    {
        return Err(format!(
            "orbital value {orbital}: an import aliased '{ORBITAL_VALUE_IMPORT_ALIAS}' (from {}) collides with the value's own source alias",
            clash.get("from").and_then(J::as_str).unwrap_or("")
        ));
    }
    let mut source_use = json!({ "from": behavior, "as": ORBITAL_VALUE_IMPORT_ALIAS });
    if let Some(app) = o.get("app") {
        source_use["config"] = json_config_declarations(app);
    }
    let mut all_uses = vec![source_use];
    all_uses.extend(uses);
    let traits: Vec<J> = o
        .get("siblings")
        .and_then(J::as_array)
        .into_iter()
        .flatten()
        .map(|sibling| {
            let mut t = json!({ "name": sibling.get("name").cloned().unwrap_or(J::Null), "ref": sibling.get("ref").cloned().unwrap_or(J::Null) });
            if let Some(linked) = sibling.get("linkedEntity") {
                t["linkedEntity"] = linked.clone();
            }
            if let Some(config) = sibling.get("config") {
                t["config"] = json_config_declarations(config);
            }
            t
        })
        .collect();
    let mut reference = Map::new();
    reference.insert("ref".to_string(), J::String(reference_ref.clone()));
    for (k, v) in o {
        if !matches!(
            k.as_str(),
            "behavior" | "orbital" | "name" | "app" | "config" | "traits" | "uses" | "siblings"
        ) {
            reference.insert(k.clone(), v.clone());
        }
    }
    if let Some(config) = o.get("config") {
        reference.insert("config".to_string(), json_config_declarations(config));
    }
    if let Some(trait_overrides) = o.get("traits").and_then(J::as_object) {
        let overrides: Map<String, J> = trait_overrides
            .iter()
            .map(|(name, over)| {
                let mut next: Map<String, J> = over
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(k, _)| *k != "config")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if let Some(config) = over.get("config") {
                    next.insert("config".to_string(), json_config_declarations(config));
                }
                (name.clone(), J::Object(next))
            })
            .collect();
        reference.insert("traits".to_string(), J::Object(overrides));
    }
    Ok(json!({
        "name": o.get("name").and_then(J::as_str).unwrap_or(orbital),
        "uses": all_uses,
        "entity": format!("{reference_ref}.entity"),
        "traits": traits,
        "pages": [],
        "reference": J::Object(reference),
    }))
}

/// A program held as data with every orbital value at `orbitals[i]` replaced by the import orbital it stands for.
/// Twin of `@almadar/core` `parseProgram`'s value step.
pub fn program_with_orbital_imports(program: &Value) -> Result<Value, String> {
    into_program_with_orbital_imports(program.clone())
}

fn is_value_orbital_literal(value: &Value) -> bool {
    matches!(
        (value.get("behavior").and_then(Value::as_str), value.get("orbital").and_then(Value::as_str)),
        (Some(behavior), Some(orbital)) if value.is_object() && is_behavior_specifier(behavior) && is_pascal(orbital)
    )
}

/// [`program_with_orbital_imports`] for a program the caller hands over: converted in place, so no orbital is copied.
pub fn into_program_with_orbital_imports(mut program: Value) -> Result<Value, String> {
    let at: Vec<usize> = match program.get("orbitals").and_then(Value::as_array) {
        Some(orbitals) => orbitals
            .iter()
            .enumerate()
            .filter(|(_, entry)| is_value_orbital_literal(entry))
            .map(|(i, _)| i)
            .collect(),
        None => return Ok(program),
    };
    for i in at {
        let entry = crate::json_value::value_to_json(&program["orbitals"][i]);
        let converted = orbital_import_from_value(&entry)?;
        program["orbitals"][i] = serde_json::from_value(converted).map_err(|e| e.to_string())?;
    }
    Ok(program)
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
        Value::object(ValueMap::from_iter([(
            "behavior".to_string(),
            Value::String(specifier.to_string().into()),
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
    Some(Value::object(ValueMap::from_iter([
        ("behavior".to_string(), value_map.get("behavior")?.clone()),
        ("bridge".to_string(), bridge.clone()),
    ])))
}

/// Parse either literal form.
pub fn parse_behavior_value_literal(raw: &str) -> Option<Value> {
    parse_trait_value_literal(raw).or_else(|| parse_orbital_value_literal(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(json: serde_json::Value) -> Value {
        serde_json::from_value(json).expect("literal")
    }

    #[test]
    fn an_orbital_value_in_a_program_is_the_import_orbital_it_stands_for() {
        let program = val(serde_json::json!({ "name": "App", "orbitals": [
            { "behavior": "almadar-behaviors/std-crm", "orbital": "ContactOrbital", "name": "Contacts",
              "config": { "pageSize": 20 }, "app": { "appName": "Crm" },
              "traits": { "ContactList": { "config": { "dense": { "type": "boolean", "default": true } }, "events": { "OPEN": "VIEW" } } },
              "siblings": [{ "name": "Extra", "ref": "Ui.traits.Card", "linkedEntity": "Contact" }],
              "entities": { "Person": "Customer" } },
            { "name": "Plain", "entity": { "name": "Thing", "fields": [] }, "traits": [], "pages": [] }
        ] }));
        let out = program_with_orbital_imports(&program).unwrap();
        assert_eq!(
            out["orbitals"][0],
            val(serde_json::json!({
                "name": "Contacts",
                "uses": [{ "from": "almadar-behaviors/std-crm", "as": "Source", "config": { "appName": { "type": "unknown", "default": "Crm" } } }],
                "entity": "Source.orbitals.ContactOrbital.entity",
                "traits": [{ "name": "Extra", "ref": "Ui.traits.Card", "linkedEntity": "Contact" }],
                "pages": [],
                "reference": {
                    "ref": "Source.orbitals.ContactOrbital",
                    "entities": { "Person": "Customer" },
                    "config": { "pageSize": { "type": "unknown", "default": 20 } },
                    "traits": { "ContactList": { "events": { "OPEN": "VIEW" }, "config": { "dense": { "type": "boolean", "default": true } } } }
                }
            }))
        );
        assert_eq!(
            out["orbitals"][1], program["orbitals"][1],
            "control: an IR orbital is untouched"
        );
    }

    #[test]
    fn a_program_handed_over_converts_in_place_to_what_the_borrowed_form_returns() {
        let program = val(serde_json::json!({ "name": "App", "orbitals": [
            { "behavior": "almadar-behaviors/std-crm", "orbital": "ContactOrbital", "name": "Contacts" },
            { "name": "Plain", "entity": { "name": "Thing", "fields": [] }, "traits": [], "pages": [] }
        ] }));
        let borrowed = program_with_orbital_imports(&program).unwrap();
        assert_eq!(into_program_with_orbital_imports(program.clone()).unwrap(), borrowed);
        assert_ne!(borrowed["orbitals"][0], program["orbitals"][0], "the value became its import orbital");
        let no_orbitals = val(serde_json::json!({ "name": "App" }));
        assert_eq!(into_program_with_orbital_imports(no_orbitals.clone()).unwrap(), no_orbitals, "control: no orbitals list, nothing to convert");
    }

    #[test]
    fn a_program_holding_no_orbital_value_is_returned_unchanged_and_unshared_copies_stay_shared() {
        let program = val(serde_json::json!({ "name": "App", "orbitals": [{ "name": "Plain", "pages": [] }] }));
        let out = into_program_with_orbital_imports(program.clone()).unwrap();
        assert_eq!(out, program);
        let (Value::Object(a), Value::Object(b)) = (&program, &out) else { panic!("objects") };
        assert!(crate::value::Shared::ptr_eq(a, b), "nothing converted, nothing copied");
    }

    #[test]
    fn edge_a_refused_orbital_value_fails_the_whole_program_handed_over() {
        let program = val(serde_json::json!({ "name": "App", "orbitals": [
            { "behavior": "std/behaviors/std-x", "orbital": "XOrbital", "uses": [{ "from": "std/behaviors/std-y", "as": "Source" }] }
        ] }));
        assert!(into_program_with_orbital_imports(program).is_err());
    }

    #[test]
    fn edge_an_orbital_value_using_the_source_alias_itself_is_refused() {
        let value = serde_json::json!({ "behavior": "std/behaviors/std-x", "orbital": "XOrbital", "uses": [{ "from": "std/behaviors/std-y", "as": "Source" }] });
        let err = orbital_import_from_value(&value).unwrap_err();
        assert!(
            err.contains("collides with the value's own source alias"),
            "{err}"
        );
        assert!(
            !is_json_orbital_value(
                &serde_json::json!({ "behavior": "not a spec", "orbital": "X" })
            ),
            "control: a non-specifier is no value"
        );
    }
}
