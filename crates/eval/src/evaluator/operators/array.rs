//! Array operators.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::value::{Shared, ValueMap};
use std::sync::Arc;

/// The numbers `array/sum` / `max` / `min` / `avg` fold: each element (or its `args[1]` field), which must be a
/// number; any other kind, or an object missing the field, is an error.
fn numbers_of(args: &[SExpression], ctx: &BindingContext) -> Result<Vec<f64>, OrbitalError> {
    let arr = eval_expr(&args[0], ctx)?;
    let array = require_array!(arr);
    let key = match args.get(1) {
        Some(k) => Some(eval_expr(k, ctx)?.text()?.to_string()),
        None => None,
    };
    let mut numbers = Vec::with_capacity(array.len());
    for item in array.iter() {
        let value = match &key {
            Some(k) => match item
                .as_object()
                .ok_or_else(|| item.kind_error("object"))?
                .get(k)
            {
                None | Some(Value::Null) => continue,
                Some(field) => field,
            },
            None => item,
        };
        numbers.push(value.number()?);
    }
    Ok(numbers)
}

/// A higher-order operator's callback, read once per call and applied per element: a `(fn …)` literal binds its
/// params and runs its body where it is written, a value that is a lambda binds its own, and any other expression is
/// a raw body run with `@item`/`@index` bound. Reading it once spares every element a copy of the body.
pub enum Callback<'a> {
    Literal {
        params: Vec<&'a Shared<String>>,
        body: &'a SExpression,
    },
    Lambda {
        params: Vec<String>,
        body: Arc<SExpression>,
    },
    Raw(&'a SExpression),
}

impl<'a> Callback<'a> {
    pub fn of(callable: &'a SExpression, ctx: &BindingContext) -> Self {
        if let SExpression::Array(items) = callable {
            if items.len() >= 3 && items[0].as_str() == Some("fn") {
                let params = match &items[1] {
                    SExpression::String(name) => Some(vec![name]),
                    SExpression::Array(names) => Some(
                        names
                            .iter()
                            .filter_map(|name| match name {
                                SExpression::String(name) => Some(name),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                };
                if let Some(params) = params {
                    return Callback::Literal {
                        params,
                        body: &items[2],
                    };
                }
            }
        }
        match eval_expr(callable, ctx) {
            Ok(Value::Lambda { params, body }) => Callback::Lambda { params, body },
            _ => Callback::Raw(callable),
        }
    }

    /// Run the callback on one element. `values` are bound to the params in order, and `@item`/`@index` to the first
    /// two (twin of almadar-evaluator's `evalWithItem`/`evalLambda`).
    pub fn apply(&self, values: Vec<Value>, ctx: &BindingContext) -> EvalResult {
        let values: Vec<Arc<Value>> = values.into_iter().map(Arc::new).collect();
        let mut child = ctx.child();
        if let Some(item) = values.first() {
            child.set_local_shared(crate::evaluator::LocalName::Fixed("item"), Arc::clone(item));
        }
        if let Some(index) = values.get(1) {
            child.set_local_shared(
                crate::evaluator::LocalName::Fixed("index"),
                Arc::clone(index),
            );
        }
        match self {
            Callback::Literal { params, body } => {
                for (param, value) in params.iter().zip(&values) {
                    child.set_local_shared(
                        crate::evaluator::LocalName::Shared((*param).clone()),
                        Arc::clone(value),
                    );
                }
                eval_expr(body, &child)
            }
            Callback::Lambda { params, body } => {
                for (param, value) in params.iter().zip(&values) {
                    child.set_local_shared(param, Arc::clone(value));
                }
                eval_expr(body, &child)
            }
            Callback::Raw(body) => eval_expr(body, &child),
        }
    }
}

/// Run a higher-order operator's callback on one element, read afresh: an operator that applies it to many elements
/// reads it once with [`Callback::of`].
pub fn apply_callback(
    callable: &SExpression,
    values: Vec<Value>,
    ctx: &BindingContext,
) -> EvalResult {
    Callback::of(callable, ctx).apply(values, ctx)
}

// ============================================================================
// Array Operators
// ============================================================================

pub struct CountOp;
impl Operator for CountOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.is_empty() {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "count".to_string(),
                expected: 1,
                actual: 0,
            }));
        }

        // Check if this is a count query: ["count", "EntityType", filterExpr]
        if args.len() >= 2 {
            if let SExpression::String(entity_type) = &args[0] {
                // This is a count query over an entity store
                if let Some(entities) = ctx.get_entities(entity_type) {
                    if args.len() > 1 {
                        // Apply filter
                        let filter = &args[1];
                        let mut count = 0usize;
                        for entity in entities.iter() {
                            let mut child_ctx = ctx.child();
                            child_ctx.set_entity((*entity).clone());
                            if eval_expr(filter, &child_ctx)?.boolean()? {
                                count += 1;
                            }
                        }
                        return Ok(Value::Number(count as f64));
                    } else {
                        return Ok(Value::Number(entities.len() as f64));
                    }
                }
            }
        }

        // Regular array count, with an optional predicate
        super::with_operand(&args[0], ctx, |val| {
            if let (Value::Array(arr), Some(predicate)) = (val, args.get(1)) {
                let mut count = 0usize;
                let callback = Callback::of(predicate, ctx);
                for (i, item) in arr.iter().enumerate() {
                    if callback
                        .apply(vec![item.clone(), Value::Number(i as f64)], ctx)?
                        .boolean()?
                    {
                        count += 1;
                    }
                }
                return Ok(Value::Number(count as f64));
            }
            Ok(match val {
                Value::Array(arr) => Value::Number(arr.len() as f64),
                Value::String(s) => Value::Number(s.len() as f64),
                Value::Object(obj) => Value::Number(obj.len() as f64),
                _ => Value::Number(0.0),
            })
        })
    }
}

pub struct ArrayLenOp;
impl Operator for ArrayLenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/len", 1);
        super::with_operand(&args[0], ctx, |val| match val {
            Value::Array(items) => Ok(Value::Number(items.len() as f64)),
            other => Err(other.kind_error("array").into()),
        })
    }
}

pub struct FirstOp;
impl Operator for FirstOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "first", 1);
        super::with_operand(&args[0], ctx, |val| match val {
            Value::Array(arr) => Ok(arr.first().cloned().unwrap_or(Value::Null)),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "array".to_string(),
                actual: val.type_name().to_string(),
            })),
        })
    }
}

pub struct LastOp;
impl Operator for LastOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "last", 1);
        super::with_operand(&args[0], ctx, |val| match val {
            Value::Array(arr) => Ok(arr.last().cloned().unwrap_or(Value::Null)),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "array".to_string(),
                actual: val.type_name().to_string(),
            })),
        })
    }
}

pub struct NthOp;
impl Operator for NthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "nth", >= 2);
        super::with_operand(&args[0], ctx, |arr| {
            let idx = eval_expr(&args[1], ctx)?;
            let Value::Array(array) = arr else {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: arr.type_name().to_string(),
                }));
            };
            let position = idx.number()?;
            if position < 0.0 || position.fract() != 0.0 {
                return Err(EvalError::InvalidArgument(format!(
                    "nth: index {position} is not a non-negative integer"
                ))
                .into());
            }
            Ok(array.get(position as usize).cloned().unwrap_or(Value::Null))
        })
    }
}

pub struct IncludesOp;
impl Operator for IncludesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "includes", >= 2);
        super::with_operand(&args[0], ctx, |arr| {
            let item = eval_expr(&args[1], ctx)?;
            match arr {
                Value::Array(a) => Ok(Value::Bool(a.iter().try_fold(false, |found, v| {
                    Ok::<_, EvalError>(found || *v == item)
                })?)),
                Value::String(s) => Ok(Value::Bool(s.contains(item.text()?))),
                _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array or string".to_string(),
                    actual: arr.type_name().to_string(),
                })),
            }
        })
    }
}

pub struct MapOp;
impl Operator for MapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "map", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let array = require_array!(arr);

        // Canonical contract: `(map arr (fn (item [index]) body))` — eval the fn-form
        // to a `Value::Lambda`, then APPLY it per element (bind params, eval body),
        // mirroring `ReduceOp`/`ArrayFindIndexOp`. The previous code `eval_expr`'d the
        // fn-form and *returned* the Lambda value, so a mapped array came back as
        // `[lambda(N params)]` — the root of every `[lambda(1 params)]` render.
        // `apply_callback` also binds `@item`/`@index`, as the TS evaluator does for a `(fn …)`.
        let callback = Callback::of(&args[1], ctx);
        if !matches!(callback, Callback::Raw(_)) {
            let results: Result<Vec<Value>, _> = array
                .into_iter()
                .enumerate()
                .map(|(i, item)| callback.apply(vec![item, Value::Number(i as f64)], ctx))
                .collect();
            return Ok(Value::array(results?));
        }

        // Legacy: a raw body expression evaluated with `@item`/`@index` bound.
        let mapper = &args[1];
        let results: Result<Vec<Value>, _> = array
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let child_ctx = ctx.with_item_and_index(item, i);
                eval_expr(mapper, &child_ctx)
            })
            .collect();

        Ok(Value::array(results?))
    }
}

pub struct FilterOp;
impl Operator for FilterOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "filter", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let array = require_array!(arr);

        // Same lambda-application fix as `map`: eval the predicate fn-form to a
        // `Value::Lambda` and apply it per element. Without this the predicate eval'd
        // to a truthy closure and `filter` silently kept every element.
        if let Ok(Value::Lambda { params, body }) = eval_expr(&args[1], ctx) {
            let results: Result<Vec<Value>, _> = array
                .into_iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let mut child_ctx = ctx.child();
                    child_ctx.set_local(crate::evaluator::LocalName::Fixed("item"), item.clone());
                    child_ctx.set_local(
                        crate::evaluator::LocalName::Fixed("index"),
                        Value::Number(i as f64),
                    );
                    if !params.is_empty() {
                        child_ctx.set_local(&params[0], item.clone());
                    }
                    if params.len() > 1 {
                        child_ctx.set_local(&params[1], Value::Number(i as f64));
                    }
                    match eval_expr(&body, &child_ctx) {
                        Ok(v) => match v.boolean() {
                            Ok(true) => Some(Ok(item)),
                            Ok(false) => None,
                            Err(e) => Some(Err(OrbitalError::Eval(e))),
                        },
                        Err(e) => Some(Err(e)),
                    }
                })
                .collect();
            return Ok(Value::array(results?));
        }

        // Legacy: raw body predicate evaluated with `@item` bound.
        let predicate = &args[1];
        let results: Result<Vec<Value>, _> = array
            .into_iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let mut child_ctx = ctx.with_item(item.clone());
                child_ctx.set_local(
                    crate::evaluator::LocalName::Fixed("index"),
                    Value::Number(i as f64),
                );
                match eval_expr(predicate, &child_ctx) {
                    Ok(v) => match v.boolean() {
                        Ok(true) => Some(Ok(item)),
                        Ok(false) => None,
                        Err(e) => Some(Err(OrbitalError::Eval(e))),
                    },
                    Err(e) => Some(Err(e)),
                }
            })
            .collect();

        Ok(Value::array(results?))
    }
}

pub struct FindOp;
impl Operator for FindOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "find", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = &args[1];

        let array = require_array!(arr);

        let callback = Callback::of(predicate, ctx);
        for (i, item) in array.into_iter().enumerate() {
            if callback
                .apply(vec![item.clone(), Value::Number(i as f64)], ctx)?
                .boolean()?
            {
                return Ok(item);
            }
        }

        Ok(Value::Null)
    }
}

// ============================================================================
// Phase 2.1: Array Essentials
// ============================================================================

/// array/slice - Extract portion of array
pub struct ArraySliceOp;
impl Operator for ArraySliceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["slice", arr, start, end?]
        require_arity!(args, "slice", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let start = require_number!(eval_expr(&args[1], ctx)?);

        let array = require_array!(arr);

        let len = array.len() as i64;
        // Handle negative indices
        let start_idx = if start < 0.0 {
            ((len + start as i64).max(0)) as usize
        } else {
            (start as usize).min(array.len())
        };

        let end_idx = if args.len() > 2 {
            let end = require_number!(eval_expr(&args[2], ctx)?);
            if end < 0.0 {
                ((len + end as i64).max(0)) as usize
            } else {
                (end as usize).min(array.len())
            }
        } else {
            array.len()
        };

        if start_idx >= end_idx {
            return Ok(Value::array(vec![]));
        }

        Ok(Value::array(array[start_idx..end_idx].to_vec()))
    }
}

/// array/concat - Concatenate arrays
pub struct ArrayConcatOp;
impl Operator for ArrayConcatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mut result = Vec::new();
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            match val {
                Value::Array(arr) => result.extend(arr),
                _ => result.push(val),
            }
        }
        Ok(Value::array(result))
    }
}

/// array/append - Add element to end of array
pub struct ArrayAppendOp;
impl Operator for ArrayAppendOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "append", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let item = eval_expr(&args[1], ctx)?;

        let mut array = require_array!(arr);

        array.push(item);
        Ok(Value::Array(array))
    }
}

fn sort_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        _ => 5,
    }
}

/// Total order (so `sort_by` can never panic): null < boolean < number (NaN last) < string < array < other,
/// then by value. Twin of almadar-evaluator `compareSortValues`.
fn sort_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ra, rb) = (sort_rank(a), sort_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => match (x.is_nan(), y.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        },
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// array/sort `(arr, key?, dir?)` - sorted copy; `key` orders objects by that field (null or empty = the
/// elements themselves), `dir` is "asc" (default) or anything else for descending. Stable. Twin of
/// almadar-evaluator `evalArraySort`.
pub struct ArraySortOp;
impl Operator for ArraySortOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "sort", 1);
        let arr = eval_expr(&args[0], ctx)?;
        let mut array = require_array!(arr);

        let key = match args.get(1) {
            Some(k) => match eval_expr(k, ctx)? {
                Value::Null => None,
                Value::String(s) if s.is_empty() => None,
                Value::String(s) => Some(s),
                _ => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: "string".to_string(),
                        actual: "sort key".to_string(),
                    }))
                }
            },
            None => None,
        };
        let ascending = match args.get(2) {
            Some(d) => eval_expr(d, ctx)?.as_str() == Some("asc"),
            None => true,
        };

        let field = |v: &Value| -> Value {
            match &key {
                Some(k) => v
                    .as_object()
                    .and_then(|o| o.get(k.as_str()))
                    .cloned()
                    .unwrap_or(Value::Null),
                None => v.clone(),
            }
        };
        array.sort_by(|a, b| {
            let ord = sort_order(&field(a), &field(b));
            if ascending {
                ord
            } else {
                ord.reverse()
            }
        });

        Ok(Value::Array(array))
    }
}

/// array/unique - Remove duplicates
pub struct ArrayUniqueOp;
impl Operator for ArrayUniqueOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "unique", 1);
        let arr = eval_expr(&args[0], ctx)?;

        let array = require_array!(arr);

        // Remove duplicates while preserving order
        let mut seen: Vec<Value> = Vec::new();
        let mut result = Vec::new();
        for item in array {
            let mut is_dup = false;
            for v in seen.iter() {
                if *v == item {
                    is_dup = true;
                    break;
                }
            }
            if !is_dup {
                seen.push(item.clone());
                result.push(item);
            }
        }

        Ok(Value::array(result))
    }
}

/// array/indexOf - Find index of element
pub struct ArrayIndexOfOp;
impl Operator for ArrayIndexOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "indexOf", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let needle = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);

        for (i, item) in array.iter().enumerate() {
            if *item == needle {
                return Ok(Value::Number(i as f64));
            }
        }

        Ok(Value::Number(-1.0))
    }
}

/// array/every - Test all elements
pub struct ArrayEveryOp;
impl Operator for ArrayEveryOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "every", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = &args[1];

        let array = require_array!(arr);

        let callback = Callback::of(predicate, ctx);
        for (i, item) in array.into_iter().enumerate() {
            if !callback
                .apply(vec![item, Value::Number(i as f64)], ctx)?
                .boolean()?
            {
                return Ok(Value::Bool(false));
            }
        }

        Ok(Value::Bool(true))
    }
}

/// array/some - Test any element
pub struct ArraySomeOp;
impl Operator for ArraySomeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "some", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = &args[1];

        let array = require_array!(arr);

        let callback = Callback::of(predicate, ctx);
        for (i, item) in array.into_iter().enumerate() {
            if callback
                .apply(vec![item, Value::Number(i as f64)], ctx)?
                .boolean()?
            {
                return Ok(Value::Bool(true));
            }
        }

        Ok(Value::Bool(false))
    }
}

/// array/sum - Sum numeric array
pub struct ArraySumOp;
impl Operator for ArraySumOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "sum", 1);
        let sum: f64 = numbers_of(args, ctx)?.into_iter().sum();

        Ok(Value::Number(sum))
    }
}

// ============================================================================
// Phase 3.1: Array Advanced
// ============================================================================

/// array/prepend - Add element to beginning
pub struct ArrayPrependOp;
impl Operator for ArrayPrependOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "prepend", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let item = eval_expr(&args[1], ctx)?;

        let mut array = require_array!(arr);

        array.insert(0, item);
        Ok(Value::Array(array))
    }
}

/// array/insert - Insert element at index
pub struct ArrayInsertOp;
impl Operator for ArrayInsertOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "insert", >= 3);
        let arr = eval_expr(&args[0], ctx)?;
        let idx = require_number!(eval_expr(&args[1], ctx)?) as usize;
        let item = eval_expr(&args[2], ctx)?;

        let mut array = require_array!(arr);

        let insert_idx = idx.min(array.len());
        array.insert(insert_idx, item);
        Ok(Value::Array(array))
    }
}

/// array/remove - Remove element at index
pub struct ArrayRemoveOp;
impl Operator for ArrayRemoveOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "remove", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let idx = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let mut array = require_array!(arr);

        if idx < array.len() {
            array.remove(idx);
        }
        Ok(Value::Array(array))
    }
}

/// array/reverse - Reverse order
pub struct ArrayReverseOp;
impl Operator for ArrayReverseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "reverse", 1);
        let arr = eval_expr(&args[0], ctx)?;

        let mut array = require_array!(arr);

        array.reverse();
        Ok(Value::Array(array))
    }
}

/// array/flatten - Flatten nested arrays (one level)
pub struct ArrayFlattenOp;
impl Operator for ArrayFlattenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "flatten", 1);
        let arr = eval_expr(&args[0], ctx)?;

        let array = require_array!(arr);

        let mut result = Vec::new();
        for item in array {
            match item {
                Value::Array(inner) => result.extend(inner),
                other => result.push(other),
            }
        }
        Ok(Value::array(result))
    }
}

/// array/zip - Combine arrays pairwise
pub struct ArrayZipOp;
impl Operator for ArrayZipOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "zip", >= 2);
        let arr1 = eval_expr(&args[0], ctx)?;
        let arr2 = eval_expr(&args[1], ctx)?;

        let array1 = match arr1 {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: arr1.type_name().to_string(),
                }))
            }
        };

        let array2 = match arr2 {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: arr2.type_name().to_string(),
                }))
            }
        };

        let result: Vec<Value> = array1
            .into_iter()
            .zip(array2)
            .map(|(a, b)| Value::array(vec![a, b]))
            .collect();

        Ok(Value::array(result))
    }
}

/// array/findIndex - Find index by predicate
pub struct ArrayFindIndexOp;
impl Operator for ArrayFindIndexOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "findIndex", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);

        // Handle lambda predicate
        let (params, body) = require_lambda!(predicate);

        for (i, item) in array.into_iter().enumerate() {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], item);
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], Value::Number(i as f64));
            }
            let result = eval_expr(&body, &child_ctx)?;
            if result.boolean()? {
                return Ok(Value::Number(i as f64));
            }
        }

        Ok(Value::Number(-1.0))
    }
}

/// array/avg - Average of numeric array
pub struct ArrayAvgOp;
impl Operator for ArrayAvgOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "avg", 1);
        let nums = numbers_of(args, ctx)?;
        if nums.is_empty() {
            return Ok(Value::Null);
        }

        let sum: f64 = nums.iter().sum();
        Ok(Value::Number(sum / nums.len() as f64))
    }
}

/// array/groupBy - Group by key function
pub struct ArrayGroupByOp;
impl Operator for ArrayGroupByOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "groupBy", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let key_fn = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);

        let mut groups: std::collections::HashMap<String, Vec<Value>> =
            std::collections::HashMap::new();

        // A field-name key (`"stage"`) groups by that field, as the TS evaluator does:
        // `String(item[key] ?? 'undefined')`.
        if let Value::String(field) = &key_fn {
            for item in array {
                let key = match &item {
                    Value::Object(row) => match row.get(field.as_str()) {
                        Some(Value::Null) | None => "undefined".to_string(),
                        Some(v) => v.to_string_value(),
                    },
                    _ => "undefined".to_string(),
                };
                groups.entry(key).or_default().push(item);
            }
            return Ok(Value::Object(
                groups
                    .into_iter()
                    .map(|(k, v)| (k, Value::array(v)))
                    .collect(),
            ));
        }

        // Handle lambda key function
        let (params, body) = require_lambda!(key_fn);

        for (i, item) in array.into_iter().enumerate() {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], item.clone());
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], Value::Number(i as f64));
            }
            let key = eval_expr(&body, &child_ctx)?.to_string_value();
            groups.entry(key).or_default().push(item);
        }

        let result: ValueMap = groups
            .into_iter()
            .map(|(k, v)| (k, Value::array(v)))
            .collect();

        Ok(Value::object(result))
    }
}

/// array/take - Take first n elements
pub struct ArrayTakeOp;
impl Operator for ArrayTakeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "take", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let n = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let array = require_array!(arr);

        let result: Vec<Value> = array.into_iter().take(n).collect();
        Ok(Value::array(result))
    }
}

/// array/drop - Drop first n elements
pub struct ArrayDropOp;
impl Operator for ArrayDropOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "drop", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let n = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let array = require_array!(arr);

        let result: Vec<Value> = array.into_iter().skip(n).collect();
        Ok(Value::array(result))
    }
}

/// array/range - Generate numeric range
pub struct ArrayRangeOp;
impl Operator for ArrayRangeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "range", 1);

        let start = require_number!(eval_expr(&args[0], ctx)?);

        let end = if args.len() > 1 {
            require_number!(eval_expr(&args[1], ctx)?)
        } else {
            // If only one arg, range from 0 to that number
            let end = start;
            return Ok(Value::Array(
                (0..end as i64).map(|i| Value::Number(i as f64)).collect(),
            ));
        };

        let step = if args.len() > 2 {
            require_number!(eval_expr(&args[2], ctx)?)
        } else {
            1.0
        };

        if step == 0.0 {
            return Ok(Value::array(vec![]));
        }

        let mut result = Vec::new();
        let mut current = start;

        if step > 0.0 {
            while current < end {
                result.push(Value::Number(current));
                current += step;
            }
        } else {
            while current > end {
                result.push(Value::Number(current));
                current += step;
            }
        }

        Ok(Value::array(result))
    }
}

// ============================================================================
// Phase 4.1: Array Extras
// ============================================================================

/// array/shuffle - Randomize order
pub struct ArrayShuffleOp;
impl Operator for ArrayShuffleOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "shuffle", 1);
        let arr = eval_expr(&args[0], ctx)?;

        let mut array = require_array!(arr);

        // Fisher-Yates shuffle using simple LCG RNG seeded from the portable clock.
        let seed = orbital_core_base::time_util::now_millis() as u64;

        let mut rng = seed;
        let len = array.len();
        for i in (1..len).rev() {
            // Simple LCG: next = (a * current + c) mod m
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            let j = (rng as usize) % (i + 1);
            array.swap(i, j);
        }

        Ok(Value::Array(array))
    }
}

/// array/reject - Inverse of filter
pub struct ArrayRejectOp;
impl Operator for ArrayRejectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "reject", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);

        let (params, body) = require_lambda!(predicate);

        let mut result = Vec::new();
        for (i, item) in array.into_iter().enumerate() {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], item.clone());
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], Value::Number(i as f64));
            }
            let should_reject = eval_expr(&body, &child_ctx)?;
            if !should_reject.boolean()? {
                result.push(item);
            }
        }

        Ok(Value::array(result))
    }
}

/// array/partition - Split by predicate
pub struct ArrayPartitionOp;
impl Operator for ArrayPartitionOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "partition", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);

        let (params, body) = require_lambda!(predicate);

        let mut matches = Vec::new();
        let mut non_matches = Vec::new();

        for (i, item) in array.into_iter().enumerate() {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], item.clone());
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], Value::Number(i as f64));
            }
            let result = eval_expr(&body, &child_ctx)?;
            if result.boolean()? {
                matches.push(item);
            } else {
                non_matches.push(item);
            }
        }

        Ok(Value::array(vec![
            Value::array(matches),
            Value::array(non_matches),
        ]))
    }
}

/// array/takeLast - Take last n elements
pub struct ArrayTakeLastOp;
impl Operator for ArrayTakeLastOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "takeLast", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let n = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let array = require_array!(arr);

        let len = array.len();
        let skip = len.saturating_sub(n);
        let result: Vec<Value> = array.into_iter().skip(skip).collect();
        Ok(Value::array(result))
    }
}

/// array/dropLast - Drop last n elements
pub struct ArrayDropLastOp;
impl Operator for ArrayDropLastOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "dropLast", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let n = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let array = require_array!(arr);

        let len = array.len();
        let take = len.saturating_sub(n);
        let result: Vec<Value> = array.into_iter().take(take).collect();
        Ok(Value::array(result))
    }
}

// ============================================================================
// Reduce Operator (Phase 1)
// ============================================================================

/// Whether `body` reads `@name` exactly once, outside any nested lambda (which may run many times), and never
/// names `name` bare (a binding of its own, which would shadow it): then that read is the value's last use.
fn read_once(body: &SExpression, name: &str) -> bool {
    fn walk(node: &SExpression, name: &str, in_lambda: bool, reads: &mut usize) -> bool {
        match node {
            SExpression::String(text) => {
                if text == name {
                    return false;
                }
                let token = format!("@{name}");
                let found = text
                    .match_indices(&token)
                    .filter(|(i, _)| {
                        !text[i + token.len()..]
                            .starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
                    .count();
                *reads += found;
                !(in_lambda && found > 0)
            }
            SExpression::Array(items) => {
                let lambda =
                    matches!(items.first(), Some(SExpression::String(head)) if head == "fn");
                items
                    .iter()
                    .all(|item| walk(item, name, in_lambda || lambda, reads))
            }
            SExpression::Object(fields) => fields
                .values()
                .all(|value| walk(value, name, in_lambda, reads)),
            _ => true,
        }
    }
    let mut reads = 0;
    walk(body, name, false, &mut reads) && reads == 1
}

pub struct ReduceOp;
impl Operator for ReduceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["reduce", array, initialValue, fn]
        require_arity!(args, "reduce", >= 3);

        let arr = eval_expr(&args[0], ctx)?;
        let initial = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);

        if let Ok(Value::Lambda { params, body }) = eval_expr(&args[2], ctx) {
            let mut accumulator = initial;
            let linear = params.first().is_some_and(|acc| read_once(&body, acc));
            for item in array {
                let mut child_ctx = ctx.child();

                // Bind parameters: first param is accumulator, second is current item
                if !params.is_empty() {
                    let acc = std::mem::replace(&mut accumulator, Value::Null);
                    if linear {
                        child_ctx.set_linear_local(&params[0], acc);
                    } else {
                        child_ctx.set_local(&params[0], acc);
                    }
                }
                if params.len() >= 2 {
                    child_ctx.set_local(&params[1], item);
                }

                accumulator = eval_expr(&body, &child_ctx)?;
            }

            return Ok(accumulator);
        }

        // Legacy: a raw body expression evaluated with `@acc`/`@item` bound,
        // mirroring MapOp's raw-body branch and the JS evaluator's fallback.
        let reducer = &args[2];
        let linear = read_once(reducer, "acc");
        let mut accumulator = initial;
        for item in array {
            let mut child_ctx = ctx.child();
            let acc = std::mem::replace(&mut accumulator, Value::Null);
            if linear {
                child_ctx.set_linear_local("acc", acc);
            } else {
                child_ctx.set_local(crate::evaluator::LocalName::Fixed("acc"), acc);
            }
            child_ctx.set_local(crate::evaluator::LocalName::Fixed("item"), item);
            accumulator = eval_expr(reducer, &child_ctx)?;
        }

        Ok(accumulator)
    }
}

// ============================================================================
// Phase 5: Parity Completion Operators
// ============================================================================

/// array/max - Maximum value in array
pub struct ArrayMaxOp;
impl Operator for ArrayMaxOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/max", 1);
        let numbers = numbers_of(args, ctx)?;
        Ok(numbers
            .into_iter()
            .reduce(f64::max)
            .map_or(Value::Null, Value::Number))
    }
}

/// array/min - Minimum value in array
pub struct ArrayMinOp;
impl Operator for ArrayMinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/min", 1);
        let numbers = numbers_of(args, ctx)?;
        Ok(numbers
            .into_iter()
            .reduce(f64::min)
            .map_or(Value::Null, Value::Number))
    }
}

/// array/removeItem - return a new array with the first element equal to `item`
/// removed (others kept; unchanged if no match).
pub struct ArrayRemoveItemOp;
impl Operator for ArrayRemoveItemOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/removeItem", >= 2);
        let item = eval_expr(&args[1], ctx)?;
        let mut array = match eval_expr(&args[0], ctx)? {
            Value::Array(a) => a,
            other => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: format!("{:?}", other),
                }))
            }
        };
        let mut found = None;
        for (pos, v) in array.iter().enumerate() {
            if *v == item {
                found = Some(pos);
                break;
            }
        }
        if let Some(pos) = found {
            array.remove(pos);
        }
        Ok(Value::Array(array))
    }
}

/// Shared cosine core for array/cosine and array/nearest — the exact twin of
/// `@almadar/evaluator`'s `cosineSimilarity` (std/array.ts), including its
/// error conditions; a length mismatch, an empty vector or a zero-magnitude vector has no similarity (`None`).
fn cosine_similarity(a: &[Value], b: &[Value]) -> Result<Option<f64>, OrbitalError> {
    if a.len() != b.len() || a.is_empty() {
        return Ok(None);
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (av, bv) in a.iter().zip(b.iter()) {
        let (av, bv) = (av.number()?, bv.number()?);
        dot += av * bv;
        norm_a += av * av;
        norm_b += bv * bv;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return Ok(None);
    }
    Ok(Some(dot / (norm_a.sqrt() * norm_b.sqrt())))
}

/// array/cosine - Cosine similarity between two numeric arrays. Pure math,
/// no side effects — works on arrays the caller already has (embeddings).
pub struct ArrayCosineOp;
impl Operator for ArrayCosineOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/cosine", >= 2);
        let a = require_array!(eval_expr(&args[0], ctx)?);
        let b = require_array!(eval_expr(&args[1], ctx)?);
        Ok(cosine_similarity(&a, &b)?.map_or(Value::Null, Value::Number))
    }
}

/// array/nearest - Best-matching candidate by cosine similarity, plus the
/// margin (best minus runner-up) that gates abstention. A single candidate
/// has no runner-up, so margin is 0 — a margin-gate must abstain rather
/// than fabricate confidence from a comparison never made.
pub struct ArrayNearestOp;
impl Operator for ArrayNearestOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/nearest", >= 2);
        let query = require_array!(eval_expr(&args[0], ctx)?);
        let candidates = require_array!(eval_expr(&args[1], ctx)?);
        if candidates.is_empty() {
            return Ok(Value::Null);
        }
        let mut scores = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            let c = match candidate {
                Value::Array(c) => c,
                other => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: "array".to_string(),
                        actual: other.type_name().to_string(),
                    }))
                }
            };
            match cosine_similarity(&query, c)? {
                Some(score) => scores.push(score),
                None => return Ok(Value::Null),
            }
        }
        let mut best_idx = 0usize;
        let mut best_score = scores[0];
        for (i, s) in scores.iter().enumerate().skip(1) {
            if *s > best_score {
                best_score = *s;
                best_idx = i;
            }
        }
        let mut second_best = f64::NEG_INFINITY;
        for (i, s) in scores.iter().enumerate() {
            if i != best_idx && *s > second_best {
                second_best = *s;
            }
        }
        let margin = if scores.len() == 1 {
            0.0
        } else {
            best_score - second_best
        };
        let mut result = ValueMap::default();
        result.insert("index".to_string(), Value::Number(best_idx as f64));
        result.insert("score".to_string(), Value::Number(best_score));
        result.insert("margin".to_string(), Value::Number(margin));
        Ok(Value::object(result))
    }
}

#[cfg(test)]
mod sort_tests {
    use serde_json::json;
    use {crate::evaluator::context::BindingContextBuilder, crate::evaluator::Evaluator};

    fn sort(args: serde_json::Value) -> Result<serde_json::Value, String> {
        let ctx = BindingContextBuilder::new().build();
        let sexpr = serde_json::from_value(args).unwrap();
        Evaluator::new()
            .evaluate(&sexpr, &ctx)
            .map(|v| serde_json::to_value(v).unwrap())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn sort_numbers_asc_desc() {
        assert_eq!(
            sort(json!(["array/sort", [3, 1, 2]])).unwrap(),
            json!([1.0, 2.0, 3.0])
        );
        assert_eq!(
            sort(json!(["array/sort", [3, 1, 2], null, "desc"])).unwrap(),
            json!([3.0, 2.0, 1.0])
        );
        assert_eq!(
            sort(json!(["array/sort", [3, 1, 2], null, "asc"])).unwrap(),
            json!([1.0, 2.0, 3.0])
        );
    }

    #[test]
    fn sort_strings_by_code_point() {
        assert_eq!(
            sort(json!(["array/sort", ["b", "a", "B", "é"]])).unwrap(),
            json!(["B", "a", "b", "é"])
        );
        assert_eq!(
            sort(json!(["array/sort", ["b", "a", "B", "é"], null, "desc"])).unwrap(),
            json!(["é", "b", "a", "B"])
        );
    }

    #[test]
    fn sort_objects_by_key_asc_desc() {
        let rows = json!([{"price": 3}, {"price": 9}, {"price": 5}]);
        assert_eq!(
            sort(json!(["array/sort", rows, "price", "desc"])).unwrap(),
            json!([{"price": 9.0}, {"price": 5.0}, {"price": 3.0}])
        );
        assert_eq!(
            sort(json!(["array/sort", rows, "price"])).unwrap(),
            json!([{"price": 3.0}, {"price": 5.0}, {"price": 9.0}])
        );
        // a direction word in the key slot is a key, never a direction
        assert_eq!(
            sort(json!(["array/sort", [3, 1, 2], "asc"])).unwrap(),
            json!([3.0, 1.0, 2.0])
        );
    }

    #[test]
    fn sort_equal_keys_keep_input_order_both_directions() {
        let rows =
            json!([{"k": 1, "i": "a"}, {"k": 0, "i": "b"}, {"k": 1, "i": "c"}, {"k": 1, "i": "d"}]);
        assert_eq!(
            sort(json!(["array/sort", rows, "k", "desc"])).unwrap(),
            json!([{"k": 1.0, "i": "a"}, {"k": 1.0, "i": "c"}, {"k": 1.0, "i": "d"}, {"k": 0.0, "i": "b"}])
        );
        assert_eq!(
            sort(json!(["array/sort", rows, "k"])).unwrap(),
            json!([{"k": 0.0, "i": "b"}, {"k": 1.0, "i": "a"}, {"k": 1.0, "i": "c"}, {"k": 1.0, "i": "d"}])
        );
    }

    #[test]
    fn sort_unknown_direction_is_descending() {
        assert_eq!(
            sort(json!(["array/sort", [1, 3, 2], null, "bogus"])).unwrap(),
            json!([3.0, 2.0, 1.0])
        );
    }

    #[test]
    fn sort_missing_and_null_keys_keep_every_row() {
        let rows = json!([{"k": 2}, {}, {"k": 1}, {"k": null}]);
        assert_eq!(
            sort(json!(["array/sort", rows, "k"]))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            sort(json!(["array/sort", rows, "k", "desc"]))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            sort(json!(["array/sort", [{"k": 2}, {"k": null}, {"k": 1}], "k"])).unwrap(),
            json!([{"k": null}, {"k": 1.0}, {"k": 2.0}])
        );
    }

    #[test]
    fn sort_astral_after_high_bmp() {
        assert_eq!(
            sort(json!(["array/sort", ["\u{FF5E}", "\u{1F600}", "a"]])).unwrap(),
            json!(["a", "\u{FF5E}", "\u{1F600}"])
        );
    }

    #[test]
    fn sort_refuses_a_list_of_unlike_element_types() {
        let mixed = json!([2, "a", true, 1, null, [1], "B", {"x": 1}, false]);
        let message = "Evaluation error: Type mismatch: expected number, got string";
        assert_eq!(sort(json!(["array/sort", mixed])).unwrap_err(), message);
        assert_eq!(
            sort(json!(["array/sort", mixed, null, "desc"])).unwrap_err(),
            message
        );
    }

    #[test]
    fn sort_places_null_first_within_one_element_type() {
        assert_eq!(
            sort(json!(["array/sort", [2, null, 1]])).unwrap(),
            json!([null, 1.0, 2.0])
        );
        assert_eq!(
            sort(json!(["array/sort", [false, null, true]])).unwrap(),
            json!([null, false, true])
        );
    }

    #[test]
    fn sort_empty_and_null() {
        assert_eq!(sort(json!(["array/sort", []])).unwrap(), json!([]));
        assert_eq!(
            sort(json!(["array/sort", [], "k", "desc"])).unwrap(),
            json!([])
        );
        assert!(sort(json!(["array/sort", null])).is_err());
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
    fn max_over_numbers_and_refuses_a_text_item() {
        assert_eq!(run(json!(["array/max", [3, 9, 2]])).unwrap(), json!(9.0));
        assert!(run(json!(["array/max", [3, "9", 2]])).is_err());
    }

    #[test]
    fn includes_is_kind_strict() {
        assert_eq!(
            run(json!(["array/includes", [1, 2], 2])).unwrap(),
            json!(true)
        );
        assert!(run(json!(["array/includes", [1, 2], "2"])).is_err());
    }

    #[test]
    fn nth_refuses_text_and_negative_indexes() {
        assert_eq!(run(json!(["array/nth", [5, 6], 1])).unwrap(), json!(6.0));
        assert!(run(json!(["array/nth", [5, 6], "1"])).is_err());
        assert!(run(json!(["array/nth", [5, 6], -1])).is_err());
    }
}
