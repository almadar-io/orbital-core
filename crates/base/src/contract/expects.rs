//! `expects` derivation (docs/Almadar_LOLO_Expects_Proposal.md §7): an orbital's consumer-side
//! requirement declarations computed from the full program it sits in, never authored. It walks
//! the program as its `.orb` JSON (policy keys, `ref` traits and `reference` imports as written):
//! `@user.<field>` reads (→ `expects entity [identity]`), relation fields, `persist`/`fetch` targets,
//! cross-orbital `linkedEntity` and import remap targets (→ `expects entity`), `navigate`
//! targets a sibling serves (→ `expects page`), and `listens` on a sibling orbital's trait
//! (→ `expects trait`). Shapes copy the provider's declared fields verbatim.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::ir::expr::{OirBinding, OirBindingRoot};
use crate::route::{extract_static_path_prefix, path_matches_pattern};
use crate::schema::sexpr::SExpression;
use crate::schema::types::ExpectDeclaration;

/// A cross-boundary usage derivation could not turn into a declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpectationDiagnostic {
    pub kind: DiagnosticKind,
    pub orbital: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticKind {
    UnknownOrbital,
    IdentityFieldNotDeclared,
    EntityFieldNotDeclared,
    /// A `listens` names an orbital the program does not have; `field` is `Orbital.Trait.EVENT`.
    EventSourceUnknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct DerivedExpectations {
    pub expectations: Vec<ExpectDeclaration>,
    pub diagnostics: Vec<ExpectationDiagnostic>,
}

/// Resolves a behavior by the `uses` path that imports it (`std/behaviors/std-mod-queue`).
pub type BehaviorLoader<'a> = &'a dyn Fn(&str) -> Option<Value>;

fn mismatch(field: &str) -> String {
    format!("expectation shape mismatch — field \"{field}\" has incompatible type constraints")
}

fn base_type(field: &Value) -> Option<&str> {
    field
        .get("type")
        .and_then(Value::as_str)
        .map(|t| if t == "enum" { "string" } else { t })
}

fn relation_core(relation: &Value) -> Value {
    let mut out = Map::new();
    for key in ["entity", "cardinality", "field", "onDelete"] {
        if let Some(v) = relation.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

fn sorted_keys(v: Option<&Value>) -> Vec<String> {
    let mut keys: Vec<String> = v
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn merge_field(prior: &Value, field: &Value) -> Result<Value, String> {
    let name = field
        .get("name")
        .or_else(|| prior.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if base_type(prior) != base_type(field) {
        return Err(mismatch(name));
    }
    let is_relation = prior.get("type") == Some(&Value::from("relation"))
        && field.get("type") == Some(&Value::from("relation"));
    if is_relation {
        let (p, f) = (&prior["relation"], &field["relation"]);
        if let (Some(a), Some(b)) = (p.get("entityId"), f.get("entityId")) {
            if a != b {
                return Err(mismatch(name));
            }
        }
        if relation_core(p) != relation_core(f) {
            return Err(mismatch(name));
        }
    }
    if prior.get("type") == Some(&Value::from("tuple"))
        && field.get("type") == Some(&Value::from("tuple"))
        && sorted_keys(prior.get("properties")) != sorted_keys(field.get("properties"))
    {
        return Err(mismatch(name));
    }
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
    let min = match (num(prior, "min"), num(field, "min")) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => Some(a.max(b)),
    };
    let max = match (num(prior, "max"), num(field, "max")) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => Some(a.min(b)),
    };
    if let (Some(lo), Some(hi)) = (min, max) {
        if lo > hi {
            return Err(mismatch(name));
        }
    }
    let mut merged = prior.as_object().cloned().unwrap_or_default();
    if prior.get("required") == Some(&Value::Bool(true))
        || field.get("required") == Some(&Value::Bool(true))
    {
        merged.insert("required".into(), Value::Bool(true));
    }
    let nullable = |v: &Value| v.get("nullable").and_then(Value::as_bool);
    if nullable(prior).is_some() || nullable(field).is_some() {
        merged.insert(
            "nullable".into(),
            Value::Bool(nullable(prior).unwrap_or(true) && nullable(field).unwrap_or(true)),
        );
    }
    let as_number = |n: f64| {
        serde_json::Number::from_f64(n).map(|x| {
            if n.fract() == 0.0 {
                Value::from(n as i64)
            } else {
                Value::Number(x)
            }
        })
    };
    if let Some(v) = min.and_then(as_number) {
        merged.insert("min".into(), v);
    }
    if let Some(v) = max.and_then(as_number) {
        merged.insert("max".into(), v);
    }
    if is_relation && prior["relation"].get("entityId").is_none() {
        if let Some(id) = field["relation"].get("entityId") {
            let mut rel = prior["relation"].as_object().cloned().unwrap_or_default();
            rel.insert("entityId".into(), id.clone());
            merged.insert("relation".into(), Value::Object(rel));
        }
    }
    if prior.get("values").is_some() || field.get("values").is_some() {
        let mut values: Vec<Value> = Vec::new();
        for v in prior
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(
                field
                    .get("values")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
        {
            if !values.contains(v) {
                values.push(v.clone());
            }
        }
        if !values.is_empty() {
            merged.insert("values".into(), Value::Array(values));
        }
    }
    if prior.get("properties").is_some() || field.get("properties").is_some() {
        let mut properties = prior
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, value) in field
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let next = match properties.get(key) {
                None => value.clone(),
                Some(existing) => merge_field(existing, value)?,
            };
            properties.insert(key.clone(), next);
        }
        for (declared, constrained) in [(prior, field), (field, prior)] {
            if let Some(additional) = constrained.get("additionalProperties") {
                for key in declared
                    .get("properties")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .map(|(key, _)| key)
                {
                    if constrained
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|fixed| fixed.contains_key(key))
                    {
                        continue;
                    }
                    if let Some(fixed) = properties.get(key) {
                        properties.insert(key.clone(), merge_field(fixed, additional)?);
                    }
                }
            }
        }
        merged.insert("properties".into(), Value::Object(properties));
    }
    if prior.get("additionalProperties").is_some() || field.get("additionalProperties").is_some() {
        let additional = match (
            prior.get("additionalProperties"),
            field.get("additionalProperties"),
        ) {
            (Some(left), Some(right)) => merge_field(left, right)?,
            (Some(value), None) | (None, Some(value)) => value.clone(),
            (None, None) => unreachable!(),
        };
        merged.insert("additionalProperties".into(), additional);
    }
    if prior.get("items").is_some() || field.get("items").is_some() {
        let items = match (prior.get("items"), field.get("items")) {
            (None, Some(b)) => b.clone(),
            (Some(a), None) => a.clone(),
            (Some(a), Some(b)) => merge_field(a, b)?,
            (None, None) => Value::Null,
        };
        merged.insert("items".into(), items);
    }
    Ok(Value::Object(merged))
}

/// Union two expectation shapes field by field: requiredness and value lists widen, ranges
/// narrow, nested properties and items merge; incompatible constraints are an error. The
/// derivation-side twin of `@almadar/core` `mergeExpectationShape` (the JS composer's).
pub fn merge_expectation_shape(
    existing: Option<&Value>,
    incoming: Option<&Value>,
) -> Result<Option<Value>, String> {
    if existing.is_none() && incoming.is_none() {
        return Ok(None);
    }
    let mut merged: Vec<Value> = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for field in incoming.and_then(Value::as_array).into_iter().flatten() {
        match merged
            .iter()
            .position(|c| c.get("name") == field.get("name"))
        {
            None => merged.push(field.clone()),
            Some(i) => merged[i] = merge_field(&merged[i], field)?,
        }
    }
    Ok(Some(Value::Array(merged)))
}

fn as_entity(v: &Value) -> Option<&Value> {
    v.as_object()
        .filter(|o| o.contains_key("fields"))
        .map(|_| v)
}

fn inline_entities(o: &Value) -> Vec<&Value> {
    let mut out: Vec<&Value> = o.get("entity").and_then(as_entity).into_iter().collect();
    out.extend(
        o.get("auxiliaryEntities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(as_entity),
    );
    out
}

fn name_of(v: &Value) -> Option<&str> {
    v.get("name").and_then(Value::as_str)
}

fn orbitals(schema: &Value) -> &[Value] {
    schema
        .get("orbitals")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn behavior_name_of(from: &str) -> String {
    let last = from.rsplit('/').next().unwrap_or(from);
    last.strip_suffix(".orb").unwrap_or(last).to_string()
}

fn is_inline_trait(t: &Value) -> bool {
    t.as_object()
        .is_some_and(|o| o.contains_key("name") && !o.contains_key("ref"))
}

/// `Alias.traits.Trait` → (`Alias`, `Trait`).
fn parse_imported_trait_ref(r: &str) -> Option<(&str, &str)> {
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
fn contributed_entities_of(
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
fn imported_identity_of(
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
fn find_matching_page_path(nav_path: &str, page_paths: &BTreeSet<String>) -> Option<String> {
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

fn navigate_target_of(node: &Value) -> Option<String> {
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

fn walk(node: &Value, visit: &mut dyn FnMut(&Value)) {
    visit(node);
    match node {
        Value::Array(items) => items.iter().for_each(|c| walk(c, visit)),
        Value::Object(map) => map.values().for_each(|c| walk(c, visit)),
        _ => {}
    }
}

fn collect_persist_payload_keys(data: &Value, out: &mut BTreeSet<String>) {
    match data {
        Value::Array(items) => items
            .iter()
            .for_each(|c| collect_persist_payload_keys(c, out)),
        Value::Object(map) => out.extend(map.keys().cloned()),
        _ => {}
    }
}

fn plain_string(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.starts_with('@'))
}

struct Walk<'a> {
    orbital_name: &'a str,
    own_defs: Vec<&'a Value>,
    own_names: HashSet<&'a str>,
    owner_by_entity: &'a HashMap<String, String>,
    owner_by_page: &'a BTreeMap<String, String>,
    page_paths: &'a BTreeSet<String>,
    user_fields: BTreeSet<String>,
    entity_refs: BTreeMap<String, BTreeSet<String>>,
    page_refs: BTreeSet<String>,
}

impl Walk<'_> {
    fn add_entity_ref(&mut self, name: &str, field: Option<&str>) {
        if self.own_names.contains(name) {
            return;
        }
        match self.owner_by_entity.get(name) {
            Some(owner) if owner != self.orbital_name => {}
            _ => return,
        }
        let fields = self.entity_refs.entry(name.to_string()).or_default();
        if let Some(f) = field {
            fields.insert(f.to_string());
        }
    }

    fn add_page_ref(&mut self, nav: &str) {
        let Some(matched) = find_matching_page_path(nav, self.page_paths) else {
            return;
        };
        if self.owner_by_page.get(&matched).map(String::as_str) == Some(self.orbital_name) {
            return;
        }
        self.page_refs.insert(matched);
    }

    fn visit(&mut self, node: &Value) {
        if let Value::String(s) = node {
            let Some(binding) = s.starts_with('@').then(|| OirBinding::parse(s)).flatten() else {
                return;
            };
            match &binding.root {
                OirBindingRoot::User => {
                    if let Some(first) = binding.path.first() {
                        self.user_fields.insert(first.clone());
                    }
                }
                OirBindingRoot::Entity if binding.path.len() >= 2 => {
                    let rel_name = binding.path[0].clone();
                    let targets: Vec<String> = self
                        .own_defs
                        .iter()
                        .flat_map(|d| {
                            d.get("fields")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                        })
                        .filter(|f| {
                            name_of(f) == Some(rel_name.as_str())
                                && f.get("type") == Some(&Value::from("relation"))
                        })
                        .filter_map(|f| {
                            f["relation"]
                                .get("entity")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect();
                    for target in targets {
                        self.add_entity_ref(&target, Some(&binding.path[1]));
                    }
                }
                OirBindingRoot::Local(root) => {
                    let root = root.clone();
                    self.add_entity_ref(&root, binding.path.first().map(String::as_str));
                }
                _ => {}
            }
            return;
        }
        if let Some(nav) = navigate_target_of(node) {
            self.add_page_ref(&nav);
            return;
        }
        let Some(arr) = node.as_array() else { return };
        match arr.first().and_then(Value::as_str) {
            Some("persist") => {
                if let Some(target) = arr.get(2).and_then(plain_string) {
                    let target = target.to_string();
                    self.add_entity_ref(&target, None);
                    if let Some(data) = arr.get(3).filter(|d| !d.is_string()) {
                        let mut keys = BTreeSet::new();
                        collect_persist_payload_keys(data, &mut keys);
                        for key in keys {
                            self.add_entity_ref(&target, Some(&key));
                        }
                    }
                }
            }
            Some("fetch") => {
                if let Some(target) = arr.get(1).and_then(plain_string) {
                    let target = target.to_string();
                    self.add_entity_ref(&target, None);
                    let include: Vec<String> = arr
                        .get(2)
                        .and_then(|o| o.get("include"))
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    for item in include {
                        self.add_entity_ref(&target, Some(&item));
                    }
                }
            }
            _ => {}
        }
    }

    fn walk(&mut self, node: Option<&Value>) {
        if let Some(n) = node {
            walk(n, &mut |v| self.visit(v));
        }
    }

    fn walk_config(&mut self, config: Option<&Value>) {
        for entry in config
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|o| o.values())
        {
            let declaration = entry.as_object().is_some_and(|o| {
                o.get("type").is_some_and(Value::is_string) && o.contains_key("default")
            });
            let value = if declaration {
                entry.get("default")
            } else {
                Some(entry)
            };
            self.walk(value);
        }
    }
}

fn expectation_key_matches(existing: &Value, derived: &Value) -> bool {
    let kind = |v: &Value| {
        v.get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let (ek, dk) = (kind(existing), kind(derived));
    if ek == "page" && dk == "page" {
        return existing.get("path") == derived.get("path");
    }
    if ek == "identity" && dk == "identity" {
        return existing.get("name") == derived.get("name")
            || existing.get("name").is_none()
            || derived.get("name").is_none();
    }
    let named = |k: &str| k == "entity" || k == "identity";
    named(&ek)
        && named(&dk)
        && existing.get("name").is_some()
        && existing.get("name") == derived.get("name")
}

fn add_expectation(expectations: &mut Vec<Value>, derived: Value) -> Result<(), String> {
    let same_kind = expectations
        .iter()
        .position(|e| e.get("kind") == derived.get("kind") && expectation_key_matches(e, &derived));
    let Some(index) = same_kind.or_else(|| {
        expectations
            .iter()
            .position(|e| expectation_key_matches(e, &derived))
    }) else {
        expectations.push(derived);
        return Ok(());
    };
    let existing = expectations[index].clone();
    let named = |v: &Value| {
        matches!(
            v.get("kind").and_then(Value::as_str),
            Some("entity" | "identity")
        )
    };
    if named(&existing) && named(&derived) {
        let shape = merge_expectation_shape(existing.get("shape"), derived.get("shape"))?;
        let mut next = existing.as_object().cloned().unwrap_or_default();
        if derived["kind"] == "identity" {
            next.insert("kind".into(), Value::from("identity"));
        }
        if existing["kind"] == "identity" && existing.get("name").is_none() {
            if let Some(name) = derived.get("name") {
                next.insert("name".into(), name.clone());
            }
        }
        if let Some(shape) = shape.filter(|s| s.as_array().is_some_and(|a| !a.is_empty())) {
            next.insert("shape".into(), shape);
        }
        expectations[index] = Value::Object(next);
    }
    Ok(())
}

/// Derive the `expects` declarations of `orbital_name` from `schema` (the program's `.orb`
/// JSON). `load` resolves imported behaviors by their `uses` path; without it, imports contribute
/// nothing. Pure; an incompatible shape union is an error.
pub fn derive_expectations(
    schema: &Value,
    orbital_name: &str,
    load: Option<BehaviorLoader<'_>>,
) -> Result<DerivedExpectations, String> {
    let all = orbitals(schema);
    let Some(orbital) = all.iter().find(|o| name_of(o) == Some(orbital_name)) else {
        return Ok(DerivedExpectations {
            expectations: vec![],
            diagnostics: vec![ExpectationDiagnostic {
                kind: DiagnosticKind::UnknownOrbital,
                orbital: orbital_name.to_string(),
                entity: None,
                field: None,
            }],
        });
    };
    let mut diagnostics = Vec::new();

    let mut owner_by_entity: HashMap<String, String> = HashMap::new();
    let mut def_by_entity: HashMap<String, Value> = HashMap::new();
    for o in all {
        for def in inline_entities(o) {
            if let (Some(n), Some(on)) = (name_of(def), name_of(o)) {
                if !owner_by_entity.contains_key(n) {
                    owner_by_entity.insert(n.to_string(), on.to_string());
                    def_by_entity.insert(n.to_string(), def.clone());
                }
            }
        }
    }

    let mut contributed: HashSet<String> = HashSet::new();
    if let Some(load) = load {
        for o in all {
            let by_alias: HashMap<&str, &str> = o
                .get("uses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|u| Some((u.get("as")?.as_str()?, u.get("from")?.as_str()?)))
                .collect();
            if by_alias.is_empty() {
                continue;
            }
            for t in o
                .get("traits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let r = match t {
                    Value::String(s) => s.as_str(),
                    _ if is_inline_trait(t) => continue,
                    _ => match t.get("ref").and_then(Value::as_str) {
                        Some(r) => r,
                        None => continue,
                    },
                };
                let Some((alias, trait_name)) = parse_imported_trait_ref(r) else {
                    continue;
                };
                let Some(behavior) = by_alias.get(alias).and_then(|b| load(b)) else {
                    continue;
                };
                let Some((bound, aux)) = contributed_entities_of(&behavior, trait_name) else {
                    continue;
                };
                let rebound = t.is_object() && t.get("linkedEntity").is_some();
                let names = if rebound {
                    aux
                } else {
                    bound.into_iter().chain(aux).collect()
                };
                for name in names {
                    if owner_by_entity.contains_key(&name) {
                        continue;
                    }
                    owner_by_entity.insert(name.clone(), name_of(o).unwrap_or("").to_string());
                    contributed.insert(name);
                }
            }
        }
    }

    let mut identity_def: Option<Value> = all
        .iter()
        .filter_map(|o| o.get("entity").and_then(as_entity))
        .find(|e| e.get("identity") == Some(&Value::Bool(true)))
        .cloned();
    if identity_def.is_none() {
        identity_def = all
            .iter()
            .flat_map(|o| inline_entities(o))
            .find(|e| e.get("identity") == Some(&Value::Bool(true)))
            .cloned();
    }
    if identity_def.is_none() {
        if let Some(load) = load {
            identity_def = all
                .iter()
                .find_map(|o| imported_identity_of(o, load, &mut HashSet::new()));
        }
    }

    let mut owner_by_page: BTreeMap<String, String> = BTreeMap::new();
    for o in all {
        for page in o
            .get("pages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(path), Some(on)) = (page.get("path").and_then(Value::as_str), name_of(o)) {
                if !path.is_empty() {
                    owner_by_page
                        .entry(path.to_string())
                        .or_insert_with(|| on.to_string());
                }
            }
        }
    }
    let page_paths: BTreeSet<String> = owner_by_page.keys().cloned().collect();

    let own_defs = inline_entities(orbital);
    let own_names: HashSet<&str> = own_defs.iter().filter_map(|d| name_of(d)).collect();
    let mut w = Walk {
        orbital_name,
        own_defs: own_defs.clone(),
        own_names,
        owner_by_entity: &owner_by_entity,
        owner_by_page: &owner_by_page,
        page_paths: &page_paths,
        user_fields: BTreeSet::new(),
        entity_refs: BTreeMap::new(),
        page_refs: BTreeSet::new(),
    };

    for def in &own_defs {
        for key in [
            "read_policy",
            "create_policy",
            "update_policy",
            "delete_policy",
        ] {
            w.walk(def.get(key));
        }
    }
    for def in &own_defs {
        for field in def
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if field.get("type") == Some(&Value::from("relation")) && field.get("name").is_some() {
                if let Some(target) = field["relation"].get("entity").and_then(Value::as_str) {
                    w.add_entity_ref(target, None);
                }
            }
        }
    }
    for target in orbital
        .get("reference")
        .and_then(|r| r.get("entities"))
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(Value::as_str)
    {
        w.add_entity_ref(target, None);
    }

    let mut events: Vec<Value> = Vec::new();
    let orbital_names: HashSet<&str> = all.iter().filter_map(|o| name_of(o)).collect();
    for t in orbital
        .get("traits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if t.is_string() {
            continue;
        }
        if let Some(le) = t.get("linkedEntity").and_then(Value::as_str) {
            w.add_entity_ref(le, None);
        }
        w.walk_config(t.get("config"));
        if is_inline_trait(t) {
            let sm = t.get("stateMachine");
            for g in sm
                .and_then(|s| s.get("guards"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(g.get("expression"));
            }
            for tr in sm
                .and_then(|s| s.get("transitions"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(tr.get("guard"));
                w.walk(tr.get("effects"));
            }
            w.walk(t.get("initialEffects"));
            for tick in t
                .get("ticks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(tick.get("guard"));
                w.walk(tick.get("effects"));
            }
            for listener in t
                .get("listens")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                w.walk(listener.get("guard"));
                for value in listener
                    .get("payloadMapping")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|m| m.values())
                {
                    w.walk(Some(value));
                }
            }
        }
        for listener in t
            .get("listens")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let source = listener.get("source");
            if source.and_then(|s| s.get("kind")).and_then(Value::as_str) != Some("orbital") {
                continue;
            }
            let (Some(src_orbital), Some(src_trait), Some(event)) = (
                source
                    .and_then(|s| s.get("orbital"))
                    .and_then(Value::as_str),
                source.and_then(|s| s.get("trait")).and_then(Value::as_str),
                listener.get("event").and_then(Value::as_str),
            ) else {
                continue;
            };
            if src_orbital == orbital_name {
                continue;
            }
            if !orbital_names.contains(src_orbital) {
                diagnostics.push(ExpectationDiagnostic {
                    kind: DiagnosticKind::EventSourceUnknown,
                    orbital: orbital_name.to_string(),
                    entity: None,
                    field: Some(format!("{src_orbital}.{src_trait}.{event}")),
                });
                continue;
            }
            let decl =
                serde_json::json!({ "kind": "event", "traitName": src_trait, "event": event });
            if !events.contains(&decl) {
                events.push(decl);
            }
        }
    }

    // `@user` is one roster per program: an authored or inherited identity expectation naming
    // another roster (an import's own) is the identity this program binds.
    let bound_identity = identity_def.as_ref().and_then(name_of).map(str::to_string);
    let mut expectations: Vec<Value> = Vec::new();
    for mut declared in orbital
        .get("expects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let is_identity = declared.get("kind") == Some(&Value::from("identity"));
        if let (Some(bound), Some(decl)) = (bound_identity.as_deref(), declared.as_object_mut()) {
            if is_identity && decl.contains_key("name") {
                decl.insert("name".into(), Value::from(bound));
            }
        }
        let earlier = expectations
            .iter()
            .position(|e| is_identity && e.get("kind") == Some(&Value::from("identity")));
        match earlier {
            Some(i) => {
                let shape =
                    merge_expectation_shape(expectations[i].get("shape"), declared.get("shape"))?;
                if let (Some(shape), Some(e)) = (shape, expectations[i].as_object_mut()) {
                    e.insert("shape".into(), shape);
                }
            }
            None => expectations.push(declared),
        }
    }

    if let Some(identity) = identity_def.as_ref().filter(|_| !w.user_fields.is_empty()) {
        if let Some(name) = name_of(identity) {
            if let Some(also) = w.entity_refs.remove(name) {
                w.user_fields.extend(also);
            }
        }
    }

    if !w.user_fields.is_empty() {
        let authored_identity = orbital
            .get("expects")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|e| e.get("kind") == Some(&Value::from("identity")));
        let mut shape = Vec::new();
        for field in &w.user_fields {
            let declared = authored_identity
                .and_then(|a| a.get("shape"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|f| name_of(f) == Some(field.as_str()))
                .or_else(|| {
                    identity_def
                        .as_ref()
                        .and_then(|d| d.get("fields"))
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .find(|f| name_of(f) == Some(field.as_str()))
                });
            match declared {
                Some(d) => shape.push(d.clone()),
                None => diagnostics.push(ExpectationDiagnostic {
                    kind: DiagnosticKind::IdentityFieldNotDeclared,
                    orbital: orbital_name.to_string(),
                    entity: identity_def.as_ref().and_then(name_of).map(str::to_string),
                    field: Some(field.clone()),
                }),
            }
        }
        let mut decl = Map::new();
        decl.insert("kind".into(), Value::from("identity"));
        if let Some(n) = identity_def.as_ref().and_then(name_of) {
            decl.insert("name".into(), Value::from(n));
        }
        if !shape.is_empty() {
            decl.insert("shape".into(), Value::Array(shape));
        }
        add_expectation(&mut expectations, Value::Object(decl))?;
    }

    for path in &w.page_refs {
        add_expectation(
            &mut expectations,
            serde_json::json!({ "kind": "page", "path": path }),
        )?;
    }

    for (name, referenced) in &w.entity_refs {
        let provider = def_by_entity.get(name);
        let mut shape = Vec::new();
        for field in referenced {
            match provider
                .and_then(|p| p.get("fields"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|f| name_of(f) == Some(field.as_str()))
            {
                Some(d) => shape.push(d.clone()),
                None if !contributed.contains(name) => diagnostics.push(ExpectationDiagnostic {
                    kind: DiagnosticKind::EntityFieldNotDeclared,
                    orbital: orbital_name.to_string(),
                    entity: Some(name.clone()),
                    field: Some(field.clone()),
                }),
                None => {}
            }
        }
        let mut decl = Map::new();
        decl.insert("kind".into(), Value::from("entity"));
        decl.insert("name".into(), Value::from(name.as_str()));
        if !shape.is_empty() {
            decl.insert("shape".into(), Value::Array(shape));
        }
        add_expectation(&mut expectations, Value::Object(decl))?;
    }

    for event in events {
        if !expectations.contains(&event) {
            expectations.push(event);
        }
    }

    let expectations = expectations
        .into_iter()
        .map(|e| {
            serde_json::from_value::<ExpectDeclaration>(e)
                .map_err(|err| format!("derived expectation is not an ExpectDeclaration: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DerivedExpectations {
        expectations,
        diagnostics,
    })
}
