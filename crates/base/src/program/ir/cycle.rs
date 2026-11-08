//! Which fields of a record a target that stores fields inline must box: a declaration that reaches itself by value
//! (not through a list or a map, which are heap storage). One rule, read by the Swift shell and the native renderers.

use crate::ir::{OirEntity, OirModule, OirType};
use std::collections::BTreeMap;

/// The declaration being emitted: a named type or an entity's row struct.
pub enum TypeRoot {
    Named(String),
    Row(String),
}

/// A Swift struct or enum holds its fields inline, so a declaration that reaches itself by value (not
/// through an array or a dictionary, which are heap storage) has infinite size: the edge back to the
/// root is boxed (`Indirect<T>`) or, for a union, the enum is `indirect`.
pub struct CycleRoot<'a> {
    pub named_types: &'a std::collections::BTreeMap<String, OirType>,
    pub entities: &'a [OirEntity],
    pub root: TypeRoot,
}

impl CycleRoot<'_> {
    pub fn reaches(&self, ty: &OirType) -> bool {
        self.reaches_from(ty, &mut std::collections::BTreeSet::new())
    }

    fn reaches_from<'t>(&'t self, ty: &'t OirType, seen: &mut std::collections::BTreeSet<(bool, &'t str)>) -> bool {
        match ty {
            OirType::Optional { inner } => self.reaches_from(inner, seen),
            OirType::Struct { fields, .. } => fields.iter().any(|f| self.reaches_from(&f.field_type, seen)),
            OirType::Union { variants } => variants.iter().any(|v| self.reaches_from(&v.field_type, seen)),
            OirType::TypeRef { name } => {
                matches!(&self.root, TypeRoot::Named(root) if root == name)
                    || (seen.insert((false, name.as_str()))
                        && self.named_types.get(name).is_some_and(|t| self.reaches_from(t, seen)))
            }
            OirType::Row { entity } => {
                matches!(&self.root, TypeRoot::Row(root) if root == entity)
                    || (seen.insert((true, entity.as_str()))
                        && self
                            .entities
                            .iter()
                            .find(|e| &e.name == entity)
                            .is_some_and(|e| e.fields.iter().any(|f| self.reaches_from(&f.field_type, seen))))
            }
            _ => false,
        }
    }
}

/// Each named type's and entity row's fields that reach their own declaration by value: model name → field names.
pub fn boxed_fields(module: &OirModule) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for (name, ty) in &module.named_types {
        let root = CycleRoot { named_types: &module.named_types, entities: &module.entities, root: TypeRoot::Named(name.clone()) };
        if let OirType::Struct { fields, .. } = ty {
            let boxed: Vec<String> = fields.iter().filter(|f| root.reaches(&f.field_type)).map(|f| f.name.clone()).collect();
            if !boxed.is_empty() {
                out.insert(name.clone(), boxed);
            }
        }
    }
    for entity in &module.entities {
        let root = CycleRoot { named_types: &module.named_types, entities: &module.entities, root: TypeRoot::Row(entity.name.clone()) };
        let boxed: Vec<String> = entity.fields.iter().filter(|f| root.reaches(&f.field_type)).map(|f| f.name.clone()).collect();
        if !boxed.is_empty() {
            out.insert(entity.name.clone(), boxed);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::OirField;

    fn entity(name: &str, fields: Vec<OirField>) -> OirEntity {
        let mut e: OirEntity = serde_json::from_value(serde_json::json!({ "name": name, "collection": name, "persistence": "runtime", "fields": [] })).expect("entity");
        e.fields = fields;
        e
    }

    #[test]
    fn a_row_field_reaching_its_own_row_by_value_is_boxed() {
        let mut module = OirModule::new("app", "1.0.0");
        module.entities = vec![entity("Ticket", vec![
            OirField::new("id", OirType::String),
            OirField::new("seedRow", OirType::optional(OirType::Row { entity: "Ticket".into() })),
            OirField::new("history", OirType::array(OirType::Row { entity: "Ticket".into() })),
            OirField::new("owner", OirType::optional(OirType::Row { entity: "Person".into() })),
        ]), entity("Person", vec![OirField::new("id", OirType::String)])];
        let boxed = boxed_fields(&module);
        assert_eq!(boxed.get("Ticket"), Some(&vec!["seedRow".to_string()]));
        assert!(!boxed.contains_key("Person"), "control: a row that never reaches itself boxes nothing");
    }
}
