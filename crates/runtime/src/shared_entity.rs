//! A trait's `@entity` as the kernel keeps it between steps: each field behind its own `Arc`, so a snapshot is a
//! refcount bump and a merge copies only the fields a step wrote. Serializes exactly like the `Value` it stands for.

use orbital_core_base::value::{Value, ValueMap};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use std::sync::Arc;

pub type SharedFields = indexmap::IndexMap<String, Arc<Value>>;

#[derive(Debug, Clone, Default)]
pub struct SharedEntity(Repr);

#[derive(Debug, Clone, Default)]
enum Repr {
    #[default]
    Null,
    Object(Arc<SharedFields>),
    Other(Arc<Value>),
}

impl SharedEntity {
    pub fn null() -> Self {
        Self(Repr::Null)
    }

    pub fn empty_object() -> Self {
        Self(Repr::Object(Arc::new(SharedFields::new())))
    }

    pub fn from_value(value: Value) -> Self {
        match value {
            Value::Null => Self(Repr::Null),
            Value::Object(fields) => Self(Repr::Object(Arc::new(
                fields.into_iter().map(|(k, v)| (k, Arc::new(v))).collect(),
            ))),
            other => Self(Repr::Other(Arc::new(other))),
        }
    }

    pub fn from_value_ref(value: &Value) -> Self {
        match value {
            Value::Null => Self(Repr::Null),
            Value::Object(fields) => Self(Repr::Object(Arc::new(
                fields.iter().map(|(k, v)| (k.clone(), Arc::new(v.clone()))).collect(),
            ))),
            other => Self(Repr::Other(Arc::new(other.clone()))),
        }
    }

    pub fn to_value(&self) -> Value {
        match &self.0 {
            Repr::Null => Value::Null,
            Repr::Object(fields) => Value::Object(
                fields.iter().map(|(k, v)| (k.clone(), (**v).clone())).collect::<ValueMap>(),
            ),
            Repr::Other(v) => (**v).clone(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self.0, Repr::Null)
    }

    pub fn fields(&self) -> Option<&SharedFields> {
        match &self.0 {
            Repr::Object(fields) => Some(fields),
            _ => None,
        }
    }

    pub fn get(&self, field: &str) -> Option<&Value> {
        self.fields()?.get(field).map(|v| &**v)
    }

    /// The `Arc` behind one field, for callers that check sharing.
    pub fn field_arc(&self, field: &str) -> Option<&Arc<Value>> {
        self.fields()?.get(field)
    }

    /// `set` laid over `self`: a non-empty object `set` overrides its fields on an object base (unchanged fields keep
    /// their `Arc`) and replaces any other base; an empty or non-object `set` leaves the base.
    pub fn merged(&self, set: &SharedEntity) -> SharedEntity {
        let Some(set_fields) = set.fields().filter(|s| !s.is_empty()) else {
            return self.clone();
        };
        match &self.0 {
            Repr::Object(base) => {
                let mut merged = SharedFields::with_capacity(base.len() + set_fields.len());
                merged.extend(base.iter().map(|(k, v)| (k.clone(), Arc::clone(v))));
                for (k, v) in set_fields.iter() {
                    merged.insert(k.clone(), Arc::clone(v));
                }
                SharedEntity(Repr::Object(Arc::new(merged)))
            }
            _ => set.clone(),
        }
    }
}

impl From<Value> for SharedEntity {
    fn from(value: Value) -> Self {
        Self::from_value(value)
    }
}

impl PartialEq for SharedEntity {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Null, Repr::Null) => true,
            (Repr::Object(a), Repr::Object(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b.iter()).all(|((ka, va), (kb, vb))| ka == kb && va == vb)
            }
            (Repr::Other(a), Repr::Other(b)) => a == b,
            _ => false,
        }
    }
}

impl Serialize for SharedEntity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Repr::Null => serializer.serialize_none(),
            Repr::Object(fields) => {
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for (k, v) in fields.iter() {
                    map.serialize_entry(k, &**v)?;
                }
                map.end()
            }
            Repr::Other(v) => v.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn val(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    #[test]
    fn a_merge_keeps_every_untouched_field_allocation() {
        let base = SharedEntity::from_value(val(json!({ "blob": "x".repeat(64), "n": 1 })));
        let set = SharedEntity::from_value(val(json!({ "n": 2 })));
        let merged = base.merged(&set);
        assert!(Arc::ptr_eq(base.field_arc("blob").unwrap(), merged.field_arc("blob").unwrap()));
        assert!(Arc::ptr_eq(set.field_arc("n").unwrap(), merged.field_arc("n").unwrap()));
        assert_eq!(merged.to_value(), val(json!({ "blob": "x".repeat(64), "n": 2 })));
    }

    #[test]
    fn merge_follows_the_entity_merge_rules() {
        let base = SharedEntity::from_value(val(json!({ "a": 1 })));
        assert_eq!(base.merged(&SharedEntity::empty_object()), base);
        assert_eq!(base.merged(&SharedEntity::null()), base);
        let set = SharedEntity::from_value(val(json!({ "b": 2 })));
        assert_eq!(SharedEntity::null().merged(&set), set);
        assert!(SharedEntity::null().merged(&SharedEntity::null()).is_null());
    }

    #[test]
    fn serializes_byte_identically_to_the_value() {
        for j in [
            json!(null),
            json!({}),
            json!({ "z": 1, "a": [1, "two", { "k": null }], "m": { "y": true, "b": 1.5 } }),
            json!("bare"),
        ] {
            let v = val(j);
            let shared = SharedEntity::from_value_ref(&v);
            assert_eq!(serde_json::to_string(&shared).unwrap(), serde_json::to_string(&v).unwrap());
            assert_eq!(shared.to_value(), v);
        }
    }
}
