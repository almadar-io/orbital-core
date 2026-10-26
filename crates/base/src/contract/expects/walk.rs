//! Walking a program's effects to collect the expectations they imply.

use super::*;

pub(super) struct Walk<'a> {
    pub(super) orbital_name: &'a str,
    pub(super) own_defs: Vec<&'a Value>,
    pub(super) own_names: HashSet<&'a str>,
    pub(super) owner_by_entity: &'a HashMap<String, String>,
    pub(super) owner_by_page: &'a BTreeMap<String, String>,
    pub(super) page_paths: &'a BTreeSet<String>,
    pub(super) user_fields: BTreeSet<String>,
    pub(super) entity_refs: BTreeMap<String, BTreeSet<String>>,
    pub(super) page_refs: BTreeSet<String>,
}

impl Walk<'_> {
    pub(super) fn add_entity_ref(&mut self, name: &str, field: Option<&str>) {
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

    pub(super) fn add_page_ref(&mut self, nav: &str) {
        let Some(matched) = find_matching_page_path(nav, self.page_paths) else {
            return;
        };
        if self.owner_by_page.get(&matched).map(String::as_str) == Some(self.orbital_name) {
            return;
        }
        self.page_refs.insert(matched);
    }

    pub(super) fn visit(&mut self, node: &Value) {
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

    pub(super) fn walk(&mut self, node: Option<&Value>) {
        if let Some(n) = node {
            walk(n, &mut |v| self.visit(v));
        }
    }

    pub(super) fn walk_config(&mut self, config: Option<&Value>) {
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

pub(super) fn expectation_key_matches(existing: &Value, derived: &Value) -> bool {
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

pub(super) fn add_expectation(expectations: &mut Vec<Value>, derived: Value) -> Result<(), String> {
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
