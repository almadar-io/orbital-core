//! JSON operators.
//!
//! `(json/parse s Shape)` — pure, deterministic: string → parsed `Value`; `Shape` names the type the value has (checked by `orb validate`,
//! and at run time against the shape `orb resolve` records or a built-in type: see `run_time_shape`). Invalid JSON, a value not of its shape or
//! non-string input → `Value::Null` (JS `null`), never throws —
//! guards can branch on null. No reviver, no prototype access, no side
//! effects. `(json/stringify v)` is the inverse: any `Value` → JSON string.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::ir::types::{DeclaredShape, OirType};
use orbital_core_base::type_compat::{parse_compat_type, CompatType};

fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(text)
}

fn json_to_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or_default()),
        serde_json::Value::String(s) => Value::string(s),
        serde_json::Value::Array(items) => {
            Value::Array(items.into_iter().map(json_to_value).collect())
        }
        serde_json::Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, json_to_value(v)))
                .collect(),
        ),
    }
}

pub struct JsonParseOp;
impl Operator for JsonParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "json/parse", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let Value::String(s) = val else {
            return Ok(Value::Null);
        };
        let Ok(parsed) = parse_json::<serde_json::Value>(&s).map(json_to_value) else {
            return Ok(Value::Null);
        };
        let Some((shape, declared)) = run_time_shape(&args[1])? else {
            return Ok(parsed);
        };
        match super::arg_types::misfit_of_shape(&parsed, &declared) {
            None => Ok(parsed),
            Some((path, actual)) => {
                tracing::debug!(
                    target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                    "json/parse: the value is not of the shape {shape}: at `{path}` it is {actual}"
                );
                Ok(Value::Null)
            }
        }
    }
}

/// The shape a `json/parse` names, with the type a run-time check reads, when the evaluator knows it: a declared shape
/// recorded as JSON (`orb resolve` writes a program alias as its type with the types it names) or a built-in type name.
pub(super) fn run_time_shape(
    arg: &SExpression,
) -> Result<Option<(String, std::rc::Rc<DeclaredShape>)>, OrbitalError> {
    let SExpression::String(text) = arg else {
        return Ok(None);
    };
    if text.starts_with('{') {
        let declared = super::arg_types::declared_shape(arg)?;
        let shown = declared
            .name
            .clone()
            .unwrap_or_else(|| super::arg_types::declared_text(&declared.ty));
        return Ok(Some((shown, declared)));
    }
    Ok(match parse_compat_type(text) {
        CompatType::Ty(OirType::Union { variants }) if variants.is_empty() => None,
        CompatType::Ty(declared) => Some((
            text.clone().into_inner(),
            std::rc::Rc::new(DeclaredShape {
                version: None,
                name: None,
                ty: declared,
                named: Default::default(),
            }),
        )),
        _ => None,
    })
}

pub struct JsonValidOp;
impl Operator for JsonValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.len() != 1 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "json/valid: expected exactly one argument".into(),
            )));
        }
        match eval_expr(&args[0], ctx)? {
            Value::String(text) => {
                // Syntax validity must not depend on numeric or Unicode value conversion.
                Ok(Value::Bool(
                    parse_json::<serde::de::IgnoredAny>(&text).is_ok(),
                ))
            }
            other => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "string".into(),
                actual: other.type_name().into(),
            })),
        }
    }
}

/// `(json/nodes v)`: `v` and every value nested in it, pre-order (a list's items in order, an object's values in key order).
pub struct JsonNodesOp;
impl Operator for JsonNodesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "json/nodes", == 1);
        let mut nodes = Vec::new();
        let mut stack = vec![eval_expr(&args[0], ctx)?];
        while let Some(value) = stack.pop() {
            match &value {
                Value::Array(items) => stack.extend(items.iter().rev().cloned()),
                Value::Object(map) => stack.extend(map.values().rev().cloned()),
                _ => {}
            }
            nodes.push(value);
        }
        Ok(Value::array(nodes))
    }
}

/// `(tree/index value heads [options])`: every call form in `value` whose head is listed in `heads`, grouped by head in
/// `heads` order, each with its path from `value` and its position `at` in the walk; one pre-order walk that continues
/// inside a hit. `options.prefixes` also collects every string leaf starting with a listed prefix, under the first
/// listed prefix that matches; a string that heads a call form is not a leaf.
pub struct TreeIndexOp;
impl Operator for TreeIndexOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "tree/index", >= 2);
        if args.len() > 3 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "tree/index".to_string(),
                expected: 3,
                actual: args.len(),
            }));
        }
        let value = eval_expr(&args[0], ctx)?;
        let invalid = || {
            OrbitalError::Eval(EvalError::InvalidArgument(
                "tree/index: heads must be a list of strings".to_string(),
            ))
        };
        let Value::Array(listed) = eval_expr(&args[1], ctx)? else {
            return Err(invalid());
        };
        let mut found: Vec<(String, Vec<Value>)> = Vec::with_capacity(listed.len());
        for head in listed.iter() {
            let Value::String(head) = head else {
                return Err(invalid());
            };
            if !found.iter().any(|(listed, _)| listed == head.as_str()) {
                found.push((head.to_string(), Vec::new()));
            }
        }
        let head_count = found.len();
        let mut prefixes: Vec<String> = Vec::new();
        if let Some(options) = args.get(2) {
            let option_error = |what: String| {
                OrbitalError::Eval(EvalError::InvalidArgument(format!("tree/index: {what}")))
            };
            let options = eval_expr(options, ctx)?;
            let Value::Object(map) = &options else {
                return Err(option_error(format!(
                    "options must be a record, not {}",
                    options.type_name()
                )));
            };
            match map.get("prefixes") {
                None | Some(Value::Null) => {}
                Some(Value::Array(items)) => {
                    for item in items.iter() {
                        let Value::String(prefix) = item else {
                            return Err(option_error(format!(
                                "options.prefixes must be a list of strings, found {}",
                                item.type_name()
                            )));
                        };
                        if !prefixes.iter().any(|listed| listed == prefix.as_str()) {
                            prefixes.push(prefix.to_string());
                            if !found.iter().any(|(listed, _)| listed == prefix.as_str()) {
                                found.push((prefix.to_string(), Vec::new()));
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(option_error(format!(
                        "options.prefixes must be a list of strings, not {}",
                        other.type_name()
                    )))
                }
            }
        }
        let hit = |path: &str, node: &Value, at: usize| {
            let mut hit = ValueMap::new();
            hit.insert("path".to_string(), Value::string(path.to_string()));
            hit.insert("node".to_string(), node.clone());
            hit.insert("at".to_string(), Value::Number(at as f64));
            Value::object(hit)
        };
        let mut visited = 0usize;
        let mut stack = vec![(value, String::new(), false)];
        while let Some((node, path, is_head)) = stack.pop() {
            let at = visited;
            visited += 1;
            match &node {
                Value::Array(items) => {
                    if let Some(Value::String(head)) = items.first() {
                        if let Some((_, list)) = found[..head_count]
                            .iter_mut()
                            .find(|(listed, _)| listed == head.as_str())
                        {
                            list.push(hit(&path, &node, at));
                        }
                    }
                    for (i, item) in items.iter().enumerate().rev() {
                        stack.push((item.clone(), format!("{path}[{i}]"), i == 0));
                    }
                }
                Value::Object(map) => {
                    for (key, item) in map.iter().rev() {
                        let child = if path.is_empty() {
                            key.to_string()
                        } else {
                            format!("{path}.{key}")
                        };
                        stack.push((item.clone(), child, false));
                    }
                }
                Value::String(text) if !is_head => {
                    if let Some(prefix) = prefixes
                        .iter()
                        .find(|prefix| text.starts_with(prefix.as_str()))
                    {
                        if let Some((_, list)) =
                            found.iter_mut().find(|(listed, _)| listed == prefix)
                        {
                            list.push(hit(&path, &node, at));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(Value::object(
            found
                .into_iter()
                .map(|(head, list)| (head, Value::array(list)))
                .collect::<ValueMap>(),
        ))
    }
}

pub struct JsonStringifyOp;
impl Operator for JsonStringifyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "json/stringify", 1);
        let val = eval_expr(&args[0], ctx)?;
        let flag = |options: &Value, key: &str| -> Result<bool, OrbitalError> {
            match options {
                Value::Object(map) => match map.get(key) {
                    None | Some(Value::Null) => Ok(false),
                    Some(Value::Bool(b)) => Ok(*b),
                    Some(other) => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                        "json/stringify: options.{key} must be a boolean, not {}",
                        other.type_name()
                    )))),
                },
                other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "json/stringify: options must be a record, not {}",
                    other.type_name()
                )))),
            }
        };
        let (pretty, sorted) = match args.get(1) {
            None => (false, false),
            Some(options) => {
                let options = eval_expr(options, ctx)?;
                (flag(&options, "pretty")?, flag(&options, "sorted")?)
            }
        };
        let json = value_to_json(&val);
        let json = if sorted {
            crate::util::canonical_json::canonical_json(&json)
        } else {
            json
        };
        Ok(Value::String(if pretty {
            orbital_core_base::json_value::js_json_string_pretty(&json).into()
        } else {
            orbital_core_base::json_value::js_json_string(&json).into()
        }))
    }
}

#[allow(unused_imports)]
pub(crate) use orbital_core_base::json_value::value_to_json;
