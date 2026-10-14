//! Operator argument types (D-12): a present argument that contradicts the signature the installed operator
//! table declares for its call raises `EvalError::TypeMismatch`. One contract on every path (the JS twin is
//! `@almadar/evaluator` `operator-arg-types.ts`): `null` is an absent value and passes; a nominal std type
//! (`tensor`, `cell`, ...) is not checked; nothing is coerced.
//!
//! The check is the signature's own: each evaluated argument is bound against its declared type by
//! [`crate::signature`], the one resolver of the signature's type variables, so two arguments bound to one
//! variable must agree (`= : T, T`, `array/append : [T], T`). A value's type is its kind — a list's is its first
//! non-null element's, a record's carries no fields — so a `key-of` argument is a static check only.
//!
//! The check rides the one evaluation entry: [`super::eval_expr`] records the argument slice of the operator
//! call it dispatches, and the evaluation of an element of that slice is bound at its position. No operator
//! implementation carries a check of its own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{discriminant, Discriminant};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use super::{get_operator, OperatorKind};
use crate::error::{EvalError, OrbitalError};
use crate::evaluator::value::Value;
use crate::ir::OirType;
use crate::schema::SExpression;
use crate::signature::{Actual, CallBindings, Known, Mismatch, Mode, Signature};

/// The signatures of the operators the evaluator checks, by operator.
static SIGNATURES: OnceLock<HashMap<Discriminant<OperatorKind>, Signature<'static>>> = OnceLock::new();
static LOAD_FAILED: AtomicBool = AtomicBool::new(false);

fn build_signatures() -> HashMap<Discriminant<OperatorKind>, Signature<'static>> {
    let mut signatures = HashMap::new();
    for (name, meta) in crate::language::registry::operators() {
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
pub(crate) fn signature_for(kind: &OperatorKind) -> Option<Signature<'static>> {
    declared_signature(kind).filter(Signature::checks_anything)
}

fn declared_signature(kind: &OperatorKind) -> Option<Signature<'static>> {
    let signatures = match SIGNATURES.get() {
        Some(signatures) => signatures,
        None => {
            if crate::language_data::operator_data_if_loaded().is_none() {
                if LOAD_FAILED.load(Ordering::Relaxed) {
                    return None;
                }
                if crate::language_data::operator_data().is_err() {
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

/// What is known of an evaluated argument: its kind, and for a list its first element's.
fn known_of(value: &Value) -> Option<Known> {
    match value {
        Value::Null | Value::Lambda { .. } => None,
        Value::Number(_) => Some(Known::Type(OirType::Number)),
        Value::String(_) => Some(Known::Type(OirType::String)),
        Value::Bool(_) => Some(Known::Type(OirType::Boolean)),
        Value::Object(_) => Some(Known::Type(OirType::Struct { fields: Vec::new() })),
        Value::Array(items) => {
            match items.iter().find_map(known_of) {
                Some(Known::Type(element)) => Some(Known::Type(OirType::array(element))),
                Some(Known::EmptyList | Known::Absent | Known::Void) | None => Some(Known::EmptyList),
            }
        }
    }
}

/// An evaluated argument as the signature resolver sees it.
pub fn actual_of(value: &Value) -> Actual<'static> {
    Actual {
        known: known_of(value),
        text: None,
        texts: None,
        lambda: None,
    }
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
pub(crate) struct CallFrame;

impl CallFrame {
    pub(crate) fn enter(signature: Signature<'static>, args: &[SExpression]) -> Self {
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
pub(crate) fn argument_slot(expr: &SExpression) -> Option<usize> {
    FRAMES.with(|f| {
        let frames = f.borrow();
        let top = frames.last()?;
        let at = expr as *const SExpression as usize;
        (at >= top.start && at < top.end).then(|| (at - top.start) / std::mem::size_of::<SExpression>())
    })
}

/// Binds the evaluated argument at `index` of the call being evaluated against its declared type.
pub(crate) fn check_argument(index: usize, value: &Value) -> Result<(), OrbitalError> {
    if value.is_null() {
        return Ok(());
    }
    FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        let Some(top) = frames.last_mut() else {
            return Ok(());
        };
        let signature = top.signature;
        signature
            .bind(&mut top.bindings, index, &actual_of(value), Mode::Runtime)
            .map_err(to_eval_error)
    })
}

#[cfg(test)]
mod tests {
    use crate::evaluator::ValueMap;
    use super::*;
    use crate::language::registry::EmbeddedOperator;
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
            sig.bind(&mut b, i, &actual_of(v), Mode::Runtime).map_err(to_eval_error)?;
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
        assert!(check(&op, &[Value::Null, s("1")]).is_ok(), "an absent operand binds nothing");
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
            check(&op, &[Value::Array(vec![num(1.0)]), s("x")]).unwrap_err().to_string(),
            "Evaluation error: Type mismatch: expected number, got string"
        );
        assert!(check(&op, &[Value::Array(vec![]), s("x")]).is_ok(), "an empty list binds nothing");
        assert!(check(&op, &[Value::Array(vec![Value::Null, num(1.0)]), num(2.0)]).is_ok());
        assert!(check(&op, &[num(5.0), num(2.0)]).is_err(), "a non-list for `[T]`");
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
