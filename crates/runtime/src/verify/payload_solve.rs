//! Payload solving for guards the symbolic solver cannot reach: payload fields the arm reads through
//! `(json/parse @payload.f Type)`. Each is planned as the JSON text of a minimal instance of the program's
//! declared `Type`, then steered by trying member changes until the arm's guard, run by the real evaluator,
//! answers as wanted.

use crate::planner::{guard_passes, synth_field, EntityCtx};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::schema::types::{OrbitalSchema, PayloadField, TransitionDefinition};
use std::collections::BTreeMap;

pub type DeclaredTypes = BTreeMap<String, Vec<PayloadField>>;

/// The program's type declarations by name, across every orbital.
pub fn declared_types(schema: &OrbitalSchema) -> DeclaredTypes {
    let mut out = DeclaredTypes::new();
    for orbital in &schema.orbitals {
        if let Some(types) = &orbital.types {
            out.extend(types.iter().map(|(name, fields)| (name.clone(), fields.clone())));
        }
    }
    out
}

#[derive(Default)]
struct Sites {
    /// Payload fields read through `(json/parse @payload.<f> <Type>)`, by declared type name.
    fields: BTreeMap<String, String>,
    /// Members of a bound value read through `(json/parse @<name>.<member> <Type>)`.
    members: BTreeMap<String, String>,
}

fn binding_member(text: &str) -> Option<(&str, &str)> {
    let (name, member) = text.strip_prefix('@')?.split_once('.')?;
    let identifier = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (identifier(name) && identifier(member)).then_some((name, member))
}

fn collect_sites(node: &SExpression, types: &DeclaredTypes, out: &mut Sites) {
    let SExpression::Array(items) = node else { return };
    if let [SExpression::String(op), SExpression::String(path), SExpression::String(type_name)] = &items[..] {
        if op == "json/parse" && types.contains_key(type_name) {
            if let Some((name, member)) = binding_member(path) {
                let target = if name == "payload" { &mut out.fields } else { &mut out.members };
                target.insert(member.to_string(), type_name.clone());
            }
        }
    }
    items.iter().for_each(|child| collect_sites(child, types, out));
}

fn sites_of(transition: &TransitionDefinition, types: &DeclaredTypes) -> Sites {
    let mut out = Sites::default();
    if let Some(guard) = &transition.guard {
        collect_sites(guard, types, &mut out);
    }
    transition.effects.iter().flatten().for_each(|e| collect_sites(e, types, &mut out));
    out
}

fn minimal_instance(
    type_name: &str,
    types: &DeclaredTypes,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut obj = serde_json::Map::new();
    for f in types.get(type_name).into_iter().flatten() {
        let element = f.field_type.strip_prefix('[').and_then(|t| t.strip_suffix(']'));
        let value = match element {
            Some(e) if types.contains_key(e) => {
                serde_json::Value::Array(vec![serde_json::Value::Object(minimal_instance(e, types, ctx, seed_id))])
            }
            _ => synth_field(f, seed_id, ctx),
        };
        obj.insert(f.name.clone(), value);
    }
    obj
}

/// The payload fields the arm parses as JSON, each the JSON text of a minimal instance of its declared type:
/// random text would parse to the type's defaults and hide what the arm does with a real value.
pub fn json_parse_payload(
    transition: &TransitionDefinition,
    types: &DeclaredTypes,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> BTreeMap<String, serde_json::Value> {
    let sites = sites_of(transition, types);
    let mut out = BTreeMap::new();
    for (field, type_name) in &sites.fields {
        let mut instance = minimal_instance(type_name, types, ctx, seed_id);
        for (member, member_type) in &sites.members {
            if matches!(instance.get(member), Some(serde_json::Value::String(_))) {
                let nested = serde_json::Value::Object(minimal_instance(member_type, types, ctx, seed_id));
                instance.insert(member.clone(), serde_json::Value::String(nested.to_string()));
            }
        }
        out.insert(field.clone(), serde_json::Value::String(serde_json::Value::Object(instance).to_string()));
    }
    out
}

fn literals_of(node: &SExpression, out: &mut Vec<serde_json::Value>) {
    match node {
        SExpression::String(s) if !s.starts_with('@') && !s.starts_with('?') => out.push(serde_json::Value::from(s.clone())),
        SExpression::Number(n) => out.push(if n.fract() == 0.0 && n.abs() < 9.0e15 {
            serde_json::Value::from(*n as i64)
        } else {
            serde_json::Number::from_f64(*n).map_or(serde_json::Value::Null, serde_json::Value::Number)
        }),
        SExpression::Array(items) => items.iter().for_each(|i| literals_of(i, out)),
        _ => {}
    }
}

#[derive(Clone)]
struct Change {
    field: String,
    member: String,
    value: serde_json::Value,
}

/// Steer the JSON instances so the arm's guard answers `want_pass`: the minimal instance first, then one and
/// two member changes drawn from the guard's own literals, a boolean's other value and an empty or
/// one-element array. The payload unchanged when none answers.
pub fn solve_json_parse_fields(
    transition: &TransitionDefinition,
    want_pass: bool,
    payload: &serde_json::Value,
    types: &DeclaredTypes,
    from: &str,
    ctx: &EntityCtx<'_>,
    seed_id: Option<&str>,
) -> serde_json::Value {
    let Some(guard) = &transition.guard else { return payload.clone() };
    let sites = sites_of(transition, types);
    if sites.fields.is_empty() {
        return payload.clone();
    }
    let mut instances: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for field in sites.fields.keys() {
        let Some(raw) = payload.get(field).and_then(serde_json::Value::as_str) else { return payload.clone() };
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(serde_json::Value::Object(map)) => instances.insert(field.clone(), map),
            _ => return payload.clone(),
        };
    }
    let mut literals = Vec::new();
    literals_of(guard, &mut literals);
    let with = |candidate: &BTreeMap<String, serde_json::Map<String, serde_json::Value>>| {
        let mut next = payload.clone();
        for (field, instance) in candidate {
            next[field] = serde_json::Value::String(serde_json::Value::Object(instance.clone()).to_string());
        }
        next
    };
    let answers = |candidate: &BTreeMap<String, serde_json::Map<String, serde_json::Value>>| {
        guard_passes(guard, &with(candidate), from) == Ok(want_pass)
    };
    let element_of = |type_name: &str, member: &str| -> Option<serde_json::Value> {
        let declared = types.get(type_name)?.iter().find(|f| f.name == member)?;
        let element = declared.field_type.strip_prefix('[')?.strip_suffix(']')?;
        types.contains_key(element).then(|| serde_json::Value::Object(minimal_instance(element, types, ctx, seed_id)))
    };
    let mut changes: Vec<Change> = Vec::new();
    for (field, instance) in &instances {
        for (member, current) in instance {
            let mut values: Vec<serde_json::Value> = Vec::new();
            match current {
                serde_json::Value::Bool(b) => values.push(serde_json::Value::Bool(!b)),
                serde_json::Value::Array(_) => {
                    values.push(serde_json::json!([]));
                    if let Some(element) = element_of(sites.fields.get(field).map_or("", String::as_str), member) {
                        values.push(serde_json::Value::Array(vec![element]));
                    }
                }
                other => {
                    for literal in &literals {
                        if std::mem::discriminant(literal) == std::mem::discriminant(other) {
                            values.push(literal.clone());
                        }
                    }
                    if other.is_number() {
                        values.push(serde_json::json!(0));
                    }
                }
            }
            for value in values {
                if &value != current {
                    changes.push(Change { field: field.clone(), member: member.clone(), value });
                }
            }
        }
    }
    let apply = |picked: &[&Change]| {
        let mut copy = instances.clone();
        for change in picked {
            if let Some(instance) = copy.get_mut(&change.field) {
                instance.insert(change.member.clone(), change.value.clone());
            }
        }
        copy
    };
    let mut attempts: Vec<Vec<&Change>> = vec![Vec::new()];
    attempts.extend(changes.iter().map(|c| vec![c]));
    for (i, a) in changes.iter().enumerate() {
        for b in &changes[i + 1..] {
            if a.field != b.field || a.member != b.member {
                attempts.push(vec![a, b]);
            }
        }
    }
    for picked in attempts {
        let candidate = apply(&picked);
        if answers(&candidate) {
            return with(&candidate);
        }
    }
    payload.clone()
}

/// Whether the arm reads any payload field through `json/parse`.
pub fn has_json_sites(transition: &TransitionDefinition, types: &DeclaredTypes) -> bool {
    !sites_of(transition, types).fields.is_empty()
}
