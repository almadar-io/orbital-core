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
/// params and runs its body where it is written, and a value that is a lambda binds its own; any other value is a
/// type mismatch (the table declares the parameter as a function). Reading it once spares every element a copy of
/// the body.
pub enum Callback<'a> {
    Literal {
        params: Vec<&'a Shared<String>>,
        body: &'a SExpression,
    },
    Lambda {
        params: Vec<String>,
        body: Arc<SExpression>,
    },
}

impl<'a> Callback<'a> {
    pub fn of(callable: &'a SExpression, ctx: &BindingContext) -> Result<Self, OrbitalError> {
        if let SExpression::Array(items) = callable {
            if items.len() >= 3 && items[0].as_str() == Some("fn") {
                let params = match &items[1] {
                    SExpression::String(name) => Some(vec![name]),
                    SExpression::Array(names) => names
                        .iter()
                        .map(|name| match name {
                            SExpression::String(name) => Some(name),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>(),
                    _ => None,
                };
                if let Some(params) = params {
                    return Ok(Callback::Literal {
                        params,
                        body: &items[2],
                    });
                }
            }
        }
        match eval_expr(callable, ctx)? {
            Value::Lambda { params, body } => Ok(Callback::Lambda { params, body }),
            other => Err(other.kind_error("function").into()),
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
    Callback::of(callable, ctx)?.apply(values, ctx)
}

/// A count or index operand: a whole number from 0 up, else `InvalidArgument` naming the operator.
pub(super) fn whole_count(
    value: &Value,
    operator: &str,
    what: &str,
) -> Result<usize, OrbitalError> {
    let n = value.number()?;
    if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
        return Err(EvalError::InvalidArgument(format!(
            "{operator}: {what} {n} is not a whole number from 0 to {}",
            u32::MAX
        ))
        .into());
    }
    Ok(n as usize)
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
            let arr = val.as_array().ok_or_else(|| val.kind_error("array"))?;
            let Some(predicate) = args.get(1) else {
                return Ok(Value::Number(arr.len() as f64));
            };
            let mut count = 0usize;
            let callback = Callback::of(predicate, ctx)?;
            for (i, item) in arr.iter().enumerate() {
                if callback
                    .apply(vec![item.clone(), Value::Number(i as f64)], ctx)?
                    .boolean()?
                {
                    count += 1;
                }
            }
            Ok(Value::Number(count as f64))
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
            let position = whole_count(&idx, "nth", "index")?;
            Ok(array.get(position).cloned().unwrap_or(Value::Null))
        })
    }
}

pub struct IncludesOp;
impl Operator for IncludesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "includes", >= 2);
        super::with_operand(&args[0], ctx, |arr| {
            let item = eval_expr(&args[1], ctx)?;
            let items = arr.as_array().ok_or_else(|| arr.kind_error("array"))?;
            Ok(Value::Bool(items.contains(&item)))
        })
    }
}

pub struct MapOp;
impl Operator for MapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "map", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let array = require_array!(arr);

        let callback = Callback::of(&args[1], ctx)?;
        let results: Result<Vec<Value>, _> = array
            .into_iter()
            .enumerate()
            .map(|(i, item)| callback.apply(vec![item, Value::Number(i as f64)], ctx))
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

        let callback = Callback::of(&args[1], ctx)?;
        let mut kept = Vec::new();
        for (i, item) in array.into_iter().enumerate() {
            if callback
                .apply(vec![item.clone(), Value::Number(i as f64)], ctx)?
                .boolean()?
            {
                kept.push(item);
            }
        }
        Ok(Value::array(kept))
    }
}

pub struct FindOp;
impl Operator for FindOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "find", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = &args[1];

        let array = require_array!(arr);

        let callback = Callback::of(predicate, ctx)?;
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
        let start = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);

        let start_idx = whole_count(&start, "slice", "start")?.min(array.len());
        let end_idx = match args.get(2) {
            Some(end) => whole_count(&eval_expr(end, ctx)?, "slice", "end")?.min(array.len()),
            None => array.len(),
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
            result.extend(require_array!(val));
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
/// elements themselves), `dir` is "asc" (default) or "desc". Stable. Twin of
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
                other => return Err(other.kind_error("string").into()),
            },
            None => None,
        };
        let ascending = match args.get(2) {
            Some(d) => {
                let dir = eval_expr(d, ctx)?;
                match dir.text()? {
                    "asc" => true,
                    "desc" => false,
                    other => {
                        return Err(EvalError::InvalidArgument(format!(
                            "sort: direction \"{other}\" is neither \"asc\" nor \"desc\""
                        ))
                        .into())
                    }
                }
            }
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
        let mut anchor: Option<Value> = None;
        for item in &array {
            let sort_key = field(item);
            if sort_key.is_null() {
                continue;
            }
            match &anchor {
                None => anchor = Some(sort_key),
                Some(first)
                    if std::mem::discriminant(first) != std::mem::discriminant(&sort_key) =>
                {
                    return Err(sort_key.kind_error(first.type_name()).into());
                }
                Some(_) => {}
            }
        }
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

        let callback = Callback::of(predicate, ctx)?;
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

        let callback = Callback::of(predicate, ctx)?;
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
        let idx = whole_count(&eval_expr(&args[1], ctx)?, "insert", "index")?;
        let item = eval_expr(&args[2], ctx)?;

        let mut array = require_array!(arr);

        if idx > array.len() {
            return Err(EvalError::InvalidArgument(format!(
                "insert: index {idx} is past the end of a {}-item array",
                array.len()
            ))
            .into());
        }
        array.insert(idx, item);
        Ok(Value::Array(array))
    }
}

/// array/remove - Remove element at index
pub struct ArrayRemoveOp;
impl Operator for ArrayRemoveOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "remove", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let idx = whole_count(&eval_expr(&args[1], ctx)?, "remove", "index")?;

        let mut array = require_array!(arr);

        if idx >= array.len() {
            return Err(EvalError::InvalidArgument(format!(
                "remove: index {idx} is outside a {}-item array",
                array.len()
            ))
            .into());
        }
        array.remove(idx);
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
                other => return Err(other.kind_error("array").into()),
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

        // A field-name key (`"stage"`) groups by that field's text; a missing or null key is an error (the program
        // `default`s it), never an implicit group.
        if let Value::String(field) = &key_fn {
            for item in array {
                let row = item.as_object().ok_or_else(|| item.kind_error("object"))?;
                let key = match row.get(field.as_str()) {
                    Some(Value::String(text)) => text.to_string(),
                    Some(Value::Null) | None => {
                        return Err(EvalError::InvalidArgument(format!(
                            "array/groupBy: an item has no `{field}` key; default it to group the item"
                        ))
                        .into())
                    }
                    Some(other) => return Err(other.kind_error("string").into()),
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
            let key = eval_expr(&body, &child_ctx)?.text()?.to_string();
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
        let n = whole_count(&eval_expr(&args[1], ctx)?, "take", "count")?;

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
        let n = whole_count(&eval_expr(&args[1], ctx)?, "drop", "count")?;

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

        let first = require_number!(eval_expr(&args[0], ctx)?);
        let end = match args.get(1) {
            Some(end) => match eval_expr(end, ctx)? {
                Value::Null => None,
                end => Some(end.number()?),
            },
            None => None,
        };
        let step = match args.get(2) {
            Some(step) => require_number!(eval_expr(step, ctx)?),
            None => 1.0,
        };
        let (start, end) = match end {
            Some(end) => (first, end),
            None => (0.0, first),
        };

        if step == 0.0 || !step.is_finite() {
            return Err(EvalError::InvalidArgument(format!(
                "range: step {step} must be a non-zero finite number"
            ))
            .into());
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
        let n = whole_count(&eval_expr(&args[1], ctx)?, "takeLast", "count")?;

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
        let n = whole_count(&eval_expr(&args[1], ctx)?, "dropLast", "count")?;

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

        let (params, body) = require_lambda!(eval_expr(&args[2], ctx)?);
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
        let mut array = require_array!(eval_expr(&args[0], ctx)?);
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
    fn sort_refuses_an_unknown_direction() {
        assert!(sort(json!(["array/sort", [1, 3, 2], null, "bogus"])).is_err());
        assert_eq!(
            sort(json!(["array/sort", [1, 3, 2], null, "desc"])).unwrap(),
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
    fn includes_reads_a_list_only_and_flatten_reads_lists_only() {
        assert_eq!(
            run(json!(["array/includes", ["ell"], "ell"])).unwrap(),
            json!(true)
        );
        assert!(run(json!(["array/includes", "hello", "ell"]))
            .unwrap_err()
            .contains("expected array, got string"));
        assert_eq!(
            run(json!(["array/flatten", [[1], [2, 3]]])).unwrap(),
            json!([1.0, 2.0, 3.0])
        );
        assert!(run(json!(["array/flatten", [[1], 2]]))
            .unwrap_err()
            .contains("expected array, got number"));
    }

    #[test]
    fn group_by_a_field_needs_a_text_key_on_every_item() {
        assert!(run(json!(["array/groupBy", [{"k": "a"}, {"j": 1}], "k"])).is_err(), "a missing key is refused");
        assert!(run(json!(["array/groupBy", [{"k": null}], "k"])).is_err(), "a null key is refused");
        assert!(run(json!(["array/groupBy", [{"k": 1}], "k"])).is_err(), "a number key is not text");
        assert_eq!(
            run(json!(["array/groupBy", [{"k": "a", "n": 1}, {"k": "a", "n": 2}], "k"])).unwrap(),
            json!({"a": [{"k": "a", "n": 1.0}, {"k": "a", "n": 2.0}]}),
            "control: text keys group"
        );
    }

    #[test]
    fn includes_compares_structurally() {
        assert_eq!(
            run(json!(["array/includes", [1, 2], 2])).unwrap(),
            json!(true)
        );
        assert_eq!(
            run(json!(["array/includes", [1, 2], "2"])).unwrap(),
            json!(false),
            "a value of another kind is unequal, never an error"
        );
        assert!(run(json!(["array/includes", "12", 2])).is_err(), "control: the list must be a list");
    }

    #[test]
    fn nth_refuses_text_and_negative_indexes() {
        assert_eq!(run(json!(["array/nth", [5, 6], 1])).unwrap(), json!(6.0));
        assert!(run(json!(["array/nth", [5, 6], "1"])).is_err());
        assert!(run(json!(["array/nth", [5, 6], -1])).is_err());
    }

    #[test]
    fn concat_takes_arrays_only() {
        assert_eq!(
            run(json!(["array/concat", [1], [2, 3]])).unwrap(),
            json!([1.0, 2.0, 3.0])
        );
        assert!(run(json!(["array/concat", [1], null])).is_err());
        assert!(run(json!(["array/concat", [1], 2])).is_err());
    }

    #[test]
    fn count_takes_an_array_with_an_optional_predicate() {
        assert_eq!(run(json!(["array/count", [1, 2, 3]])).unwrap(), json!(3.0));
        assert_eq!(
            run(json!([
                "array/count",
                [1, 2, 3],
                ["fn", "x", [">", "@x", 1]]
            ]))
            .unwrap(),
            json!(2.0)
        );
        assert!(run(json!(["array/count", null])).is_err());
        assert!(run(json!(["array/count", "abc"])).is_err());
    }

    #[test]
    fn a_callback_must_be_a_function() {
        let double = json!(["fn", "x", ["*", "@x", 2]]);
        assert_eq!(
            run(json!(["array/map", [1, 2], double])).unwrap(),
            json!([2.0, 4.0])
        );
        assert!(run(json!(["array/map", [1, 2], null])).is_err());
        assert!(run(json!(["array/map", [1, 2], ["*", "@item", 2]])).is_err());
        assert!(run(json!(["array/filter", [1, 2], true])).is_err());
        assert!(run(json!(["array/reduce", [1, 2], 0, ["+", "@acc", "@item"]])).is_err());
        assert_eq!(
            run(json!([
                "array/reduce",
                [1, 2],
                0,
                ["fn", ["a", "x"], ["+", "@a", "@x"]]
            ]))
            .unwrap(),
            json!(3.0)
        );
    }

    #[test]
    fn counts_and_indexes_are_whole_numbers_in_range() {
        assert_eq!(
            run(json!(["array/take", [1, 2, 3], 2])).unwrap(),
            json!([1.0, 2.0])
        );
        assert!(run(json!(["array/take", [1, 2, 3], -1])).is_err());
        assert!(run(json!(["array/drop", [1, 2, 3], 1.5])).is_err());
        assert!(run(json!(["array/takeLast", [1, 2, 3], -2])).is_err());
        assert!(run(json!(["array/dropLast", [1, 2, 3], 0.5])).is_err());
        assert_eq!(
            run(json!(["array/slice", [1, 2, 3], 1, 2])).unwrap(),
            json!([2.0])
        );
        assert!(run(json!(["array/slice", [1, 2, 3], -1])).is_err());
        assert!(run(json!(["array/slice", [1, 2, 3], 0, 1.5])).is_err());
        assert_eq!(
            run(json!(["array/insert", [1, 3], 1, 2])).unwrap(),
            json!([1.0, 2.0, 3.0])
        );
        assert!(run(json!(["array/insert", [1, 3], 3, 2])).is_err());
        assert_eq!(
            run(json!(["array/remove", [1, 2], 0])).unwrap(),
            json!([2.0])
        );
        assert!(run(json!(["array/remove", [1, 2], 2])).is_err());
    }

    #[test]
    fn range_treats_a_null_end_as_omitted_and_refuses_a_zero_step() {
        assert_eq!(
            run(json!(["array/range", 0, 5, 2])).unwrap(),
            json!([0.0, 2.0, 4.0])
        );
        assert_eq!(
            run(json!(["array/range", 5, null, 2])).unwrap(),
            json!([0.0, 2.0, 4.0])
        );
        assert_eq!(
            run(json!(["array/range", 3])).unwrap(),
            json!([0.0, 1.0, 2.0])
        );
        assert!(run(json!(["array/range", 0, 5, 0])).is_err());
        assert!(run(json!(["array/range", 0, "5"])).is_err());
    }

    #[test]
    fn group_by_demands_object_items_and_a_text_key() {
        assert!(run(json!(["array/groupBy", [{"k": "a"}], "k"])).is_ok());
        assert!(run(json!(["array/groupBy", [1, 2], "k"])).is_err());
        assert!(run(json!(["array/groupBy", [1, 2], ["fn", "x", "@x"]])).is_err());
        assert!(run(json!([
            "array/groupBy",
            [1, 2],
            ["fn", "x", ["str/concat", "k", "@x"]]
        ]))
        .is_ok());
    }
}
