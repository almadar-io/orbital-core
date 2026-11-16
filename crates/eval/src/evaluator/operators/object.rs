//! Object operators (and the `path` builder).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::util::deep_equals;
use super::*;
use orbital_core_base::value::{value_map_with_capacity, Shared, ValueMap};

/// path - Build a dot-separated path string from segments
/// Used with set effect for dynamic field paths
/// Example: ["path", "formValues", "@payload.fieldId"] => "formValues.customerName"
pub struct PathOp;
impl StrictOperator for PathOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "path", 1);

        // Evaluate each segment and join with "."
        let segments: Result<Vec<String>, _> = args
            .iter()
            .map(|arg| -> Result<String, OrbitalError> {
                Ok(arg.text()?.to_string())
            })
            .collect();

        Ok(Value::string(segments?.join(".")))
    }
}

/// The path an `object/get` / `object/has` / `object/set` key operand names. A literal string (anything not starting with `@`)
/// is a dotted path, `\.` escaping a dot; any computed operand (a binding or a call) is ONE whole key, so a Map key
/// that contains a dot is read as written. The kernel cannot tell a Map from a record at run time (both are objects), so the
/// operand's form decides.
fn key_path(literal: bool, key: String) -> String {
    if literal {
        key
    } else {
        key.replace('\\', "\\\\").replace('.', "\\.")
    }
}

/// Whether an operand is written as a literal string: not a binding, not a call.
pub(super) fn written_literal(operand: &SExpression) -> bool {
    matches!(operand, SExpression::String(text) if !text.starts_with('@'))
}

// ============================================================================
// Phase 2.3: Object Operations
// ============================================================================

/// object/entries - Get key-value pairs as [[key, value], ...]
pub struct ObjectEntriesOp;
owning_operator!(ObjectEntriesOp);
impl OwningOperator for ObjectEntriesOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/entries", 1);

        let obj = std::mem::take(&mut args[0]);
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
        let key = eval_expr(&args[1], ctx)?;
        let value = eval_expr(&args[2], ctx)?;
        set_at(obj, &key, written_literal(&args[1]), value)
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        if args.len() < 3 {
            return Some(Err(arity("object/set", 3, args.len())));
        }
        let (obj, value) = (std::mem::take(&mut args[0]), std::mem::take(&mut args[2]));
        Some(set_at(obj, &args[1], site.literal(1), value))
    }
}

fn arity(operator: &str, expected: usize, actual: usize) -> OrbitalError {
    OrbitalError::Eval(EvalError::ArityMismatch { operator: operator.to_string(), expected, actual })
}

fn set_at(obj: Value, key: &Value, literal: bool, value: Value) -> EvalResult {
    let path = key_path(literal, key.text()?.to_string());
    let mut result = match obj {
        Value::Object(o) => Value::Object(o),
        _ => return Err(obj.kind_error("object").into()),
    };
    if !result.set_path(&path, value) {
        return Ok(Value::Null);
    }
    Ok(result)
}

fn has_at(obj: &Value, key: &Value, literal: bool) -> EvalResult {
    let path = key_path(literal, key.text()?.to_string());
    match obj {
        Value::Object(_) | Value::Sealed(_) => Ok(Value::Bool(obj.get_path(&path).is_some())),
        _ => Err(obj.kind_error("object").into()),
    }
}

/// The value at `key` of `obj`, `None` when nothing is there.
fn found_at(obj: &Value, key: &Value, literal: bool) -> Result<Option<Value>, OrbitalError> {
    let path = match (key, obj) {
        (Value::Number(_) | Value::Int(_), Value::Array(_)) => super::array::whole_count(key, "object/get", "index")?.to_string(),
        _ => key_path(literal, key.text()?.to_string()),
    };
    Ok(obj.get_path(&path).cloned())
}

fn gettable(obj: &Value) -> Result<(), OrbitalError> {
    if matches!(obj, Value::Object(_) | Value::Array(_) | Value::Sealed(_)) {
        Ok(())
    } else {
        Err(obj.kind_error("object").into())
    }
}

/// object/has - Check if property exists
pub struct ObjectHasOp;
impl Operator for ObjectHasOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/has", >= 2);

        super::with_operand(&args[0], ctx, |obj| has_at(obj, &eval_expr(&args[1], ctx)?, written_literal(&args[1])))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        if args.len() < 2 {
            return Some(Err(arity("object/has", 2, args.len())));
        }
        Some(has_at(&args[0], &args[1], site.literal(1)))
    }
}

/// object/merge - Shallow merge objects
pub struct ObjectMergeOp;
owning_operator!(ObjectMergeOp);
impl OwningOperator for ObjectMergeOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        let mut result = ValueMap::default();

        for arg in args.iter_mut() {
            let val = std::mem::take(arg);
            match val {
                Value::Object(obj) => {
                    for (k, v) in obj {
                        result.insert(k, v);
                    }
                }
                _ => return Err(val.kind_error("object").into()),
            }
        }

        Ok(Value::object(result))
    }
}

// ============================================================================
// Phase 3.3: Object Advanced
// ============================================================================

/// object/fromEntries - Create object from entries
pub struct ObjectFromEntriesOp;
owning_operator!(ObjectFromEntriesOp);
impl OwningOperator for ObjectFromEntriesOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "fromEntries", 1);

        let entries = std::mem::take(&mut args[0]);
        let array = match entries {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: entries.type_name().to_string(),
                }))
            }
        };

        let mut result = value_map_with_capacity(array.len());
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
            result.insert(key.into(), value.clone());
        }

        Ok(Value::object(result))
    }
}

/// object/deepMerge - Deep merge objects
pub struct ObjectDeepMergeOp;
owning_operator!(ObjectDeepMergeOp);
impl OwningOperator for ObjectDeepMergeOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        fn deep_merge(base: ValueMap, overlay: ValueMap) -> ValueMap {
            let mut result = base;
            for (key, value) in overlay {
                match (result.get(&key), &value) {
                    (Some(Value::Object(base_obj)), Value::Object(overlay_obj)) => {
                        result.insert(
                            key,
                            Value::object(deep_merge(
                                (**base_obj).clone(),
                                (**overlay_obj).clone(),
                            )),
                        );
                    }
                    _ => {
                        result.insert(key, value);
                    }
                }
            }
            result
        }

        let mut result = ValueMap::default();

        for arg in args.iter_mut() {
            let val = std::mem::take(arg);
            match val {
                Value::Object(obj) => result = deep_merge(result, obj.into_inner()),
                _ => return Err(val.kind_error("object").into()),
            }
        }

        Ok(Value::object(result))
    }
}

/// object/pick - Pick specific keys
pub struct ObjectPickOp;
owning_operator!(ObjectPickOp);
impl OwningOperator for ObjectPickOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "pick", >= 2);

        let obj = std::mem::take(&mut args[0]);
        let keys_val = std::mem::take(&mut args[1]);

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
owning_operator!(ObjectOmitOp);
impl OwningOperator for ObjectOmitOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "omit", >= 2);

        let obj = std::mem::take(&mut args[0]);
        let keys_val = std::mem::take(&mut args[1]);

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

/// The record an operand holds, owned; any other kind is a type mismatch.
fn record_of(obj: Value) -> Result<Shared<ValueMap>, OrbitalError> {
    match obj {
        Value::Object(o) => Ok(o),
        _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
            expected: "object".to_string(),
            actual: obj.type_name().to_string(),
        })),
    }
}

/// object/mapValues - Transform all values
pub struct ObjectMapValuesOp;
impl Operator for ObjectMapValuesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "mapValues", >= 2);

        let obj = eval_expr(&args[0], ctx)?;
        let mapper = eval_expr(&args[1], ctx)?;
        let source = record_of(obj)?;
        let mapped = super::array::bound_lambda(&mapper, ctx)?;
        values_mapped(source, mapped)
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "mapValues", 2);
        Some(record_of(std::mem::take(&mut args[0])).and_then(|source| values_mapped(source, |values| site.invoke(1, values))))
    }
}

/// `source` with each value replaced by what `mapped` answers for `(value, key)`.
fn values_mapped(source: Shared<ValueMap>, mut mapped: impl FnMut(Vec<Value>) -> EvalResult) -> EvalResult {
    let mut result = value_map_with_capacity(source.len());
    for (key, value) in source {
        let new_value = mapped(vec![value, Value::string(key.clone())])?;
        result.insert(key, new_value);
    }
    Ok(Value::object(result))
}

/// object/filter - Filter by predicate
pub struct ObjectFilterOp;
impl Operator for ObjectFilterOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/filter", >= 2);

        let obj = eval_expr(&args[0], ctx)?;
        let source = record_of(obj)?;
        entries_kept(source, &super::array::Callback::of(&args[1], ctx)?, ctx)
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "object/filter", 2);
        Some(
            record_of(std::mem::take(&mut args[0]))
                .and_then(|source| entries_kept(source, &super::array::Callback::Site { site, index: 1 }, ctx)),
        )
    }
}

/// The entries of `source` the callback accepts, given `(key, value)`.
fn entries_kept(source: Shared<ValueMap>, callback: &super::array::Callback, ctx: &BindingContext) -> EvalResult {
    let mut result = ValueMap::default();
    for (key, value) in source {
        let should_include = callback.apply(vec![Value::string(key.clone()), value.clone()], ctx)?;
        if should_include.boolean()? {
            result.insert(key, value);
        }
    }
    Ok(Value::object(result))
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
        let source = record_of(obj)?;
        let mapped = super::array::bound_lambda(&mapper, ctx)?;
        keys_mapped(source, mapped)
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "mapKeys", 2);
        Some(record_of(std::mem::take(&mut args[0])).and_then(|source| keys_mapped(source, |values| site.invoke(1, values))))
    }
}

/// `source` with each key replaced by the text `mapped` answers for `(key, value)`.
fn keys_mapped(source: Shared<ValueMap>, mut mapped: impl FnMut(Vec<Value>) -> EvalResult) -> EvalResult {
    let mut result = value_map_with_capacity(source.len());
    for (key, value) in source {
        let new_key = mapped(vec![Value::string(key), value.clone()])?.text()?.to_string();
        result.insert(new_key.into(), value);
    }
    Ok(Value::object(result))
}

/// object/equals - Deep equality check
pub struct ObjectEqualsOp;
impl StrictOperator for ObjectEqualsOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/equals", >= 2);
        let obj1 = args[0];
        let obj2 = args[1];
        for obj in [obj1, obj2] {
            if obj.as_object().is_none() {
                return Err(obj.kind_error("object").into());
            }
        }

        Ok(Value::Bool(deep_equals(obj1, obj2)))
    }
}

/// object/clone - Shallow clone
pub struct ObjectCloneOp;
owning_operator!(ObjectCloneOp);
impl OwningOperator for ObjectCloneOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/clone", 1);
        let obj = std::mem::take(&mut args[0]);

        match obj {
            Value::Object(o) => Ok(Value::Object(o)),
            _ => Err(obj.kind_error("object").into()),
        }
    }
}

/// object/deepClone - Deep clone
pub struct ObjectDeepCloneOp;
impl StrictOperator for ObjectDeepCloneOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/deepClone", 1);
        let val = args[0];
        if val.as_object().is_none() {
            return Err(val.kind_error("object").into());
        }

        fn deep_clone(v: &Value) -> Value {
            match v {
                Value::Null => Value::Null,
                Value::Bool(b) => Value::Bool(*b),
                Value::Number(n) => Value::Number(*n),
                Value::Int(i) => Value::Int(*i),
                Value::Bytes(b) => Value::Bytes(b.clone()),
                Value::String(s) => Value::String(s.clone()),
                Value::Array(a) => Value::Array(a.iter().map(deep_clone).collect()),
                Value::Object(o) => {
                    Value::Object(o.iter().map(|(k, v)| (k.clone(), deep_clone(v))).collect())
                }
                Value::Lambda(l) => Value::Lambda(l.clone()),
                Value::Sealed(s) => Value::Sealed(s.clone()),
                Value::Blob(b) => Value::Blob(b.clone()),
            }
        }

        Ok(deep_clone(val))
    }
}

// ============================================================================
// Object Operators (Phase 1)
// ============================================================================

pub struct ObjectKeysOp;
impl StrictOperator for ObjectKeysOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/keys", 1);

        let obj = args[0];
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
owning_operator!(ObjectValuesOp);
impl OwningOperator for ObjectValuesOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "object/values", 1);

        let obj = std::mem::take(&mut args[0]);
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
            gettable(obj)?;
            found_at(obj, &eval_expr(&args[1], ctx)?, written_literal(&args[1]))
        })?;

        match found {
            Some(v) => Ok(v),
            None => match args.get(2) {
                Some(default) => eval_expr(default, ctx),
                None => Ok(Value::Null),
            },
        }
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    /// The default (operand 2) is evaluated on demand: the site gives it as a callable.
    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        if args.len() < 2 {
            return Some(Err(arity("object/get", 2, args.len())));
        }
        let found = gettable(&args[0]).and_then(|()| found_at(&args[0], &args[1], site.literal(1)));
        Some(match found {
            Err(error) => Err(error),
            Ok(Some(value)) => Ok(value),
            Ok(None) if args.len() > 2 => site.invoke(2, Vec::new()),
            Ok(None) => Ok(Value::Null),
        })
    }
}

#[cfg(test)]
mod strict_tests {
    use serde_json::json;
    use orbital_core_base::value::{value_map_with_capacity, Value, ValueMap};
    use {crate::evaluator::context::BindingContextBuilder, crate::evaluator::Evaluator};

    fn run(args: serde_json::Value) -> Result<serde_json::Value, String> {
        let ctx = BindingContextBuilder::new().build();
        let sexpr = serde_json::from_value(args).unwrap();
        Evaluator::new()
            .evaluate(&sexpr, &ctx)
            .map(|v| serde_json::to_value(v).unwrap())
            .map_err(|e| e.to_string())
    }

    fn sealed_run(expr: serde_json::Value) -> Result<serde_json::Value, String> {
        let mut fields = value_map_with_capacity(2);
        fields.insert("subject".into(), Value::string("user-1"));
        fields.insert("issuer".into(), Value::string("iss"));
        let mut payload = ValueMap::default();
        payload.insert("v".into(), Value::sealed("Verified", fields));
        let mut ctx = BindingContextBuilder::new().build();
        ctx.set_payload(Value::object(payload));
        let sexpr = serde_json::from_value(expr).unwrap();
        Evaluator::new().evaluate(&sexpr, &ctx).map(|v| serde_json::to_value(v).unwrap()).map_err(|e| e.to_string())
    }

    #[test]
    fn a_sealed_record_answers_has_and_get_by_its_declared_fields_like_a_read() {
        assert_eq!(sealed_run(json!(["object/has", "@payload.v", "subject"])).unwrap(), json!(true));
        assert_eq!(sealed_run(json!(["object/has", "@payload.v", "kind"])).unwrap(), json!(false));
        assert_eq!(sealed_run(json!(["object/get", "@payload.v", "subject"])).unwrap(), json!("user-1"));
        assert_eq!(sealed_run(json!(["object/get", "@payload.v", "kind", "none"])).unwrap(), json!("none"));
        assert_eq!(sealed_run(json!("@payload.v.subject")).unwrap(), json!("user-1"));
    }

    fn map_run(expr: serde_json::Value) -> Result<serde_json::Value, String> {
        let mut inner = ValueMap::default();
        inner.insert("c.d".into(), Value::Int(7));
        let mut b = ValueMap::default();
        b.insert("c".into(), Value::Int(2));
        let mut root = ValueMap::default();
        root.insert("a.b".into(), Value::Int(1));
        root.insert("rec".into(), Value::object(b));
        root.insert("nested".into(), Value::object(inner));
        let mut payload = ValueMap::default();
        payload.insert("m".into(), Value::object(root));
        payload.insert("dotted".into(), Value::string("a.b"));
        payload.insert("path".into(), Value::string("rec.c"));
        let mut ctx = BindingContextBuilder::new().build();
        ctx.set_payload(Value::object(payload));
        let sexpr = serde_json::from_value(expr).unwrap();
        Evaluator::new().evaluate(&sexpr, &ctx).map(|v| serde_json::to_value(v).unwrap()).map_err(|e| e.to_string())
    }

    #[test]
    fn a_computed_key_with_a_dot_reads_one_whole_map_key() {
        assert_eq!(map_run(json!(["object/get", "@payload.m", "@payload.dotted"])).unwrap(), json!(1));
        assert_eq!(map_run(json!(["object/has", "@payload.m", "@payload.dotted"])).unwrap(), json!(true));
        assert_eq!(map_run(json!(["object/get", "@payload.m", ["str/concat", "a", ".", "b"]])).unwrap(), json!(1));
        assert_eq!(map_run(json!(["object/get", "@payload.m", "@payload.path"])).unwrap(), json!(null), "a computed dotted path does not descend");
        assert_eq!(map_run(json!(["object/has", "@payload.m", "@payload.path"])).unwrap(), json!(false));
    }

    #[test]
    fn a_literal_key_stays_a_dotted_path_and_an_escaped_dot_is_one_key() {
        assert_eq!(map_run(json!(["object/get", "@payload.m", "rec.c"])).unwrap(), json!(2));
        assert_eq!(map_run(json!(["object/has", "@payload.m", "rec.c"])).unwrap(), json!(true));
        assert_eq!(map_run(json!(["object/get", "@payload.m", "nested.c\\.d"])).unwrap(), json!(7), "a literal path into a nested map");
        assert_eq!(map_run(json!(["object/get", "@payload.m", "a\\.b"])).unwrap(), json!(1));
        assert_eq!(map_run(json!(["object/get", "@payload.m", "a.b"])).unwrap(), json!(null), "control: the plain literal is a path");
    }

    #[test]
    fn a_computed_key_is_written_whole_by_object_set() {
        let set = map_run(json!(["object/set", "@payload.m", "@payload.dotted", 9])).unwrap();
        assert_eq!(set["a.b"], json!(9.0));
        assert!(set.get("a").is_none());
        let literal = map_run(json!(["object/set", "@payload.m", "x.y", 9])).unwrap();
        assert_eq!(literal["x"]["y"], json!(9.0));
    }

    #[test]
    fn a_sealed_record_is_still_not_an_object_for_writes_and_a_non_object_still_refuses_has() {
        assert!(sealed_run(json!(["object/set", "@payload.v", "subject", "root"])).is_err());
        assert!(sealed_run(json!(["object/has", "@payload.v.subject", "x"])).is_err());
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
