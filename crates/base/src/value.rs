//! Value type for S-expression evaluation results
//!
//! The Value type represents the result of evaluating an S-expression.
//! It supports all JSON-compatible types plus additional runtime types.

use crate::schema::sexpr::SExpression;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::sync::Arc;

/// The fields of an object value, in insertion order: `object/keys`, `object/values` and `object/entries` list
/// them in the order the fields were written, as the JS evaluator does.
pub type ValueMap = indexmap::IndexMap<String, Value>;

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
    String(String),
    /// Array of values
    Array(Vec<Value>),
    /// Object/map of values
    Object(ValueMap),
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
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(item) = seq.next_element::<Value>()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = ValueMap::with_capacity(map.size_hint().unwrap_or(0).min(4096));
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            object.insert(key, value);
        }
        Ok(Value::Object(object))
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
    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Check if value is truthy
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            Value::Lambda { .. } => true, // Lambdas are always truthy
        }
    }

    /// Convert to boolean
    pub fn to_bool(&self) -> bool {
        self.is_truthy()
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

    /// Try to convert to number (with coercion)
    pub fn to_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            Value::Bool(true) => Some(1.0),
            Value::Bool(false) => Some(0.0),
            Value::String(s) => s.parse().ok(),
            Value::Null => Some(0.0),
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

    /// The text of this value as a string operator reads it: absence (`null`) is the empty string, as the
    /// compiled TypeScript's `join('')` and `toUpperCase()` over a missing value read it.
    pub fn to_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            other => other.to_string_value(),
        }
    }

    /// Convert to string representation
    pub fn to_string_value(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => crate::str_utils::js_number_string(*n),
            Value::String(s) => s.clone(),
            Value::Array(_) | Value::Object(_) => serde_json::to_string(self).unwrap_or_default(),
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
                        obj.insert(part.to_string(), value);
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
                            obj.insert(part.to_string(), Value::Object(ValueMap::new()));
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

    /// Compare two values for equality (with type coercion)
    pub fn loose_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => {
                // Handle NaN comparison
                if a.is_nan() && b.is_nan() {
                    return true;
                }
                (a - b).abs() < f64::EPSILON
            }
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.loose_eq(y))
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).is_some_and(|bv| v.loose_eq(bv)))
            }
            // Type coercion cases
            (Value::Number(n), Value::String(s)) | (Value::String(s), Value::Number(n)) => s
                .parse::<f64>()
                .is_ok_and(|sn| (n - sn).abs() < f64::EPSILON),
            (Value::Bool(b), Value::Number(n)) | (Value::Number(n), Value::Bool(b)) => {
                let bn = if *b { 1.0 } else { 0.0 };
                (n - bn).abs() < f64::EPSILON
            }
            _ => false,
        }
    }

    /// Compare two values for ordering
    pub fn compare(&self, other: &Value) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            // Coerce to numbers for comparison
            _ => {
                let a = self.to_number()?;
                let b = other.to_number()?;
                a.partial_cmp(&b)
            }
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
            SExpression::String(s) => Value::String(s),
            SExpression::Array(arr) => Value::Array(arr.into_iter().map(Value::from).collect()),
            SExpression::Object(obj) => {
                Value::Object(obj.into_iter().map(|(k, v)| (k, Value::from(v))).collect())
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
            Value::String(s) => SExpression::String(s),
            Value::Array(arr) => {
                SExpression::Array(arr.into_iter().map(SExpression::from).collect())
            }
            Value::Object(obj) => SExpression::Object(
                obj.into_iter()
                    .map(|(k, v)| (k, SExpression::from(v)))
                    .collect(),
            ),
            // Lambda cannot be directly converted to SExpression (would need ["fn", params, body])
            Value::Lambda { params, body } => {
                let mut arr = vec![SExpression::String("fn".to_string())];
                if params.len() == 1 {
                    arr.push(SExpression::String(params[0].clone()));
                } else {
                    arr.push(SExpression::Array(
                        params.into_iter().map(SExpression::String).collect(),
                    ));
                }
                arr.push((*body).clone());
                SExpression::Array(arr)
            }
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
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
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
        let mut v = Value::Object(ValueMap::new());
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
    fn test_truthy() {
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(Value::Bool(true).is_truthy());
        assert!(!Value::Number(0.0).is_truthy());
        assert!(Value::Number(1.0).is_truthy());
        assert!(Value::Number(-1.0).is_truthy());
        assert!(!Value::String("".to_string()).is_truthy());
        assert!(Value::String("hello".to_string()).is_truthy());
        assert!(!Value::Array(vec![]).is_truthy());
        assert!(Value::Array(vec![Value::Null]).is_truthy());
    }

    #[test]
    fn test_get_path() {
        let mut obj = ValueMap::new();
        let mut address = ValueMap::new();
        address.insert("city".to_string(), Value::String("NYC".to_string()));
        address.insert("zip".to_string(), Value::String("10001".to_string()));
        obj.insert("name".to_string(), Value::String("John".to_string()));
        obj.insert("address".to_string(), Value::Object(address));

        let value = Value::Object(obj);

        assert_eq!(
            value.get_path("name"),
            Some(&Value::String("John".to_string()))
        );
        assert_eq!(
            value.get_path("address.city"),
            Some(&Value::String("NYC".to_string()))
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
        let data = Value::Array(vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
        ]);
        let mut root = ValueMap::new();
        root.insert("data".to_string(), data);
        let value = Value::Object(root);

        assert_eq!(
            value.get_path_owned("data.length"),
            Some(Value::Number(3.0))
        );
        assert_eq!(value.get_path_owned("data.count"), Some(Value::Number(3.0)));
        assert_eq!(
            value.get_path_owned("data.1"),
            Some(Value::String("b".to_string()))
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
        let mut root = ValueMap::new();
        root.insert("data".to_string(), Value::Array(vec![]));
        let value = Value::Object(root);
        assert_eq!(
            value.get_path_owned("data.length"),
            Some(Value::Number(0.0))
        );
    }

    #[test]
    fn test_set_path() {
        let mut value = Value::Object(ValueMap::new());

        assert!(value.set_path("name", Value::String("John".to_string())));
        assert_eq!(
            value.get_path("name"),
            Some(&Value::String("John".to_string()))
        );

        assert!(value.set_path("address.city", Value::String("NYC".to_string())));
        assert_eq!(
            value.get_path("address.city"),
            Some(&Value::String("NYC".to_string()))
        );
    }

    #[test]
    fn test_loose_eq() {
        assert!(Value::Number(1.0).loose_eq(&Value::String("1".to_string())));
        assert!(Value::Bool(true).loose_eq(&Value::Number(1.0)));
        assert!(Value::Bool(false).loose_eq(&Value::Number(0.0)));
        assert!(!Value::Number(1.0).loose_eq(&Value::String("2".to_string())));
    }

    #[test]
    fn an_empty_string_is_a_present_value_not_null() {
        let empty = Value::String(String::new());
        assert!(!Value::Null.loose_eq(&empty));
        assert!(!empty.loose_eq(&Value::Null));
        assert!(Value::Null.loose_eq(&Value::Null), "control: null is null");
        assert!(empty.loose_eq(&Value::String(String::new())), "control: \"\" is \"\"");
        assert!(!Value::Null.loose_eq(&Value::String("x".to_string())), "control: null is not a non-empty string");
    }

    #[test]
    fn test_compare() {
        assert_eq!(
            Value::Number(1.0).compare(&Value::Number(2.0)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            Value::String("a".to_string()).compare(&Value::String("b".to_string())),
            Some(std::cmp::Ordering::Less)
        );
    }
}
