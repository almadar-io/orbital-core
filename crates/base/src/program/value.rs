//! Value type for S-expression evaluation results
//!
//! The Value type represents the result of evaluating an S-expression.
//! It supports all JSON-compatible types plus additional runtime types.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::sexpr::SExpression;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::sync::Arc;

/// The fields of an object value, in insertion order: `object/keys`, `object/values` and `object/entries` list
/// them in the order the fields were written, as the JS evaluator does.
pub type ValueMap = crate::collections::IndexMap<Shared<String>, Value>;

/// An empty object map holding exactly `fields` entries, so a record of known width keeps no spare entry capacity.
pub fn value_map_with_capacity(fields: usize) -> ValueMap {
    ValueMap::with_capacity_and_hasher(fields, Default::default())
}

/// The one shared `Shared<String>` for a literal key: built on first use, then a reference-count bump per call.
#[macro_export]
macro_rules! static_key {
    ($text:literal) => {{
        static KEY: $crate::sync::OnceLock<$crate::value::Shared<String>> = $crate::sync::OnceLock::new();
        KEY.get_or_init(|| $crate::value::Shared::new($text.to_string())).clone()
    }};
}

/// A copy-on-write handle: cloning shares the data; a write through `DerefMut` copies only when it is shared.
/// It is how a value's lists and records are held, so reading one is a reference-count bump, not a deep copy,
/// and an accumulator written in a loop is updated in place.
#[derive(Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    pub fn new(value: T) -> Self {
        Shared(Arc::new(value))
    }

    /// Whether `a` and `b` hold the same allocation.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }

    /// The allocation's address: one shared allocation, however many handles reach it.
    pub fn as_ptr(this: &Self) -> *const T {
        Arc::as_ptr(&this.0)
    }

    /// Whether another handle shares this allocation.
    pub fn is_shared(this: &Self) -> bool {
        Arc::strong_count(&this.0) > 1
    }

    /// A handle to the allocation that does not keep it alive.
    pub fn downgrade(this: &Self) -> std::sync::Weak<T> {
        Arc::downgrade(&this.0)
    }
}

impl<T: Clone> Shared<T> {
    /// The data, moved out when this is its only handle, else copied.
    pub fn into_inner(self) -> T {
        Arc::unwrap_or_clone(self.0)
    }
}

impl<T: Clone + PartialEq> Shared<T> {
    /// Runs `edit` on a private copy and swaps it in only when it changed the data, so a pass that finds nothing to
    /// rewrite leaves every holder of this handle sharing one allocation.
    pub fn edit(&mut self, edit: impl FnOnce(&mut T)) {
        let mut copy = T::clone(&self.0);
        edit(&mut copy);
        if copy != *self.0 {
            self.0 = Arc::new(copy);
        }
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(Arc::clone(&self.0))
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> std::ops::DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

impl<T: fmt::Display> fmt::Display for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq<str> for Shared<String> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Shared<String> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for Shared<String> {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<Shared<String>> for String {
    fn eq(&self, other: &Shared<String>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Shared<String>> for str {
    fn eq(&self, other: &Shared<String>) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Shared<String>> for &str {
    fn eq(&self, other: &Shared<String>) -> bool {
        *self == other.as_str()
    }
}

impl From<Shared<String>> for String {
    fn from(text: Shared<String>) -> Self {
        text.into_inner()
    }
}

impl From<&Shared<String>> for String {
    fn from(text: &Shared<String>) -> Self {
        text.as_str().to_owned()
    }
}

impl AsRef<str> for Shared<String> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for Shared<String> {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<String> for Shared<String> {
    fn borrow(&self) -> &String {
        &self.0
    }
}


impl From<&str> for Shared<String> {
    fn from(text: &str) -> Self {
        Shared::new(text.to_string())
    }
}

impl<T: fmt::Debug> fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Shared::new(value)
    }
}

impl<T, A> FromIterator<A> for Shared<T>
where
    T: FromIterator<A>,
{
    fn from_iter<I: IntoIterator<Item = A>>(iter: I) -> Self {
        Shared::new(iter.into_iter().collect())
    }
}

impl<T: Clone + IntoIterator> IntoIterator for Shared<T> {
    type Item = T::Item;
    type IntoIter = T::IntoIter;
    fn into_iter(self) -> T::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Shared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        (&*self.0).into_iter()
    }
}

impl<'a, T: Clone> IntoIterator for &'a mut Shared<T>
where
    &'a mut T: IntoIterator,
{
    type Item = <&'a mut T as IntoIterator>::Item;
    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        Arc::make_mut(&mut self.0).into_iter()
    }
}

impl<T: Serialize> Serialize for Shared<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de> + 'static> Deserialize<'de> for Shared<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_newtype_struct(crate::value_deserialize::SHARED_NEWTYPE, SharedRead(std::marker::PhantomData))
    }
}

struct SharedRead<T>(std::marker::PhantomData<T>);

impl<'de, T: Deserialize<'de> + 'static> serde::de::Visitor<'de> for SharedRead<T> {
    type Value = Shared<T>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a shared value")
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, deserializer: D) -> Result<Shared<T>, D::Error> {
        let node = crate::value_deserialize::take_shared_node();
        if let Some(read) = crate::value_deserialize::shared_read::<Shared<T>>(node) {
            return Ok(read);
        }
        let read = Shared::new(T::deserialize(deserializer)?);
        crate::value_deserialize::shared_keep(node, &read);
        Ok(read)
    }
    // A format that reads a newtype as its content hands the content here; it is read as `T` reads it.
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::BoolDeserializer::new(v)).map(Shared::new)
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::I64Deserializer::new(v)).map(Shared::new)
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::U64Deserializer::new(v)).map(Shared::new)
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::F64Deserializer::new(v)).map(Shared::new)
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::StrDeserializer::new(v)).map(Shared::new)
    }

    fn visit_borrowed_str<E: serde::de::Error>(self, v: &'de str) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::BorrowedStrDeserializer::new(v)).map(Shared::new)
    }

    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::StringDeserializer::new(v)).map(Shared::new)
    }

    fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::BytesDeserializer::new(v)).map(Shared::new)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Shared<T>, E> {
        T::deserialize(serde::de::value::UnitDeserializer::new()).map(Shared::new)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Shared<T>, A::Error> {
        T::deserialize(serde::de::value::SeqAccessDeserializer::new(seq)).map(Shared::new)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Shared<T>, A::Error> {
        T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Shared::new)
    }

    fn visit_enum<A: serde::de::EnumAccess<'de>>(self, data: A) -> Result<Shared<T>, A::Error> {
        T::deserialize(serde::de::value::EnumAccessDeserializer::new(data)).map(Shared::new)
    }
}

/// Runtime value resulting from S-expression evaluation
#[derive(Debug, Clone, Default)]
pub enum Value {
    /// Null/undefined value
    #[default]
    Null,
    /// Boolean value
    Bool(bool),
    /// A floating-point number
    Number(f64),
    /// An exact 64-bit integer (EXEC-EVAL-11)
    Int(i64),
    /// A byte string; on the wire and at rest it is standard padded base64 text (EXEC-EVAL-11)
    Bytes(Shared<Vec<u8>>),
    /// String value
    String(Shared<String>),
    /// Array of values
    Array(Shared<Vec<Value>>),
    /// Object/map of values
    Object(Shared<ValueMap>),
    /// Lambda function (params, body)
    Lambda(Arc<Lambda>),
    /// A record only the kernel can make: readable like an object, never built from a literal, a parse, a cast or a
    /// wire payload (EXEC-IDENTITY-2).
    Sealed(Arc<Sealed>),
    /// A value whose bytes only the host holds (EXEC-DIST-16b): held, stored, compared and written by its id; no read
    /// reaches inside it in this kernel.
    Blob(Arc<crate::blobs::Blob>),
}

/// `new` sharing `old`'s allocations where they are equal, and whether the result is `old` itself.
fn share_unchanged(new: Value, old: &Value) -> (Value, bool) {
    match (new, old) {
        (Value::Object(fields), Value::Object(before)) => {
            if Shared::ptr_eq(&fields, before) {
                return (old.clone(), true);
            }
            let mut fields = fields.into_inner();
            let mut whole = fields.len() == before.len();
            for (index, (key, value)) in fields.iter_mut().enumerate() {
                // The same key at the same place is the common case: found without hashing the key.
                let (was, placed) = match before.get_index(index).filter(|(at, _)| *at == key) {
                    Some((_, was)) => (was, true),
                    None => match before.get_full(key) {
                        Some((at, _, was)) => (was, at == index),
                        None => {
                            whole = false;
                            continue;
                        }
                    },
                };
                let (shared, same) = share_unchanged(std::mem::take(value), was);
                *value = shared;
                whole &= same && placed;
            }
            if whole {
                (old.clone(), true)
            } else {
                (Value::Object(Shared::new(fields)), false)
            }
        }
        (Value::Array(items), Value::Array(before)) => {
            if Shared::ptr_eq(&items, before) {
                return (old.clone(), true);
            }
            let mut by_id: Option<crate::collections::IndexMap<&str, &Value>> = None;
            let mut items = items.into_inner();
            let mut whole = items.len() == before.len();
            for (index, item) in items.iter_mut().enumerate() {
                // An element that is the allocation held at its place needs no id index.
                let held = before.get(index).filter(|at| at.shares_allocation(item));
                let was = match (held, element_id(item)) {
                    (Some(at), _) => Some(at),
                    (None, Some(id)) => by_id
                        .get_or_insert_with(|| before.iter().filter_map(|item| Some((element_id(item)?, item))).collect())
                        .get(id)
                        .copied(),
                    (None, None) => before.get(index),
                };
                let Some(was) = was else {
                    whole = false;
                    continue;
                };
                let (shared, same) = share_unchanged(std::mem::take(item), was);
                *item = shared;
                whole &= same && before.get(index).is_some_and(|at| std::ptr::eq(at, was));
            }
            if whole {
                (old.clone(), true)
            } else {
                (Value::Array(Shared::new(items)), false)
            }
        }
        (Value::String(text), Value::String(before)) => {
            if text == *before {
                (old.clone(), true)
            } else {
                (Value::String(text), false)
            }
        }
        (new @ (Value::Null | Value::Bool(_)), _)
        | (new @ (Value::Number(_) | Value::Int(_)), _) => {
            let same = match (&new, old) {
                (Value::Null, Value::Null) => true,
                (Value::Bool(a), Value::Bool(b)) => a == b,
                (Value::Number(a), Value::Number(b)) => a.to_bits() == b.to_bits(),
                (Value::Int(a), Value::Int(b)) => a == b,
                _ => false,
            };
            (new, same)
        }
        (new, _) => (new, false),
    }
}

fn element_id(item: &Value) -> Option<&str> {
    match item {
        Value::Object(fields) => match fields.get("id") {
            Some(Value::String(id)) => Some(id.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// The largest magnitude below which every integer is exactly a float.
pub const MAX_EXACT_INT: i64 = 1 << 53;

/// `f` as the int it exactly is, if it is one.
pub fn exact_int(f: f64) -> Option<i64> {
    (f.fract() == 0.0 && f.abs() <= MAX_EXACT_INT as f64).then_some(f as i64)
}

/// `i` as the float it exactly is, if it is one.
pub fn exact_float(i: i64) -> Option<f64> {
    (i.unsigned_abs() <= MAX_EXACT_INT as u64).then_some(i as f64)
}

/// The numeric order of an int and a float, exact for every pair; `None` for NaN.
fn cmp_int_float(i: i64, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if f.is_nan() {
        return None;
    }
    if f >= 9_223_372_036_854_775_808.0 {
        return Some(Ordering::Less);
    }
    if f < -9_223_372_036_854_775_808.0 {
        return Some(Ordering::Greater);
    }
    let whole = f.trunc();
    match i.cmp(&(whole as i64)) {
        Ordering::Equal => 0.0f64.partial_cmp(&(f - whole)),
        other => Some(other),
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Int(i), Value::Number(f)) | (Value::Number(f), Value::Int(i)) => {
                cmp_int_float(*i, *f) == Some(std::cmp::Ordering::Equal)
            }
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Object(a), Value::Object(b)) => a == b,
            (Value::Lambda(a), Value::Lambda(b)) => a == b,
            (Value::Sealed(a), Value::Sealed(b)) => a == b,
            (Value::Blob(a), Value::Blob(b)) => a.id() == b.id(),
            _ => false,
        }
    }
}

/// The record of a [`Value::Sealed`] and the name of the sealed type it is (`Verified`).
#[derive(Debug, Clone, PartialEq)]
pub struct Sealed {
    kind: &'static str,
    fields: ValueMap,
}

impl Sealed {
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn fields(&self) -> &ValueMap {
        &self.fields
    }
}

/// The parameters and body of a lambda, shared behind one pointer so `Value` stays 16 bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Lambda {
    pub params: Vec<String>,
    pub body: Arc<SExpression>,
}

// Custom Serialize implementation that handles Lambda by converting to SExpression
impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Value::Null => serializer.serialize_none(),
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Number(n) => serializer.serialize_f64(*n),
            Value::Int(i) => serializer.serialize_i64(*i),
            Value::Bytes(b) => serializer.serialize_str(&crate::codec::base64_encode(b)),
            Value::String(s) => serializer.serialize_str(s),
            Value::Array(arr) => match crate::held_values::write(self, serializer) {
                crate::held_values::Held::Done(out) => out,
                crate::held_values::Held::Inline(serializer) => arr.serialize(serializer),
            },
            Value::Object(obj) => match crate::held_values::write(self, serializer) {
                crate::held_values::Held::Done(out) => out,
                crate::held_values::Held::Inline(serializer) => obj.serialize(serializer),
            },
            Value::Sealed(sealed) => sealed.fields.serialize(serializer),
            Value::Blob(blob) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(crate::blobs::BLOB_MARKER, &blob.id())?;
                map.end()
            }
            Value::Lambda(_) => {
                // Serialize lambda as ["fn", params, body]
                let sexpr = SExpression::try_from(self.clone()).map_err(serde::ser::Error::custom)?;
                sexpr.serialize(serializer)
            }
        }
    }
}

// Custom Deserialize implementation
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        crate::intern::scope(|| deserializer.deserialize_any(ValueVisitor))
    }
}

/// One visit per node, building the `Value` directly: the same structure `Value::from(SExpression)` gives, without
/// the intermediate tree.
struct ValueVisitor;

impl<'de> serde::de::Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value: null, a boolean, a number, a string, an array or an object")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Int(value))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(i64::try_from(value).map_or(Value::Number(value as f64), Value::Int))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(Value::Number(value))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(crate::intern::shared(value)))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(crate::intern::shared_owned(value)))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(item) = seq.next_element::<Value>()? {
            items.push(item);
        }
        Ok(crate::intern::value_list(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = ValueMap::with_capacity_and_hasher(map.size_hint().unwrap_or(0).min(4096), Default::default());
        let Some(first) = map.next_key::<String>()? else {
            return Ok(crate::intern::value_record(object));
        };
        if first == crate::blobs::BLOB_MARKER && crate::blobs::reading() {
            let id: u64 = map.next_value()?;
            if map.next_key::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom("a $blob marker has no other keys"));
            }
            return crate::blobs::handle(id).map(Value::Blob).map_err(serde::de::Error::custom);
        }
        if first == crate::def_refs::DEF_MARKER && crate::def_refs::reading() {
            let id: u64 = map.next_value()?;
            return match map.next_key::<String>()? {
                None => crate::intern::defined_value(id).map_err(serde::de::Error::custom),
                Some(key) if key == "value" => {
                    let node: Value = map.next_value()?;
                    if map.next_key::<serde::de::IgnoredAny>()?.is_some() {
                        return Err(serde::de::Error::custom(format!("`$def` {id} carries a key besides `value`")));
                    }
                    crate::intern::define_value(id, node.clone()).map_err(serde::de::Error::custom)?;
                    Ok(node)
                }
                Some(key) => Err(serde::de::Error::custom(format!("`$def` {id} carries `{key}`, not `value`"))),
            };
        }
        object.insert(crate::intern::shared_owned(first), map.next_value::<Value>()?);
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            object.insert(crate::intern::shared_owned(key), value);
        }
        Ok(crate::intern::value_record(object))
    }
}

/// The segments of a value path: `.` separates them, `\.` is a literal dot and `\\` a literal backslash inside a
/// segment (so `http\.method` is the one key `http.method`); any other backslash stays as written. A path with no
/// backslash is walked in place, with no allocation.
pub enum PathSegments<'a> {
    Plain(std::str::Split<'a, char>),
    Escaped(std::vec::IntoIter<std::borrow::Cow<'a, str>>),
}

impl<'a> Iterator for PathSegments<'a> {
    type Item = std::borrow::Cow<'a, str>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Plain(parts) => parts.next().map(std::borrow::Cow::Borrowed),
            Self::Escaped(parts) => parts.next(),
        }
    }
}

pub fn path_segments(path: &str) -> PathSegments<'_> {
    use std::borrow::Cow;
    if !path.contains('\\') {
        return PathSegments::Plain(path.split('.'));
    }
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => segments.push(Cow::Owned(std::mem::take(&mut current))),
            '\\' => match chars.peek() {
                Some(&next @ ('.' | '\\')) => {
                    current.push(next);
                    chars.next();
                }
                _ => current.push('\\'),
            },
            other => current.push(other),
        }
    }
    segments.push(Cow::Owned(current));
    PathSegments::Escaped(segments.into_iter())
}

/// [`path_segments`] collected, for a caller that indexes the segments.
pub fn split_path(path: &str) -> Vec<std::borrow::Cow<'_, str>> {
    path_segments(path).collect()
}

impl Value {
    pub fn array(items: impl Into<Shared<Vec<Value>>>) -> Value {
        Value::Array(items.into())
    }

    pub fn object(fields: impl Into<Shared<ValueMap>>) -> Value {
        Value::Object(fields.into())
    }

    pub fn string(text: impl Into<Shared<String>>) -> Value {
        Value::String(text.into())
    }

    pub fn bytes(data: impl Into<Shared<Vec<u8>>>) -> Value {
        Value::Bytes(data.into())
    }

    /// This value with every subtree that equals its counterpart in `previous` replaced by `previous`'s allocation,
    /// so a re-render that changed one leaf leaves every unchanged sibling's identity intact. Record fields pair by
    /// key; list elements pair by their string `id` field when they have one, else by position.
    pub fn shared_with(self, previous: &Value) -> Value {
        share_unchanged(self, previous).0
    }

    /// Whether this list or record is `other`'s allocation.
    pub fn shares_allocation(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Object(a), Value::Object(b)) => Shared::ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => Shared::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Equality that takes a shared allocation as equal without reading it: what a held value compared with its
    /// next version costs is then the part that changed.
    pub fn equals_sharing(&self, other: &Value) -> bool {
        if self.shares_allocation(other) {
            return true;
        }
        match (self, other) {
            (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| a.equals_sharing(b)),
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter().enumerate().all(|(index, (key, value))| {
                        match b.get_index(index).filter(|(at, _)| *at == key) {
                            Some((_, held)) => value.equals_sharing(held),
                            None => b.get(key).is_some_and(|held| value.equals_sharing(held)),
                        }
                    })
            }
            _ => self == other,
        }
    }

    /// A sealed record of the type `kind`. Only a kernel operator calls this; no wire form, parse or cast reaches it.
    pub fn sealed(kind: &'static str, fields: ValueMap) -> Value {
        Value::Sealed(Arc::new(Sealed { kind, fields }))
    }

    /// Whether this is a sealed record of the type `kind`.
    pub fn is_sealed_as(&self, kind: &str) -> bool {
        matches!(self, Value::Sealed(sealed) if sealed.kind == kind)
    }

    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Try to get as boolean
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// This value as a float: a number, or an int promoted to the nearest float (EXEC-EVAL-11).
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// This value as an int: an int, or a number that is exactly one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Number(n) => exact_int(*n),
            _ => None,
        }
    }

    /// Whether this is a number or an int.
    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Number(_) | Value::Int(_))
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b.as_slice()),
            _ => None,
        }
    }

    /// Try to get as string reference
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Convert to string representation
    pub fn to_string_value(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => crate::str_utils::js_number_string(*n),
            Value::Int(i) => i.to_string(),
            Value::Bytes(b) => crate::codec::base64_encode(b),
            Value::String(s) => s.clone().into_inner(),
            Value::Array(_) | Value::Object(_) | Value::Sealed(_) | Value::Blob(_) => crate::json_value::js_json_string(self),
            Value::Lambda(l) => format!("<fn({})>", l.params.join(", ")),
        }
    }

    /// Try to get as array reference
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Try to get as mutable array reference
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Try to get as object reference
    pub fn as_object(&self) -> Option<&ValueMap> {
        match self {
            Value::Object(o) => Some(o),
            Value::Sealed(sealed) => Some(&sealed.fields),
            _ => None,
        }
    }

    /// Try to get as mutable object reference
    pub fn as_object_mut(&mut self) -> Option<&mut ValueMap> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// The field `key` of an object value; `None` for a missing field or a value that is not an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object().and_then(|fields| fields.get(key))
    }

    /// [`Value::get`], mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.as_object_mut().and_then(|fields| fields.get_mut(key))
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// The value, leaving `Null` in its place.
    pub fn take(&mut self) -> Value {
        std::mem::take(self)
    }

    /// Get a nested value by path (e.g., "address.city")
    pub fn get_path(&self, path: &str) -> Option<&Value> {
        let mut current = self;

        for part in path_segments(path) {
            match current {
                Value::Object(obj) => {
                    current = obj.get(part.as_ref())?;
                }
                Value::Sealed(sealed) => {
                    current = sealed.fields.get(part.as_ref())?;
                }
                Value::Array(arr) => {
                    let index: usize = part.parse().ok()?;
                    current = arr.get(index)?;
                }
                _ => return None,
            }
        }

        Some(current)
    }

    /// Owned variant of [`get_path`] that also resolves the virtual array
    /// properties `length` / `count` (e.g. `@payload.data.length`). The borrowed
    /// `get_path` can't return a computed length (it yields `&Value`), so
    /// binding resolution (`BindingContext::resolve`) uses this. A `length`/
    /// `count` segment on a non-array, or anything past a length, yields `None`
    /// (length is terminal). Numeric segments still index; object segments still
    /// look up keys.
    pub fn get_path_owned(&self, path: &str) -> Option<Value> {
        self.walk_path_owned(&split_path(path), false)
    }

    /// [`Value::get_path_owned`] that reads through a `(quote x)` value (`["quote", <body>]`, G-CROSS-041): the path
    /// continues inside the decoded `x`, so a quoted config value answers `machine.transitions` as it answers
    /// `machine` (G-CROSS-076). Only an authored value is walked this way; runtime data keeps its shape.
    pub fn get_path_owned_through_quotes(&self, path: &str) -> Option<Value> {
        self.walk_path_owned(&split_path(path), true)
    }

    fn walk_path_owned(&self, segments: &[std::borrow::Cow<'_, str>], through_quotes: bool) -> Option<Value> {
        let mut current: &Value = self;

        for (i, part) in segments.iter().enumerate() {
            let last = i + 1 == segments.len();
            let seg: &str = part.as_ref();
            match current {
                Value::Object(obj) => {
                    current = obj.get(seg)?;
                }
                Value::Sealed(sealed) => {
                    current = sealed.fields.get(seg)?;
                }
                Value::Array(arr) => {
                    if through_quotes {
                        if let [Value::String(head), Value::String(body)] = arr.as_slice() {
                            if head.as_str() == crate::schema::quote::QUOTE_HEAD {
                                let decoded = Value::from(crate::schema::quote::decode_quote_body(body)?);
                                return decoded.walk_path_owned(&segments[i..], true);
                            }
                        }
                    }
                    if seg == "length" || seg == "count" {
                        return if last {
                            Some(Value::Number(arr.len() as f64))
                        } else {
                            None
                        };
                    }
                    let index: usize = seg.parse().ok()?;
                    current = arr.get(index)?;
                }
                _ => return None,
            }
        }

        Some(current.clone())
    }

    /// Set a nested value by path
    pub fn set_path(&mut self, path: &str, value: Value) -> bool {
        let parts = split_path(path);
        if parts.is_empty() {
            return false;
        }

        let mut current = self;

        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                // Last part - set the value
                match current {
                    Value::Object(obj) => {
                        obj.insert(part.to_string().into(), value);
                        return true;
                    }
                    Value::Array(arr) => {
                        if let Ok(index) = part.parse::<usize>() {
                            if index < arr.len() {
                                arr[index] = value;
                                return true;
                            }
                        }
                        return false;
                    }
                    _ => return false,
                }
            } else {
                // Intermediate part - navigate
                match current {
                    Value::Object(obj) => {
                        if !obj.contains_key(part.as_ref()) {
                            obj.insert(part.to_string().into(), Value::object(ValueMap::default()));
                        }
                        match obj.get_mut(part.as_ref()) {
                            Some(v) => current = v,
                            None => return false,
                        }
                    }
                    Value::Array(arr) => {
                        let index: usize = match part.parse() {
                            Ok(i) => i,
                            Err(_) => return false,
                        };
                        match arr.get_mut(index) {
                            Some(v) => current = v,
                            None => return false,
                        }
                    }
                    _ => return false,
                }
            }
        }

        false
    }

    /// Get the type name of this value
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::Int(_) => "int",
            Value::Bytes(_) => "bytes",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Lambda(_) => "function",
            Value::Sealed(sealed) => sealed.kind,
            Value::Blob(_) => "blob",
        }
    }

    pub fn lambda(params: Vec<String>, body: Arc<SExpression>) -> Self {
        Value::Lambda(Arc::new(Lambda { params, body }))
    }

    /// Check if value is a lambda
    pub fn is_lambda(&self) -> bool {
        matches!(self, Value::Lambda(_))
    }

    /// Try to get lambda params and body
    pub fn as_lambda(&self) -> Option<(&[String], &SExpression)> {
        match self {
            Value::Lambda(l) => Some((l.params.as_slice(), l.body.as_ref())),
            _ => None,
        }
    }

    /// A type mismatch naming `expected` and this value's kind.
    pub fn kind_error(&self, expected: &str) -> crate::error::EvalError {
        crate::error::EvalError::TypeMismatch { expected: expected.to_string(), actual: self.type_name().to_string() }
    }

    /// This value as a number, or a type mismatch: no other kind reads as a number.
    pub fn number(&self) -> Result<f64, crate::error::EvalError> {
        self.as_number().ok_or_else(|| self.kind_error("number"))
    }

    /// This value as an int, or a type mismatch: a number reads as an int only when it is exactly one.
    pub fn int(&self) -> Result<i64, crate::error::EvalError> {
        self.as_int().ok_or_else(|| self.kind_error("int"))
    }

    /// This value as bytes, or a type mismatch: text is never bytes.
    pub fn byte_string(&self) -> Result<&[u8], crate::error::EvalError> {
        self.as_bytes().ok_or_else(|| self.kind_error("bytes"))
    }

    /// The bytes a `bytes`-typed argument holds: a bytes value, or the standard padded base64 text a `bytes`
    /// literal, default or stored row is written in (EXEC-EVAL-11). Any other value is a type mismatch.
    pub fn bytes_view(&self) -> Result<std::borrow::Cow<'_, [u8]>, crate::error::EvalError> {
        match self {
            Value::Bytes(b) => Ok(std::borrow::Cow::Borrowed(b.as_slice())),
            Value::String(text) => crate::codec::base64_decode(text).map(std::borrow::Cow::Owned).map_err(|err| {
                crate::error::EvalError::InvalidArgument(format!("bytes: not standard padded base64: {err}"))
            }),
            other => Err(other.kind_error("bytes")),
        }
    }

    /// This value as text, or a type mismatch: no other kind reads as text.
    pub fn text(&self) -> Result<&str, crate::error::EvalError> {
        self.as_str().ok_or_else(|| self.kind_error("string"))
    }

    /// This value as a boolean, or a type mismatch: no other kind is true or false.
    pub fn boolean(&self) -> Result<bool, crate::error::EvalError> {
        self.as_bool().ok_or_else(|| self.kind_error("boolean"))
    }

    /// The order of two values of one orderable kind (number, string, boolean); any other pair is a type mismatch.
    pub fn compare(&self, other: &Value) -> Result<std::cmp::Ordering, crate::error::EvalError> {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => {
                a.partial_cmp(b).ok_or_else(|| crate::error::EvalError::InvalidArgument("NaN has no order".to_string()))
            }
            (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
            (Value::Int(i), Value::Number(f)) => cmp_int_float(*i, *f)
                .ok_or_else(|| crate::error::EvalError::InvalidArgument("NaN has no order".to_string())),
            (Value::Number(f), Value::Int(i)) => cmp_int_float(*i, *f)
                .map(std::cmp::Ordering::reverse)
                .ok_or_else(|| crate::error::EvalError::InvalidArgument("NaN has no order".to_string())),
            (Value::String(a), Value::String(b)) => Ok(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Ok(a.cmp(b)),
            (a, b) => Err(a.kind_error(b.type_name())),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Number(n) => {
                if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{}", n)
                }
            }
            Value::Int(i) => write!(f, "{}", i),
            Value::Bytes(b) => write!(f, "{}", crate::codec::base64_encode(b)),
            Value::String(s) => write!(f, "{}", s),
            Value::Array(arr) => {
                write!(f, "[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Object(obj) => {
                write!(f, "{{")?;
                for (i, (k, v)) in obj.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
            Value::Lambda(l) => {
                write!(f, "<fn({})>", l.params.join(", "))
            }
            Value::Sealed(sealed) => write!(f, "<{}>", sealed.kind),
            Value::Blob(blob) => write!(f, "<blob {}>", blob.id()),
        }
    }
}

// Conversion from SExpression to Value
impl From<SExpression> for Value {
    fn from(expr: SExpression) -> Self {
        match expr {
            SExpression::Null => Value::Null,
            SExpression::Bool(b) => Value::Bool(b),
            SExpression::Number(n) => Value::Number(n),
            SExpression::String(s) => Value::string(s),
            SExpression::Array(arr) => Value::Array(arr.into_inner().into_iter().map(Value::from).collect()),
            SExpression::Object(obj) => {
                Value::Object(obj.into_inner().into_iter().map(|(k, v)| (k, Value::from(v))).collect())
            }
        }
    }
}

// Conversion from Value to SExpression: an int becomes a literal only while it is exactly a float, so a value is
// never rounded into code; bytes become their wire text, the form a `bytes` literal is written in.
impl TryFrom<Value> for SExpression {
    type Error = crate::error::EvalError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Ok(match value {
            Value::Null => SExpression::Null,
            Value::Bool(b) => SExpression::Bool(b),
            Value::Number(n) => SExpression::Number(n),
            Value::Int(i) => SExpression::Number(exact_float(i).ok_or_else(|| {
                crate::error::EvalError::InvalidArgument(format!("the int {i} is beyond 2^53 and has no exact literal"))
            })?),
            Value::Bytes(b) => SExpression::string(crate::codec::base64_encode(&b)),
            Value::String(s) => SExpression::string(s.into_inner()),
            Value::Array(arr) => {
                SExpression::Array(arr.into_iter().map(SExpression::try_from).collect::<Result<_, _>>()?)
            }
            Value::Object(obj) => SExpression::object(
                obj.into_iter()
                    .map(|(k, v)| Ok((k.into(), SExpression::try_from(v)?)))
                    .collect::<Result<_, Self::Error>>()?,
            ),
            Value::Sealed(sealed) => SExpression::try_from(Value::object(Arc::unwrap_or_clone(sealed).fields))?,
            Value::Blob(blob) => {
                return Err(crate::error::EvalError::InvalidArgument(format!("blob {} is held by the host and has no literal here", blob.id())))
            }
            // Lambda cannot be directly converted to SExpression (would need ["fn", params, body])
            Value::Lambda(l) => {
                let Lambda { params, body } = Arc::unwrap_or_clone(l);
                let mut arr = vec![SExpression::string("fn")];
                if params.len() == 1 {
                    arr.push(SExpression::string(params[0].clone()));
                } else {
                    arr.push(SExpression::Array(
                        params.into_iter().map(SExpression::string).collect(),
                    ));
                }
                arr.push((*body).clone());
                SExpression::Array(arr.into())
            }
        })
    }
}

static NULL: Value = Value::Null;

/// `value["field"]`: the field of an object, `Null` when absent or when the value is not an object.
impl std::ops::Index<&str> for Value {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
}

/// `value["field"] = x`: a `Null` value becomes an object, a missing field is added as `Null`; any other value is
/// not indexable by name and panics.
impl std::ops::IndexMut<&str> for Value {
    fn index_mut(&mut self, key: &str) -> &mut Value {
        if self.is_null() {
            *self = Value::object(ValueMap::default());
        }
        match self {
            Value::Object(fields) => fields.entry(key.to_string().into()).or_insert(Value::Null),
            other => panic!("cannot index a {} value by field name `{key}`", other.type_name()),
        }
    }
}

/// `value[i]`: the element of an array, `Null` when out of range or when the value is not an array.
impl std::ops::Index<usize> for Value {
    type Output = Value;
    fn index(&self, at: usize) -> &Value {
        self.as_array().and_then(|items| items.get(at)).unwrap_or(&NULL)
    }
}

impl std::ops::IndexMut<usize> for Value {
    fn index_mut(&mut self, at: usize) -> &mut Value {
        match self {
            Value::Array(items) => items.get_mut(at).unwrap_or_else(|| panic!("array index {at} is out of range")),
            other => panic!("cannot index a {} value by position {at}", other.type_name()),
        }
    }
}

// Convenience conversions
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Int(n.into())
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::string(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::string(s)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Value::Array(v.into_iter().map(Into::into).collect())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(opt: Option<T>) -> Self {
        match opt {
            Some(v) => v.into(),
            None => Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Shared, Value, ValueMap};

    fn parsed(text: &str) -> Value {
        from_json_str(text).unwrap()
    }

    fn items(value: &Value) -> &[Value] {
        match value {
            Value::Array(items) => items,
            other => panic!("a list, not {other:?}"),
        }
    }

    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Object(a), Value::Object(b)) => Shared::ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => Shared::ptr_eq(a, b),
            (Value::String(a), Value::String(b)) => Shared::ptr_eq(a, b),
            _ => false,
        }
    }

    #[test]
    fn a_value_shared_with_its_previous_paint_reuses_every_unchanged_subtree() {
        let old = parsed(
            r#"{"type": "canvas", "drawables": [{"id": "a", "x": 1}, {"id": "b", "x": 2}, {"id": "c", "x": 3}]}"#,
        );
        let new = parsed(
            r#"{"type": "canvas", "drawables": [{"id": "a", "x": 1}, {"id": "b", "x": 20}, {"id": "c", "x": 3}]}"#,
        );
        let shared = new.clone().shared_with(&old);
        assert_eq!(shared, new, "sharing never changes the value");
        let (o, s) = (
            items(old.get("drawables").unwrap()),
            items(shared.get("drawables").unwrap()),
        );
        assert!(
            same(&o[0], &s[0]) && same(&o[2], &s[2]),
            "unchanged drawables are the previous allocations"
        );
        assert!(!same(&o[1], &s[1]), "control: the changed drawable is new");
        assert!(
            !same(&old, &shared),
            "control: a container holding a change is new"
        );
        let again = parsed(
            r#"{"type": "canvas", "drawables": [{"id": "a", "x": 1}, {"id": "b", "x": 2}, {"id": "c", "x": 3}]}"#,
        );
        assert!(
            same(&old, &again.shared_with(&old)),
            "an equal value is the previous allocation whole"
        );
    }

    #[test]
    fn a_drawable_keeps_its_allocation_when_one_is_inserted_before_it() {
        let old = parsed(r#"[{"id": "a", "x": 1}, {"id": "b", "x": 2}]"#);
        let new = parsed(r#"[{"id": "z", "x": 0}, {"id": "a", "x": 1}, {"id": "b", "x": 2}]"#);
        let shared = new.shared_with(&old);
        let (o, s) = (items(&old), items(&shared));
        assert!(
            same(&o[0], &s[1]) && same(&o[1], &s[2]),
            "elements with an id are matched by id, not position"
        );
        let plain_old = parsed(r#"[[1, 2], [3, 4]]"#);
        let plain = parsed(r#"[[0], [1, 2], [3, 4]]"#).shared_with(&plain_old);
        assert!(
            !same(&items(&plain_old)[0], &items(&plain)[1]),
            "control: without ids elements are matched by position"
        );
    }

    #[test]
    fn elements_already_the_previous_allocations_stay_them_beside_a_changed_one_and_a_moved_key() {
        let old = parsed(r#"{"k": 1, "list": [{"id": "a", "x": 1}, {"id": "b", "x": 2}, [7]]}"#);
        let kept = items(old.get("list").unwrap());
        let mut fields = value_map_with_capacity(2);
        fields.insert("list".to_string().into(), Value::array(vec![kept[0].clone(), parsed(r#"{"id": "b", "x": 3}"#), kept[2].clone()]));
        fields.insert("k".to_string().into(), parsed("1"));
        let new = Value::object(fields);
        let shared = new.clone().shared_with(&old);
        assert_eq!(shared, new, "sharing never changes the value");
        let s = items(shared.get("list").unwrap());
        assert!(same(&kept[0], &s[0]) && same(&kept[2], &s[2]), "an element held at its place stays that allocation");
        assert!(!same(&kept[1], &s[1]), "control: the changed element is new");
        assert!(!same(&old, &shared), "control: keys in another order are another record");
        let mut again = value_map_with_capacity(2);
        again.insert("k".to_string().into(), parsed("1"));
        again.insert("list".to_string().into(), Value::array(kept.to_vec()));
        assert!(same(&old, &Value::object(again).shared_with(&old)), "the same elements under the same keys are the previous record whole");
    }

    #[test]
    fn equality_by_sharing_answers_as_equality_does() {
        let a = parsed(r#"{"k": 1, "list": [{"id": "a", "x": 1}, [7], "t"]}"#);
        let cases = [
            (r#"{"k": 1, "list": [{"id": "a", "x": 1}, [7], "t"]}"#, true),
            (r#"{"list": [{"id": "a", "x": 1}, [7], "t"], "k": 1}"#, true),
            (r#"{"k": 1, "list": [{"id": "a", "x": 2}, [7], "t"]}"#, false),
            (r#"{"k": 1, "list": [{"id": "a", "x": 1}, [7]]}"#, false),
            (r#"{"k": 1, "list": [{"id": "a", "x": 1}, [7], "t"], "more": null}"#, false),
            (r#"{"k": 1, "other": [{"id": "a", "x": 1}, [7], "t"]}"#, false),
            (r#"{"k": 1.0, "list": [{"x": 1, "id": "a"}, [7], "t"]}"#, true),
        ];
        for (text, equal) in cases {
            let b = parsed(text);
            assert_eq!(a == b, equal, "control: {text}");
            assert_eq!(a.equals_sharing(&b), equal, "{text}");
            assert_eq!(b.equals_sharing(&a), equal, "{text}, the other way");
        }
        assert!(a.equals_sharing(&a.clone()), "a shared allocation is equal");
    }

    #[test]
    fn value_from_text_holds_equal_strings_once() {
        let value = parsed(r#"["idle", "idle", {"state": "idle"}]"#);
        let list = items(&value);
        let (Value::String(a), Value::String(b)) = (&list[0], &list[1]) else { panic!("strings") };
        assert!(Shared::ptr_eq(a, b), "equal text read twice is one allocation");
        let Value::Object(map) = &list[2] else { panic!("object") };
        let Some(Value::String(c)) = map.get("state") else { panic!("a string field") };
        assert!(Shared::ptr_eq(a, c), "a field value shares it too");
    }

    #[test]
    fn value_from_text_holds_equal_keys_once() {
        let value = parsed(r#"[{"from": 1}, {"from": 2}]"#);
        let list = items(&value);
        let key = |v: &Value| match v {
            Value::Object(map) => map.keys().next().unwrap().clone(),
            other => panic!("object, not {other:?}"),
        };
        assert!(Shared::ptr_eq(&key(&list[0]), &key(&list[1])));
    }

    #[test]
    fn value_from_text_holds_equal_subtrees_once() {
        let value = parsed(r#"[{"effects": [["set", "@x", "a"]]}, {"effects": [["set", "@x", "a"]]}]"#);
        let list = items(&value);
        let (Value::Object(a), Value::Object(b)) = (&list[0], &list[1]) else { panic!("objects") };
        assert!(Shared::ptr_eq(a, b), "an equal record read twice is one node");
    }

    #[test]
    fn control_value_from_text_keeps_different_subtrees_apart_and_equal_to_a_built_value() {
        let value = parsed(r#"[{"effects": [["set", "@x", "a"]]}, {"effects": [["set", "@x", "b"]]}]"#);
        let list = items(&value);
        let (Value::Object(a), Value::Object(b)) = (&list[0], &list[1]) else { panic!("objects") };
        assert!(!Shared::ptr_eq(a, b));
        let built = |last: &str| {
            Value::object(ValueMap::from_iter([(
                "effects".to_string().into(),
                Value::array(vec![Value::array(vec![Value::string("set".to_string()), Value::string("@x".to_string()), Value::string(last.to_string())])]),
            )]))
        };
        assert_eq!(value, Value::array(vec![built("a"), built("b")]));
    }

    #[test]
    fn control_values_read_by_two_parses_share_nothing() {
        let (first, second) = (parsed(r#"["idle"]"#), parsed(r#"["idle"]"#));
        let (Value::String(a), Value::String(b)) = (&items(&first)[0], &items(&second)[0]) else { panic!("strings") };
        assert!(!Shared::ptr_eq(a, b), "the table lives for one parse only");
    }

    #[test]
    fn g_cross_076_a_path_reads_through_a_quote_value_only_when_asked() {
        use crate::schema::quote::quote_expr;
        let body: SExpression = from_json_str(r#"{"transitions":[1,2],"name":"m"}"#).unwrap();
        let quoted = Value::from(quote_expr(&body));
        let holder = Value::object(ValueMap::from_iter([("machine".to_string().into(), quoted)]));
        assert_eq!(
            holder.get_path_owned_through_quotes("machine.transitions"),
            Some(Value::array(vec![Value::Number(1.0), Value::Number(2.0)]))
        );
        assert_eq!(holder.get_path_owned_through_quotes("machine.transitions.length"), Some(Value::Number(2.0)));
        assert_eq!(holder.get_path_owned("machine.transitions"), None, "control: the plain walk keeps the quote's shape");
    }

    #[test]
    fn value_is_sixteen_bytes() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
    }

    #[test]
    fn lambda_round_trips_to_fn_form_and_compares_by_content() {
        let body = Arc::new(SExpression::Number(1.0));
        let a = Value::lambda(vec!["x".into()], body.clone());
        let b = Value::lambda(vec!["x".into()], body.clone());
        assert_eq!(a, b);
        assert_ne!(a, Value::lambda(vec!["y".into()], body.clone()), "control: params differ");
        let Ok(SExpression::Array(arr)) = SExpression::try_from(a) else { panic!("fn form is an array") };
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn index_reads_a_missing_or_non_object_field_as_null_and_writes_through_null() {
        let mut v = Value::Null;
        v["a"]["b"] = Value::Number(1.0);
        assert_eq!(v["a"]["b"], Value::Number(1.0));
        assert_eq!(v.get("a").and_then(|a| a.get("b")), Some(&Value::Number(1.0)));
        assert_eq!(v["missing"], Value::Null);
        assert_eq!(Value::Number(2.0)["a"], Value::Null, "control: a number has no fields");
        assert_eq!(Value::Number(2.0).get("a"), None);
    }

    #[test]
    fn index_by_position_reads_null_out_of_range_and_writes_in_range() {
        let mut v = Value::array(vec![Value::Number(1.0), Value::Number(2.0)]);
        v[1] = Value::Number(3.0);
        assert_eq!(v[1], Value::Number(3.0));
        assert_eq!(v[5], Value::Null);
        assert_eq!(Value::Null[0], Value::Null, "control: a non-array has no elements");
    }

    #[test]
    fn take_leaves_null_and_the_kind_tests_agree_with_the_variant() {
        let mut v = Value::object(ValueMap::default());
        assert!(v.is_object() && !v.is_array() && !v.is_string());
        let taken = v.take();
        assert!(taken.is_object() && v.is_null());
    }


    #[test]
    fn split_path_reads_an_escaped_dot_and_backslash_as_literals() {
        let segs = |p: &str| split_path(p).iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(segs("a.b"), ["a", "b"]);
        assert_eq!(segs(r"http\.method"), ["http.method"]);
        assert_eq!(segs(r"a.b\.c"), ["a", "b.c"]);
        assert_eq!(segs(r"a\\.b"), [r"a\", "b"]);
        assert_eq!(segs(r"a\\\.b"), [r"a\.b"]);
        assert_eq!(segs(r"a\x.b"), [r"a\x", "b"], "any other backslash stays as written");
        assert_eq!(segs(""), [""]);
    }

    #[test]
    fn get_and_set_path_agree_on_an_escaped_dot() {
        let mut v = Value::object(ValueMap::default());
        assert!(v.set_path(r"a.b\.c", Value::Number(1.0)));
        assert_eq!(v.get_path(r"a.b\.c"), Some(&Value::Number(1.0)));
        assert_eq!(v.get_path_owned(r"a.b\.c"), Some(Value::Number(1.0)));
        assert_eq!(v.get_path("a.b.c"), None, "control: a plain path is not the literal key");
    }
    use super::*;
    use crate::json_value::{from_json_str, from_json_value};

    #[test]
    fn a_value_reads_from_json_as_the_expression_tree_converted() {
        let json = r#"{"a":[null,true,3,-2,1.5,"@x",{"z":[],"b":{}}],"k":"v"}"#;
        let direct: Value = from_json_str(json).unwrap();
        let via_tree = Value::from(from_json_str::<SExpression>(json).unwrap());
        assert_eq!(direct, via_tree);
        let Value::Object(map) = &direct else {
            panic!("an object");
        };
        assert_eq!(map.keys().collect::<Vec<_>>(), ["a", "k"]);
    }

    #[test]
    fn control_a_value_from_malformed_json_is_an_error() {
        assert!(from_json_str::<Value>("{\"a\": ").is_err());
    }

    #[test]
    fn a_strict_accessor_reads_only_its_own_kind() {
        assert_eq!(Value::Bool(true).boolean().ok(), Some(true), "control: a boolean is a boolean");
        for not_bool in [Value::Null, Value::Number(1.0), Value::string("true"), Value::array(vec![])] {
            assert!(not_bool.boolean().is_err(), "{not_bool:?} is not a boolean");
        }
        assert_eq!(Value::Number(3.0).number().ok(), Some(3.0), "control: a number is a number");
        for not_number in [Value::Null, Value::Bool(true), Value::string("3")] {
            assert!(not_number.number().is_err(), "{not_number:?} is not a number");
        }
        assert_eq!(Value::string("a").text().ok(), Some("a"), "control: text is text");
        assert!(Value::Null.text().is_err(), "null is not the empty string");
        assert!(Value::Number(1.0).text().is_err(), "a number is not text");
    }

    #[test]
    fn test_get_path() {
        let mut obj = ValueMap::default();
        let mut address = ValueMap::default();
        address.insert("city".into(), Value::string("NYC"));
        address.insert("zip".into(), Value::string("10001"));
        obj.insert("name".into(), Value::string("John"));
        obj.insert("address".into(), Value::object(address));

        let value = Value::object(obj);

        assert_eq!(
            value.get_path("name"),
            Some(&Value::string("John"))
        );
        assert_eq!(
            value.get_path("address.city"),
            Some(&Value::string("NYC"))
        );
        assert_eq!(value.get_path("address.country"), None);
        assert_eq!(value.get_path("nonexistent"), None);
    }

    #[test]
    fn test_get_path_owned_array_length_and_index() {
        // `@payload.data.length` (and `.count`) resolve to the array length; numeric
        // segments still index; nested objects still traverse. Pins the owned walk
        // `BindingContext::resolve` uses so a length badge bound to a fetched
        // collection renders a number, not the literal token.
        let data = Value::array(vec![
            Value::String(("a".to_string()).into()),
            Value::String(("b".to_string()).into()),
            Value::String(("c".to_string()).into()),
        ]);
        let mut root = ValueMap::default();
        root.insert("data".into(), data);
        let value = Value::object(root);

        assert_eq!(
            value.get_path_owned("data.length"),
            Some(Value::Number(3.0))
        );
        assert_eq!(value.get_path_owned("data.count"), Some(Value::Number(3.0)));
        assert_eq!(
            value.get_path_owned("data.1"),
            Some(Value::string("b"))
        );
        // length is terminal — anything past it does not resolve.
        assert_eq!(value.get_path_owned("data.length.foo"), None);
        // length on a non-array does not resolve.
        assert_eq!(value.get_path_owned("data.1.length"), None);
        // missing key still None.
        assert_eq!(value.get_path_owned("nope.length"), None);
    }

    #[test]
    fn test_get_path_owned_empty_array_length_is_zero() {
        let mut root = ValueMap::default();
        root.insert("data".into(), Value::array(vec![]));
        let value = Value::object(root);
        assert_eq!(
            value.get_path_owned("data.length"),
            Some(Value::Number(0.0))
        );
    }

    #[test]
    fn test_set_path() {
        let mut value = Value::object(ValueMap::default());

        assert!(value.set_path("name", Value::string("John")));
        assert_eq!(
            value.get_path("name"),
            Some(&Value::string("John"))
        );

        assert!(value.set_path("address.city", Value::string("NYC")));
        assert_eq!(
            value.get_path("address.city"),
            Some(&Value::string("NYC"))
        );
    }

    #[test]
    fn equality_is_structural_and_different_kinds_are_unequal() {
        assert_ne!(Value::Number(1.0), Value::string("1"), "a number and a string are unequal");
        assert_ne!(Value::Bool(true), Value::Number(1.0), "a boolean and a number are unequal");
        assert_eq!(Value::Number(1.0), Value::Number(1.0), "control: same kind, same value");
        assert_ne!(Value::Null, Value::string(String::new()), "null is not the empty string");
        assert_eq!(Value::Null, Value::Null, "control: null is null");
        let list = |n: f64| Value::array(vec![Value::Number(n)]);
        assert_eq!(list(1.0), list(1.0), "control: lists compare item by item");
        assert_ne!(Value::array(vec![Value::string("emit")]), Value::object(ValueMap::default()), "a list and an object are unequal");
    }

    #[test]
    fn ordering_is_within_one_orderable_kind() {
        assert_eq!(Value::Number(1.0).compare(&Value::Number(2.0)).ok(), Some(std::cmp::Ordering::Less));
        assert_eq!(Value::string("a").compare(&Value::string("b")).ok(), Some(std::cmp::Ordering::Less));
        assert!(Value::Number(1.0).compare(&Value::string("2")).is_err(), "a number and a string have no order");
        assert!(Value::Null.compare(&Value::Number(0.0)).is_err(), "null is not zero");
        assert!(Value::Number(f64::NAN).compare(&Value::Number(1.0)).is_err(), "NaN has no order");
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    enum Kind {
        Plain,
        Wrapped(u32),
        Pair(i8, String),
        Shaped { zed: bool, alpha: Option<f64> },
    }

    #[derive(serde::Serialize)]
    struct Sample {
        zeta: String,
        alpha: Vec<Kind>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped: Option<u8>,
        by_key: std::collections::BTreeMap<u32, (i64, u64, f32)>,
        raw: serde_json::Value,
        unit: (),
        hole: f64,
    }

    fn sample() -> Sample {
        Sample {
            zeta: "z".into(),
            alpha: vec![Kind::Plain, Kind::Wrapped(7), Kind::Pair(-2, "p".into()), Kind::Shaped { zed: true, alpha: Some(1.5) }],
            skipped: None,
            by_key: [(10, (-1, 2, 0.5)), (2, (3, 4, 1.0))].into_iter().collect(),
            raw: serde_json::json!({"b": [1, {"y": null, "x": "s"}], "a": 2.5}),
            unit: (),
            hole: f64::NAN,
        }
    }

    #[test]
    fn a_value_built_by_the_serializer_is_the_canonical_json_tree_converted() {
        let sample = sample();
        fn sorted(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(map) => {
                    map.sort_keys();
                    map.values_mut().for_each(sorted);
                }
                serde_json::Value::Array(items) => items.iter_mut().for_each(sorted),
                _ => {}
            }
        }
        let mut tree = serde_json::to_value(&sample).unwrap();
        sorted(&mut tree);
        let via_json: Value = from_json_value(&tree).unwrap();
        let direct = crate::value_serialize::to_canonical_value(&sample).unwrap();
        assert_eq!(direct, via_json);
        let keys: Vec<String> = match &direct {
            Value::Object(fields) => fields.keys().map(|k| k.to_string()).collect(),
            other => panic!("{other:?}"),
        };
        assert_eq!(keys, ["alpha", "by_key", "hole", "raw", "unit", "zeta"]);
        assert_eq!(direct.get_path("hole"), Some(&Value::Null), "a non-finite number is null");
    }

    #[test]
    fn control_an_uncanonical_value_keeps_the_serialized_order_and_equal_text_is_shared() {
        let direct = crate::value_serialize::to_value(&sample()).unwrap();
        let keys: Vec<String> = match &direct {
            Value::Object(fields) => fields.keys().map(|k| k.to_string()).collect(),
            other => panic!("{other:?}"),
        };
        assert_eq!(keys, ["zeta", "alpha", "by_key", "raw", "unit", "hole"]);
        let twice = crate::value_serialize::to_value(&vec!["same".to_string(), "same".to_string()]).unwrap();
        let Value::Array(items) = twice else { panic!() };
        let (Value::String(a), Value::String(b)) = (&items[0], &items[1]) else { panic!() };
        assert!(Shared::ptr_eq(a, b));
    }

    #[test]
    fn a_map_key_that_is_not_text_or_a_number_is_refused() {
        let bad: std::collections::BTreeMap<Vec<u8>, u8> = [(vec![1], 1)].into_iter().collect();
        assert!(crate::value_serialize::to_value(&bad).is_err());
    }

    #[test]
    fn equal_subtrees_of_a_serialized_value_are_one_node_and_a_write_copies_only_its_own() {
        let leaf = serde_json::json!({"k": [1, "a"], "z": null});
        let other = serde_json::json!({"k": [1, "b"], "z": null});
        let tree = serde_json::json!({"x": leaf, "y": leaf, "w": other});
        let mut value = crate::value_serialize::to_canonical_value(&tree).unwrap();
        let node = |value: &Value, key: &str| match value.get_path(key) {
            Some(Value::Object(fields)) => fields.clone(),
            other => panic!("{other:?}"),
        };
        assert!(Shared::ptr_eq(&node(&value, "x"), &node(&value, "y")));
        assert!(!Shared::ptr_eq(&node(&value, "x"), &node(&value, "w")), "control: a different leaf is a different node");
        assert!(value.set_path("x.z", Value::Number(1.0)));
        assert_eq!(value.get_path("x.z"), Some(&Value::Number(1.0)));
        assert_eq!(value.get_path("y.z"), Some(&Value::Null), "the sibling still reads the shared original");
    }

    #[test]
    fn a_sealed_value_reads_like_an_object_and_never_comes_back_from_the_wire() {
        let fields: ValueMap = [("subject".into(), Value::string("u1")), ("n".into(), Value::Number(2.0))].into_iter().collect();
        let sealed = Value::sealed("Verified", fields.clone());
        assert!(sealed.is_sealed_as("Verified"));
        assert!(!sealed.is_sealed_as("Viewer"));
        assert_eq!(sealed.get_path("subject"), Some(&Value::string("u1")));
        assert_eq!(sealed.get_path_owned("n"), Some(Value::Number(2.0)));
        assert_eq!(sealed.get("subject"), Some(&Value::string("u1")));
        assert_eq!(sealed.type_name(), "Verified");
        let wire = serde_json::to_value(&sealed).unwrap();
        assert_eq!(wire, serde_json::json!({"subject": "u1", "n": 2.0}));
        let back: Value = from_json_value(&wire).unwrap();
        assert!(!back.is_sealed_as("Verified"));
        assert_ne!(back, sealed, "control: the plain object with the same fields is a different value");
        let mut copy = sealed.clone();
        assert!(!copy.set_path("subject", Value::string("root")), "a sealed record is not written through a path");
        assert_eq!(copy.get_path("subject"), Some(&Value::string("u1")));
    }

    #[test]
    fn edit_that_changes_nothing_keeps_the_allocation_shared() {
        let original = Shared::new(vec![1, 2, 3]);
        let mut held = original.clone();
        held.edit(|items| {
            for item in items.iter_mut() {
                *item = std::convert::identity(*item);
            }
        });
        assert!(Shared::ptr_eq(&original, &held), "an edit that wrote the same values must not copy");
        held.edit(|_| {});
        assert!(Shared::ptr_eq(&original, &held), "an edit that did nothing must not copy");
    }

    #[test]
    fn edit_that_changes_something_leaves_other_holders_untouched() {
        let original = Shared::new(vec![1, 2, 3]);
        let mut held = original.clone();
        held.edit(|items| items.push(4));
        assert!(!Shared::ptr_eq(&original, &held), "control: a real change takes its own allocation");
        assert_eq!(*original, vec![1, 2, 3], "the other holder still reads the old data");
        assert_eq!(*held, vec![1, 2, 3, 4]);
    }

    // EXEC-EVAL-11
    const BEYOND_EXACT: i64 = (1i64 << 53) + 1;

    #[test]
    fn an_int_equals_a_number_only_when_the_number_is_exactly_it() {
        assert_eq!(Value::Int(5), Value::Number(5.0));
        assert_eq!(Value::Number(5.0), Value::Int(5));
        assert_ne!(Value::Int(5), Value::Number(5.5), "control: a fraction is a different number");
        assert_ne!(Value::Int(BEYOND_EXACT), Value::Number(BEYOND_EXACT as f64), "the rounded float is not the int");
        assert_ne!(Value::Int(1), Value::string("1"), "control: text is never a number");
    }

    #[test]
    fn bytes_equal_bytewise_and_never_equal_their_text() {
        assert_eq!(Value::bytes(vec![1, 2]), Value::bytes(vec![1, 2]));
        assert_ne!(Value::bytes(vec![1, 2]), Value::bytes(vec![1, 3]), "control: one byte differs");
        assert_ne!(Value::bytes(b"AQI=".to_vec()), Value::string("AQI="));
        assert_ne!(Value::bytes(vec![1, 2]), Value::string("AQI="), "the wire text of bytes is not the bytes");
    }

    #[test]
    fn ints_and_numbers_order_numerically() {
        assert_eq!(Value::Int(3).compare(&Value::Number(3.5)).ok(), Some(std::cmp::Ordering::Less));
        assert_eq!(Value::Number(3.0).compare(&Value::Int(3)).ok(), Some(std::cmp::Ordering::Equal));
        assert_eq!(Value::Int(i64::MAX).compare(&Value::Number(9.2e18)).ok(), Some(std::cmp::Ordering::Greater));
        assert_eq!(Value::Int(BEYOND_EXACT).compare(&Value::Number((1u64 << 53) as f64)).ok(), Some(std::cmp::Ordering::Greater));
        assert!(Value::Int(1).compare(&Value::Number(f64::NAN)).is_err(), "NaN has no order");
        assert!(Value::bytes(vec![1]).compare(&Value::bytes(vec![2])).is_err(), "control: bytes are not ordered");
    }

    #[test]
    fn json_integers_read_as_ints_and_fractions_as_numbers() {
        let v: Value = from_json_str(r#"{"i": 5, "f": 5.5, "w": 5.0, "big": 1152921504606846976}"#).unwrap();
        assert!(matches!(v["i"], Value::Int(5)));
        assert!(matches!(v["f"], Value::Number(n) if n == 5.5));
        assert!(matches!(v["w"], Value::Number(n) if n == 5.0), "a written fraction stays a number");
        assert!(matches!(v["big"], Value::Int(1152921504606846976)), "beyond 2^53 the int is exact");
    }

    #[test]
    fn ints_write_as_json_integers_and_bytes_as_base64_text() {
        assert_eq!(serde_json::to_string(&Value::Int(1152921504606846977)).unwrap(), "1152921504606846977");
        assert_eq!(serde_json::to_string(&Value::bytes(vec![1, 2])).unwrap(), r#""AQI=""#);
        assert_eq!(Value::Int(-7).to_string_value(), "-7");
        assert_eq!(Value::bytes(vec![1, 2]).to_string_value(), "AQI=");
        assert_eq!(crate::json_value::value_json_string(&Value::Int(1152921504606846977), false), "1152921504606846977");
    }

    #[test]
    fn an_int_reads_as_a_number_by_promotion_to_the_nearest_float() {
        assert_eq!(Value::Int(42).as_number(), Some(42.0));
        assert_eq!(Value::Int(BEYOND_EXACT).as_number(), Some(BEYOND_EXACT as f64));
        assert!(SExpression::try_from(Value::Int(BEYOND_EXACT)).is_err(), "control: represented as code it is never rounded");
        assert_eq!(Value::Number(42.0).as_int(), Some(42));
        assert_eq!(Value::Number(42.5).as_int(), None, "control: a fraction is not an int");
        assert_eq!(Value::Int(BEYOND_EXACT).as_int(), Some(BEYOND_EXACT));
        assert_eq!(Value::bytes(vec![9]).as_bytes(), Some(&[9u8][..]));
        assert_eq!(Value::string("CQ==").as_bytes(), None, "control: text is not bytes");
        assert_eq!(Value::Int(1).type_name(), "int");
        assert_eq!(Value::bytes(vec![]).type_name(), "bytes");
    }

    #[test]
    fn an_int_becomes_code_only_when_exact_and_bytes_as_their_wire_text() {
        assert_eq!(SExpression::try_from(Value::Int(5)).ok(), Some(SExpression::Number(5.0)));
        assert!(SExpression::try_from(Value::Int(BEYOND_EXACT)).is_err(), "never rounded into a literal");
        assert_eq!(SExpression::try_from(Value::bytes(vec![1, 2])).ok(), Some(SExpression::string("AQI=")));
    }

    #[test]
    fn edit_on_the_only_holder_applies_the_change() {
        let mut only = Shared::new(String::from("a"));
        only.edit(|text| text.push('b'));
        assert_eq!(*only, "ab");
        let mut empty: Shared<Vec<u8>> = Shared::new(Vec::new());
        empty.edit(|items| items.clear());
        assert!(empty.is_empty(), "an empty list edited to empty stays empty");
    }
}
