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
use orbital_core_base::ir::types::{DeclaredShape, OirType};
use orbital_core_base::language::registry::{OperatorTypeRef, OperatorTypeRefKind};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::signature::{
    absent_slot, Actual, CallBindings, Known, Mismatch, Mode, Signature,
};
use orbital_core_base::value::Value;
use std::collections::BTreeMap;

/// Hashes an operator's discriminant to itself: the table is read on every operator call, and a keyed hash of one
/// small integer is most of what a lookup cost.
#[derive(Default)]
struct DiscriminantHasher(u64);

impl std::hash::Hasher for DiscriminantHasher {
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self.0.wrapping_mul(0x100_0000_01b3).wrapping_add(u64::from(*byte) + 1);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type SignatureTable =
    HashMap<Discriminant<OperatorKind>, Signature<'static>, std::hash::BuildHasherDefault<DiscriminantHasher>>;

/// The signatures of the operators the evaluator checks, by operator.
static SIGNATURES: OnceLock<SignatureTable> = OnceLock::new();
static LOAD_FAILED: AtomicBool = AtomicBool::new(false);

fn build_signatures() -> SignatureTable {
    let mut signatures = SignatureTable::default();
    for (name, meta) in orbital_core_base::language::registry::operators() {
        if meta.has_side_effects {
            continue;
        }
        let Some(kind) = get_operator(name) else {
            continue;
        };
        if matches!(
            kind,
            OperatorKind::BehaviorCompositionStubOp(_) | OperatorKind::AgentStubOp(_) | OperatorKind::TypedOp
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
        Value::Object(_) => Some(Known::Type(OirType::Struct { fields: Vec::new(), additional: None })),
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
            .map(
                |(i, item)| match views(item, slots.get(i)).into_iter().next() {
                    Some(Known::Type(ty)) => ty,
                    _ => absent_slot(),
                },
            )
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
    let mut note = |element: OirType| match &first {
        None => first = Some(element),
        Some(seen) if *seen == element => {}
        Some(_) => {
            if !others.contains(&element) {
                others.push(element);
            }
        }
    };
    let tuple_formal = matches!(
        element_formal,
        Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Tuple { .. }))
    );
    let nested_formal = matches!(
        element_formal,
        Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Array { .. }))
    );
    let mut inner: Vec<Vec<OirType>> = Vec::new();
    for item in items {
        match item {
            Value::Array(_) if tuple_formal || nested_formal => {
                for known in views(item, element_formal) {
                    if let Known::Type(element) = known {
                        note(element);
                    }
                }
            }
            Value::Array(_) => {
                if let Some(positions) = inner_list_positions(item) {
                    if !inner.contains(&positions) {
                        inner.push(positions);
                    }
                }
            }
            _ => {
                if let Some(Known::Type(element)) = scalar_known(item) {
                    note(element);
                }
            }
        }
    }
    if inner.is_empty() {
        let Some(first) = first else {
            return each(Known::EmptyList);
        };
        each(Known::Type(OirType::array(first)))?;
        for other in others {
            each(Known::Type(OirType::array(other)))?;
        }
        return Ok(());
    }
    let mut types: Vec<OirType> = first.into_iter().collect();
    types.extend(others);
    for element in settle_inner_lists(inner, types) {
        each(Known::Type(OirType::array(element)))?;
    }
    Ok(())
}

/// The positions of a list that is an element of another list, read without a declared type: one type per entry,
/// an absent slot for a null. `None` for a list with no typed entry.
fn inner_list_positions(item: &Value) -> Option<Vec<OirType>> {
    let Value::Array(entries) = item else {
        return None;
    };
    let positions: Vec<OirType> = entries
        .iter()
        .map(|entry| match entry {
            Value::Array(_) => inner_list_positions(entry).map(settle_positions),
            other => match scalar_known(other) {
                Some(Known::Type(ty)) => Some(ty),
                _ => None,
            },
        })
        .map(|ty| ty.unwrap_or_else(absent_slot))
        .collect();
    positions
        .iter()
        .any(|ty| *ty != absent_slot())
        .then_some(positions)
}

/// Whether the entries of a list differ in type: such a list is a tuple.
fn is_tuple(positions: &[OirType]) -> bool {
    let mut present = positions.iter().filter(|ty| **ty != absent_slot());
    present
        .next()
        .is_some_and(|first| present.any(|ty| ty != first))
}

/// A list's own type: a list of its entries' one type, or a tuple of their types.
fn settle_positions(positions: Vec<OirType>) -> OirType {
    if is_tuple(&positions) {
        return OirType::Tuple { items: positions };
    }
    match positions.into_iter().find(|ty| *ty != absent_slot()) {
        Some(element) => OirType::array(element),
        None => OirType::Tuple { items: Vec::new() },
    }
}

/// The element types of a list of lists. When any of them is a tuple they are one element type, whatever their
/// lengths: the positions they share, an absent slot where they differ or one is shorter, so a list of unlike
/// tuples (the op tuples of a `persist batch`) is a list. Without a tuple each list is a list of its entries' type,
/// and two of unlike entries stay two types.
fn settle_inner_lists(types: Vec<Vec<OirType>>, mut out: Vec<OirType>) -> Vec<OirType> {
    if types.iter().any(|positions| is_tuple(positions)) {
        let width = types.iter().map(Vec::len).max().unwrap_or(0);
        let merged = (0..width)
            .map(|slot| {
                let mut at = types.iter().map(|positions| positions.get(slot));
                match at.next().flatten() {
                    Some(first) if at.all(|other| other == Some(first)) => first.clone(),
                    _ => absent_slot(),
                }
            })
            .collect();
        out.push(OirType::Tuple { items: merged });
        return out;
    }
    for positions in types {
        let element = settle_positions(positions);
        if !out.contains(&element) {
            out.push(element);
        }
    }
    out
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
        Mismatch::NullOperand { operator, operand } => OrbitalError::Eval(EvalError::TypeMismatch {
            expected: format!("a present operand for `{operator}`"),
            actual: operand,
        }),
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

/// Whether `formal` is a bare type variable, or an unevaluated expression whose value is one.
fn is_bare_variable(formal: Option<&OperatorTypeRef>) -> bool {
    match formal {
        Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Var { .. })) => true,
        Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Sexpr { of })) => {
            is_bare_variable(of.as_deref())
        }
        _ => false,
    }
}

thread_local! {
    static DECLARED: RefCell<HashMap<String, std::rc::Rc<DeclaredShape>>> = RefCell::new(HashMap::new());
}

/// The declared shape a `typed` form or a `json/parse` shape records as JSON text: a bare `OirType`, or the type with
/// the definitions of the program types it names (`DeclaredShape`).
pub fn declared_shape(arg: &SExpression) -> Result<std::rc::Rc<DeclaredShape>, OrbitalError> {
    let SExpression::String(text) = arg else {
        return Err(OrbitalError::Eval(EvalError::InvalidArgument("typed: the declared type must be JSON text".into())));
    };
    if let Some(known) = DECLARED.with(|d| d.borrow().get(text).cloned()) {
        return Ok(known);
    }
    let shape = DeclaredShape::read(text).map_err(|error| {
        OrbitalError::Eval(EvalError::InvalidArgument(format!("typed: the declared type is not a type: {error}")))
    })?;
    let shape = std::rc::Rc::new(shape);
    DECLARED.with(|d| d.borrow_mut().insert(text.clone(), std::rc::Rc::clone(&shape)));
    Ok(shape)
}

/// The type a `typed` form records: its second argument's declared type.
pub fn declared_type(arg: &SExpression) -> Result<OirType, OrbitalError> {
    Ok(declared_shape(arg)?.ty.clone())
}

/// The declared type of `expr` when it is a `typed` form.
pub fn typed_declaration(expr: &SExpression) -> Result<Option<OirType>, OrbitalError> {
    match expr {
        SExpression::Array(items) if items.len() == 3 && matches!(&items[0], SExpression::String(head) if head == "typed") => {
            declared_type(&items[2]).map(Some)
        }
        _ => Ok(None),
    }
}

/// The type a declared type is written as in a message: `[number | string]`, `[Adv | string]`, `[SExpr]`.
pub fn declared_text(ty: &OirType) -> String {
    match ty {
        OirType::Array { inner } => format!("[{}]", declared_text(inner)),
        OirType::Optional { inner } => format!("{}?", declared_text(inner)),
        OirType::Union { variants } => variants
            .iter()
            .map(|v| match &v.field_type {
                OirType::Struct { .. } | OirType::TypeRef { .. } | OirType::Row { .. } => v.name.clone(),
                other => declared_text(other),
            })
            .collect::<Vec<_>>()
            .join(" | "),
        OirType::Map { key, value } => format!("Map {} {}", declared_text(key), declared_text(value)),
        OirType::Tuple { items } => format!("[{}]", items.iter().map(declared_text).collect::<Vec<_>>().join(", ")),
        OirType::SExpr => "SExpr".to_string(),
        OirType::FieldRef { .. } => "string".to_string(),
        OirType::Struct { .. } | OirType::File => "object".to_string(),
        OirType::Row { entity } | OirType::Reference { entity } => entity.clone(),
        OirType::TypeRef { name } | OirType::Enum { name } | OirType::Var { name } => name.clone(),
        OirType::Integer | OirType::Money => "number".to_string(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
            .unwrap_or_else(|| "a value".to_string()),
    }
}

/// The type a by-name reference resolves to: a `TypeRef` or `Reference` whose name the recorded shape's `named` table
/// defines is that definition, through a chain of aliases; `None` when `ty` names no defined type (or a cycle of bare
/// aliases).
fn declared_alias(ty: &OirType, named: &BTreeMap<String, OirType>) -> Option<OirType> {
    let mut seen: Vec<String> = Vec::new();
    let mut resolved: Option<OirType> = None;
    loop {
        let name = match resolved.as_ref().unwrap_or(ty) {
            OirType::TypeRef { name } | OirType::Reference { entity: name } => name.clone(),
            _ => return resolved,
        };
        if seen.contains(&name) {
            return None;
        }
        match named.get(&name) {
            Some(next) => resolved = Some(next.clone()),
            None => return resolved,
        }
        seen.push(name);
    }
}

/// The first mismatch under the explicit legacy or nullable-field contract. A name `named` defines is checked as its definition,
/// so each item of a list of a recursive union matches a member of the union on its own; a closed string set
/// (`"a" | "b"`) admits only its members.
fn first_misfit(value: &Value, ty: &OirType, named: &BTreeMap<String, OirType>, strict: bool) -> Option<(String, &'static str)> {
    if value.is_null() && (!strict || matches!(ty, OirType::Optional { .. })) {
        return None;
    }
    if let Some(declared) = declared_alias(ty, named) {
        return first_misfit(value, &declared, named, strict);
    }
    let misfit = || Some((String::new(), value.type_name()));
    if strict && value.is_null() && !matches!(ty, OirType::Union { .. }) {
        return misfit();
    }
    let at = |prefix: String| move |(path, actual): (String, &'static str)| (format!("{prefix}{path}"), actual);
    let items = |items: &[Value], inner: &OirType| {
        items.iter().enumerate().find_map(|(i, item)| first_misfit(item, inner, named, strict).map(at(format!("[{i}]"))))
    };
    match ty {
        OirType::SExpr | OirType::Json | OirType::Node | OirType::Trait | OirType::Orbital | OirType::Behavior => None,
        // An unbound variable admits any value; a bound one was substituted before run time.
        OirType::Var { .. } => None,
        OirType::Optional { inner } => first_misfit(value, inner, named, strict),
        OirType::Union { variants } => {
            if variants.iter().any(|v| first_misfit(value, &v.field_type, named, strict).is_none()) {
                None
            } else {
                misfit()
            }
        }
        OirType::Array { inner } => match value {
            Value::Array(list) => items(list, inner),
            _ => misfit(),
        },
        OirType::Tuple { items: slots } => match value {
            Value::Array(list) if list.len() == slots.len() => list
                .iter()
                .zip(slots)
                .enumerate()
                .find_map(|(i, (item, slot))| first_misfit(item, slot, named, strict).map(at(format!("[{i}]")))),
            _ => misfit(),
        },
        OirType::Map { value: inner, .. } => match value {
            Value::Object(map) => {
                map.iter().find_map(|(k, item)| first_misfit(item, inner, named, strict).map(at(format!(".{k}"))))
            }
            _ => misfit(),
        },
        OirType::Struct { fields, additional } => match value {
            Value::Object(map) => fields.iter().find_map(|field| match map.get(&field.name) {
                None => field.required.then(|| (format!(".{}", field.name), "absent")),
                Some(item) => first_misfit(item, &field.field_type, named, strict).map(at(format!(".{}", field.name))),
            }).or_else(|| additional.as_deref().and_then(|additional| {
                map.iter().filter(|(key, _)| !fields.iter().any(|field| field.name.as_str() == key.as_str()))
                    .find_map(|(key, item)| first_misfit(item, additional, named, strict).map(at(format!(".{key}"))))
            })),
            _ => misfit(),
        },
        OirType::File | OirType::Row { .. } | OirType::TypeRef { .. } => {
            (!matches!(value, Value::Object(_))).then(|| (String::new(), value.type_name()))
        }
        OirType::Reference { .. } => {
            (!matches!(value, Value::Object(_) | Value::String(_))).then(|| (String::new(), value.type_name()))
        }
        OirType::Number | OirType::Integer | OirType::Money => {
            (!matches!(value, Value::Number(_))).then(|| (String::new(), value.type_name()))
        }
        OirType::Boolean => (!matches!(value, Value::Bool(_))).then(|| (String::new(), value.type_name())),
        OirType::Date | OirType::DateTime => {
            (!matches!(value, Value::String(_) | Value::Number(_))).then(|| (String::new(), value.type_name()))
        }
        OirType::Bytes => match value {
            Value::String(text) if orbital_core_base::codec::base64_decode(text).is_ok() => None,
            _ => misfit(),
        },
        OirType::Enum { name } => match (value, orbital_core_base::type_compat::decode_literal_union(name)) {
            (Value::String(text), Ok(Some(members))) => (!members.contains(text)).then(|| (String::new(), "string")),
            (Value::String(_), Ok(None)) => None,
            (Value::String(_), Err(_)) if !strict => None,
            _ => misfit(),
        },
        _ => (!matches!(value, Value::String(_))).then(|| (String::new(), value.type_name())),
    }
}

/// Where `value` departs from the declared `ty` (`""` for the value itself, `.rows[2].id` inside it) and what kind of
/// value is there, when it does; a by-name reference reads its definition from `named`.
pub fn misfit_of_declared(value: &Value, ty: &OirType, named: &BTreeMap<String, OirType>) -> Option<(String, &'static str)> {
    first_misfit(value, ty, named, false)
}

/// `value` is of the declared `ty`, kind by kind, and a record's declared fields field by field.
pub fn value_of_declared(value: &Value, ty: &OirType, named: &BTreeMap<String, OirType>) -> Result<(), OrbitalError> {
    match first_misfit(value, ty, named, false) {
        None => Ok(()),
        Some((_, actual)) => Err(OrbitalError::Eval(EvalError::TypeMismatch {
            expected: declared_text(ty),
            actual: actual.to_string(),
        })),
    }
}

pub fn misfit_of_shape(value: &Value, declared: &DeclaredShape) -> Option<(String, &'static str)> {
    if declared.version.is_some_and(|version| version != 1) {
        return Some((String::new(), "unsupported declared-shape version"));
    }
    first_misfit(value, &declared.ty, &declared.named, declared.version == Some(1))
}

pub fn value_of_shape(value: &Value, declared: &DeclaredShape) -> Result<(), OrbitalError> {
    match misfit_of_shape(value, declared) {
        None => Ok(()),
        Some((_, actual)) => Err(OrbitalError::Eval(EvalError::TypeMismatch {
            expected: declared_text(&declared.ty),
            actual: actual.to_string(),
        })),
    }
}

/// Binds the argument at `index` of the call being evaluated to the type the program declared for it.
pub fn check_declared_argument(index: usize, declared: &OirType) -> Result<(), OrbitalError> {
    FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        let Some(top) = frames.last_mut() else {
            return Ok(());
        };
        let signature = top.signature;
        signature
            .bind(&mut top.bindings, index, &actual_of_known(Some(Known::Type(declared.clone()))), Mode::Runtime)
            .map_err(to_eval_error)
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
        // A primitive of the declared kind binds no variable and cannot disagree: the common call, answered without the binder.
        if let Some(OperatorTypeRef::Primitive(kind)) = signature.formal(index) {
            match (kind.as_str(), value) {
                ("number", Value::Number(_)) | ("string", Value::String(_)) | ("boolean", Value::Bool(_)) => return Ok(()),
                _ => {}
            }
        }
        if !matches!(value, Value::Array(_)) {
            return signature
                .bind(
                    &mut top.bindings,
                    index,
                    &actual_of_known(scalar_known(value)),
                    Mode::Runtime,
                )
                .map_err(to_eval_error);
        }
        let formal = signature.formal(index);
        // A list handed to a bare variable is one value, typed by the type of each of its entries.
        if is_bare_variable(formal) {
            // An empty list (or one of nulls) has no entry to type, so it binds nothing: `(!= xs [])` holds for any `xs`.
            if matches!(value, Value::Array(items) if items.iter().all(Value::is_null)) {
                return Ok(());
            }
            if let Some(positions) = inner_list_positions(value) {
                return signature
                    .bind(
                        &mut top.bindings,
                        index,
                        &actual_of_known(Some(Known::Type(OirType::Tuple { items: positions }))),
                        Mode::Runtime,
                    )
                    .map_err(to_eval_error);
            }
        }
        if let (Value::Array(items), false) = (
            value,
            matches!(
                formal,
                Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Tuple { .. }))
            ),
        ) {
            let element_formal = match formal {
                Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Array { of })) => Some(&**of),
                _ => None,
            };
            return list_views(items, element_formal, &mut |known| {
                signature
                    .bind(
                        &mut top.bindings,
                        index,
                        &actual_of_known(Some(known)),
                        Mode::Runtime,
                    )
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

    #[test]
    fn strict_record_contract_checks_null_presence_and_catchall_on_both_value_entries() {
        use crate::evaluator::context::BindingContext;
        use crate::evaluator::operators::{Operator, cast::TypedOp, json::JsonParseOp};
        let context = BindingContext::new();
        let shape = serde_json::json!({"version":1,"type":{"kind":"struct","fields":[
            {"name":"id","fieldType":{"kind":"string"},"required":false},
            {"name":"token","fieldType":{"kind":"optional","inner":{"kind":"string"}},"required":true}
        ],"additional":{"kind":"number"}}});
        let declaration = SExpression::String(shape.to_string());
        for (value, accepted) in [
            (serde_json::json!({"token":null,"id":"","extra":2}), true),
            (serde_json::json!({"token":"ok"}), true),
            (serde_json::json!({}), false),
            (serde_json::json!({"token":false}), false),
            (serde_json::json!({"token":null,"id":null}), false),
            (serde_json::json!({"token":null,"extra":"bad"}), false),
            (serde_json::json!({"token":null,"extra":null}), false),
        ] {
            let literal: SExpression = serde_json::from_value(value.clone()).unwrap();
            let quoted = orbital_core_base::schema::quote::quote_expr(&literal);
            let typed = TypedOp.evaluate(&[quoted, declaration.clone()], &context);
            assert_eq!(typed.is_ok(), accepted, "typed {value}: {typed:?}");
            let parsed = JsonParseOp.evaluate(&[SExpression::String(value.to_string()), declaration.clone()], &context).unwrap();
            assert_eq!(!parsed.is_null(), accepted, "parse {value}: {parsed:?}");
        }
        let legacy = SExpression::String(serde_json::json!({"type":{"kind":"string"}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::Null, legacy], &context).is_ok(), "old unmarked null semantics remain");
        let strict = SExpression::String(serde_json::json!({"version":1,"type":{"kind":"string"}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::Null, strict], &context).is_err(), "new nonnull root refuses null");
    }

    #[test]
    fn malformed_quoted_enum_is_not_a_nominal_type_in_a_strict_descriptor() {
        use crate::evaluator::context::BindingContext;
        use crate::evaluator::operators::{Operator, cast::TypedOp, json::JsonParseOp};
        let context = BindingContext::new();
        for name in [r#""bad\q""#, r#""unterminated"#, r#""a" |"#] {
            let declaration = SExpression::String(serde_json::json!({"version":1,"type":{"kind":"enum","name":name}}).to_string());
            assert!(TypedOp.evaluate(&[SExpression::String("anything".into()), declaration.clone()], &context).is_err(), "{name}");
            let parsed = JsonParseOp.evaluate(&[SExpression::String("\"anything\"".into()), declaration], &context).unwrap();
            assert!(parsed.is_null(), "{name}: {parsed:?}");
            let legacy = SExpression::String(serde_json::json!({"type":{"kind":"enum","name":name}}).to_string());
            assert!(TypedOp.evaluate(&[SExpression::String("anything".into()), legacy], &context).is_ok());
        }
        let nominal = SExpression::String(serde_json::json!({"version":1,"type":{"kind":"enum","name":"Status"}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::String("ready".into()), nominal], &context).is_ok());
        let old_quoted = SExpression::String(serde_json::json!({"version":1,"type":{"kind":"enum","name":"\"line1\nline2\" | \"ready\""}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::String("line1\nline2".into()), old_quoted], &context).is_ok());
    }

    #[test]
    fn unknown_descriptor_version_cannot_fall_back_to_a_bare_type() {
        for wire in [
            serde_json::json!({"version":2,"kind":"string"}),
            serde_json::json!({"version":2,"type":{"kind":"string"},"kind":"string"}),
        ] {
            assert!(declared_shape(&SExpression::String(wire.to_string())).is_err(), "{wire}");
        }
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
    fn declared_bytes_require_the_canonical_base64_wire_encoding() {
        let named = BTreeMap::new();
        for valid in ["", "AA==", "AP9h", "aGVsbG8="] {
            assert!(value_of_declared(&s(valid), &OirType::Bytes, &named).is_ok(), "{valid}");
        }
        for invalid in ["plain text", "AA", "AA-_", "!!", "AB=="] {
            assert!(value_of_declared(&s(invalid), &OirType::Bytes, &named).is_err(), "{invalid}");
            assert!(value_of_declared(&s(invalid), &OirType::String, &named).is_ok());
        }
        assert!(value_of_declared(&num(1.0), &OirType::Bytes, &named).is_err());
        let bytes = OirType::array(OirType::Bytes);
        assert!(value_of_declared(&Value::Array(vec![s("AA==")]), &bytes, &named).is_ok());
        assert_eq!(
            misfit_of_declared(&Value::Array(vec![s("AA=="), s("invalid bytes")]), &bytes, &named),
            Some(("[1]".to_string(), "string"))
        );
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

    fn check_rt(op: EmbeddedOperator, args: &[Value]) -> Result<(), OrbitalError> {
        let op: &'static EmbeddedOperator = Box::leak(Box::new(op));
        let exprs: Vec<SExpression> = args.iter().map(|_| SExpression::Null).collect();
        let _frame = CallFrame::enter(Signature::of(op), &exprs);
        args.iter()
            .enumerate()
            .try_for_each(|(i, v)| check_argument(i, v))
    }

    fn list_of_var() -> EmbeddedOperator {
        operator(
            json!([{ "name": "arr", "type": { "kind": "array", "of": { "kind": "var", "name": "T" } } }]),
            Some(1),
        )
    }

    fn pair(a: Value, b: Value) -> Value {
        Value::Array(vec![a, b])
    }

    #[test]
    fn a_list_of_heterogeneous_tuples_is_a_list() {
        let op = list_of_var;
        let entries = Value::Array(vec![
            pair(s("a"), num(1.0)),
            pair(s("b"), s("x")),
            pair(s("c"), num(2.0)),
        ]);
        assert_eq!(
            check_rt(op(), &[entries]).map_err(|e| e.to_string()),
            Ok(())
        );
        let same_shape = Value::Array(vec![pair(s("a"), num(1.0)), pair(s("b"), num(2.0))]);
        assert!(check_rt(op(), &[same_shape]).is_ok());
        let records = Value::Array(vec![
            Value::Object(ValueMap::new()),
            Value::Object(ValueMap::new()),
        ]);
        assert!(check_rt(op(), &[records]).is_ok());
    }

    fn sexpr_of_var() -> EmbeddedOperator {
        operator(
            json!([{ "name": "body", "type": { "kind": "sexpr", "of": { "kind": "var", "name": "T" } } }]),
            Some(1),
        )
    }

    #[test]
    fn a_body_that_is_a_list_of_unlike_items_is_one_value_of_an_unevaluated_variable() {
        let tuple = Value::Array(vec![s("create"), s("Row"), num(1.0)]);
        assert_eq!(
            check_rt(sexpr_of_var(), &[tuple]).map_err(|e| e.to_string()),
            Ok(())
        );
    }

    #[test]
    fn control_an_unevaluated_variable_still_refuses_a_second_argument_of_another_kind() {
        let op = operator(
            json!([
                { "name": "a", "type": { "kind": "sexpr", "of": { "kind": "var", "name": "T" } } },
                { "name": "b", "type": { "kind": "var", "name": "T" } }
            ]),
            Some(2),
        );
        assert!(check_rt(op, &[num(1.0), s("x")]).is_err());
    }

    fn op_tuple(action: &str, widths: usize) -> Value {
        let mut items = vec![s(action), s("Row"), s("id")];
        if widths > 3 {
            items.push(Value::Object(ValueMap::new()));
        }
        Value::Array(items)
    }

    #[test]
    fn a_list_of_op_tuples_of_unlike_widths_is_a_list() {
        let ops = Value::Array(vec![
            op_tuple("delete", 3),
            op_tuple("update", 4),
            op_tuple("delete", 3),
        ]);
        assert_eq!(
            check_rt(list_of_var(), &[ops.clone()]).map_err(|e| e.to_string()),
            Ok(())
        );
        let updates = Value::Array(vec![op_tuple("update", 4), op_tuple("update", 4)]);
        assert_eq!(
            check_rt(append(), &[updates, op_tuple("delete", 3)]).map_err(|e| e.to_string()),
            Ok(())
        );
        let deletes = Value::Array(vec![op_tuple("delete", 3)]);
        assert_eq!(
            check_rt(append(), &[deletes, op_tuple("update", 4)]).map_err(|e| e.to_string()),
            Ok(())
        );
    }

    #[test]
    fn control_op_tuples_of_unlike_widths_do_not_join_a_list_of_scalars() {
        let numbers = Value::Array(vec![num(1.0), num(2.0)]);
        assert!(check_rt(append(), &[numbers, op_tuple("update", 4)]).is_err());
        let mixed = Value::Array(vec![op_tuple("update", 4), num(1.0)]);
        assert!(check_rt(list_of_var(), &[mixed]).is_err());
    }

    fn nth() -> EmbeddedOperator {
        operator(
            json!([
                { "name": "arr", "type": { "kind": "union", "of": [
                    { "kind": "array", "of": { "kind": "var", "name": "T" } },
                    { "kind": "array", "of": { "kind": "var", "name": "E", "each": true } }
                ] } },
                { "name": "index", "type": "number" },
            ]),
            Some(2),
        )
    }

    #[test]
    fn a_pair_of_unlike_types_is_indexed_by_array_nth() {
        let entry = Value::Array(vec![s("a"), Value::Object(ValueMap::new())]);
        assert_eq!(check_rt(nth(), &[entry, num(1.0)]).map_err(|e| e.to_string()), Ok(()));
        let entry = Value::Array(vec![s("a"), num(1.0)]);
        assert_eq!(check_rt(nth(), &[entry, num(1.0)]).map_err(|e| e.to_string()), Ok(()));
        let numbers = Value::Array(vec![num(1.0), num(2.0)]);
        assert_eq!(check_rt(nth(), &[numbers, num(0.0)]).map_err(|e| e.to_string()), Ok(()));
    }

    #[test]
    fn control_an_operator_without_the_each_position_still_refuses_an_unlike_pair() {
        let entry = Value::Array(vec![s("a"), num(1.0)]);
        assert!(check_rt(list_of_var(), &[entry]).is_err());
        assert!(check_rt(nth(), &[Value::Array(vec![s("a")]), s("0")]).is_err(), "a non-number index is refused");
    }

    #[test]
    fn control_a_mixed_scalar_list_is_still_refused() {
        let op = list_of_var;
        assert!(check_rt(op(), &[Value::Array(vec![num(1.0), s("a")])]).is_err());
        let lists = Value::Array(vec![
            Value::Array(vec![num(1.0)]),
            Value::Array(vec![s("a")]),
        ]);
        assert!(check_rt(op(), &[lists]).is_err());
    }

    #[test]
    fn control_a_tuple_of_the_wrong_length_is_still_refused() {
        let op = || {
            operator(
                json!([{ "name": "entries", "type": { "kind": "array", "of": { "kind": "tuple", "items": ["string", "number"] } } }]),
                Some(1),
            )
        };
        assert!(check_rt(op(), &[Value::Array(vec![pair(s("a"), num(1.0))])]).is_ok());
        let short = Value::Array(vec![pair(s("a"), num(1.0)), Value::Array(vec![s("b")])]);
        assert!(check_rt(op(), &[short]).is_err());
        let mismatched = Value::Array(vec![pair(s("a"), num(1.0)), pair(s("b"), s("x"))]);
        assert!(check_rt(op(), &[mismatched]).is_err());
    }

    fn from_entries() -> EmbeddedOperator {
        operator(
            json!([{ "name": "entries", "type": { "kind": "array", "of": { "kind": "tuple", "items": [
                { "kind": "var", "name": "K" }, { "kind": "var", "name": "V", "each": true }
            ] } } }]),
            Some(1),
        )
    }

    #[test]
    fn pairs_whose_values_differ_in_type_are_entries_of_a_record() {
        let mixed = Value::Array(vec![
            pair(s("a"), num(1.0)),
            pair(s("b"), s("x")),
            pair(s("c"), Value::Bool(true)),
        ]);
        assert_eq!(
            check_rt(from_entries(), &[mixed]).map_err(|e| e.to_string()),
            Ok(())
        );
        let same = Value::Array(vec![pair(s("a"), num(1.0)), pair(s("b"), num(2.0))]);
        assert!(check_rt(from_entries(), &[same]).is_ok());
    }

    #[test]
    fn control_pairs_whose_keys_differ_in_type_are_still_refused() {
        let keys = Value::Array(vec![pair(s("a"), num(1.0)), pair(num(2.0), num(3.0))]);
        assert_eq!(
            check_rt(from_entries(), &[keys]).unwrap_err().to_string(),
            "Evaluation error: Type mismatch: expected string, got number"
        );
    }

    fn append() -> EmbeddedOperator {
        operator(
            json!([
                { "name": "arr", "type": { "kind": "array", "of": { "kind": "var", "name": "T" } } },
                { "name": "item", "type": { "kind": "var", "name": "T" } },
            ]),
            Some(2),
        )
    }

    #[test]
    fn a_pair_of_other_value_types_appends_to_a_list_of_pairs() {
        let acc = Value::Array(vec![pair(s("a"), num(1.0))]);
        assert_eq!(
            check_rt(append(), &[acc.clone(), pair(s("b"), s("x"))]).map_err(|e| e.to_string()),
            Ok(())
        );
        assert!(check_rt(append(), &[acc, pair(s("b"), num(2.0))]).is_ok());
    }

    #[test]
    fn control_a_pair_does_not_append_to_a_list_of_numbers_or_of_triples() {
        let numbers = Value::Array(vec![num(1.0), num(2.0)]);
        assert!(check_rt(append(), &[numbers, pair(s("b"), num(2.0))]).is_err());
        let strings = Value::Array(vec![pair(s("a"), s("b"))]);
        assert!(check_rt(append(), &[strings, pair(num(1.0), num(2.0))]).is_err());
    }

    #[test]
    fn a_tuple_of_another_width_joins_a_list_of_tuples() {
        let pairs = Value::Array(vec![pair(s("a"), num(1.0))]);
        let triple = Value::Array(vec![s("b"), num(2.0), s("c")]);
        assert!(check_rt(append(), &[pairs, triple]).is_ok());
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
