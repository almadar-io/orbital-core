//! Object operators (and the `path` builder).

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
            .map(|arg| eval_expr(arg, ctx).map(|v| v.to_string_value()))
            .collect();

        Ok(Value::String(segments?.join(".")))
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
                    .map(|(k, v)| Value::Array(vec![Value::String(k), v]))
                    .collect();
                Ok(Value::Array(entries))
            }
            Value::Null => Ok(Value::Array(vec![])),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "object".to_string(),
                actual: obj.type_name().to_string(),
            })),
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
        let path = eval_expr(&args[1], ctx)?.to_string_value();
        let value = eval_expr(&args[2], ctx)?;

        if orbital_core_base::value::split_path(&path)
            .iter()
            .any(|segment| super::util::is_refused_key(segment))
        {
            return Ok(Value::Null);
        }

        let mut result = match obj {
            Value::Object(o) => Value::Object(o),
            Value::Null => Value::Object(ValueMap::new()),
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: obj.type_name().to_string(),
                }))
            }
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
            let path = eval_expr(&args[1], ctx)?.to_string_value();
            match obj {
                Value::Object(_) => Ok(Value::Bool(obj.get_path(&path).is_some())),
                Value::Null => Ok(Value::Bool(false)),
                _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: obj.type_name().to_string(),
                })),
            }
        })
    }
}

/// object/merge - Shallow merge objects
pub struct ObjectMergeOp;
impl Operator for ObjectMergeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mut result = ValueMap::new();
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
                Value::Null => {} // Skip nulls
                _ => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: "object".to_string(),
                        actual: val.type_name().to_string(),
                    }))
                }
            }
        }

        if refused {
            return Ok(Value::Null);
        }
        Ok(Value::Object(result))
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

        let mut result = ValueMap::new();
        for entry in array {
            if let Value::Array(pair) = entry {
                if pair.len() >= 2 {
                    let key = pair[0].to_string_value();
                    if super::util::is_refused_key(&key) {
                        return Ok(Value::Null);
                    }
                    let value = pair[1].clone();
                    result.insert(key, value);
                }
            }
        }

        Ok(Value::Object(result))
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
                            Value::Object(deep_merge(base_obj.clone(), overlay_obj.clone())?),
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

        let mut result = ValueMap::new();
        let mut refused = false;

        for arg in args {
            let val = eval_expr(arg, ctx)?;
            match val {
                Value::Object(obj) => match deep_merge(result.clone(), obj) {
                    Some(merged) => result = merged,
                    None => refused = true,
                },
                Value::Null => {} // Skip nulls
                _ => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: "object".to_string(),
                        actual: val.type_name().to_string(),
                    }))
                }
            }
        }

        if refused {
            return Ok(Value::Null);
        }
        Ok(Value::Object(result))
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
            Value::Array(a) => a.iter().map(|v| v.to_string_value()).collect::<Vec<_>>(),
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: keys_val.type_name().to_string(),
                }))
            }
        };

        let mut result = ValueMap::new();
        for key in keys {
            if let Some(value) = source.get(&key) {
                result.insert(key, value.clone());
            }
        }

        Ok(Value::Object(result))
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
            Value::Array(a) => a.iter().map(|v| v.to_string_value()).collect(),
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: keys_val.type_name().to_string(),
                }))
            }
        };

        let result: ValueMap = source
            .into_iter()
            .filter(|(k, _)| !omit_keys.contains(k))
            .collect();

        Ok(Value::Object(result))
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

        let mut result = ValueMap::new();
        for (key, value) in source {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], value);
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], Value::String(key.clone()));
            }
            let new_value = eval_expr(&body, &child_ctx)?;
            result.insert(key, new_value);
        }

        Ok(Value::Object(result))
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

        let mut result = ValueMap::new();
        for (key, value) in source {
            let should_include = super::array::apply_callback(
                &args[1],
                vec![Value::String(key.clone()), value.clone()],
                ctx,
            )?;
            if should_include.is_truthy() {
                result.insert(key, value);
            }
        }

        Ok(Value::Object(result))
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

        let mut result = ValueMap::new();
        let mut refused = false;
        for (key, value) in source {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], Value::String(key));
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], value.clone());
            }
            let new_key = eval_expr(&body, &child_ctx)?.to_string_value();
            refused |= super::util::is_refused_key(&new_key);
            result.insert(new_key, value);
        }

        if refused {
            return Ok(Value::Null);
        }
        Ok(Value::Object(result))
    }
}

/// object/equals - Deep equality check
pub struct ObjectEqualsOp;
impl Operator for ObjectEqualsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/equals", >= 2);
        let obj1 = eval_expr(&args[0], ctx)?;
        let obj2 = eval_expr(&args[1], ctx)?;

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
            Value::Object(o) => Ok(Value::Object(o.clone())),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "object".to_string(),
                actual: obj.type_name().to_string(),
            })),
        }
    }
}

/// object/deepClone - Deep clone
pub struct ObjectDeepCloneOp;
impl Operator for ObjectDeepCloneOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/deepClone", 1);
        let val = eval_expr(&args[0], ctx)?;

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
                let keys: Vec<Value> = map.keys().map(|k| Value::String(k.clone())).collect();
                Ok(Value::Array(keys))
            }
            Value::Null => Ok(Value::Array(vec![])),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "object".to_string(),
                actual: obj.type_name().to_string(),
            })),
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
                let values: Vec<Value> = map.into_values().collect();
                Ok(Value::Array(values))
            }
            Value::Null => Ok(Value::Array(vec![])),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "object".to_string(),
                actual: obj.type_name().to_string(),
            })),
        }
    }
}

pub struct ObjectGetOp;
impl Operator for ObjectGetOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["object/get", obj, "path.to.key", default?]
        require_arity!(args, "object/get", >= 2);

        let found = super::with_operand(&args[0], ctx, |obj| {
            let path = eval_expr(&args[1], ctx)?.to_string_value();
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
