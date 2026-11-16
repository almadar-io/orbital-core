//! Array operators.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;
use orbital_core_base::value::{Shared, ValueMap};
use std::sync::Arc;

/// The numbers `array/sum` / `max` / `min` / `avg` fold: each element (or its `args[1]` field), which must be a
/// number or an int; any other kind, or an object missing the field, is an error.
fn numbers_of(args: &[&Value], _ctx: &BindingContext) -> Result<Vec<Value>, OrbitalError> {
    let arr = args[0];
    let array = require_array!(arr);
    let key = match args.get(1).copied() {
        Some(k) => Some(k.text()?.to_string()),
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
        if !value.is_numeric() {
            return Err(value.kind_error("number").into());
        }
        numbers.push(value.clone());
    }
    Ok(numbers)
}

fn all_ints(numbers: &[Value]) -> bool {
    numbers.iter().all(|n| matches!(n, Value::Int(_)))
}

fn floats_of(numbers: &[Value]) -> Result<Vec<f64>, OrbitalError> {
    numbers.iter().map(|n| n.number().map_err(OrbitalError::from)).collect()
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
    /// The callable the call site gives at an operand position (a value entry, see [`CallSite::invoke`]).
    Site {
        site: &'a dyn CallSite,
        index: usize,
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
            Value::Lambda(l) => Ok(Callback::Lambda { params: l.params.clone(), body: l.body.clone() }),
            other => Err(other.kind_error("function").into()),
        }
    }

    /// Run the callback on one element. `values` are bound to the params in order, and `@item`/`@index` to the first
    /// two (twin of almadar-evaluator's `evalWithItem`/`evalLambda`).
    pub fn apply(&self, values: Vec<Value>, ctx: &BindingContext) -> EvalResult {
        if let Callback::Site { site, index } = self {
            return site.invoke(*index, values);
        }
        let values: Vec<Arc<Value>> /* shared so `item`, `index` and the params bind the same values */ = values.into_iter().map(Arc::new).collect();
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
            Callback::Site { .. } => unreachable!("a site callback is answered before the bindings"),
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
    if !value.is_numeric() {
        return Err(value.kind_error("number").into());
    }
    match value.as_int().filter(|n| (0..=i64::from(u32::MAX)).contains(n)) {
        Some(n) => Ok(n as usize),
        None => Err(EvalError::InvalidArgument(format!(
            "{operator}: {what} {} is not a whole number from 0 to {}",
            value.to_string_value(),
            u32::MAX
        ))
        .into()),
    }
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
                        return Ok(super::util::int_of(count));
                    } else {
                        return Ok(super::util::int_of(entities.len()));
                    }
                }
            }
        }

        // Regular array count, with an optional predicate
        super::with_operand(&args[0], ctx, |val| {
            let arr = val.as_array().ok_or_else(|| val.kind_error("array"))?;
            let Some(predicate) = args.get(1) else {
                return Ok(super::util::int_of(arr.len()));
            };
            counted(arr, &Callback::of(predicate, ctx)?, ctx)
        })
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "count", 1);
        // The count query over a registered entity store reads its filter under each row as `@entity`: no value entry.
        if args.len() >= 2 && site.literal(0) && matches!(&args[0], Value::String(name) if ctx.get_entities(name).is_some()) {
            return None;
        }
        let Some(arr) = args[0].as_array() else {
            return Some(Err(args[0].kind_error("array").into()));
        };
        Some(if args.len() < 2 {
            Ok(super::util::int_of(arr.len()))
        } else {
            counted(arr, &Callback::Site { site, index: 1 }, ctx)
        })
    }
}

/// How many elements of `arr` the callback accepts.
fn counted(arr: &[Value], callback: &Callback, ctx: &BindingContext) -> EvalResult {
    let mut count = 0usize;
    for (i, item) in arr.iter().enumerate() {
        if callback.apply(vec![item.clone(), super::util::int_of(i)], ctx)?.boolean()? {
            count += 1;
        }
    }
    Ok(super::util::int_of(count))
}

pub struct ArrayLenOp;
impl StrictOperator for ArrayLenOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/len", 1);
        let val = args[0];
        match val {
            Value::Array(items) => Ok(super::util::int_of(items.len())),
            other => Err(other.kind_error("array").into()),
        }
    }
}

pub struct FirstOp;
impl StrictOperator for FirstOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "first", 1);
        let val = args[0];
        match val {
            Value::Array(arr) => Ok(arr.first().cloned().unwrap_or(Value::Null)),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "array".to_string(),
                actual: val.type_name().to_string(),
            })),
        }
    }
}

pub struct LastOp;
impl StrictOperator for LastOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "last", 1);
        let val = args[0];
        match val {
            Value::Array(arr) => Ok(arr.last().cloned().unwrap_or(Value::Null)),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "array".to_string(),
                actual: val.type_name().to_string(),
            })),
        }
    }
}

pub struct NthOp;
impl StrictOperator for NthOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "nth", >= 2);
        let arr = args[0];
        let idx = args[1];
        let Value::Array(array) = arr else {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "array".to_string(),
                actual: arr.type_name().to_string(),
            }));
        };
        let position = whole_count(idx, "nth", "index")?;
        Ok(array.get(position).cloned().unwrap_or(Value::Null))
    }
}

pub struct IncludesOp;
impl StrictOperator for IncludesOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "includes", >= 2);
        let arr = args[0];
        let item = args[1];
        let items = arr.as_array().ok_or_else(|| arr.kind_error("array"))?;
        Ok(Value::Bool(items.contains(item)))
    }
}

pub struct MapOp;
impl Operator for MapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "map", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let array = require_array!(arr);
        map_each(array, &Callback::of(&args[1], ctx)?, ctx)
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "map", 2);
        Some(listed(std::mem::take(&mut args[0])).and_then(|array| map_each(array, &Callback::Site { site, index: 1 }, ctx)))
    }
}

fn listed(value: Value) -> Result<Shared<Vec<Value>>, OrbitalError> {
    Ok(require_array!(value))
}

fn map_each(array: Shared<Vec<Value>>, callback: &Callback, ctx: &BindingContext) -> EvalResult {
    let results: Result<Vec<Value>, _> = array
        .into_iter()
        .enumerate()
        .map(|(i, item)| callback.apply(vec![item, super::util::int_of(i)], ctx))
        .collect();
    Ok(Value::array(results?))
}

/// The first element the predicate accepts with its position, or every element it accepts.
fn accepted(array: Shared<Vec<Value>>, callback: &Callback, ctx: &BindingContext, all: bool) -> Result<Vec<Value>, OrbitalError> {
    let mut kept = Vec::new();
    for (i, item) in array.into_iter().enumerate() {
        if callback.apply(vec![item.clone(), super::util::int_of(i)], ctx)?.boolean()? {
            kept.push(item);
            if !all {
                break;
            }
        }
    }
    Ok(kept)
}

/// Whether the predicate answers `wanted` for some element: `some` looks for a `true`, `every` for a `false`.
fn any_answers(array: Shared<Vec<Value>>, callback: &Callback, ctx: &BindingContext, wanted: bool) -> Result<bool, OrbitalError> {
    for (i, item) in array.into_iter().enumerate() {
        if callback.apply(vec![item, super::util::int_of(i)], ctx)?.boolean()? == wanted {
            return Ok(true);
        }
    }
    Ok(false)
}

pub struct FilterOp;
impl Operator for FilterOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "filter", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let array = require_array!(arr);
        Ok(Value::array(accepted(array, &Callback::of(&args[1], ctx)?, ctx, true)?))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "filter", 2);
        Some(
            listed(std::mem::take(&mut args[0]))
                .and_then(|array| accepted(array, &Callback::Site { site, index: 1 }, ctx, true))
                .map(Value::array),
        )
    }
}

pub struct FindOp;
impl Operator for FindOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "find", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = &args[1];

        let array = require_array!(arr);
        Ok(accepted(array, &Callback::of(predicate, ctx)?, ctx, false)?.pop().unwrap_or(Value::Null))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "find", 2);
        Some(
            listed(std::mem::take(&mut args[0]))
                .and_then(|array| accepted(array, &Callback::Site { site, index: 1 }, ctx, false))
                .map(|mut found| found.pop().unwrap_or(Value::Null)),
        )
    }
}

// ============================================================================
// Phase 2.1: Array Essentials
// ============================================================================

/// array/slice - Extract portion of array
pub struct ArraySliceOp;
impl StrictOperator for ArraySliceOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        // ["slice", arr, start, end?]
        require_arity!(args, "slice", >= 2);
        let arr = args[0];
        let start = args[1];

        let array = require_array!(arr);

        let start_idx = whole_count(start, "slice", "start")?.min(array.len());
        let end_idx = match args.get(2).copied() {
            Some(end) => whole_count(end, "slice", "end")?.min(array.len()),
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
owning_operator!(ArrayConcatOp);
impl OwningOperator for ArrayConcatOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        let mut result = Vec::new();
        for arg in args.iter_mut() {
            result.extend(require_array!(std::mem::take(arg)));
        }
        Ok(Value::array(result))
    }
}

/// array/append - Add element to end of array
pub struct ArrayAppendOp;
owning_operator!(ArrayAppendOp);
impl OwningOperator for ArrayAppendOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "append", >= 2);
        let arr = std::mem::take(&mut args[0]);
        let item = std::mem::take(&mut args[1]);

        let mut array = require_array!(arr);

        array.push(item);
        Ok(Value::Array(array))
    }
}

fn sort_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) | Value::Int(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        _ => 5,
    }
}

/// Total order of two present (non-null) values (so `sort_by` can never panic): boolean < number (NaN last) <
/// string < array < other, then by value; `array/sort` places null after every value in both directions. Twin of almadar-evaluator `compareSortValues`.
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
        (Value::Int(_), Value::Int(_) | Value::Number(_)) | (Value::Number(_), Value::Int(_)) => {
            let nan = |v: &Value| matches!(v, Value::Number(n) if n.is_nan());
            match (nan(a), nan(b)) {
                (true, _) => Ordering::Greater,
                (_, true) => Ordering::Less,
                _ => a.compare(b).unwrap_or(Ordering::Equal),
            }
        }
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// array/sort `(arr, key?, dir?)` - sorted copy; `key` orders objects by that field (null or empty = the
/// elements themselves), `dir` is "asc" (default) or "desc". Stable. Null and a row missing the key sort last in
/// both directions. Twin of
/// almadar-evaluator `evalArraySort`.
pub struct ArraySortOp;
owning_operator!(ArraySortOp);
impl OwningOperator for ArraySortOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "sort", 1);
        let arr = std::mem::take(&mut args[0]);
        let mut array = require_array!(arr);

        let key = match args.get(1) {
            Some(k) => match k {
                Value::Null => None,
                Value::String(s) if s.is_empty() => None,
                Value::String(s) => Some(s.clone()),
                other => return Err(other.kind_error("string").into()),
            },
            None => None,
        };
        let ascending = match args.get(2) {
            Some(d) => {
                let dir = d;
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
                    if !(first.is_numeric() && sort_key.is_numeric())
                        && std::mem::discriminant(first) != std::mem::discriminant(&sort_key) =>
                {
                    return Err(sort_key.kind_error(first.type_name()).into());
                }
                Some(_) => {}
            }
        }
        array.sort_by(|a, b| {
            let (x, y) = (field(a), field(b));
            match (x.is_null(), y.is_null()) {
                (true, true) => return std::cmp::Ordering::Equal,
                (true, false) => return std::cmp::Ordering::Greater,
                (false, true) => return std::cmp::Ordering::Less,
                (false, false) => {}
            }
            let ord = sort_order(&x, &y);
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
owning_operator!(ArrayUniqueOp);
impl OwningOperator for ArrayUniqueOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "unique", 1);
        let arr = std::mem::take(&mut args[0]);

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
impl StrictOperator for ArrayIndexOfOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "indexOf", >= 2);
        let arr = args[0];
        let needle = args[1];

        let array = require_array!(arr);

        for (i, item) in array.iter().enumerate() {
            if *item == *needle {
                return Ok(super::util::int_of(i));
            }
        }

        Ok(Value::Int(-1))
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
        Ok(Value::Bool(!any_answers(array, &Callback::of(predicate, ctx)?, ctx, false)?))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "every", 2);
        Some(
            listed(std::mem::take(&mut args[0]))
                .and_then(|array| any_answers(array, &Callback::Site { site, index: 1 }, ctx, false))
                .map(|refused| Value::Bool(!refused)),
        )
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
        Ok(Value::Bool(any_answers(array, &Callback::of(predicate, ctx)?, ctx, true)?))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "some", 2);
        Some(
            listed(std::mem::take(&mut args[0]))
                .and_then(|array| any_answers(array, &Callback::Site { site, index: 1 }, ctx, true))
                .map(Value::Bool),
        )
    }
}

/// array/sum - Sum numeric array
pub struct ArraySumOp;
impl StrictOperator for ArraySumOp {
    fn apply(&self, args: &[&Value], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "sum", 1);
        let numbers = numbers_of(args, ctx)?;
        if all_ints(&numbers) {
            let mut sum = 0i64;
            for n in &numbers {
                sum = n
                    .as_int()
                    .and_then(|n| sum.checked_add(n))
                    .ok_or_else(|| EvalError::InvalidArgument("array/sum: int overflow".to_string()))?;
            }
            return Ok(Value::Int(sum));
        }

        Ok(Value::Number(floats_of(&numbers)?.into_iter().sum()))
    }
}

// ============================================================================
// Phase 3.1: Array Advanced
// ============================================================================

/// array/prepend - Add element to beginning
pub struct ArrayPrependOp;
owning_operator!(ArrayPrependOp);
impl OwningOperator for ArrayPrependOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "prepend", >= 2);
        let arr = std::mem::take(&mut args[0]);
        let item = std::mem::take(&mut args[1]);

        let mut array = require_array!(arr);

        array.insert(0, item);
        Ok(Value::Array(array))
    }
}

/// array/insert - Insert element at index
pub struct ArrayInsertOp;
owning_operator!(ArrayInsertOp);
impl OwningOperator for ArrayInsertOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "insert", >= 3);
        let arr = std::mem::take(&mut args[0]);
        let idx = whole_count(&args[1], "insert", "index")?;
        let item = std::mem::take(&mut args[2]);

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
owning_operator!(ArrayRemoveOp);
impl OwningOperator for ArrayRemoveOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "remove", >= 2);
        let arr = std::mem::take(&mut args[0]);
        let idx = whole_count(&args[1], "remove", "index")?;

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
owning_operator!(ArrayReverseOp);
impl OwningOperator for ArrayReverseOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "reverse", 1);
        let arr = std::mem::take(&mut args[0]);

        let mut array = require_array!(arr);

        array.reverse();
        Ok(Value::Array(array))
    }
}

/// array/flatten - Flatten nested arrays (one level)
pub struct ArrayFlattenOp;
owning_operator!(ArrayFlattenOp);
impl OwningOperator for ArrayFlattenOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "flatten", 1);
        let arr = std::mem::take(&mut args[0]);

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
owning_operator!(ArrayZipOp);
impl OwningOperator for ArrayZipOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "zip", >= 2);
        let arr1 = std::mem::take(&mut args[0]);
        let arr2 = std::mem::take(&mut args[1]);

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
        first_index(array, |item, index| {
            let mut child_ctx = ctx.child();
            if !params.is_empty() {
                child_ctx.set_local(&params[0], item);
            }
            if params.len() > 1 {
                child_ctx.set_local(&params[1], index);
            }
            eval_expr(&body, &child_ctx)
        })
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "findIndex", 2);
        Some(listed(std::mem::take(&mut args[0])).and_then(|array| first_index(array, |item, index| site.invoke(1, vec![item, index]))))
    }
}

/// The position of the first element `test` accepts, -1 when it accepts none.
fn first_index(array: Shared<Vec<Value>>, mut test: impl FnMut(Value, Value) -> EvalResult) -> EvalResult {
    for (i, item) in array.into_iter().enumerate() {
        if test(item, super::util::int_of(i))?.boolean()? {
            return Ok(super::util::int_of(i));
        }
    }
    Ok(Value::Int(-1))
}

/// array/avg - Average of numeric array
pub struct ArrayAvgOp;
impl StrictOperator for ArrayAvgOp {
    fn apply(&self, args: &[&Value], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "avg", 1);
        let nums = floats_of(&numbers_of(args, ctx)?)?;
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
        if let Value::String(field) = &key_fn {
            return grouped(array, |item, _| field_group(item, field));
        }
        let key = bound_lambda(&key_fn, ctx)?;
        grouped(array, |item, i| Ok(key(vec![item.clone(), super::util::int_of(i)])?.text()?.to_string()))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "groupBy", 2);
        let key_fn = std::mem::take(&mut args[1]);
        Some(listed(std::mem::take(&mut args[0])).and_then(|array| match &key_fn {
            Value::String(field) => grouped(array, |item, _| field_group(item, field)),
            _ => grouped(array, |item, i| Ok(site.invoke(1, vec![item.clone(), super::util::int_of(i)])?.text()?.to_string())),
        }))
    }
}

/// A lambda value as the function of its own parameters (`@item` and `@index` are not bound): how an operator that
/// reads its callback as a value runs it.
pub(super) fn bound_lambda<'a>(
    callable: &Value,
    ctx: &'a BindingContext,
) -> Result<impl Fn(Vec<Value>) -> EvalResult + Sync + Send + 'a, OrbitalError> {
    let (params, body) = require_lambda!(callable);
    Ok(move |values: Vec<Value>| {
        let mut child = ctx.child();
        for (param, value) in params.iter().zip(values) {
            child.set_local(param, value);
        }
        eval_expr(&body, &child)
    })
}

/// The group a field-name key (`"stage"`) puts `item` in: that field's text. A missing or null key is an error (the
/// program `default`s it), never an implicit group.
fn field_group(item: &Value, field: &str) -> Result<String, OrbitalError> {
    let row = item.as_object().ok_or_else(|| item.kind_error("object"))?;
    match row.get(field) {
        Some(Value::String(text)) => Ok(text.to_string()),
        Some(Value::Null) | None => Err(EvalError::InvalidArgument(format!(
            "array/groupBy: an item has no `{field}` key; default it to group the item"
        ))
        .into()),
        Some(other) => Err(other.kind_error("string").into()),
    }
}

/// The items of `array` by the group `key_of` gives each, groups in first-seen order.
fn grouped(array: Shared<Vec<Value>>, mut key_of: impl FnMut(&Value, usize) -> Result<String, OrbitalError>) -> EvalResult {
    let mut groups: orbital_core_base::collections::IndexMap<String, Vec<Value>> = Default::default();
    for (i, item) in array.into_iter().enumerate() {
        let key = key_of(&item, i)?;
        groups.entry(key).or_default().push(item);
    }
    Ok(Value::Object(groups.into_iter().map(|(k, v)| (k.into(), Value::array(v))).collect()))
}

/// array/take - Take first n elements
pub struct ArrayTakeOp;
owning_operator!(ArrayTakeOp);
impl OwningOperator for ArrayTakeOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "take", >= 2);
        let arr = std::mem::take(&mut args[0]);
        let n = whole_count(&args[1], "take", "count")?;

        let array = require_array!(arr);

        let result: Vec<Value> = array.into_iter().take(n).collect();
        Ok(Value::array(result))
    }
}

/// array/drop - Drop first n elements
pub struct ArrayDropOp;
owning_operator!(ArrayDropOp);
impl OwningOperator for ArrayDropOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "drop", >= 2);
        let arr = std::mem::take(&mut args[0]);
        let n = whole_count(&args[1], "drop", "count")?;

        let array = require_array!(arr);

        let result: Vec<Value> = array.into_iter().skip(n).collect();
        Ok(Value::array(result))
    }
}

/// array/range - Generate numeric range
pub struct ArrayRangeOp;
impl StrictOperator for ArrayRangeOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "range", 1);

        let first = args[0].clone();
        let end = match args.get(1).copied() {
            Some(end) => match end {
                Value::Null => None,
                end => Some(end.clone()),
            },
            None => None,
        };
        let step = match args.get(2).copied() {
            Some(step) => step.clone(),
            None => Value::Int(1),
        };
        let (start, end) = match end {
            Some(end) => (first, end),
            None => (Value::Int(0), first),
        };

        if let (Value::Int(start), Value::Int(end), Value::Int(step)) = (&start, &end, &step) {
            if *step == 0 {
                return Err(EvalError::InvalidArgument("range: step 0 must be a non-zero finite number".to_string()).into());
            }
            let mut result = Vec::new();
            let mut current = *start;
            while if *step > 0 { current < *end } else { current > *end } {
                result.push(Value::Int(current));
                match current.checked_add(*step) {
                    Some(next) => current = next,
                    None => break,
                }
            }
            return Ok(Value::array(result));
        }

        let (start, end, step) = (require_number!(start), require_number!(end), require_number!(step));

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
owning_operator!(ArrayShuffleOp);
impl OwningOperator for ArrayShuffleOp {
    fn apply_owned(&self, args: &mut [Value], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "shuffle", 1);
        let arr = std::mem::take(&mut args[0]);

        let array = require_array!(arr);

        // Fisher-Yates over the sealed-entropy stream (see `math::drawn`); the journal records the input and the order.
        let journaled = [Value::Array(array.clone())];
        super::math::drawn(ctx, "array/shuffle", &journaled, |rng| {
            let mut shuffled = array.clone();
            for i in (1..shuffled.len()).rev() {
                let j = super::math::below(rng, i as u64 + 1) as usize;
                shuffled.swap(i, j);
            }
            Value::Array(shuffled)
        })
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
        let test = bound_lambda(&predicate, ctx)?;
        Ok(Value::array(parted(array, test)?.1))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "reject", 2);
        Some(listed(std::mem::take(&mut args[0])).and_then(|array| Ok(Value::array(parted(array, |values| site.invoke(1, values))?.1))))
    }
}

/// The items of `array` the test accepts, then the ones it does not; the test is given the item and its index.
fn parted(array: Shared<Vec<Value>>, mut test: impl FnMut(Vec<Value>) -> EvalResult) -> Result<(Vec<Value>, Vec<Value>), OrbitalError> {
    let (mut matches, mut non_matches) = (Vec::new(), Vec::new());
    for (i, item) in array.into_iter().enumerate() {
        if test(vec![item.clone(), super::util::int_of(i)])?.boolean()? {
            matches.push(item);
        } else {
            non_matches.push(item);
        }
    }
    Ok((matches, non_matches))
}

/// array/partition - Split by predicate
pub struct ArrayPartitionOp;
impl Operator for ArrayPartitionOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "partition", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let predicate = eval_expr(&args[1], ctx)?;

        let array = require_array!(arr);
        let test = bound_lambda(&predicate, ctx)?;
        parted(array, test).map(halves)
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "partition", 2);
        Some(listed(std::mem::take(&mut args[0])).and_then(|array| parted(array, |values| site.invoke(1, values)).map(halves)))
    }
}

fn halves((matches, non_matches): (Vec<Value>, Vec<Value>)) -> Value {
    Value::array(vec![Value::array(matches), Value::array(non_matches)])
}

/// array/takeLast - Take last n elements
pub struct ArrayTakeLastOp;
owning_operator!(ArrayTakeLastOp);
impl OwningOperator for ArrayTakeLastOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "takeLast", >= 2);
        let arr = std::mem::take(&mut args[0]);
        let n = whole_count(&args[1], "takeLast", "count")?;

        let array = require_array!(arr);

        let len = array.len();
        let skip = len.saturating_sub(n);
        let result: Vec<Value> = array.into_iter().skip(skip).collect();
        Ok(Value::array(result))
    }
}

/// array/dropLast - Drop last n elements
pub struct ArrayDropLastOp;
owning_operator!(ArrayDropLastOp);
impl OwningOperator for ArrayDropLastOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "dropLast", >= 2);
        let arr = std::mem::take(&mut args[0]);
        let n = whole_count(&args[1], "dropLast", "count")?;

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
        let linear = params.first().is_some_and(|acc| read_once(&body, acc));
        folded(array, initial, |acc, item| fold_step(&params, &body, linear, acc, item, ctx))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "reduce", 3);
        let initial = std::mem::take(&mut args[1]);
        Some(listed(std::mem::take(&mut args[0])).and_then(|array| folded(array, initial, |acc, item| site.invoke(2, vec![acc, item]))))
    }
}

/// `array` folded left to right from `initial`: each step is given the accumulator and the element, both owned.
fn folded(array: Shared<Vec<Value>>, initial: Value, mut step: impl FnMut(Value, Value) -> EvalResult) -> EvalResult {
    let mut accumulator = initial;
    for item in array {
        accumulator = step(accumulator, item)?;
    }
    Ok(accumulator)
}

/// Elements per `array/preduce` chunk; the grouping is part of the operator's meaning, so a result never depends on
/// how many threads ran it.
const PREDUCE_CHUNK: usize = 1024;

/// `0..count` through `f`, in index order: on worker threads when the `parallel` feature is on, else on this one.
/// The first error in index order is the one returned, whichever thread met an error first.
fn indexed<T: Send>(count: usize, f: impl Fn(usize) -> Result<T, OrbitalError> + Sync + Send) -> Result<Vec<T>, OrbitalError> {
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    {
        use rayon::prelude::*;
        let results: Vec<Result<T, OrbitalError>> = (0..count).into_par_iter().map(f).collect();
        results.into_iter().collect()
    }
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    {
        (0..count).map(f).collect()
    }
}

/// array/pmap (array, fn) - `array/map` with the elements evaluated in parallel; the result is in input order
pub struct ArrayPmapOp;
impl Operator for ArrayPmapOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/pmap", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let items: Vec<Value> = require_array!(arr).into_iter().collect();
        each_in_parallel(items, &Callback::of(&args[1], ctx)?, ctx)
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "array/pmap", 2);
        Some(
            listed(std::mem::take(&mut args[0]))
                .and_then(|array| each_in_parallel(array.into_iter().collect(), &Callback::Site { site, index: 1 }, ctx)),
        )
    }
}

fn each_in_parallel(items: Vec<Value>, callback: &Callback, ctx: &BindingContext) -> EvalResult {
    indexed(items.len(), |i| callback.apply(vec![items[i].clone(), super::util::int_of(i)], ctx)).map(Value::array)
}

fn fold_step(params: &[String], body: &SExpression, linear: bool, acc: Value, item: Value, ctx: &BindingContext) -> EvalResult {
    let mut child = ctx.child();
    if let Some(name) = params.first() {
        if linear {
            child.set_linear_local(name, acc);
        } else {
            child.set_local(name, acc);
        }
    }
    if let Some(name) = params.get(1) {
        child.set_local(name, item);
    }
    eval_expr(body, &child)
}

/// array/preduce (array, init, fn) - `array/reduce` with a fixed grouping: the array splits into chunks of 1024, each
/// chunk folds left to right from its first element (chunks in parallel), then the chunk results fold left to right
/// from `init`. An empty array answers `init`.
pub struct ArrayPreduceOp;
impl Operator for ArrayPreduceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/preduce", >= 3);
        let arr = eval_expr(&args[0], ctx)?;
        let initial = eval_expr(&args[1], ctx)?;
        let items: Vec<Value> = require_array!(arr).into_iter().collect();
        let (params, body) = require_lambda!(eval_expr(&args[2], ctx)?);
        let linear = params.first().is_some_and(|acc| read_once(&body, acc));
        folded_in_chunks(&items, initial, |acc, item| fold_step(&params, &body, linear, acc, item, ctx))
    }

    fn evaluation(&self) -> OperatorEvaluation {
        OperatorEvaluation::Site
    }

    fn apply_values(&self, args: &mut [Value], site: &dyn CallSite, _ctx: &BindingContext) -> Option<EvalResult> {
        require_arity_some!(args, "array/preduce", 3);
        let initial = std::mem::take(&mut args[1]);
        Some(listed(std::mem::take(&mut args[0])).and_then(|array| {
            let items: Vec<Value> = array.into_iter().collect();
            folded_in_chunks(&items, initial, |acc, item| site.invoke(2, vec![acc, item]))
        }))
    }
}

fn folded_in_chunks(items: &[Value], initial: Value, step: impl Fn(Value, Value) -> EvalResult + Sync + Send) -> EvalResult {
    let chunks: Vec<&[Value]> = items.chunks(PREDUCE_CHUNK).collect();
    let folded = indexed(chunks.len(), |c| {
        let mut rest = chunks[c].iter().cloned();
        let mut accumulator = rest.next().unwrap_or(Value::Null);
        for item in rest {
            accumulator = step(accumulator, item)?;
        }
        Ok(accumulator)
    })?;
    let mut accumulator = initial;
    for chunk in folded {
        accumulator = step(accumulator, chunk)?;
    }
    Ok(accumulator)
}

// ============================================================================
// Phase 5: Parity Completion Operators
// ============================================================================

/// array/max - Maximum value in array
pub struct ArrayMaxOp;
impl StrictOperator for ArrayMaxOp {
    fn apply(&self, args: &[&Value], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/max", 1);
        let numbers = numbers_of(args, ctx)?;
        if all_ints(&numbers) {
            return Ok(numbers
                .iter()
                .filter_map(Value::as_int)
                .reduce(i64::max)
                .map_or(Value::Null, Value::Int));
        }
        Ok(floats_of(&numbers)?
            .into_iter()
            .reduce(f64::max)
            .map_or(Value::Null, Value::Number))
    }
}

/// array/min - Minimum value in array
pub struct ArrayMinOp;
impl StrictOperator for ArrayMinOp {
    fn apply(&self, args: &[&Value], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/min", 1);
        let numbers = numbers_of(args, ctx)?;
        if all_ints(&numbers) {
            return Ok(numbers
                .iter()
                .filter_map(Value::as_int)
                .reduce(i64::min)
                .map_or(Value::Null, Value::Int));
        }
        Ok(floats_of(&numbers)?
            .into_iter()
            .reduce(f64::min)
            .map_or(Value::Null, Value::Number))
    }
}

/// array/removeItem - return a new array with the first element equal to `item`
/// removed (others kept; unchanged if no match).
pub struct ArrayRemoveItemOp;
owning_operator!(ArrayRemoveItemOp);
impl OwningOperator for ArrayRemoveItemOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/removeItem", >= 2);
        let item = std::mem::take(&mut args[1]);
        let mut array = require_array!(std::mem::take(&mut args[0]));
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
owning_operator!(ArrayCosineOp);
impl OwningOperator for ArrayCosineOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/cosine", >= 2);
        let a = require_array!(std::mem::take(&mut args[0]));
        let b = require_array!(std::mem::take(&mut args[1]));
        Ok(cosine_similarity(&a, &b)?.map_or(Value::Null, Value::Number))
    }
}

/// array/nearest - Best-matching candidate by cosine similarity, plus the
/// margin (best minus runner-up) that gates abstention. A single candidate
/// has no runner-up, so margin is 0 — a margin-gate must abstain rather
/// than fabricate confidence from a comparison never made.
pub struct ArrayNearestOp;
owning_operator!(ArrayNearestOp);
impl OwningOperator for ArrayNearestOp {
    fn apply_owned(&self, args: &mut [Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "array/nearest", >= 2);
        let query = require_array!(std::mem::take(&mut args[0]));
        let candidates = require_array!(std::mem::take(&mut args[1]));
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
        result.insert("index".into(), super::util::int_of(best_idx));
        result.insert("score".into(), Value::Number(best_score));
        result.insert("margin".into(), Value::Number(margin));
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
            json!([{"k": 1.0}, {"k": 2.0}, {"k": null}])
        );
        assert_eq!(
            sort(json!(["array/sort", [{"k": 2}, {}, {"k": 1}, {"k": null}], "k", "desc"])).unwrap(),
            json!([{"k": 2.0}, {"k": 1.0}, {}, {"k": null}])
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
    fn sort_places_null_last_in_both_directions() {
        assert_eq!(
            sort(json!(["array/sort", [2, null, 1]])).unwrap(),
            json!([1.0, 2.0, null])
        );
        assert_eq!(
            sort(json!(["array/sort", [2, null, 1], null, "desc"])).unwrap(),
            json!([2.0, 1.0, null])
        );
        assert_eq!(
            sort(json!(["array/sort", [false, null, true]])).unwrap(),
            json!([false, true, null])
        );
        assert_eq!(
            sort(json!(["array/sort", [false, null, true], null, "desc"])).unwrap(),
            json!([true, false, null])
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
        assert_eq!(run(json!(["array/count", [1, 2, 3]])).unwrap(), json!(3));
        assert_eq!(
            run(json!([
                "array/count",
                [1, 2, 3],
                ["fn", "x", [">", "@x", 1]]
            ]))
            .unwrap(),
            json!(2)
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

    fn inner_reduce(acc_name: &str, item: &str, body_item: serde_json::Value) -> serde_json::Value {
        json!([
            "array/reduce",
            ["array/range", 0, 2],
            [],
            ["fn", [acc_name, item], ["array/concat", format!("@{acc_name}"), ["list", body_item]]]
        ])
    }

    #[test]
    fn a_nested_reduce_reusing_the_accumulator_name_reads_its_own() {
        let inner = inner_reduce("acc", "j", json!(["+", "@i", "@j"]));
        assert_eq!(
            run(json!([
                "array/reduce",
                ["array/range", 0, 2],
                [],
                ["fn", ["acc", "i"], ["array/concat", "@acc", inner]]
            ]))
            .unwrap(),
            json!([0.0, 1.0, 1.0, 2.0])
        );
    }

    #[test]
    fn the_outer_accumulator_is_read_again_after_the_inner_reduce_returns() {
        let inner = inner_reduce("acc", "j", json!("@j"));
        assert_eq!(
            run(json!([
                "array/reduce",
                ["array/range", 0, 2],
                [],
                ["fn", ["acc", "i"], ["array/concat", inner, "@acc"]]
            ]))
            .unwrap(),
            json!([0.0, 1.0, 0.0, 1.0])
        );
    }

    #[test]
    fn three_nested_reduces_sharing_a_name_each_read_their_own() {
        let innermost = inner_reduce("acc", "k", json!(["+", ["+", "@i", "@j"], "@k"]));
        let middle = json!([
            "array/reduce",
            ["array/range", 0, 2],
            [],
            ["fn", ["acc", "j"], ["array/concat", "@acc", innermost]]
        ]);
        assert_eq!(
            run(json!([
                "array/reduce",
                ["array/range", 0, 2],
                [],
                ["fn", ["acc", "i"], ["array/concat", "@acc", middle]]
            ]))
            .unwrap(),
            json!([0.0, 1.0, 1.0, 2.0, 1.0, 2.0, 2.0, 3.0])
        );
    }

    #[test]
    fn a_let_inside_a_reduce_shadows_the_accumulator_and_the_fn_restores_it() {
        assert_eq!(
            run(json!([
                "array/reduce",
                [1, 2],
                [],
                ["fn", ["acc", "i"], ["array/concat", "@acc", ["let", [["acc", ["list", "@i"]]], "@acc"]]]
            ]))
            .unwrap(),
            json!([1.0, 2.0])
        );
    }

    #[test]
    fn a_reduce_inside_a_let_of_the_same_name_reads_its_own_accumulator() {
        assert_eq!(
            run(json!([
                "let",
                [["acc", [100]]],
                [
                    "array/concat",
                    ["array/reduce", [1, 2], [], ["fn", ["acc", "i"], ["array/concat", "@acc", ["list", "@i"]]]],
                    "@acc"
                ]
            ]))
            .unwrap(),
            json!([1.0, 2.0, 100.0])
        );
    }

    #[test]
    fn nested_map_params_shadow_and_the_outer_param_returns_after() {
        assert_eq!(
            run(json!([
                "array/map",
                [1, 2],
                ["fn", "x", ["array/concat", ["array/map", [10, 20], ["fn", "x", ["+", "@x", 1]]], ["list", "@x"]]]
            ]))
            .unwrap(),
            json!([[11.0, 21.0, 1.0], [11.0, 21.0, 2.0]])
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
