//! A trait's `@entity` as the kernel keeps it: each field behind its own `Arc`, so binding it to a step, a snapshot
//! and a merge copy no field. The whole value is built once, only when something reads `@entity` whole. Serializes
//! exactly like the `Value` it stands for.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::{path_segments, Value, ValueMap};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use std::borrow::Cow;
use crate::sync::OnceLock;
use std::sync::Arc;

pub type SharedFields = crate::collections::IndexMap<String, Arc<Value>>;

#[derive(Debug, Clone, Default)]
pub struct SharedEntity(Repr);

#[derive(Debug, Clone, Default)]
enum Repr {
    #[default]
    Null,
    Object(Arc<SharedFields>, OnceLock<Arc<Value>>),
    Other(Arc<Value>),
}

/// The entity as held, for a reader that walks it without building the whole value.
pub enum EntityParts<'a> {
    Null,
    Fields(&'a SharedFields),
    Value(&'a Value),
}

static NULL: Value = Value::Null;

/// The first segment of a value path, decoded as [`path_segments`] decodes it, and the rest after its separator.
fn split_first_segment(path: &str) -> (Cow<'_, str>, Option<&str>) {
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if matches!(bytes.get(i + 1), Some(b'.' | b'\\')) => i += 2,
            b'.' => return (first_segment(&path[..i]), Some(&path[i + 1..])),
            _ => i += 1,
        }
    }
    (first_segment(path), None)
}

fn first_segment(segment: &str) -> Cow<'_, str> {
    path_segments(segment).next().unwrap_or(Cow::Borrowed(""))
}

impl SharedEntity {
    pub fn null() -> Self {
        Self(Repr::Null)
    }

    pub fn empty_object() -> Self {
        Self::object(SharedFields::default())
    }

    fn object(fields: SharedFields) -> Self {
        Self(Repr::Object(Arc::new(fields), OnceLock::new()))
    }

    pub fn from_value(value: Value) -> Self {
        match value {
            Value::Null => Self(Repr::Null),
            Value::Object(fields) => {
                Self::object(fields.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
            }
            other => Self(Repr::Other(Arc::new(other))),
        }
    }

    /// The entity kept as one value, unsplit: for a context that rebinds `@entity` per row or per paint and never
    /// writes a field of it. Reads walk the value; a merge or a field write splits it first.
    pub fn whole(value: Value) -> Self {
        Self::from_arc(Arc::new(value))
    }

    pub fn from_arc(value: Arc<Value>) -> Self {
        if value.is_null() {
            Self(Repr::Null)
        } else {
            Self(Repr::Other(value))
        }
    }

    /// The entity as an owned value, taking each part it alone holds instead of copying it.
    pub fn into_value(self) -> Value {
        match self.0 {
            Repr::Null => Value::Null,
            Repr::Object(fields, _) => match Arc::try_unwrap(fields) {
                Ok(fields) => Value::Object(
                    fields
                        .into_iter()
                        .map(|(k, v)| (k, Arc::try_unwrap(v).unwrap_or_else(|v| (*v).clone())))
                        .collect(),
                ),
                Err(fields) => Value::Object(
                    fields
                        .iter()
                        .map(|(k, v)| (k.clone(), (**v).clone()))
                        .collect(),
                ),
            },
            Repr::Other(v) => Arc::try_unwrap(v).unwrap_or_else(|v| (*v).clone()),
        }
    }

    /// The fields to write over: as held, or split from a whole object value.
    fn object_fields(&self) -> Option<Cow<'_, SharedFields>> {
        match &self.0 {
            Repr::Object(fields, _) => Some(Cow::Borrowed(&**fields)),
            Repr::Other(v) => match &**v {
                Value::Object(map) => Some(Cow::Owned(
                    map.iter()
                        .map(|(k, v)| (k.clone(), Arc::new(v.clone())))
                        .collect(),
                )),
                _ => None,
            },
            Repr::Null => None,
        }
    }

    pub fn from_value_ref(value: &Value) -> Self {
        match value {
            Value::Null => Self(Repr::Null),
            Value::Object(fields) => Self::object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), Arc::new(v.clone())))
                    .collect(),
            ),
            other => Self(Repr::Other(Arc::new(other.clone()))),
        }
    }

    pub fn parts(&self) -> EntityParts<'_> {
        match &self.0 {
            Repr::Null => EntityParts::Null,
            Repr::Object(fields, _) => EntityParts::Fields(fields),
            Repr::Other(v) => EntityParts::Value(v),
        }
    }

    pub fn to_value(&self) -> Value {
        match &self.0 {
            Repr::Null => Value::Null,
            Repr::Object(fields, whole) => match whole.get() {
                Some(value) => (**value).clone(),
                None => Value::Object(
                    (fields
                        .iter()
                        .map(|(k, v)| (k.clone(), (**v).clone()))
                        .collect::<ValueMap>()).into(),
                ),
            },
            Repr::Other(v) => (**v).clone(),
        }
    }

    /// The whole entity as one `Value`, built on the first whole read and shared by every clone after it.
    pub fn as_value(&self) -> &Value {
        match &self.0 {
            Repr::Null => &NULL,
            Repr::Object(fields, whole) => whole.get_or_init(|| {
                Arc::new(Value::Object(
                    (fields
                        .iter()
                        .map(|(k, v)| (k.clone(), (**v).clone()))
                        .collect::<ValueMap>()).into(),
                ))
            }),
            Repr::Other(v) => v,
        }
    }

    /// [`Value::get_path`] over the entity, reading the named field where it is held.
    pub fn get_path(&self, path: &str) -> Option<&Value> {
        match &self.0 {
            Repr::Object(fields, _) => {
                let (field, rest) = split_first_segment(path);
                let value = fields.get(field.as_ref())?;
                match rest {
                    Some(rest) => value.get_path(rest),
                    None => Some(value),
                }
            }
            _ => self.as_value().get_path(path),
        }
    }

    /// [`Value::get_path_owned`] over the entity, copying only what the path reaches.
    pub fn get_path_owned(&self, path: &str) -> Option<Value> {
        match &self.0 {
            Repr::Object(fields, _) => {
                let (field, rest) = split_first_segment(path);
                let value = fields.get(field.as_ref())?;
                match rest {
                    Some(rest) => value.get_path_owned(rest),
                    None => Some((**value).clone()),
                }
            }
            _ => self.as_value().get_path_owned(path),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self.0, Repr::Null)
    }

    pub fn fields(&self) -> Option<&SharedFields> {
        match &self.0 {
            Repr::Object(fields, _) => Some(fields),
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

    /// Writes one field; every other field keeps its `Arc`, and the field map is copied only while another holder
    /// shares it. `false` when `self` is not an object.
    pub fn set_field(&mut self, field: String, value: Value) -> bool {
        if let Repr::Other(_) = &self.0 {
            match self.object_fields() {
                Some(fields) => *self = Self::object(fields.into_owned()),
                None => return false,
            }
        }
        match &mut self.0 {
            Repr::Object(fields, whole) => {
                Arc::make_mut(fields).insert(field, Arc::new(value));
                *whole = OnceLock::new();
                true
            }
            _ => false,
        }
    }

    /// `set` laid over `self`: a non-empty object `set` overrides its fields on an object base (unchanged fields keep
    /// their `Arc`) and replaces any other base; an empty or non-object `set` leaves the base.
    pub fn merged(&self, set: &SharedEntity) -> SharedEntity {
        let Some(set_fields) = set.object_fields().filter(|s| !s.is_empty()) else {
            return self.clone();
        };
        match self.object_fields() {
            Some(base) => {
                let mut merged = SharedFields::with_capacity_and_hasher(base.len() + set_fields.len(), Default::default());
                merged.extend(base.iter().map(|(k, v)| (k.clone(), Arc::clone(v))));
                for (k, v) in set_fields.iter() {
                    merged.insert(k.clone(), Arc::clone(v));
                }
                Self::object(merged)
            }
            None => set.clone(),
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
            (Repr::Object(a, _), Repr::Object(b, _)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((ka, va), (kb, vb))| ka == kb && va == vb)
            }
            (Repr::Null, _) | (_, Repr::Null) => false,
            _ => self.as_value() == other.as_value(),
        }
    }
}

impl Serialize for SharedEntity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Repr::Null => serializer.serialize_none(),
            Repr::Object(fields, _) => {
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
        assert!(Arc::ptr_eq(
            base.field_arc("blob").unwrap(),
            merged.field_arc("blob").unwrap()
        ));
        assert!(Arc::ptr_eq(
            set.field_arc("n").unwrap(),
            merged.field_arc("n").unwrap()
        ));
        assert_eq!(
            merged.to_value(),
            val(json!({ "blob": "x".repeat(64), "n": 2 }))
        );
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
            assert_eq!(
                serde_json::to_string(&shared).unwrap(),
                serde_json::to_string(&v).unwrap()
            );
            assert_eq!(shared.to_value(), v);
        }
    }

    #[test]
    fn a_path_read_agrees_with_the_whole_value_walk() {
        let whole = val(json!({
            "n": 1, "list": [10, { "k": "v" }], "m": { "deep": { "x": true } },
            "http.method": "GET", "a\\b": 2, "": "empty", "length": 7
        }));
        let shared = SharedEntity::from_value_ref(&whole);
        for path in [
            "n",
            "list",
            "list.1.k",
            "list.length",
            "list.count",
            "list.9",
            "m.deep.x",
            "m.deep.missing",
            "http\\.method",
            "http.method",
            "a\\\\b",
            "",
            "length",
            "missing",
            "n.more",
            "list.length.more",
        ] {
            assert_eq!(
                shared.get_path(path),
                whole.get_path(path),
                "borrowed {path:?}"
            );
            assert_eq!(
                shared.get_path_owned(path),
                whole.get_path_owned(path),
                "owned {path:?}"
            );
        }
    }

    #[test]
    fn control_a_non_object_entity_walks_as_its_value() {
        for j in [json!(null), json!([1, 2]), json!("bare")] {
            let whole = val(j);
            let shared = SharedEntity::from_value_ref(&whole);
            for path in ["0", "length", "x"] {
                assert_eq!(shared.get_path(path), whole.get_path(path));
                assert_eq!(shared.get_path_owned(path), whole.get_path_owned(path));
            }
            assert_eq!(shared.as_value(), &whole);
        }
    }

    #[test]
    fn a_field_write_keeps_every_other_field_allocation() {
        let base = SharedEntity::from_value(val(json!({ "blob": "x".repeat(64), "n": 1 })));
        let mut written = base.clone();
        assert!(written.set_field("n".into(), val(json!(2))));
        assert!(Arc::ptr_eq(
            base.field_arc("blob").unwrap(),
            written.field_arc("blob").unwrap()
        ));
        assert_eq!(
            written.to_value(),
            val(json!({ "blob": "x".repeat(64), "n": 2 }))
        );
        assert_eq!(
            base.to_value(),
            val(json!({ "blob": "x".repeat(64), "n": 1 }))
        );
        assert!(!SharedEntity::null().set_field("n".into(), Value::Null));
        let mut whole = SharedEntity::whole(val(json!({ "a": 1 })));
        assert!(whole.set_field("b".into(), val(json!(2))));
        assert_eq!(whole.to_value(), val(json!({ "a": 1, "b": 2 })));
    }

    #[test]
    fn the_whole_value_is_built_once_and_matches_the_fields() {
        let shared = SharedEntity::from_value(val(json!({ "a": 1, "b": [true] })));
        let first: *const Value = shared.as_value();
        assert!(std::ptr::eq(first, shared.as_value()));
        let mut written = shared.clone();
        written.set_field("a".into(), val(json!(9)));
        assert_eq!(written.as_value(), &val(json!({ "a": 9, "b": [true] })));
        assert_eq!(shared.as_value(), &val(json!({ "a": 1, "b": [true] })));
    }
}
