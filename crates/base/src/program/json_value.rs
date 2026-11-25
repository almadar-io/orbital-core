#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use crate::str_utils::js_number_string;
use crate::value::Value;

/// JS-semantics conversion for stringify: integer-valued numbers serialize
/// without a decimal point (`JSON.stringify(1)` → `"1"`, never `"1.0"`),
/// matching `to_string_value`'s integer rule. A lambda has no JSON form (JS
/// `JSON.stringify(fn)` → undefined) and serializes as null.
pub fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                serde_json::Value::Number((*n as i64).into())
            } else {
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }
        }
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Bytes(b) => serde_json::Value::String(crate::codec::base64_encode(b)),
        Value::String(s) => serde_json::Value::String(s.clone().into_inner()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect(),
        ),
        Value::Sealed(sealed) => value_to_json(&Value::object(sealed.fields().clone())),
        Value::Blob(blob) => serde_json::json!({ (crate::blobs::BLOB_MARKER): blob.id() }),
        Value::Lambda(_) => serde_json::Value::Null,
    }
}
/// JSON text as JavaScript's `JSON.stringify` writes it: every number through [`js_number_string`], a non-finite
/// number as `null`.
pub fn js_json_string<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let mut out = String::new();
    write_js_json(&to_json_tree(value), None, 0, &mut out);
    out
}

/// [`js_json_string`] with two-space indentation, as `JSON.stringify(v, null, 2)` writes it.
pub fn js_json_string_pretty<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let mut out = String::new();
    write_js_json(&to_json_tree(value), Some("  "), 0, &mut out);
    out
}

/// [`js_json_string`] / [`js_json_string_pretty`] of a core value, written straight from it: no JSON tree is built
/// between, so a large value costs its text and nothing else. The text is the one `value_to_json` then
/// `js_json_string` write.
pub fn value_json_string(value: &Value, pretty: bool) -> String {
    let mut out = String::new();
    write_value_json(value, pretty.then_some("  "), 0, &mut None, &mut out);
    out
}

/// [`value_json_string`] with every list or record the value holds more than once (the same shared node) written in
/// full the first time as `{ "$def": n, "value": … }` and afterwards as `{ "$def": n }` (G-CROSS-637, EXEC-DIST-16b).
/// [`crate::blobs::blob_json`] reads it back as the plain text; a value holding no node twice is written as the plain text.
pub fn value_json_string_with_defs(value: &Value) -> String {
    let mut seen = std::collections::HashMap::new();
    count_nodes(value, &mut seen);
    let repeated: std::collections::HashSet<usize> = seen.into_iter().filter(|(_, n)| *n > 1).map(|(at, _)| at).collect();
    let mut defs = (!repeated.is_empty()).then(|| Defs { repeated, ids: std::collections::HashMap::new() });
    let mut out = String::new();
    write_value_json(value, None, 0, &mut defs, &mut out);
    out
}

/// The non-empty lists and records written more than once, and the `$def` id each took when first written.
struct Defs {
    repeated: std::collections::HashSet<usize>,
    ids: std::collections::HashMap<usize, usize>,
}

fn node_at(value: &Value) -> Option<usize> {
    match value {
        Value::Array(items) if !items.is_empty() => Some(crate::value::Shared::as_ptr(items) as usize),
        Value::Object(map) if !map.is_empty() => Some(crate::value::Shared::as_ptr(map) as usize),
        _ => None,
    }
}

fn count_nodes(value: &Value, seen: &mut std::collections::HashMap<usize, u32>) {
    if let Some(at) = node_at(value) {
        let times = seen.entry(at).or_insert(0);
        *times += 1;
        if *times > 1 {
            return;
        }
    }
    match value {
        Value::Array(items) => items.iter().for_each(|item| count_nodes(item, seen)),
        Value::Object(map) => map.values().for_each(|item| count_nodes(item, seen)),
        _ => {}
    }
}

fn write_value_json(value: &Value, indent: Option<&str>, depth: usize, defs: &mut Option<Defs>, out: &mut String) {
    if let (Some(table), Some(at)) = (defs.as_mut(), node_at(value)) {
        if table.repeated.contains(&at) {
            if let Some(id) = table.ids.get(&at) {
                out.push_str("{\"$def\":");
                out.push_str(&id.to_string());
                out.push('}');
                return;
            }
            let id = table.ids.len();
            table.ids.insert(at, id);
            out.push_str("{\"$def\":");
            out.push_str(&id.to_string());
            out.push_str(",\"value\":");
            write_node_json(value, indent, depth, defs, out);
            out.push('}');
            return;
        }
    }
    write_node_json(value, indent, depth, defs, out);
}

fn write_node_json(value: &Value, indent: Option<&str>, depth: usize, defs: &mut Option<Defs>, out: &mut String) {
    let newline = |out: &mut String, depth: usize| {
        if let Some(unit) = indent {
            out.push('\n');
            for _ in 0..depth {
                out.push_str(unit);
            }
        }
    };
    match value {
        Value::Sealed(sealed) => write_value_json(&Value::object(sealed.fields().clone()), indent, depth, &mut None, out),
        Value::Null | Value::Lambda(_) => out.push_str("null"),
        Value::Blob(blob) => {
            out.push_str("{\"");
            out.push_str(crate::blobs::BLOB_MARKER);
            out.push_str("\":");
            out.push_str(&blob.id().to_string());
            out.push('}');
        }
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                out.push_str(&(*n as i64).to_string());
            } else if n.is_finite() {
                out.push_str(&js_number_string(*n));
            } else {
                out.push_str("null");
            }
        }
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Bytes(b) => out.push_str(&serde_json::to_string(&crate::codec::base64_encode(b)).expect("a string always writes")),
        Value::String(text) => out.push_str(&serde_json::to_string(text.as_str()).expect("a string always writes")),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, depth + 1);
                write_value_json(item, indent, depth + 1, defs, out);
            }
            if !items.is_empty() {
                newline(out, depth);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, depth + 1);
                out.push_str(&serde_json::to_string(key.as_str()).expect("a string always writes"));
                out.push_str(if indent.is_some() { ": " } else { ":" });
                write_value_json(item, indent, depth + 1, defs, out);
            }
            if !map.is_empty() {
                newline(out, depth);
            }
            out.push('}');
        }
    }
}

fn to_json_tree<T: serde::Serialize + ?Sized>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("JSON text of an in-memory value always writes")
}

fn write_js_json(value: &serde_json::Value, indent: Option<&str>, depth: usize, out: &mut String) {
    let newline = |out: &mut String, depth: usize| {
        if let Some(unit) = indent {
            out.push('\n');
            for _ in 0..depth {
                out.push_str(unit);
            }
        }
    };
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => match (n.is_f64(), n.as_f64()) {
            (true, Some(f)) => out.push_str(&js_number_string(f)),
            _ => out.push_str(&n.to_string()),
        },
        serde_json::Value::String(text) => {
            out.push_str(&serde_json::to_string(text).expect("a string always writes"))
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, depth + 1);
                write_js_json(item, indent, depth + 1, out);
            }
            if !items.is_empty() {
                newline(out, depth);
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, depth + 1);
                out.push_str(&serde_json::to_string(key).expect("a string always writes"));
                out.push_str(if indent.is_some() { ": " } else { ":" });
                write_js_json(item, indent, depth + 1, out);
            }
            if !map.is_empty() {
                newline(out, depth);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_json_string_writes_numbers_as_ecmascript() {
        let value = Value::array(vec![
            Value::Number(1e19),
            Value::Number(1e-6),
            Value::Number(1.0),
            Value::Number(-0.0),
            Value::Number(1e21),
            Value::Number(f64::NAN),
        ]);
        assert_eq!(
            js_json_string(&value),
            "[10000000000000000000,0.000001,1,0,1e+21,null]"
        );
        assert_eq!(
            js_json_string(&value_to_json(&value)),
            "[10000000000000000000,0.000001,1,0,1e+21,null]"
        );
    }

    #[test]
    fn js_json_text_matches_serde_json_text_when_no_float_is_involved() {
        let cases = [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"a": [], "b": {}, "c": [1, -2, true, null]}),
            serde_json::json!({"q\"uote": "back\\slash \u{1} \n\t\r é 💡 /", "nested": [[{"k": [0]}]]}),
            serde_json::json!([u64::MAX, i64::MIN, "", " "]),
        ];
        for case in &cases {
            assert_eq!(js_json_string(case), serde_json::to_string(case).unwrap(), "compact {case}");
            assert_eq!(js_json_string_pretty(case), serde_json::to_string_pretty(case).unwrap(), "pretty {case}");
        }
    }

    #[test]
    fn a_value_is_written_as_the_tree_route_writes_it() {
        let nested = Value::Object(
            [
                ("ints".into(), Value::array(vec![Value::Number(0.0), Value::Number(-0.0), Value::Number(-7.0), Value::Number(9.2e18)])),
                ("floats".into(), Value::array(vec![Value::Number(0.5), Value::Number(1e21), Value::Number(1e-7), Value::Number(1.7976931348623157e308)])),
                ("non_finite".into(), Value::array(vec![Value::Number(f64::NAN), Value::Number(f64::INFINITY), Value::Number(f64::NEG_INFINITY)])),
                ("text".into(), Value::string("q\"uote \\ \u{1} \n é 💡 /".to_string())),
                ("empty".into(), Value::Object(Default::default())),
                ("none".into(), Value::array(Vec::new())),
                ("flags".into(), Value::array(vec![Value::Bool(true), Value::Bool(false), Value::Null])),
            ]
            .into_iter()
            .collect(),
        );
        for value in [Value::Null, Value::Number(3.0), Value::string("x".to_string()), nested] {
            assert_eq!(value_json_string(&value, false), js_json_string(&value_to_json(&value)), "compact {value:?}");
            assert_eq!(value_json_string(&value, true), js_json_string_pretty(&value_to_json(&value)), "pretty {value:?}");
        }
    }

    #[test]
    fn edge_a_value_keeps_its_keys_in_the_order_it_holds_them() {
        let value = Value::Object([("b".into(), Value::Number(1.0)), ("a".into(), Value::Number(2.0))].into_iter().collect());
        assert_eq!(value_json_string(&value, false), "{\"b\":1,\"a\":2}");
    }

    #[test]
    fn js_json_pretty_writes_floats_as_ecmascript_inside_indented_containers() {
        let value = Value::Object([("n".into(), Value::Number(1e21)), ("f".into(), Value::Number(0.5))].into_iter().collect());
        assert_eq!(js_json_string_pretty(&value), "{\n  \"n\": 1e+21,\n  \"f\": 0.5\n}");
    }

    #[test]
    fn control_serde_json_default_number_text_is_not_ecmascript() {
        assert_eq!(serde_json::to_string(&Value::Number(1e19)).unwrap(), "1e+19");
    }
}

/// The deepest nesting a boundary parse accepts, on every host.
pub const MAX_JSON_DEPTH: usize = 8192;

#[derive(Debug)]
pub enum DeepJsonError {
    TooDeep { limit: usize },
    Json(serde_json::Error),
}

impl core::fmt::Display for DeepJsonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DeepJsonError::TooDeep { limit } => write!(f, "nesting exceeds {limit} levels"),
            DeepJsonError::Json(e) => write!(f, "{e}"),
        }
    }
}

impl From<serde_json::Error> for DeepJsonError {
    fn from(e: serde_json::Error) -> Self {
        DeepJsonError::Json(e)
    }
}

fn check_depth(bytes: &[u8]) -> Result<usize, DeepJsonError> {
    let (mut depth, mut deepest, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for &b in bytes {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(DeepJsonError::TooDeep { limit: MAX_JSON_DEPTH });
                }
                deepest = deepest.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(deepest)
}

/// Text nested at most this deep is read on the caller's stack.
const SHALLOW_DEPTH: usize = 64;

/// Stack reserved per nesting level when a deeper text is read on a grown stack.
#[cfg(feature = "std")]
const STACK_PER_LEVEL: usize = 16 * 1024;

#[cfg(feature = "std")]
fn on_deep_stack<R>(depth: usize, run: impl FnOnce() -> R) -> R {
    if depth <= SHALLOW_DEPTH {
        return run();
    }
    stacker::grow(depth.saturating_mul(STACK_PER_LEVEL).max(1 << 20), run)
}

#[cfg(not(feature = "std"))]
fn on_deep_stack<R>(_depth: usize, run: impl FnOnce() -> R) -> R {
    run()
}

/// Parse JSON up to [`MAX_JSON_DEPTH`] levels: a resolved program nests as deep as its expressions. The depth is
/// pre-scanned so every host refuses at the same bound. Each type is compiled for one plain text reader; a deep
/// text is read on one grown stack (std builds) instead of growing the stack at every level.
pub fn from_deep_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, DeepJsonError> {
    let depth = check_depth(bytes)?;
    on_deep_stack(depth, || {
        let mut de = serde_json::Deserializer::from_slice(bytes);
        de.disable_recursion_limit();
        let value = T::deserialize(&mut de)?;
        de.end()?;
        Ok(value)
    })
}

/// Whether `bytes` holds an object key `$def` (`def_refs`), read by the same scan as the depth: a value that only
/// spells the marker is text.
pub fn holds_def_key(bytes: &[u8]) -> bool {
    let marker = crate::def_refs::DEF_MARKER.as_bytes();
    let (mut in_string, mut escaped, mut start, mut pending) = (false, false, 0usize, false);
    for (at, &b) in bytes.iter().enumerate() {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => {
                    in_string = false;
                    pending = &bytes[start..at] == marker;
                }
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                start = at + 1;
            }
            b':' if pending => return true,
            b' ' | b'\n' | b'\r' | b'\t' => {}
            _ => pending = false,
        }
    }
    false
}

/// A program document (a resolved or written `.orb`): text that holds `$def` records (`json_value::value_json_string_with_defs`,
/// `def_refs`) is read as one shared [`Value`] with every record resolved, whatever position it sits in, then as `T`;
/// any other text is read by [`from_json_slice`] as before.
pub fn program_from_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    if !holds_def_key(bytes) {
        return from_json_slice(bytes);
    }
    crate::intern::scope(|| {
        let value: Value = crate::def_refs::reading_program(|| from_json_slice(bytes))?;
        crate::value_deserialize::from_value(&value)
    })
}

/// The kernel's one JSON text entry: [`from_deep_json`] with `serde_json`'s error type. Nesting past
/// [`MAX_JSON_DEPTH`] is a custom error.
pub fn from_json_slice<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    from_deep_json(bytes).map_err(|e| match e {
        DeepJsonError::Json(e) => e,
        too_deep => <serde_json::Error as serde::de::Error>::custom(too_deep),
    })
}

/// [`from_json_slice`] over text.
pub fn from_json_str<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    from_json_slice(text.as_bytes())
}

/// Reads a value through a `Value`, so the derived reader `read` (a `#[serde(remote = "Self")]` derive) is compiled
/// for `&Value` alone rather than once per deserializer. For the heavy internally tagged enums.
pub fn deserialize_via_value<'de, D, T>(
    deserializer: D,
    read: fn(&serde_json::Value) -> Result<T, serde_json::Error>,
) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    read(&value).map_err(<D::Error as serde::de::Error>::custom)
}

/// The kernel's one `Value` entry: deserializes from a borrowed `Value`, so each type is compiled
/// for one `Value` deserializer and the caller keeps (and never clones) its value.
pub fn from_json_value<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Result<T, serde_json::Error> {
    T::deserialize(value)
}

/// [`from_deep_json`] for a host wire string, its failure worded `parse {what}: {error}`.
pub fn parse_deep_json<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> Result<T, String> {
    from_deep_json(json.as_bytes()).map_err(|e| format!("parse {what}: {e}"))
}

#[cfg(test)]
mod deep_json_tests {
    use super::*;

    fn nested(depth: usize) -> String {
        format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn deep_json_control_a_shallow_text_parses_and_brackets_in_strings_do_not_count() {
        let parsed: serde_json::Value = from_deep_json(br#"{"a":["[[[[", "\\\"[", 1]}"#).expect("parses");
        assert_eq!(parsed["a"][2], serde_json::json!(1));
    }

    #[test]
    fn deep_json_the_bound_itself_parses_and_one_past_it_is_a_typed_refusal() {
        assert!(from_deep_json::<serde_json::Value>(nested(MAX_JSON_DEPTH).as_bytes()).is_ok());
        let err = from_deep_json::<serde_json::Value>(nested(MAX_JSON_DEPTH + 1).as_bytes()).unwrap_err();
        assert!(matches!(err, DeepJsonError::TooDeep { limit: MAX_JSON_DEPTH }));
        let text = parse_deep_json::<serde_json::Value>(&nested(MAX_JSON_DEPTH + 1), "values").unwrap_err();
        assert_eq!(text, "parse values: nesting exceeds 8192 levels");
    }
}

#[cfg(test)]
mod float_parse_tests {
    use super::*;

    fn kernel_parse(text: &str) -> f64 {
        from_deep_json::<f64>(text.as_bytes()).expect("a finite float parses")
    }

    fn sample(count: usize) -> Vec<f64> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let x = f64::from_bits(state);
            if x.is_finite() {
                out.push(x);
            }
        }
        out
    }

    #[test]
    fn every_shortest_float_text_parses_back_to_the_same_bits() {
        let wrong: Vec<String> = sample(200_000)
            .into_iter()
            .map(|x| format!("{x:e}"))
            .filter(|text| kernel_parse(text).to_bits() != text.parse::<f64>().unwrap().to_bits())
            .collect();
        assert!(wrong.is_empty(), "{} of 200000 floats parse to other bits, e.g. {:?}", wrong.len(), wrong.first());
    }

    #[test]
    fn float_parse_edges_keep_their_bits() {
        for x in [f64::MIN_POSITIVE, f64::MAX, f64::MIN, 5e-324, 2.2250738585072009e-308, 0.1, 1.0 / 3.0, -0.0] {
            assert_eq!(kernel_parse(&format!("{x:e}")).to_bits(), x.to_bits(), "{x:e}");
        }
    }

    #[test]
    fn float_parse_control_short_decimals_were_always_exact() {
        for (text, x) in [("0.5", 0.5), ("1.25", 1.25), ("-3", -3.0), ("1e3", 1000.0)] {
            assert_eq!(kernel_parse(text).to_bits(), f64::to_bits(x), "{text}");
        }
    }
}

#[cfg(test)]
mod json_entry_tests {
    use super::*;

    fn nested(depth: usize) -> String {
        format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn json_entry_parses_past_serde_json_default_depth_up_to_the_kernel_bound() {
        assert!(serde_json::from_str::<serde_json::Value>(&nested(200)).is_err());
        assert!(from_json_str::<serde_json::Value>(&nested(200)).is_ok());
        assert!(from_json_str::<serde_json::Value>(&nested(MAX_JSON_DEPTH)).is_ok());
        let err = from_json_str::<serde_json::Value>(&nested(MAX_JSON_DEPTH + 1)).unwrap_err();
        assert_eq!(err.to_string(), "nesting exceeds 8192 levels");
    }

    #[test]
    fn json_entry_control_keeps_serde_json_errors_and_values() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Row {
            id: String,
            n: f64,
        }
        let text = r#"{"id":"a","n":1.5}"#;
        assert_eq!(from_json_str::<Row>(text).unwrap(), serde_json::from_str::<Row>(text).unwrap());
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(from_json_value::<Row>(&value).unwrap(), serde_json::from_value::<Row>(value.clone()).unwrap());
        let bad = r#"{"id":1}"#;
        assert_eq!(
            from_json_str::<Row>(bad).unwrap_err().to_string(),
            serde_json::from_str::<Row>(bad).unwrap_err().to_string()
        );
        assert!(from_json_str::<Row>("{} trailing").is_err());
    }

    use crate::value::ValueMap;

    fn with_a_repeated_part() -> Value {
        let mut style = ValueMap::default();
        style.insert("fill".to_owned().into(), Value::String("#0d0f1e".to_owned().into()));
        style.insert("stroke".to_owned().into(), Value::array(vec![Value::Int(1), Value::Int(2)]));
        let style = Value::object(style);
        Value::array(vec![style.clone(), Value::array(vec![style.clone(), Value::Int(3)]), style])
    }

    #[test]
    fn a_part_held_twice_is_written_once_and_reads_back_as_the_plain_text() {
        let value = with_a_repeated_part();
        let plain = value_json_string(&value, false);
        let with_defs = value_json_string_with_defs(&value);
        assert_eq!(with_defs.matches("#0d0f1e").count(), 1, "{with_defs}");
        assert!(with_defs.len() < plain.len());
        let back = crate::blobs::blob_json(with_defs.as_bytes()).unwrap();
        assert_eq!(back, serde_json::from_str::<serde_json::Value>(&plain).unwrap());
    }

    #[test]
    fn control_a_value_with_no_part_held_twice_is_written_as_the_plain_text() {
        let mut fields = ValueMap::default();
        fields.insert("a".to_owned().into(), Value::array(vec![Value::Int(1)]));
        fields.insert("b".to_owned().into(), Value::array(vec![Value::Int(1)]));
        fields.insert("empty".to_owned().into(), Value::array(vec![]));
        let value = Value::object(fields);
        assert_eq!(value_json_string_with_defs(&value), value_json_string(&value, false));
    }

    #[test]
    fn control_a_part_held_outside_the_value_but_once_inside_it_is_written_plain() {
        let value = with_a_repeated_part();
        let outside = value.clone();
        let Value::Array(items) = &value else { unreachable!() };
        let once = items[1].clone();
        assert_eq!(value_json_string_with_defs(&once), value_json_string(&once, false));
        drop(outside);
    }
}

#[cfg(test)]
mod json_entry_depth_tests {
    use super::*;

    fn nested_call(depth: usize) -> String {
        format!("{}1{}", "[\"f\",".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn a_recursive_kernel_type_reads_at_the_full_bound_on_a_test_thread() {
        let expr = from_json_str::<crate::schema::SExpression>(&nested_call(MAX_JSON_DEPTH)).expect("reads at the bound");
        // Dropping a tree this deep overflows a test thread's stack (G-ORBITAL-CORE-199).
        core::mem::forget(expr);
        assert!(from_json_str::<crate::schema::SExpression>(&nested_call(MAX_JSON_DEPTH + 1)).is_err());
    }

    #[test]
    fn a_shallow_text_reads_on_the_callers_stack_control() {
        assert_eq!(from_json_str::<crate::schema::SExpression>(&nested_call(3)).is_ok(), true);
    }
}
