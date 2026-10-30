//! YAML operators.
//!
//! `(yaml/parse s Shape)` — pure, deterministic: YAML 1.2 text (core schema) → parsed `Value`, then the one typed decode
//! `json/parse` runs (`json::typed_decode`): a value not of `Shape` is `Value::Null`. Exactly one document is read; an
//! alias resolves to a copy of its anchor's node. Null for text that is not one well-formed document (empty, several
//! documents, an alias without an anchor) and for what a JSON value cannot hold: a mapping key that is not a string, a
//! tag outside the core schema, NaN or an infinity. The 1.1 spellings (`yes`, `on`, the `<<` merge key) are plain
//! strings. A repeated key keeps its last value, as `json/parse` does. Non-string input (null included) is a type mismatch.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use saphyr::{ScalarStyle, Tag, Yaml, YamlLoader};
use saphyr_parser::Parser;
use std::borrow::Cow;

fn is_decimal(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn core_integer(text: &str) -> Option<f64> {
    if let Some(octal) = text.strip_prefix("0o") {
        let ok = !octal.is_empty() && octal.bytes().all(|b| (b'0'..=b'7').contains(&b));
        return ok
            .then(|| u64::from_str_radix(octal, 8).ok())
            .flatten()
            .map(|n| n as f64);
    }
    if let Some(hex) = text.strip_prefix("0x") {
        let ok = !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
        return ok
            .then(|| u64::from_str_radix(hex, 16).ok())
            .flatten()
            .map(|n| n as f64);
    }
    is_decimal(text.strip_prefix(['+', '-']).unwrap_or(text))
        .then(|| text.parse::<f64>().ok())
        .flatten()
}

fn core_float(text: &str) -> Option<f64> {
    match text {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return Some(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => return Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (is_decimal(whole) && (fraction.is_empty() || is_decimal(fraction)))
                || (whole.is_empty() && is_decimal(fraction))
        }
        None => is_decimal(mantissa),
    };
    let exponent_ok = exponent.map_or(true, |e| {
        is_decimal(e.strip_prefix(['+', '-']).unwrap_or(e))
    });
    (mantissa_ok && exponent_ok)
        .then(|| text.parse::<f64>().ok())
        .flatten()
}

fn core_null(text: &str) -> bool {
    matches!(text, "" | "~" | "null" | "Null" | "NULL")
}

fn core_bool(text: &str) -> Option<bool> {
    match text {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// A plain scalar resolved by the YAML 1.2 core schema (spec 10.3.2), or by the core tag written on it. `None` is a
/// scalar the tag does not fit or a number a JSON value cannot hold (NaN, an infinity).
fn resolve_plain(text: &str, tag: Option<&str>) -> Option<Value> {
    let finite = |n: Option<f64>| n.filter(|f| f.is_finite()).map(Value::Number);
    match tag {
        Some("str") => Some(Value::string(text.to_owned())),
        Some("null") => core_null(text).then_some(Value::Null),
        Some("bool") => core_bool(text).map(Value::Bool),
        Some("int") => finite(core_integer(text)),
        Some("float") => finite(core_float(text).or_else(|| core_integer(text))),
        Some(_) => None,
        None => {
            if core_null(text) {
                Some(Value::Null)
            } else if let Some(b) = core_bool(text) {
                Some(Value::Bool(b))
            } else if let Some(n) = core_integer(text).or_else(|| core_float(text)) {
                finite(Some(n))
            } else {
                Some(Value::string(text.to_owned()))
            }
        }
    }
}

fn core_tag<'a>(tag: &'a Option<Cow<'_, Tag>>) -> Result<Option<&'a str>, ()> {
    match tag.as_deref() {
        None => Ok(None),
        Some(tag) if tag.is_yaml_core_schema() => Ok(Some(tag.suffix.as_str())),
        Some(_) => Err(()),
    }
}

fn to_value(node: Yaml<'_>) -> Option<Value> {
    match node {
        Yaml::Representation(text, style, tag) => match core_tag(&tag).ok()? {
            tag if style != ScalarStyle::Plain => {
                matches!(tag, None | Some("str")).then(|| Value::string(text.into_owned()))
            }
            tag => resolve_plain(&text, tag),
        },
        Yaml::Sequence(items) => items
            .into_iter()
            .map(to_value)
            .collect::<Option<Vec<_>>>()
            .map(Value::array),
        Yaml::Mapping(map) => {
            let mut entries = Vec::with_capacity(map.len());
            for (key, value) in map {
                let Value::String(key) = to_value(key)? else {
                    return None;
                };
                entries.push((key.to_string(), to_value(value)?));
            }
            Some(Value::Object(
                entries.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            ))
        }
        _ => None,
    }
}

fn parse_yaml(text: &str) -> Option<Value> {
    let mut loader = YamlLoader::<Yaml>::default();
    loader.early_parse(false);
    Parser::new_from_str(text).load(&mut loader, true).ok()?;
    let mut documents = loader.into_documents();
    if documents.len() != 1 {
        return None;
    }
    to_value(documents.pop()?)
}

pub struct YamlParseOp;
impl Operator for YamlParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "yaml/parse", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let s = val.text()?;
        super::json::shape_operand(&args[1])?;
        let Some(parsed) = parse_yaml(s) else {
            return Ok(Value::Null);
        };
        super::json::typed_decode("yaml/parse", parsed, &args[1])
    }
}
