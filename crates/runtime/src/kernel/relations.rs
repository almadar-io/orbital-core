//! Relation hydration for `fetch … { include: […] }` and relation-option injection for `render-ui`.
//!
//! Both read the related entity's rows through the kernel's entity source, so on the server role they are
//! host-answered like any other read. JS twins: `OrbitalServerRuntime.populateRelations` and
//! `effect-stage.ts` `injectRelationOptions`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::entity_source::EntitySource;
use orbital_core_base::value::ValueMap;
use orbital_core_base::schema::types::{Cardinality, EntityDefinition, FieldDefinition, FieldType};
use orbital_core_base::value::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The deepest `include` nesting a fetch hydrates; a deeper level is left as ids.
const MAX_DEPTH: usize = 2;

/// Every entity the kernel knows, by name.
pub type EntityDefinitions = HashMap<String, Arc<EntityDefinition>>;

fn named_fields(def: &EntityDefinition) -> impl Iterator<Item = (&str, &FieldDefinition)> {
    def.fields
        .iter()
        .filter_map(|f| f.name.as_deref().filter(|n| !n.is_empty()).map(|n| (n, f)))
}

fn ids_of(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(id)) => vec![id.clone().into_inner()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn id_of(row: &Value) -> Option<String> {
    row.get_path("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Attach the rows each `include`d relation of `entity_type` points at to `rows`, under the relation's
/// name without a trailing `Id`. A self-referential to-many relation hydrates as a tree of clones that
/// ends where an id repeats one of its ancestors, so the payload has no shared or circular references.
/// A related table the source cannot list is skipped; a source still waiting on its host also lands
/// here, and the caller discards the suspended step.
pub fn populate_relations(
    source: &dyn EntitySource,
    defs: &EntityDefinitions,
    rows: &mut [Value],
    entity_type: &str,
    include: &[String],
    depth: usize,
    visited: &mut HashSet<String>,
) {
    if depth >= MAX_DEPTH || visited.contains(entity_type) {
        return;
    }
    visited.insert(entity_type.to_string());
    let Some(def) = defs.get(entity_type) else {
        return;
    };
    for include_field in include {
        let relation_field = named_fields(def).find(|(name, field)| {
            matches!(field.field_type, FieldType::Relation)
                && (*name == include_field
                    || *name == format!("{include_field}Id")
                    || name.strip_suffix("Id").unwrap_or(name) == include_field)
        });
        let Some((fk_field, field)) = relation_field else {
            continue;
        };
        let Some(relation) = field.relation.as_ref() else {
            continue;
        };
        let related_type = relation
            .entity_id
            .as_ref()
            .and_then(|id| {
                defs.values()
                    .find(|d| d.id.as_deref() == Some(id.as_str()))
                    .map(|d| d.name.clone())
            })
            .unwrap_or_else(|| relation.entity.clone());
        let to_one = matches!(
            relation.cardinality.clone().unwrap_or(Cardinality::One),
            Cardinality::One | Cardinality::ManyToOne
        );

        let wanted: HashSet<String> = rows
            .iter()
            .flat_map(|row| ids_of(row.get_path(fk_field)))
            .filter(|id| !id.is_empty())
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let Ok(listed) = source.list(&related_type) else {
            continue;
        };
        let all: ValueMap = listed
            .into_iter()
            .filter_map(|row| id_of(&row).map(|id| (id.into(), (*row).clone())))
            .collect();
        let mut related: ValueMap = wanted
            .iter()
            .filter_map(|id| all.get(id).map(|row| (id.clone().into(), row.clone())))
            .collect();

        let is_self = related_type == entity_type;
        let self_tree = is_self && !to_one;
        if self_tree {
            let mut frontier: Vec<Value> = related.values().cloned().collect();
            while !frontier.is_empty() {
                let mut next = Vec::new();
                for row in &frontier {
                    for id in ids_of(row.get_path(fk_field)) {
                        if related.contains_key(&id) {
                            continue;
                        }
                        if let Some(loaded) = all.get(&id) {
                            related.insert(id.into(), loaded.clone());
                            next.push(loaded.clone());
                        }
                    }
                }
                frontier = next;
            }
        }

        let hydrate = |id: &str, ancestors: &HashSet<String>| -> Option<Value> {
            fn clone_of(
                id: &str,
                ancestors: &HashSet<String>,
                related: &ValueMap,
                fk_field: &str,
                self_tree: bool,
                is_self: bool,
            ) -> Option<Value> {
                let row = related.get(id)?;
                let mut copy = row.clone();
                if let Value::Object(map) = &mut copy {
                    if self_tree {
                        let mut lineage = ancestors.clone();
                        lineage.insert(id.to_string());
                        let children = ids_of(row.get_path(fk_field))
                            .into_iter()
                            .filter(|child| !lineage.contains(child))
                            .filter_map(|child| {
                                clone_of(&child, &lineage, related, fk_field, self_tree, is_self)
                            })
                            .collect();
                        map.insert(fk_field.to_string().into(), Value::Array(children));
                    } else if is_self {
                        map.insert(fk_field.to_string().into(), Value::array(Vec::new()));
                    }
                }
                Some(copy)
            }
            clone_of(id, ancestors, &related, fk_field, self_tree, is_self)
        };

        let populated = include_field
            .strip_suffix("Id")
            .unwrap_or(include_field)
            .to_string();
        for row in rows.iter_mut() {
            let lineage: HashSet<String> = id_of(row).into_iter().collect();
            let fk_value = row.get_path(fk_field).cloned();
            let attached = if to_one {
                match &fk_value {
                    Some(Value::String(id)) if related.contains_key(id.as_str()) => hydrate(id, &lineage),
                    _ => None,
                }
            } else {
                let ids: Vec<String> = ids_of(fk_value.as_ref())
                    .into_iter()
                    .filter(|id| !lineage.contains(id) && related.contains_key(id))
                    .collect();
                if matches!(fk_value, Some(Value::Array(_))) || !ids.is_empty() {
                    Some(Value::Array(
                        ids.iter().filter_map(|id| hydrate(id, &lineage)).collect(),
                    ))
                } else {
                    None
                }
            };
            if let (Some(attached), Value::Object(map)) = (attached, row) {
                map.insert(populated.clone().into(), attached);
            }
        }
    }
}

/// The entity a relation field points at and the field of it the related record is shown by.
struct RelationTarget {
    entity: String,
    display: Option<String>,
}

/// The relation fields of `entity_type`, by field name, with the entity each points at.
fn relation_targets(defs: &EntityDefinitions, entity_type: &str) -> HashMap<String, RelationTarget> {
    let Some(def) = defs.get(entity_type) else {
        return HashMap::new();
    };
    named_fields(def)
        .filter(|(_, field)| matches!(field.field_type, FieldType::Relation))
        .filter_map(|(name, field)| {
            field.relation.as_ref().map(|relation| {
                (
                    name.to_string(),
                    RelationTarget {
                        entity: relation.entity.clone(),
                        display: relation.display.clone(),
                    },
                )
            })
        })
        .collect()
}

/// Walk a `render-ui` pattern tree: every node whose pattern declares a `@fieldsContract` and lists
/// relation-typed fields among its `fields` / `columns` gets `relationsData: { <field>: [{ value, label }] }`
/// read from the relation's target entity (label: the relation's declared `display` field, the record id when
/// it declares none or the row has no value there). Authored `relationsData` is kept.
/// A target table the source cannot list gives that field no options (the select renders empty); a source
/// still waiting on its host also lands here, and the caller discards the suspended step.
pub fn inject_relation_options(
    source: &dyn EntitySource,
    defs: &EntityDefinitions,
    linked_entity: &str,
    pattern: &mut Value,
) {
    let targets = relation_targets(defs, linked_entity);
    if !targets.is_empty() {
        visit(source, &targets, pattern);
    }
}

fn field_name(entry: &Value) -> Option<String> {
    match entry {
        Value::String(name) => Some(name.clone().into_inner()),
        Value::Object(map) => ["field", "name", "key"]
            .iter()
            .find_map(|k| map.get(*k))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn visit(source: &dyn EntitySource, targets: &HashMap<String, RelationTarget>, node: &mut Value) {
    match node {
        Value::Array(items) => {
            for item in items {
                visit(source, targets, item);
            }
        }
        Value::Object(map) => {
            let consumes_fields = map.get("type").and_then(Value::as_str).is_some_and(|t| {
                orbital_core_base::language::pattern_registry::get_fields_contract(t).is_some()
            });
            if consumes_fields && !map.contains_key("relationsData") {
                let entries = match map.get("fields").or_else(|| map.get("columns")) {
                    Some(Value::Array(entries)) => entries.clone(),
                    _ => Vec::new().into(),
                };
                let mut data: ValueMap = ValueMap::default();
                for entry in &entries {
                    let Some(name) = field_name(entry) else {
                        continue;
                    };
                    let Some(target) = targets.get(&name) else {
                        continue;
                    };
                    let Ok(rows) = source.list(&target.entity) else {
                        continue;
                    };
                    let options = rows
                        .iter()
                        .map(|row| {
                            let text = |key: &str| {
                                row.get_path(key)
                                    .filter(|v| !v.is_null())
                                    .map(Value::to_string_value)
                            };
                            let id = text("id").unwrap_or_default();
                            let label = target
                                .display
                                .as_deref()
                                .and_then(text)
                                .unwrap_or_else(|| id.clone());
                            Value::object(ValueMap::from_iter([
                                ("value".into(), Value::String(id.into())),
                                ("label".into(), Value::String(label.into())),
                            ]))
                        })
                        .collect();
                    data.insert(name.into(), Value::Array(options));
                }
                if !data.is_empty() {
                    map.insert("relationsData".into(), Value::object(data));
                }
            }
            if let Some(children) = map.get_mut("children") {
                visit(source, targets, children);
            }
        }
        _ => {}
    }
}
