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

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Shared<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Shared::new)
    }
}

/// Runtime value resulting from S-expression evaluation
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    /// Null/undefined value
    #[default]
    Null,
    /// Boolean value
    Bool(bool),
    /// Numeric value (all numbers are f64 for simplicity)
    Number(f64),
    /// String value
    String(Shared<String>),
    /// Array of values
    Array(Shared<Vec<Value>>),
    /// Object/map of values
    Object(Shared<ValueMap>),
    /// Lambda function (params, body)
    Lambda {
        params: Vec<String>,
        body: Arc<SExpression>,
    },
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
            Value::String(s) => serializer.serialize_str(s),
            Value::Array(arr) => arr.serialize(serializer),
            Value::Object(obj) => obj.serialize(serializer),
            Value::Lambda { .. } => {
                // Serialize lambda as ["fn", params, body]
                let sexpr: SExpression = self.clone().into();
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
        deserializer.deserialize_any(ValueVisitor)
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
        Ok(Value::Number(value as f64))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value as f64))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(Value::Number(value))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::string(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::string(value))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(item) = seq.next_element::<Value>()? {
            items.push(item);
        }
        Ok(Value::array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = ValueMap::with_capacity_and_hasher(map.size_hint().unwrap_or(0).min(4096), Default::default());
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            object.insert(key.into(), value);
        }
        Ok(Value::object(object))
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

    /// Try to get as number
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
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
            Value::String(s) => s.clone().into_inner(),
            Value::Array(_) | Value::Object(_) => crate::json_value::js_json_string(self),
            Value::Lambda { params, .. } => format!("<fn({})>", params.join(", ")),
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
        let mut current: &Value = self;
        let mut parts = path_segments(path).peekable();

        while let Some(part) = parts.next() {
            let last = parts.peek().is_none();
            let seg: &str = part.as_ref();
            match current {
                Value::Object(obj) => {
                    current = obj.get(seg)?;
                }
                Value::Array(arr) => {
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
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Lambda { .. } => "function",
        }
    }

    /// Check if value is a lambda
    pub fn is_lambda(&self) -> bool {
        matches!(self, Value::Lambda { .. })
    }

    /// Try to get lambda params and body
    pub fn as_lambda(&self) -> Option<(&[String], &SExpression)> {
        match self {
            Value::Lambda { params, body } => Some((params.as_slice(), body.as_ref())),
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
            Value::Lambda { params, .. } => {
                write!(f, "<fn({})>", params.join(", "))
            }
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

// Conversion from Value to SExpression
impl From<Value> for SExpression {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => SExpression::Null,
            Value::Bool(b) => SExpression::Bool(b),
            Value::Number(n) => SExpression::Number(n),
            Value::String(s) => SExpression::string(s.into_inner()),
            Value::Array(arr) => {
                SExpression::Array(arr.into_iter().map(SExpression::from).collect())
            }
            Value::Object(obj) => SExpression::object(
                obj.into_iter()
                    .map(|(k, v)| (k.into(), SExpression::from(v)))
                    .collect(),
            ),
            // Lambda cannot be directly converted to SExpression (would need ["fn", params, body])
            Value::Lambda { params, body } => {
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
        }
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
        Value::Number(n as f64)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n as f64)
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

    #[test]
    fn a_value_reads_from_json_as_the_expression_tree_converted() {
        let json = r#"{"a":[null,true,3,-2,1.5,"@x",{"z":[],"b":{}}],"k":"v"}"#;
        let direct: Value = serde_json::from_str(json).unwrap();
        let via_tree = Value::from(serde_json::from_str::<SExpression>(json).unwrap());
        assert_eq!(direct, via_tree);
        let Value::Object(map) = &direct else {
            panic!("an object");
        };
        assert_eq!(map.keys().collect::<Vec<_>>(), ["a", "k"]);
    }

    #[test]
    fn control_a_value_from_malformed_json_is_an_error() {
        assert!(serde_json::from_str::<Value>("{\"a\": ").is_err());
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
        let via_json: Value = serde_json::from_value(tree).unwrap();
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
    fn edit_that_changes_nothing_keeps_the_allocation_shared() {
        let original = Shared::new(vec![1, 2, 3]);
        let mut held = original.clone();
        held.edit(|items| {
            for item in items.iter_mut() {
                *item = *item;
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
