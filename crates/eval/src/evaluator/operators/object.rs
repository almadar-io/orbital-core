//! Object operators (and the `path` builder).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::util::deep_equals;
use super::*;
use orbital_core_base::value::ValueMap;

/// path - Build a dot-separated path string from segments
/// Used with set effect for dynamic field paths
/// Example: ["path", "formValues", "@payload.fieldId"] => "formValues.customerName"
pub struct PathOp;
impl Operator for PathOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "path", 1);

        // Evaluate each segment and join with "."
        let segments: Result<Vec<String>, _> = args
            .iter()
            .map(|arg| -> Result<String, OrbitalError> {
                Ok(eval_expr(arg, ctx)?.text()?.to_string())
            })
            .collect();

        Ok(Value::string(segments?.join(".")))
    }
}

// ============================================================================
// Phase 2.3: Object Operations
// ============================================================================

/// object/entries - Get key-value pairs as [[key, value], ...]
pub struct ObjectEntriesOp;
impl Operator for ObjectEntriesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/entries", 1);

        let obj = eval_expr(&args[0], ctx)?;
        match obj {
            Value::Object(map) => {
                let entries: Vec<Value> = map
                    .into_iter()
                    .map(|(k, v)| Value::array(vec![Value::String(k.into()), v]))
                    .collect();
                Ok(Value::array(entries))
            }
            _ => Err(obj.kind_error("object").into()),
        }
    }
}

/// object/set - Set nested property (immutable, returns new object)
pub struct ObjectSetOp;
impl Operator for ObjectSetOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["object/set", obj, "path.to.key", value]; `\.` in a segment is a literal dot
        require_arity!(args, "object/set", >= 3);

        let obj = eval_expr(&args[0], ctx)?;
        let path = eval_expr(&args[1], ctx)?.text()?.to_string();
        let value = eval_expr(&args[2], ctx)?;

        if orbital_core_base::value::split_path(&path)
            .iter()
            .any(|segment| super::util::is_refused_key(segment))
        {
            return Ok(Value::Null);
        }

        let mut result = match obj {
            Value::Object(o) => Value::Object(o),
            _ => return Err(obj.kind_error("object").into()),
        };

        if !result.set_path(&path, value) {
            return Ok(Value::Null);
        }

        Ok(result)
    }
}

/// object/has - Check if property exists
pub struct ObjectHasOp;
impl Operator for ObjectHasOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/has", >= 2);

        super::with_operand(&args[0], ctx, |obj| {
            let path = eval_expr(&args[1], ctx)?.text()?.to_string();
            match obj {
                Value::Object(_) => Ok(Value::Bool(obj.get_path(&path).is_some())),
                _ => Err(obj.kind_error("object").into()),
            }
        })
    }
}

/// object/merge - Shallow merge objects
pub struct ObjectMergeOp;
impl Operator for ObjectMergeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mut result = ValueMap::default();
        let mut refused = false;

        for arg in args {
            let val = eval_expr(arg, ctx)?;
            match val {
                Value::Object(obj) => {
                    for (k, v) in obj {
                        refused |= super::util::is_refused_key(&k);
                        result.insert(k, v);
                    }
                }
                _ => return Err(val.kind_error("object").into()),
            }
        }

        if refused {
            return Ok(Value::Null);
        }
        Ok(Value::object(result))
    }
}

// ============================================================================
// Phase 3.3: Object Advanced
// ============================================================================

/// object/fromEntries - Create object from entries
pub struct ObjectFromEntriesOp;
impl Operator for ObjectFromEntriesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "fromEntries", 1);

        let entries = eval_expr(&args[0], ctx)?;
        let array = match entries {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: entries.type_name().to_string(),
                }))
            }
        };

        let mut result = ValueMap::default();
        for entry in array {
            let pair = entry.as_array().ok_or_else(|| entry.kind_error("array"))?;
            let [key, value] = pair.as_slice() else {
                return Err(EvalError::InvalidArgument(format!(
                    "object/fromEntries: an entry is a [key, value] pair, got {} items",
                    pair.len()
                ))
                .into());
            };
            let key = key.text()?.to_string();
            if super::util::is_refused_key(&key) {
                return Ok(Value::Null);
            }
            result.insert(key.into(), value.clone());
        }

        Ok(Value::object(result))
    }
}

/// object/deepMerge - Deep merge objects
pub struct ObjectDeepMergeOp;
impl Operator for ObjectDeepMergeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        fn deep_merge(base: ValueMap, overlay: ValueMap) -> Option<ValueMap> {
            let mut result = base;
            for (key, value) in overlay {
                if super::util::is_refused_key(&key) {
                    return None;
                }
                match (result.get(&key), &value) {
                    (Some(Value::Object(base_obj)), Value::Object(overlay_obj)) => {
                        result.insert(
                            key,
                            Value::object(deep_merge(
                                (**base_obj).clone(),
                                (**overlay_obj).clone(),
                            )?),
                        );
                    }
                    _ => {
                        if super::util::has_refused_key_in(&value) {
                            return None;
                        }
                        result.insert(key, value);
                    }
                }
            }
            Some(result)
        }

        let mut result = ValueMap::default();
        let mut refused = false;

        for arg in args {
            let val = eval_expr(arg, ctx)?;
            match val {
                Value::Object(obj) => match deep_merge(result.clone(), obj.into_inner()) {
                    Some(merged) => result = merged,
                    None => refused = true,
                },
                _ => return Err(val.kind_error("object").into()),
            }
        }

        if refused {
            return Ok(Value::Null);
        }
        Ok(Value::object(result))
    }
}

/// object/pick - Pick specific keys
pub struct ObjectPickOp;
impl Operator for ObjectPickOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "pick", >= 2);

        let obj = eval_expr(&args[0], ctx)?;
        let keys_val = eval_expr(&args[1], ctx)?;

        let source = match obj {
            Value::Object(o) => o,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: obj.type_name().to_string(),
                }))
            }
        };

        let keys = match keys_val {
            Value::Array(a) => a
                .iter()
                .map(|v| v.text().map(str::to_string))
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: keys_val.type_name().to_string(),
                }))
            }
        };

        let mut result = ValueMap::default();
        for key in keys {
            if let Some(value) = source.get(&key) {
                result.insert(key.into(), value.clone());
            }
        }

        Ok(Value::object(result))
    }
}

/// object/omit - Omit specific keys
pub struct ObjectOmitOp;
impl Operator for ObjectOmitOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "omit", >= 2);

        let obj = eval_expr(&args[0], ctx)?;
        let keys_val = eval_expr(&args[1], ctx)?;

        let source = match obj {
            Value::Object(o) => o,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: obj.type_name().to_string(),
                }))
            }
        };

        let omit_keys: std::collections::HashSet<String> = match keys_val {
            Value::Array(a) => a
                .iter()
                .map(|v| v.text().map(str::to_string))
                .collect::<Result<_, _>>()?,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: keys_val.type_name().to_string(),
                }))
            }
        };

        let result: ValueMap = source
            .into_iter()
            .filter(|(k, _)| !omit_keys.contains(k.as_str()))
            .collect();

        Ok(Value::object(result))
    }
}

/// object/mapValues - Transform all values
pub struct ObjectMapValuesOp;
impl Operator for ObjectMapValuesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "mapValues", >= 2);

        let obj = eval_expr(&args[0], ctx)?;
        let mapper = eval_expr(&args[1], ctx)?;

        let source = match obj {
            Value::Object(o) => o,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: obj.type_name().to_string(),
                }))
            }
        };

        // Handle lambda mapper
        let (params, body) = require_lambda!(mapper);

        let mut result = ValueMap::default();
        for (key, value) in source {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], value);
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], Value::string(key.clone()));
            }
            let new_value = eval_expr(&body, &child_ctx)?;
            result.insert(key, new_value);
        }

        Ok(Value::object(result))
    }
}

/// object/filter - Filter by predicate
pub struct ObjectFilterOp;
impl Operator for ObjectFilterOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/filter", >= 2);

        let obj = eval_expr(&args[0], ctx)?;

        let source = match obj {
            Value::Object(o) => o,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: obj.type_name().to_string(),
                }))
            }
        };

        let mut result = ValueMap::default();
        let callback = super::array::Callback::of(&args[1], ctx)?;
        for (key, value) in source {
            let should_include =
                callback.apply(vec![Value::string(key.clone()), value.clone()], ctx)?;
            if should_include.boolean()? {
                result.insert(key, value);
            }
        }

        Ok(Value::object(result))
    }
}

// ============================================================================
// Phase 4.3: Object Extras
// ============================================================================

/// object/mapKeys - Transform all keys
pub struct ObjectMapKeysOp;
impl Operator for ObjectMapKeysOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "mapKeys", >= 2);

        let obj = eval_expr(&args[0], ctx)?;
        let mapper = eval_expr(&args[1], ctx)?;

        let source = match obj {
            Value::Object(o) => o,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: obj.type_name().to_string(),
                }))
            }
        };

        let (params, body) = require_lambda!(mapper);

        let mut result = ValueMap::default();
        let mut refused = false;
        for (key, value) in source {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], Value::string(key));
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], value.clone());
            }
            let new_key = eval_expr(&body, &child_ctx)?.text()?.to_string();
            refused |= super::util::is_refused_key(&new_key);
            result.insert(new_key.into(), value);
        }

        if refused {
            return Ok(Value::Null);
        }
        Ok(Value::object(result))
    }
}

/// object/equals - Deep equality check
pub struct ObjectEqualsOp;
impl Operator for ObjectEqualsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/equals", >= 2);
        let obj1 = eval_expr(&args[0], ctx)?;
        let obj2 = eval_expr(&args[1], ctx)?;
        for obj in [&obj1, &obj2] {
            if obj.as_object().is_none() {
                return Err(obj.kind_error("object").into());
            }
        }

        Ok(Value::Bool(deep_equals(&obj1, &obj2)))
    }
}

/// object/clone - Shallow clone
pub struct ObjectCloneOp;
impl Operator for ObjectCloneOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/clone", 1);
        let obj = eval_expr(&args[0], ctx)?;

        match obj {
            Value::Object(o) => Ok(Value::Object(o)),
            _ => Err(obj.kind_error("object").into()),
        }
    }
}

/// object/deepClone - Deep clone
pub struct ObjectDeepCloneOp;
impl Operator for ObjectDeepCloneOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/deepClone", 1);
        let val = eval_expr(&args[0], ctx)?;
        if val.as_object().is_none() {
            return Err(val.kind_error("object").into());
        }

        fn deep_clone(v: &Value) -> Value {
            match v {
                Value::Null => Value::Null,
                Value::Bool(b) => Value::Bool(*b),
                Value::Number(n) => Value::Number(*n),
                Value::String(s) => Value::String(s.clone()),
                Value::Array(a) => Value::Array(a.iter().map(deep_clone).collect()),
                Value::Object(o) => {
                    Value::Object(o.iter().map(|(k, v)| (k.clone(), deep_clone(v))).collect())
                }
                Value::Lambda { params, body } => Value::Lambda {
                    params: params.clone(),
                    body: body.clone(),
                },
            }
        }

        Ok(deep_clone(&val))
    }
}

// ============================================================================
// Object Operators (Phase 1)
// ============================================================================

pub struct ObjectKeysOp;
impl Operator for ObjectKeysOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/keys", 1);

        let obj = eval_expr(&args[0], ctx)?;
        match obj {
            Value::Object(map) => {
                let keys: Vec<Value> = map.keys().map(|k| Value::string(k.clone())).collect();
                Ok(Value::array(keys))
            }
            _ => Err(obj.kind_error("object").into()),
        }
    }
}

pub struct ObjectValuesOp;
impl Operator for ObjectValuesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/values", 1);

        let obj = eval_expr(&args[0], ctx)?;
        match obj {
            Value::Object(map) => {
                let values: Vec<Value> = map.into_inner().into_values().collect();
                Ok(Value::array(values))
            }
            _ => Err(obj.kind_error("object").into()),
        }
    }
}

pub struct ObjectGetOp;
impl Operator for ObjectGetOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["object/get", obj, "path.to.key", default?]
        require_arity!(args, "object/get", >= 2);

        let found = super::with_operand(&args[0], ctx, |obj| {
            if !matches!(obj, Value::Object(_) | Value::Array(_)) {
                return Err(obj.kind_error("object").into());
            }
            let key = eval_expr(&args[1], ctx)?;
            let path = match (&key, obj) {
                (Value::Number(_), Value::Array(_)) => {
                    super::array::whole_count(&key, "object/get", "index")?.to_string()
                }
                _ => key.text()?.to_string(),
            };
            Ok(obj.get_path(&path).cloned())
        })?;

        match found {
            Some(v) => Ok(v),
            None => match args.get(2) {
                Some(default) => eval_expr(default, ctx),
                None => Ok(Value::Null),
            },
        }
    }
}

#[cfg(test)]
mod strict_tests {
    use serde_json::json;
    use {crate::evaluator::context::BindingContextBuilder, crate::evaluator::Evaluator};

    fn run(args: serde_json::Value) -> Result<serde_json::Value, String> {
        let ctx = BindingContextBuilder::new().build();
        let sexpr = serde_json::from_value(args).unwrap();
        Evaluator::new()
            .evaluate(&sexpr, &ctx)
            .map(|v| serde_json::to_value(v).unwrap())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_null_object_is_refused_not_read_as_empty() {
        assert_eq!(run(json!(["object/keys", {"a": 1}])).unwrap(), json!(["a"]));
        assert_eq!(
            run(json!(["object/values", {"a": 1}])).unwrap(),
            json!([1.0])
        );
        assert_eq!(
            run(json!(["object/entries", {"a": 1}])).unwrap(),
            json!([["a", 1.0]])
        );
        assert_eq!(
            run(json!(["object/has", {"a": 1}, "a"])).unwrap(),
            json!(true)
        );
        assert_eq!(
            run(json!(["object/set", {"a": 1}, "b", 2])).unwrap(),
            json!({"a": 1.0, "b": 2.0})
        );
        for op in [
            "object/keys",
            "object/values",
            "object/entries",
            "object/deepClone",
        ] {
            assert!(run(json!([op, null])).is_err(), "{op}");
        }
        assert!(run(json!(["object/has", null, "a"])).is_err());
        assert!(run(json!(["object/set", null, "b", 2])).is_err());
        assert!(run(json!(["object/get", null, "a"])).is_err());
    }

    #[test]
    fn merge_and_equals_take_objects_only() {
        assert_eq!(
            run(json!(["object/merge", {"a": 1}, {"b": 2}])).unwrap(),
            json!({"a": 1.0, "b": 2.0})
        );
        assert_eq!(
            run(json!(["object/deepMerge", {"a": {"x": 1}}, {"a": {"y": 2}}])).unwrap(),
            json!({"a": {"x": 1.0, "y": 2.0}})
        );
        assert_eq!(
            run(json!(["object/equals", {"a": 1}, {"a": 1}])).unwrap(),
            json!(true)
        );
        assert!(run(json!(["object/merge", {"a": 1}, null])).is_err());
        assert!(run(json!(["object/deepMerge", null, {"a": 1}])).is_err());
        assert!(run(json!(["object/equals", {"a": 1}, null])).is_err());
        assert!(run(json!(["object/equals", 1, 1])).is_err());
    }

    #[test]
    fn get_takes_a_number_path_only_on_an_array() {
        assert_eq!(
            run(json!(["object/get", [10, 20], "1"])).unwrap(),
            json!(20.0)
        );
        assert_eq!(
            run(json!(["object/get", {"a": [10, 20]}, "a.1"])).unwrap(),
            json!(20.0)
        );
        assert!(run(json!(["object/get", {"1": "x"}, 1])).is_err());
        assert!(run(json!(["object/get", [10, 20], -1])).is_err());
        assert!(run(json!(["object/get", [10, 20], 0.5])).is_err());
    }

    #[test]
    fn from_entries_demands_pairs() {
        assert_eq!(
            run(json!(["object/fromEntries", [["a", 1]]])).unwrap(),
            json!({"a": 1.0})
        );
        assert!(run(json!(["object/fromEntries", [["a"]]])).is_err());
        assert!(run(json!(["object/fromEntries", ["a"]])).is_err());
    }
}
