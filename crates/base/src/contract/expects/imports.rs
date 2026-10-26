//! What an orbital imports and contributes: imported traits, contributed entities, and the page a navigate reaches.

use super::*;

pub(super) fn name_of(v: &Value) -> Option<&str> {
    v.get("name").and_then(Value::as_str)
}

pub(super) fn orbitals(schema: &Value) -> &[Value] {
    schema
        .get("orbitals")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub(super) fn behavior_name_of(from: &str) -> String {
    let last = from.rsplit('/').next().unwrap_or(from);
    last.strip_suffix(".orb").unwrap_or(last).to_string()
}

pub(super) fn is_inline_trait(t: &Value) -> bool {
    t.as_object()
        .is_some_and(|o| o.contains_key("name") && !o.contains_key("ref"))
}

/// `Alias.traits.Trait` → (`Alias`, `Trait`).
pub(super) fn parse_imported_trait_ref(r: &str) -> Option<(&str, &str)> {
    let mut parts = r.split('.');
    let (alias, kw, name) = (parts.next()?, parts.next()?, parts.next()?);
    let pascal = |s: &str| {
        s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && s.chars().all(|c| c.is_ascii_alphanumeric())
    };
    (parts.next().is_none() && kw == "traits" && pascal(alias) && pascal(name))
        .then_some((alias, name))
}

/// Entities an imported behavior contributes when one of its traits is composed (the compiler's
/// Gap #22 synthesis): the trait's own entity unless the call site rebinds, and the owning
/// orbital's auxiliaries always.
pub(super) fn contributed_entities_of(
    behavior: &Value,
    trait_name: &str,
) -> Option<(Option<String>, Vec<String>)> {
    for o in orbitals(behavior) {
        for t in o
            .get("traits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !is_inline_trait(t) || name_of(t) != Some(trait_name) {
                continue;
            }
            let primary = o.get("entity").and_then(as_entity).and_then(name_of);
            let bound = t
                .get("linkedEntity")
                .and_then(Value::as_str)
                .or(primary)
                .map(str::to_string);
            let aux = o
                .get("auxiliaryEntities")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(as_entity)
                .filter_map(name_of)
                .map(str::to_string)
                .collect();
            return Some((bound, aux));
        }
    }
    None
}

/// The `[identity]` roster a reference-form orbital brings in, named as the import renames it.
pub(super) fn imported_identity_of(
    o: &Value,
    load: BehaviorLoader<'_>,
    seen: &mut HashSet<String>,
) -> Option<Value> {
    let reference = o.get("reference")?;
    let r = reference.get("ref").and_then(Value::as_str)?;
    let mut parts = r.split('.');
    let alias = parts.next()?;
    let upstream_name = parts.nth(1)?;
    let from = o
        .get("uses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|u| u.get("as").and_then(Value::as_str) == Some(alias))?
        .get("from")?
        .as_str()?;
    if !seen.insert(format!("{}.{upstream_name}", behavior_name_of(from))) {
        return None;
    }
    let loaded = load(from)?;
    let upstream = orbitals(&loaded)
        .iter()
        .find(|u| name_of(u) == Some(upstream_name))?
        .clone();
    let def = match upstream.get("entity").and_then(as_entity) {
        Some(primary) => {
            (primary.get("identity") == Some(&Value::Bool(true))).then(|| primary.clone())
        }
        None => imported_identity_of(&upstream, load, seen),
    }?;
    match reference.get("entity").and_then(Value::as_str) {
        Some(rename) => {
            let mut renamed = def;
            renamed["name"] = Value::from(rename);
            Some(renamed)
        }
        None => Some(def),
    }
}

/// The page a navigate target reaches, deterministically: exact, then the one parameterised
/// route a trailing-slash prefix adds a segment to, then a `:param` match, then a page beneath
/// the prefix.
pub(super) fn find_matching_page_path(nav_path: &str, page_paths: &BTreeSet<String>) -> Option<String> {
    if page_paths.contains(nav_path) {
        return Some(nav_path.to_string());
    }
    if nav_path.ends_with('/') {
        let prefix: Vec<&str> = nav_path.split('/').filter(|s| !s.is_empty()).collect();
        let found = page_paths.iter().find(|p| {
            let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
            parts.len() == prefix.len() + 1
                && parts.last().is_some_and(|s| s.starts_with(':'))
                && prefix.iter().zip(&parts).all(|(a, b)| a == b)
        });
        if let Some(p) = found {
            return Some(p.clone());
        }
    }
    page_paths
        .iter()
        .find(|p| path_matches_pattern(nav_path, p))
        .or_else(|| {
            page_paths
                .iter()
                .find(|p| p.starts_with(nav_path.trim_end_matches('/')))
        })
        .cloned()
}

pub(super) fn navigate_target_of(node: &Value) -> Option<String> {
    let arr = node.as_array()?;
    if arr.len() < 2 || arr[0] != "navigate" {
        return None;
    }
    match &arr[1] {
        Value::String(s) => (!s.starts_with('@')).then(|| s.clone()),
        other => {
            extract_static_path_prefix(&serde_json::from_value::<SExpression>(other.clone()).ok()?)
        }
    }
}

pub(super) fn walk(node: &Value, visit: &mut dyn FnMut(&Value)) {
    visit(node);
    match node {
        Value::Array(items) => items.iter().for_each(|c| walk(c, visit)),
        Value::Object(map) => map.values().for_each(|c| walk(c, visit)),
        _ => {}
    }
}

pub(super) fn collect_persist_payload_keys(data: &Value, out: &mut BTreeSet<String>) {
    match data {
        Value::Array(items) => items
            .iter()
            .for_each(|c| collect_persist_payload_keys(c, out)),
        Value::Object(map) => out.extend(map.keys().cloned()),
        _ => {}
    }
}

pub(super) fn plain_string(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.starts_with('@'))
}
