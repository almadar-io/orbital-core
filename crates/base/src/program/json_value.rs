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
        Value::String(s) => serde_json::Value::String(s.clone().into_inner()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
        Value::Lambda { .. } => serde_json::Value::Null,
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
    write_value_json(value, pretty.then_some("  "), 0, &mut out);
    out
}

fn write_value_json(value: &Value, indent: Option<&str>, depth: usize, out: &mut String) {
    let newline = |out: &mut String, depth: usize| {
        if let Some(unit) = indent {
            out.push('\n');
            for _ in 0..depth {
                out.push_str(unit);
            }
        }
    };
    match value {
        Value::Null | Value::Lambda { .. } => out.push_str("null"),
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
        Value::String(text) => out.push_str(&serde_json::to_string(text.as_str()).expect("a string always writes")),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, depth + 1);
                write_value_json(item, indent, depth + 1, out);
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
                write_value_json(item, indent, depth + 1, out);
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
                ("ints".to_string(), Value::array(vec![Value::Number(0.0), Value::Number(-0.0), Value::Number(-7.0), Value::Number(9.2e18)])),
                ("floats".to_string(), Value::array(vec![Value::Number(0.5), Value::Number(1e21), Value::Number(1e-7), Value::Number(1.7976931348623157e308)])),
                ("non_finite".to_string(), Value::array(vec![Value::Number(f64::NAN), Value::Number(f64::INFINITY), Value::Number(f64::NEG_INFINITY)])),
                ("text".to_string(), Value::string("q\"uote \\ \u{1} \n é 💡 /".to_string())),
                ("empty".to_string(), Value::Object(Default::default())),
                ("none".to_string(), Value::array(Vec::new())),
                ("flags".to_string(), Value::array(vec![Value::Bool(true), Value::Bool(false), Value::Null])),
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
        let value = Value::Object([("b".to_string(), Value::Number(1.0)), ("a".to_string(), Value::Number(2.0))].into_iter().collect());
        assert_eq!(value_json_string(&value, false), "{\"b\":1,\"a\":2}");
    }

    #[test]
    fn js_json_pretty_writes_floats_as_ecmascript_inside_indented_containers() {
        let value = Value::Object([("n".to_string(), Value::Number(1e21)), ("f".to_string(), Value::Number(0.5))].into_iter().collect());
        assert_eq!(js_json_string_pretty(&value), "{\n  \"n\": 1e+21,\n  \"f\": 0.5\n}");
    }

    #[test]
    fn control_serde_json_default_number_text_is_not_ecmascript() {
        assert_eq!(serde_json::to_string(&Value::Number(1e19)).unwrap(), "1e+19");
    }
}
