//! Reading a typed value straight out of a core [`Value`]: the same result as `from_json_value(&value_to_json(v))`,
//! without the `serde_json::Value` copy of the whole value between (P-049: a resolved program's copy is ~10x the value).

use super::value::Value;
use serde::de::{self, IntoDeserializer, Visitor};
use serde::forward_to_deserialize_any;

type Error = serde_json::Error;

/// `T` read from `value`, as `from_json_value(&value_to_json(value))` reads it. A `Shared` record read twice from one
/// source node (an interned program subtree) is read once and shared, so a program holds each repeat once in memory.
pub fn from_value<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, Error> {
    let _scope = SharedScope::open();
    T::deserialize(Reader(value))
}

/// The newtype name `Shared` asks for, so the reader can tell it which source node it reads.
pub(crate) const SHARED_NEWTYPE: &str = "$orbital::Shared";

type SharedMemo = std::collections::HashMap<(usize, std::any::TypeId), Box<dyn std::any::Any>>;

thread_local! {
    static SHARED_MEMO: std::cell::RefCell<Option<SharedMemo>> = const { std::cell::RefCell::new(None) };
    static SHARED_NODE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The memo lives for the outermost `from_value` only, while every node it is keyed by is borrowed.
struct SharedScope(bool);

impl SharedScope {
    fn open() -> Self {
        SharedScope(SHARED_MEMO.with(|m| {
            let mut m = m.borrow_mut();
            let outer = m.is_none();
            if outer {
                *m = Some(SharedMemo::new());
            }
            outer
        }))
    }
}

impl Drop for SharedScope {
    fn drop(&mut self) {
        if self.0 {
            SHARED_MEMO.with(|m| m.borrow_mut().take());
        }
    }
}

fn node_key(value: &Value) -> usize {
    match value {
        Value::Object(map) => crate::value::Shared::as_ptr(map) as usize,
        Value::Array(items) => crate::value::Shared::as_ptr(items) as usize,
        Value::String(text) => crate::value::Shared::as_ptr(text) as usize,
        _ => 0,
    }
}

/// The source node the reader is on for this `Shared` read, once; 0 outside a `from_value` read.
pub(crate) fn take_shared_node() -> usize {
    SHARED_NODE.with(|n| n.replace(0))
}

pub(crate) fn shared_read<R: Clone + 'static>(node: usize) -> Option<R> {
    if node == 0 {
        return None;
    }
    SHARED_MEMO.with(|m| m.borrow().as_ref().and_then(|memo| memo.get(&(node, std::any::TypeId::of::<R>())).and_then(|hit| hit.downcast_ref::<R>().cloned())))
}

pub(crate) fn shared_keep<R: Clone + 'static>(node: usize, read: &R) {
    if node == 0 {
        return;
    }
    SHARED_MEMO.with(|m| {
        if let Some(memo) = m.borrow_mut().as_mut() {
            memo.insert((node, std::any::TypeId::of::<R>()), Box::new(read.clone()));
        }
    });
}

/// A whole number `value_to_json` writes as an integer, else `None` (it writes a float, or null when not finite).
fn json_integer(n: f64) -> Option<i64> {
    (n.fract() == 0.0 && n.abs() < i64::MAX as f64).then_some(n as i64)
}

fn visit_integer<'de, V: Visitor<'de>>(i: i64, visitor: V) -> Result<V::Value, Error> {
    match u64::try_from(i) {
        Ok(u) => visitor.visit_u64(u),
        Err(_) => visitor.visit_i64(i),
    }
}

#[derive(Clone, Copy)]
struct Reader<'de>(&'de Value);

impl<'de> de::Deserializer<'de> for Reader<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::Null | Value::Lambda(_) => visitor.visit_unit(),
            Value::Bool(b) => visitor.visit_bool(*b),
            Value::Number(n) => match json_integer(*n) {
                Some(i) => visit_integer(i, visitor),
                None if n.is_finite() => visitor.visit_f64(*n),
                None => visitor.visit_unit(),
            },
            Value::Int(i) => visit_integer(*i, visitor),
            Value::Bytes(b) => visitor.visit_string(crate::codec::base64_encode(b)),
            Value::String(s) => visitor.visit_borrowed_str(s.as_str()),
            Value::Array(items) => {
                let mut seq = Seq(items.iter());
                let read = visitor.visit_seq(&mut seq)?;
                match seq.0.len() {
                    0 => Ok(read),
                    _ => Err(de::Error::invalid_length(items.len(), &"fewer elements in array")),
                }
            }
            Value::Object(map) => {
                let mut entries = Map { entries: map.iter(), value: None };
                let read = visitor.visit_map(&mut entries)?;
                match entries.entries.len() {
                    0 => Ok(read),
                    _ => Err(de::Error::invalid_length(map.len(), &"fewer elements in map")),
                }
            }
            Value::Sealed(_) | Value::Blob(_) => {
                de::Deserializer::deserialize_any(super::json_value::value_to_json(self.0), visitor)
            }
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::Null | Value::Lambda(_) => visitor.visit_none(),
            Value::Number(n) if !n.is_finite() => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _name: &'static str, _variants: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::String(s) => visitor.visit_enum(Variant { name: s.as_str(), content: None }),
            Value::Object(map) => {
                let mut entries = map.iter();
                match (entries.next(), entries.next()) {
                    (Some((name, content)), None) => visitor.visit_enum(Variant { name: name.as_str(), content: Some(content) }),
                    _ => Err(de::Error::invalid_value(de::Unexpected::Map, &"map with a single key")),
                }
            }
            Value::Sealed(_) | Value::Blob(_) => {
                de::Deserializer::deserialize_enum(super::json_value::value_to_json(self.0), _name, _variants, visitor)
            }
            _ => Err(de::Error::invalid_type(unexpected(self.0), &"string or map")),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, name: &'static str, visitor: V) -> Result<V::Value, Error> {
        if name == SHARED_NEWTYPE {
            SHARED_NODE.with(|n| n.set(node_key(self.0)));
        }
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf unit unit_struct seq tuple
        tuple_struct map struct identifier
    }
}

fn unexpected(value: &Value) -> de::Unexpected<'_> {
    match value {
        Value::Null | Value::Lambda(_) => de::Unexpected::Unit,
        Value::Bool(b) => de::Unexpected::Bool(*b),
        Value::Number(n) => de::Unexpected::Float(*n),
        Value::Int(i) => de::Unexpected::Signed(*i),
        Value::Bytes(_) | Value::String(_) => de::Unexpected::Str(""),
        Value::Array(_) => de::Unexpected::Seq,
        Value::Object(_) | Value::Sealed(_) | Value::Blob(_) => de::Unexpected::Map,
    }
}

struct Seq<'de>(std::slice::Iter<'de, Value>);

impl<'de> de::SeqAccess<'de> for Seq<'de> {
    type Error = Error;

    fn next_element_seed<T: de::DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.0.next().map(|item| seed.deserialize(Reader(item))).transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}

struct Map<'de, I: Iterator<Item = (&'de crate::value::Shared<String>, &'de Value)>> {
    entries: I,
    value: Option<&'de Value>,
}

impl<'de, I: ExactSizeIterator<Item = (&'de crate::value::Shared<String>, &'de Value)>> de::MapAccess<'de> for Map<'de, I> {
    type Error = Error;

    fn next_key_seed<K: de::DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Error> {
        match self.entries.next() {
            Some((key, value)) => {
                self.value = Some(value);
                seed.deserialize(Key(key.as_str())).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<T: de::DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, Error> {
        match self.value.take() {
            Some(value) => seed.deserialize(Reader(value)),
            None => Err(de::Error::custom("value is missing")),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

/// A map key: text, read as a number or a boolean when the key's type asks for one (as `serde_json` reads keys).
struct Key<'de>(&'de str);

macro_rules! parsed_key {
    ($($method:ident => $visit:ident : $ty:ty),* $(,)?) => {
        $(fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            match self.0.parse::<$ty>() {
                Ok(parsed) => visitor.$visit(parsed),
                Err(_) => Err(de::Error::invalid_value(de::Unexpected::Str(self.0), &visitor)),
            }
        })*
    };
}

impl<'de> de::Deserializer<'de> for Key<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_borrowed_str(self.0)
    }

    parsed_key! {
        deserialize_bool => visit_bool: bool,
        deserialize_i8 => visit_i8: i8,
        deserialize_i16 => visit_i16: i16,
        deserialize_i32 => visit_i32: i32,
        deserialize_i64 => visit_i64: i64,
        deserialize_i128 => visit_i128: i128,
        deserialize_u8 => visit_u8: u8,
        deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32,
        deserialize_u64 => visit_u64: u64,
        deserialize_u128 => visit_u128: u128,
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _name: &'static str, _variants: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        visitor.visit_enum(self.0.into_deserializer())
    }

    forward_to_deserialize_any! {
        f32 f64 char str string bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

struct Variant<'de> {
    name: &'de str,
    content: Option<&'de Value>,
}

impl<'de> de::EnumAccess<'de> for Variant<'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: de::DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let name: de::value::BorrowedStrDeserializer<'de, Error> = de::value::BorrowedStrDeserializer::new(self.name);
        seed.deserialize(name).map(|variant| (variant, self))
    }
}

impl<'de> de::VariantAccess<'de> for Variant<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        match self.content {
            None | Some(Value::Null) => Ok(()),
            Some(other) => Err(de::Error::invalid_type(unexpected(other), &"unit variant")),
        }
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        match self.content {
            Some(content) => seed.deserialize(Reader(content)),
            None => Err(de::Error::invalid_type(de::Unexpected::UnitVariant, &"newtype variant")),
        }
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        match self.content {
            Some(content @ Value::Array(_)) => de::Deserializer::deserialize_any(Reader(content), visitor),
            Some(other) => Err(de::Error::invalid_type(unexpected(other), &"tuple variant")),
            None => Err(de::Error::invalid_type(de::Unexpected::UnitVariant, &"tuple variant")),
        }
    }

    fn struct_variant<V: Visitor<'de>>(self, _fields: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        match self.content {
            Some(content @ Value::Object(_)) => de::Deserializer::deserialize_any(Reader(content), visitor),
            Some(other) => Err(de::Error::invalid_type(unexpected(other), &"struct variant")),
            None => Err(de::Error::invalid_type(de::Unexpected::UnitVariant, &"struct variant")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::from_value;
    use crate::json_value::{from_json_value, value_to_json};
    use crate::value::Value;

    fn same_as_round_trip(value: &Value) {
        let direct: serde_json::Value = from_value(value).expect("reads directly");
        assert_eq!(direct, value_to_json(value));
    }

    fn value_of(json: serde_json::Value) -> Value {
        from_json_value(&json).expect("a value")
    }

    #[test]
    fn plain_values_read_as_their_json_reads() {
        for json in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(3),
            serde_json::json!(-7),
            serde_json::json!(2.5),
            serde_json::json!("text"),
            serde_json::json!([1, "two", null, [3.5]]),
            serde_json::json!({ "a": { "b": [1, 2, { "c": false }] }, "d": "e" }),
        ] {
            same_as_round_trip(&value_of(json));
        }
    }

    #[test]
    fn the_core_only_variants_read_as_their_json_forms() {
        same_as_round_trip(&Value::Int(9_007_199_254_740_993));
        same_as_round_trip(&Value::Number(1.0e300));
        same_as_round_trip(&Value::Number(f64::NAN));
        same_as_round_trip(&Value::Bytes(crate::value::Shared::new(vec![0, 1, 2, 250])));
    }

    #[test]
    fn a_resolved_program_reads_the_same_as_through_json() {
        let program = serde_json::json!({
            "name": "p", "typeContractVersion": 1,
            "orbitals": [{
                "name": "O",
                "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string", "required": true }, { "name": "n", "type": "number", "default": 0 }] },
                "traits": [{ "name": "T", "linkedEntity": "E", "config": { "k": { "type": "string", "default": "v" } }, "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "guard": ["=", "@entity.n", 0], "effects": [["set", "@entity.n", ["+", "@entity.n", 1.5]], ["render-ui", "main", { "type": "typography", "content": "@config.k" }]] }]
                } }],
                "pages": [{ "name": "P", "path": "/", "traits": [{ "ref": "T" }] }]
            }]
        });
        let as_value = value_of(program);
        let direct: crate::schema::types::OrbitalSchema = from_value(&as_value).expect("reads directly");
        let through_json: crate::schema::types::OrbitalSchema = from_json_value(&value_to_json(&as_value)).expect("reads through json");
        assert_eq!(serde_json::to_value(&direct).unwrap(), serde_json::to_value(&through_json).unwrap());
    }

    /// The other direction: a program written straight into a `Value` is the value its `serde_json` copy reads back as
    /// (P-049: `program/self` answered through the copy).
    #[test]
    fn a_program_written_straight_to_a_value_equals_its_json_round_trip() {
        let program: crate::schema::types::OrbitalSchema = from_json_value(&serde_json::json!({
            "name": "p", "typeContractVersion": 1,
            "orbitals": [{
                "name": "O",
                "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "n", "type": "number", "default": 0 }] },
                "traits": [{ "name": "T", "linkedEntity": "E", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["set", "@entity.n", ["+", "@entity.n", 1.5]], ["set", "@entity.n", 3]] }]
                } }],
                "pages": [{ "name": "P", "path": "/", "traits": [{ "ref": "T" }] }]
            }]
        }))
        .expect("a program");
        let direct = crate::value_serialize::to_value(&program).expect("writes directly");
        let through_json: Value = from_json_value(&serde_json::to_value(&program).unwrap()).expect("reads the copy");
        assert_eq!(value_to_json(&direct), value_to_json(&through_json));
        assert_eq!(direct, through_json);
    }

    #[derive(serde::Deserialize)]
    struct TwoLists {
        a: crate::value::Shared<Vec<crate::schema::types::PayloadField>>,
        b: crate::value::Shared<Vec<crate::schema::types::PayloadField>>,
    }

    fn fields() -> Value {
        value_of(serde_json::json!([{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }]))
    }

    #[test]
    fn one_shared_source_node_reads_into_one_shared_record() {
        let list = fields();
        let mut map = crate::value::ValueMap::default();
        map.insert("a".into(), list.clone());
        map.insert("b".into(), list);
        let read: TwoLists = from_value(&Value::object(map)).expect("reads");
        assert!(crate::value::Shared::ptr_eq(&read.a, &read.b), "the two holders of one source list hold two copies");
    }

    #[test]
    fn control_two_equal_but_separate_source_nodes_read_into_separate_records() {
        let mut map = crate::value::ValueMap::default();
        map.insert("a".into(), fields());
        map.insert("b".into(), fields());
        let read: TwoLists = from_value(&Value::object(map)).expect("reads");
        assert!(!crate::value::Shared::ptr_eq(&read.a, &read.b));
        assert_eq!(serde_json::to_value(&*read.a).unwrap(), serde_json::to_value(&*read.b).unwrap());
    }

    #[test]
    fn control_a_shared_record_read_through_json_is_its_own_copy() {
        let read: TwoLists = from_json_value(&serde_json::json!({ "a": [{ "name": "id", "type": "string" }], "b": [{ "name": "id", "type": "string" }] })).expect("reads");
        assert!(!crate::value::Shared::ptr_eq(&read.a, &read.b));
    }

    /// A program holds tens of thousands of these records; a rare nested part is boxed so it costs a pointer when absent.
    #[test]
    fn program_records_stay_small() {
        use crate::schema::types::*;
        assert!(std::mem::size_of::<PayloadField>() <= 192, "PayloadField grew to {}", std::mem::size_of::<PayloadField>());
        assert!(std::mem::size_of::<FieldDefinition>() <= 296, "FieldDefinition grew to {}", std::mem::size_of::<FieldDefinition>());
        assert!(std::mem::size_of::<ConfigField>() <= 336, "ConfigField grew to {}", std::mem::size_of::<ConfigField>());
    }

    #[test]
    fn control_a_shape_mismatch_is_refused_like_the_round_trip() {
        let value = value_of(serde_json::json!({ "name": 3 }));
        let direct = from_value::<crate::schema::types::OrbitalSchema>(&value);
        let through_json = from_json_value::<crate::schema::types::OrbitalSchema>(&value_to_json(&value));
        assert!(direct.is_err() && through_json.is_err());
    }
}
