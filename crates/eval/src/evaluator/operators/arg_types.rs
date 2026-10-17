//! Operator argument types (D-12): a present argument that contradicts the signature the installed operator
//! table declares for its call raises `EvalError::TypeMismatch`. One contract on every path (the JS twin is
//! `@almadar/evaluator` `operator-arg-types.ts`): `null` is an absent value and passes; a nominal std type
//! (`tensor`, `cell`, ...) is not checked; nothing is coerced.
//!
//! The check is the signature's own: each evaluated argument is bound against its declared type by
//! [`crate::signature`], the one resolver of the signature's type variables, so two arguments bound to one
//! variable must agree (`= : T, T`, `array/append : [T], T`). A value's type is its kind — a list's is the set of its
//! elements' types, each bound against the declared element type, a record's carries no fields — so a `key-of` argument is a static check only.
//!
//! The check rides the one evaluation entry: [`super::eval_expr`] records the argument slice of the operator
//! call it dispatches, and the evaluation of an element of that slice is bound at its position. No operator
//! implementation carries a check of its own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{discriminant, Discriminant};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use super::get_operator;
use crate::evaluator::operators::kind::OperatorKind;
use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::ir::types::OirType;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::language::registry::{OperatorTypeRef, OperatorTypeRefKind};
use orbital_core_base::signature::{absent_slot, Actual, CallBindings, Known, Mismatch, Mode, Signature};
use orbital_core_base::value::Value;

/// The signatures of the operators the evaluator checks, by operator.
static SIGNATURES: OnceLock<HashMap<Discriminant<OperatorKind>, Signature<'static>>> =
    OnceLock::new();
static LOAD_FAILED: AtomicBool = AtomicBool::new(false);

fn build_signatures() -> HashMap<Discriminant<OperatorKind>, Signature<'static>> {
    let mut signatures = HashMap::new();
    for (name, meta) in orbital_core_base::language::registry::operators() {
        if meta.has_side_effects {
            continue;
        }
        let Some(kind) = get_operator(name) else {
            continue;
        };
        if matches!(
            kind,
            OperatorKind::BehaviorCompositionStubOp(_) | OperatorKind::AgentStubOp(_)
        ) {
            continue;
        }
        signatures.insert(discriminant(&kind), Signature::of(meta));
    }
    signatures
}

/// The signature the installed operator table declares for `kind`, when a call to it checks anything. Without an
/// installed table nothing is checked; a failed load is not retried, but a table installed later is still honored.
pub fn signature_for(kind: &OperatorKind) -> Option<Signature<'static>> {
    declared_signature(kind).filter(Signature::checks_anything)
}

fn declared_signature(kind: &OperatorKind) -> Option<Signature<'static>> {
    let signatures = match SIGNATURES.get() {
        Some(signatures) => signatures,
        None => {
            if orbital_core_base::language_data::operator_data_if_loaded().is_none() {
                if LOAD_FAILED.load(Ordering::Relaxed) {
                    return None;
                }
                if crate::language_loader::operator_data().is_err() {
                    LOAD_FAILED.store(true, Ordering::Relaxed);
                    return None;
                }
            }
            SIGNATURES.get_or_init(build_signatures)
        }
    };
    signatures.get(&discriminant(kind)).copied()
}

/// The signature the installed operator table declares for the evaluated operator `name` (an alias resolves to
/// its operator); the one source the validator reads, so a static check and the runtime check cannot drift.
pub fn operator_signature(name: &str) -> Option<Signature<'static>> {
    declared_signature(&get_operator(name)?)
}

/// What is known of an evaluated scalar argument: its kind. `None` for null and for a lambda.
fn scalar_known(value: &Value) -> Option<Known> {
    match value {
        Value::Null | Value::Lambda { .. } | Value::Array(_) => None,
        Value::Number(_) => Some(Known::Type(OirType::Number)),
        Value::String(_) => Some(Known::Type(OirType::String)),
        Value::Bool(_) => Some(Known::Type(OirType::Boolean)),
        Value::Object(_) => Some(Known::Type(OirType::Struct { fields: Vec::new() })),
    }
}

fn push_unique(into: &mut Vec<Known>, known: Known) {
    if !into.contains(&known) {
        into.push(known);
    }
}

/// What is known of an evaluated argument, read against the type `formal` declares for it: one entry for a
/// scalar, and for a list one entry per distinct element type, so every element is bound (a list of unlike
/// elements raises on the first that disagrees). A tuple position is its own type, a null one absent. An empty
/// list, or one of nulls, is [`Known::EmptyList`]. A null or a lambda has no entry.
fn views(value: &Value, formal: Option<&OperatorTypeRef>) -> Vec<Known> {
    let Value::Array(items) = value else {
        return scalar_known(value).into_iter().collect();
    };
    if let Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Tuple { items: slots })) = formal {
        if items.is_empty() {
            return vec![Known::EmptyList];
        }
        let positions = items
            .iter()
            .enumerate()
            .map(|(i, item)| match views(item, slots.get(i)).into_iter().next() {
                Some(Known::Type(ty)) => ty,
                _ => absent_slot(),
            })
            .collect();
        return vec![Known::Type(OirType::Tuple { items: positions })];
    }
    let element_formal = match formal {
        Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Array { of })) => Some(&**of),
        _ => None,
    };
    let mut out = Vec::new();
    let _ = list_views(items, element_formal, &mut |known| {
        push_unique(&mut out, known);
        Ok::<(), ()>(())
    });
    out
}

/// A list's views without collecting them: every element is read (by reference, no per-element list), and each
/// distinct element type is handed to `each` once as `[T]`; an empty list, or one of nulls, is [`Known::EmptyList`].
fn list_views<E>(
    items: &[Value],
    element_formal: Option<&OperatorTypeRef>,
    each: &mut dyn FnMut(Known) -> Result<(), E>,
) -> Result<(), E> {
    let mut first: Option<OirType> = None;
    let mut others: Vec<OirType> = Vec::new();
    let mut note = |known: Known| {
        if let Known::Type(element) = known {
            match &first {
                None => first = Some(element),
                Some(seen) if *seen == element => {}
                Some(_) => {
                    if !others.contains(&element) {
                        others.push(element);
                    }
                }
            }
        }
    };
    for item in items {
        match item {
            Value::Array(_) => views(item, element_formal).into_iter().for_each(&mut note),
            _ => {
                if let Some(known) = scalar_known(item) {
                    note(known);
                }
            }
        }
    }
    match first {
        None => each(Known::EmptyList),
        Some(element) => {
            each(Known::Type(OirType::array(element)))?;
            for other in others {
                each(Known::Type(OirType::array(other)))?;
            }
            Ok(())
        }
    }
}

fn actual_of_known(known: Option<Known>) -> Actual<'static> {
    Actual {
        known,
        text: None,
        texts: None,
        lambda: None,
    }
}

/// An evaluated argument as the signature resolver sees it, read without a declared type: the first entry of its
/// [`views`].
pub fn actual_of(value: &Value) -> Actual<'static> {
    actual_of_known(views(value, None).into_iter().next())
}

fn to_eval_error(mismatch: Mismatch) -> OrbitalError {
    match mismatch {
        Mismatch::Type { expected, actual } | Mismatch::Conflict { expected, actual } => {
            OrbitalError::Eval(EvalError::TypeMismatch { expected, actual })
        }
        Mismatch::NoField { path, record } => OrbitalError::Eval(EvalError::TypeMismatch {
            expected: format!("a field of {record}"),
            actual: path,
        }),
        // A run-time value always has a kind, so the binder raises this only over static argument types.
        Mismatch::Undetermined { reason } => OrbitalError::Eval(EvalError::TypeMismatch {
            expected: "a value of a determined type".to_string(),
            actual: reason,
        }),
        // Raised by the static checker only: at run time a missing value is a value, and evaluation is unchanged.
        Mismatch::MaybeAbsent { operator, operand } => {
            OrbitalError::Eval(EvalError::TypeMismatch {
                expected: format!("a present operand for `{operator}`"),
                actual: operand,
            })
        }
    }
}

struct Frame {
    start: usize,
    end: usize,
    signature: Signature<'static>,
    bindings: CallBindings,
}

thread_local! {
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// Marks `args` as the argument slice of the operator call being evaluated until dropped.
pub struct CallFrame;

impl CallFrame {
    pub fn enter(signature: Signature<'static>, args: &[SExpression]) -> Self {
        let start = args.as_ptr() as usize;
        let end = start + std::mem::size_of_val(args);
        FRAMES.with(|f| {
            f.borrow_mut().push(Frame {
                start,
                end,
                signature,
                bindings: CallBindings::new(),
            })
        });
        CallFrame
    }
}

impl Drop for CallFrame {
    fn drop(&mut self) {
        FRAMES.with(|f| {
            f.borrow_mut().pop();
        });
    }
}

/// The argument position `expr` holds in the operator call being evaluated, when it is one of that call's
/// arguments.
pub fn argument_slot(expr: &SExpression) -> Option<usize> {
    FRAMES.with(|f| {
        let frames = f.borrow();
        let top = frames.last()?;
        let at = expr as *const SExpression as usize;
        (at >= top.start && at < top.end)
            .then(|| (at - top.start) / std::mem::size_of::<SExpression>())
    })
}

/// Binds the evaluated argument at `index` of the call being evaluated against its declared type.
pub fn check_argument(index: usize, value: &Value) -> Result<(), OrbitalError> {
    if value.is_null() {
        return Ok(());
    }
    FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        let Some(top) = frames.last_mut() else {
            return Ok(());
        };
        let signature = top.signature;
        if !matches!(value, Value::Array(_)) {
            return signature
                .bind(&mut top.bindings, index, &actual_of_known(scalar_known(value)), Mode::Runtime)
                .map_err(to_eval_error);
        }
        let formal = signature.formal(index);
        if let (Value::Array(items), false) =
            (value, matches!(formal, Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Tuple { .. }))))
        {
            let element_formal = match formal {
                Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Array { of })) => Some(&**of),
                _ => None,
            };
            return list_views(items, element_formal, &mut |known| {
                signature
                    .bind(&mut top.bindings, index, &actual_of_known(Some(known)), Mode::Runtime)
                    .map_err(to_eval_error)
            });
        }
        let mut known: Vec<Option<Known>> = views(value, formal).into_iter().map(Some).collect();
        if known.is_empty() {
            known.push(None);
        }
        for k in known {
            signature
                .bind(&mut top.bindings, index, &actual_of_known(k), Mode::Runtime)
                .map_err(to_eval_error)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::language::registry::EmbeddedOperator;
    use orbital_core_base::value::ValueMap;
    use serde_json::json;

    fn operator(params: serde_json::Value, max_arity: Option<u32>) -> EmbeddedOperator {
        serde_json::from_value(json!({
            "category": "std-array", "minArity": 0, "maxArity": max_arity,
            "params": params, "returnType": "boolean",
        }))
        .unwrap()
    }

    fn check(op: &EmbeddedOperator, args: &[Value]) -> Result<(), OrbitalError> {
        let sig = Signature::of(op);
        let mut b = CallBindings::new();
        for (i, v) in args.iter().enumerate() {
            if v.is_null() {
                continue;
            }
            sig.bind(&mut b, i, &actual_of(v), Mode::Runtime)
                .map_err(to_eval_error)?;
        }
        Ok(())
    }

    fn num(n: f64) -> Value {
        Value::Number(n)
    }

    fn s(t: &str) -> Value {
        Value::String(t.to_string())
    }

    #[test]
    fn a_primitive_declares_its_kind_and_a_non_null_mismatch_raises() {
        let op = operator(json!([{ "name": "s", "type": "string" }]), Some(1));
        assert!(check(&op, &[s("a")]).is_ok());
        let err = check(&op, &[num(5.0)]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Evaluation error: Type mismatch: expected string, got number"
        );
    }

    #[test]
    fn null_is_absent_and_passes_every_declared_kind() {
        let op = operator(json!([{ "name": "n", "type": "number" }]), Some(1));
        assert!(check(&op, &[Value::Null]).is_ok());
    }

    #[test]
    fn a_union_accepts_any_member_and_skips_the_null_member() {
        let op = operator(
            json!([{ "name": "v", "type": { "kind": "union", "of": ["number", { "kind": "literal", "value": null }] } }]),
            Some(1),
        );
        assert!(check(&op, &[num(1.0)]).is_ok());
        assert!(check(&op, &[s("x")]).is_err());
        let list_or_string = operator(
            json!([{ "name": "v", "type": { "kind": "union", "of": ["string", { "kind": "array", "of": { "kind": "var", "name": "T" } }] } }]),
            Some(1),
        );
        assert!(check(&list_or_string, &[Value::Array(vec![])]).is_ok());
        assert_eq!(
            check(&list_or_string, &[num(1.0)]).unwrap_err().to_string(),
            "Evaluation error: Type mismatch: expected string | array, got number"
        );
    }

    #[test]
    fn a_nominal_std_type_is_not_checked() {
        let op = operator(json!([{ "name": "x", "type": "tensor" }]), Some(1));
        assert!(check(&op, &[s("anything")]).is_ok());
        assert!(check(&op, &[num(3.0)]).is_ok());
    }

    #[test]
    fn a_rest_parameter_checks_its_type_past_the_declared_positions() {
        let op = operator(json!([{ "name": "...nums", "type": "number" }]), None);
        assert!(check(&op, &[num(1.0), num(2.0), num(3.0), num(4.0)]).is_ok());
        assert!(check(&op, &[num(1.0), num(2.0), num(3.0), s("1")]).is_err());
    }

    #[test]
    fn positions_past_a_bounded_signature_are_unchecked() {
        let op = operator(json!([{ "name": "n", "type": "number" }]), Some(1));
        assert!(check(&op, &[num(1.0), s("anything")]).is_ok());
    }

    #[test]
    fn two_arguments_bound_to_one_variable_must_agree_in_kind() {
        let op = operator(
            json!([{ "name": "a", "type": { "kind": "var", "name": "T" } }, { "name": "b", "type": { "kind": "var", "name": "T" } }]),
            Some(2),
        );
        assert!(check(&op, &[num(1.0), num(2.0)]).is_ok());
        assert_eq!(
            check(&op, &[num(1.0), s("1")]).unwrap_err().to_string(),
            "Evaluation error: Type mismatch: expected number, got string"
        );
        assert!(
            check(&op, &[Value::Null, s("1")]).is_ok(),
            "an absent operand binds nothing"
        );
    }

    #[test]
    fn a_list_and_an_item_agree_through_the_list_element() {
        let op = operator(
            json!([
                { "name": "arr", "type": { "kind": "array", "of": { "kind": "var", "name": "T" } } },
                { "name": "item", "type": { "kind": "var", "name": "T" } },
            ]),
            Some(2),
        );
        assert!(check(&op, &[Value::Array(vec![num(1.0)]), num(2.0)]).is_ok());
        assert_eq!(
            check(&op, &[Value::Array(vec![num(1.0)]), s("x")])
                .unwrap_err()
                .to_string(),
            "Evaluation error: Type mismatch: expected number, got string"
        );
        assert!(
            check(&op, &[Value::Array(vec![]), s("x")]).is_ok(),
            "an empty list binds nothing"
        );
        assert!(check(&op, &[Value::Array(vec![Value::Null, num(1.0)]), num(2.0)]).is_ok());
        assert!(
            check(&op, &[num(5.0), num(2.0)]).is_err(),
            "a non-list for `[T]`"
        );
    }

    #[test]
    fn a_record_variable_takes_an_object_and_not_an_array() {
        let op = operator(
            json!([{ "name": "obj", "type": { "kind": "var", "name": "R", "bound": { "kind": "record" } } }]),
            Some(1),
        );
        assert!(check(&op, &[Value::Object(ValueMap::new())]).is_ok());
        assert_eq!(
            check(&op, &[Value::Array(vec![])]).unwrap_err().to_string(),
            "Evaluation error: Type mismatch: expected object, got array"
        );
    }
}
