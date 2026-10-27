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
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{discriminant, Discriminant};
use std::sync::atomic::{AtomicBool, Ordering};
use orbital_core_base::sync::OnceLock;

use super::get_operator;
use crate::evaluator::operators::kind::OperatorKind;
use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::ir::types::{DeclaredShape, OirType};
use orbital_core_base::language::registry::{OperatorTypeRef, OperatorTypeRefKind};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::signature::{
    absent_slot, is_absent_slot, Actual, CallBindings, Known, Mismatch, Mode, Signature,
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
            OperatorKind::BehaviorCompositionStubOp(_) | OperatorKind::AgentStubOp(_) | OperatorKind::TypedOp | OperatorKind::ProvenOp
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

/// The scalar kind of an evaluated value, as an index into the distinct kinds a list can hold; `None` for a value that has no scalar type.
fn scalar_kind(value: &Value) -> Option<usize> {
    match value {
        Value::Number(_) => Some(0),
        Value::String(_) => Some(1),
        Value::Bool(_) => Some(2),
        Value::Object(_) => Some(3),
        Value::Null | Value::Lambda { .. } | Value::Array(_) => None,
    }
}

/// Whether `entry` is the entry whose position type is `ty` in [`inner_list_positions`], answered without building the type.
fn entry_has_position(entry: &Value, ty: &OirType) -> bool {
    match entry {
        Value::Number(_) => matches!(ty, OirType::Number),
        Value::String(_) => matches!(ty, OirType::String),
        Value::Bool(_) => matches!(ty, OirType::Boolean),
        Value::Object(_) => matches!(ty, OirType::Struct { fields, additional: None } if fields.is_empty()),
        Value::Null | Value::Lambda { .. } => is_absent_slot(ty),
        Value::Array(_) => false,
    }
}

/// Whether a list with no list among its entries already has the positions `known`: the repeat of a shape seen, which adds nothing.
fn flat_list_repeats(entries: &[Value], known: &[OirType]) -> bool {
    entries.len() == known.len()
        && entries
            .iter()
            .zip(known)
            .all(|(entry, ty)| entry_has_position(entry, ty))
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
    let mut seen_kinds = [false; 4];
    for item in items {
        match item {
            Value::Array(_) if tuple_formal || nested_formal => {
                for known in views(item, element_formal) {
                    if let Known::Type(element) = known {
                        note(element);
                    }
                }
            }
            Value::Array(entries) => {
                if !entries.iter().any(|entry| matches!(entry, Value::Array(_)))
                    && inner.iter().any(|known| flat_list_repeats(entries, known))
                {
                    continue;
                }
                if let Some(positions) = inner_list_positions(item) {
                    if !inner.contains(&positions) {
                        inner.push(positions);
                    }
                }
            }
            _ => {
                if let Some(kind) = scalar_kind(item) {
                    if !seen_kinds[kind] {
                        seen_kinds[kind] = true;
                        if let Some(Known::Type(element)) = scalar_known(item) {
                            note(element);
                        }
                    }
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
    // A list of lists settles its entries together, as a list of lists does at the top: unlike op tuples are one element type.
    if !entries.is_empty() && entries.iter().all(|entry| matches!(entry, Value::Array(_) | Value::Null)) {
        let nested: Vec<Option<Vec<OirType>>> = entries.iter().map(inner_list_positions).collect();
        let typed: Vec<Vec<OirType>> = nested.iter().flatten().cloned().collect();
        if !typed.is_empty() {
            if let [element] = settle_inner_lists(typed, Vec::new()).as_slice() {
                return Some(
                    nested
                        .iter()
                        .map(|positions| match positions {
                            Some(_) => element.clone(),
                            None => absent_slot(),
                        })
                        .collect(),
                );
            }
        }
    }
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
        .any(|ty| !is_absent_slot(ty))
        .then_some(positions)
}

/// Whether the entries of a list differ in type: such a list is a tuple.
fn is_tuple(positions: &[OirType]) -> bool {
    let mut present = positions.iter().filter(|ty| !is_absent_slot(ty));
    present
        .next()
        .is_some_and(|first| present.any(|ty| ty != first))
}

/// A list's own type: a list of its entries' one type, or a tuple of their types.
fn settle_positions(positions: Vec<OirType>) -> OirType {
    if is_tuple(&positions) {
        return OirType::Tuple { items: positions };
    }
    match positions.into_iter().find(|ty| !is_absent_slot(ty)) {
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
        Mismatch::MaybeAbsent { operator, operand } | Mismatch::MissingResult { operator, operand, .. } => {
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
    /// The positions whose argument is not checked: proven by the static checker (`proven`).
    skip: u64,
}

orbital_core_base::kernel_local! {
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// Marks `args` as the argument slice of the operator call being evaluated until dropped.
pub struct CallFrame;

impl CallFrame {
    pub fn enter(signature: Signature<'static>, args: &[SExpression]) -> Self {
        Self::enter_skipping(signature, args, 0)
    }

    /// [`CallFrame::enter`] for a call whose arguments at the positions in `proven` (bit `i` for position `i`) the
    /// static checker proved. A proven position is still checked while it shares a type variable with an unproven
    /// one, whose binding it constrains.
    pub fn enter_proven(signature: Signature<'static>, args: &[SExpression], proven: u64) -> Self {
        let skip = trusted_positions(&signature, args.len(), proven);
        Self::enter_skipping(signature, args, skip)
    }

    fn enter_skipping(signature: Signature<'static>, args: &[SExpression], skip: u64) -> Self {
        let start = args.as_ptr() as usize;
        let end = start + std::mem::size_of_val(args);
        FRAMES.with(|f| {
            f.borrow_mut().push(Frame {
                start,
                end,
                signature,
                bindings: CallBindings::new(),
                skip,
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
        if at < top.start || at >= top.end {
            return None;
        }
        let index = (at - top.start) / std::mem::size_of::<SExpression>();
        (index >= 64 || top.skip & (1 << index) == 0).then_some(index)
    })
}

fn is_proven(proven: u64, index: usize) -> bool {
    index < 64 && proven & (1 << index) != 0
}

/// The proven positions of a call of `argc` arguments that share no type variable with an unproven position.
fn trusted_positions(signature: &Signature<'static>, argc: usize, proven: u64) -> u64 {
    let mut skip = 0;
    for index in (0..argc.min(64)).filter(|&index| is_proven(proven, index)) {
        let mut names = Vec::new();
        if let Some(formal) = signature.formal(index) {
            variables_of(formal, &mut names);
        }
        let constrained = (0..argc).any(|other| {
            !is_proven(proven, other)
                && signature
                    .formal(other)
                    .is_some_and(|formal| names.iter().any(|name| names_variable(formal, name)))
        });
        if !constrained {
            skip |= 1 << index;
        }
    }
    skip
}

/// The type variables `ty` names, as [`names_variable`] reads them.
fn variables_of<'a>(ty: &'a OperatorTypeRef, out: &mut Vec<&'a str>) {
    let OperatorTypeRef::Tagged(kind) = ty else {
        return;
    };
    match kind {
        OperatorTypeRefKind::Var { name, bound, .. } => {
            out.push(name);
            if let Some(bound) = bound.as_deref() {
                variables_of(bound, out);
            }
        }
        OperatorTypeRefKind::Union { of } => of.iter().for_each(|member| variables_of(member, out)),
        OperatorTypeRefKind::Array { of } => variables_of(of, out),
        OperatorTypeRefKind::Tuple { items } => items.iter().for_each(|item| variables_of(item, out)),
        OperatorTypeRefKind::Map { key, value } => {
            variables_of(key, out);
            variables_of(value, out);
        }
        OperatorTypeRefKind::Object { fields, .. } => fields.values().for_each(|field| variables_of(field, out)),
        OperatorTypeRefKind::Binding { shape } => {
            if let Some(shape) = shape.as_deref() {
                variables_of(shape, out);
            }
        }
        OperatorTypeRefKind::Sexpr { of } => {
            if let Some(of) = of.as_deref() {
                variables_of(of, out);
            }
        }
        OperatorTypeRefKind::Fn { params, returns } => {
            params.iter().for_each(|param| variables_of(param, out));
            variables_of(returns, out);
        }
        OperatorTypeRefKind::PickOf { of }
        | OperatorTypeRefKind::OmitOf { of }
        | OperatorTypeRefKind::MergeOf { of }
        | OperatorTypeRefKind::ChildrenOf { of, .. } => out.push(of),
        OperatorTypeRefKind::KeyOf { .. }
        | OperatorTypeRefKind::FieldOf { .. }
        | OperatorTypeRefKind::Literal { .. }
        | OperatorTypeRefKind::Entity { .. }
        | OperatorTypeRefKind::EntityRef
        | OperatorTypeRefKind::EventKey { .. }
        | OperatorTypeRefKind::UiSlot
        | OperatorTypeRefKind::PatternType
        | OperatorTypeRefKind::Record
        | OperatorTypeRefKind::Shape => {}
    }
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

orbital_core_base::kernel_local! {
    static DECLARED: RefCell<HashMap<String, std::rc::Rc<DeclaredShape>>> = RefCell::new(HashMap::new());
}

/// The declared shape a `typed` form or a `json/parse` shape records as JSON text: a bare `OirType`, or the type with
/// the definitions of the program types it names (`DeclaredShape`).
pub fn declared_shape(arg: &SExpression) -> Result<std::rc::Rc<DeclaredShape>, OrbitalError> {
    let SExpression::String(text) = arg else {
        return Err(OrbitalError::Eval(EvalError::InvalidArgument("typed: the declared type must be JSON text".into())));
    };
    if let Some(known) = DECLARED.with(|d| d.borrow().get(text.as_str()).cloned()) {
        return Ok(known);
    }
    let shape = DeclaredShape::read(text).map_err(|error| {
        OrbitalError::Eval(EvalError::InvalidArgument(format!("typed: the declared type is not a type: {error}")))
    })?;
    let shape = std::rc::Rc::new(shape);
    DECLARED.with(|d| d.borrow_mut().insert(text.clone().into_inner(), std::rc::Rc::clone(&shape)));
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
        OirType::Integer | OirType::Money | OirType::Instant => "number".to_string(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
            .unwrap_or_else(|| "a value".to_string()),
    }
}

/// The type a by-name reference resolves to: a `TypeRef` or `Reference` whose name the recorded shape's `named` table
/// defines is that definition, through a chain of aliases; `None` when `ty` names no defined type (or a cycle of bare
/// aliases).
fn declared_alias<'a>(ty: &'a OirType, named: &'a BTreeMap<String, OirType>) -> Option<&'a OirType> {
    let mut seen: Vec<&str> = Vec::new();
    let mut resolved: Option<&'a OirType> = None;
    loop {
        let name = match resolved.unwrap_or(ty) {
            OirType::TypeRef { name } | OirType::Reference { entity: name } => name.as_str(),
            _ => return resolved,
        };
        if seen.contains(&name) {
            return None;
        }
        match named.get(name) {
            Some(next) => resolved = Some(next),
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
    // A reference is the related row's id, or the row itself, which is checked as one.
    if let (OirType::Reference { .. }, Value::String(_)) = (ty, value) {
        return None;
    }
    if let Some(declared) = declared_alias(ty, named) {
        return first_misfit(value, declared, named, strict);
    }
    let misfit = || Some((String::new(), value.type_name()));
    if strict && value.is_null() && !matches!(ty, OirType::Union { .. }) {
        return misfit();
    }
    // The path is written only for a misfit: a value that fits allocates nothing.
    let at = |prefix: std::fmt::Arguments<'_>, (path, actual): (String, &'static str)| (format!("{prefix}{path}"), actual);
    let items = |items: &[Value], inner: &OirType| {
        items.iter().enumerate().find_map(|(i, item)| first_misfit(item, inner, named, strict).map(|misfit| at(format_args!("[{i}]"), misfit)))
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
                .find_map(|(i, (item, slot))| first_misfit(item, slot, named, strict).map(|misfit| at(format_args!("[{i}]"), misfit))),
            _ => misfit(),
        },
        OirType::Map { value: inner, .. } => match value {
            Value::Object(map) => {
                map.iter().find_map(|(k, item)| first_misfit(item, inner, named, strict).map(|misfit| at(format_args!(".{k}"), misfit)))
            }
            _ => misfit(),
        },
        OirType::Struct { fields, additional } => match value {
            Value::Object(map) => fields.iter().find_map(|field| match map.get(&field.name) {
                None => field.required.then(|| (format!(".{}", field.name), "absent")),
                Some(item) => first_misfit(item, &field.field_type, named, strict).map(|misfit| at(format_args!(".{}", field.name), misfit)),
            }).or_else(|| additional.as_deref().and_then(|additional| {
                map.iter().filter(|(key, _)| !fields.iter().any(|field| field.name.as_str() == key.as_str()))
                    .find_map(|(key, item)| first_misfit(item, additional, named, strict).map(|misfit| at(format_args!(".{key}"), misfit)))
            })),
            _ => misfit(),
        },
        OirType::File | OirType::Row { .. } | OirType::TypeRef { .. } => {
            (!matches!(value, Value::Object(_))).then(|| (String::new(), value.type_name()))
        }
        OirType::Reference { .. } => {
            (!matches!(value, Value::Object(_) | Value::String(_))).then(|| (String::new(), value.type_name()))
        }
        OirType::Number | OirType::Integer | OirType::Money | OirType::Instant => {
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

/// Whether `ty` names the type variable `name` anywhere, other than as the `key-of` / `field-of` reference that reads a
/// record's declared fields and so never binds the variable.
fn names_variable(ty: &OperatorTypeRef, name: &str) -> bool {
    let OperatorTypeRef::Tagged(kind) = ty else {
        return false;
    };
    match kind {
        OperatorTypeRefKind::Var { name: own, bound, .. } => {
            own == name || bound.as_deref().is_some_and(|bound| names_variable(bound, name))
        }
        OperatorTypeRefKind::Union { of } => of.iter().any(|member| names_variable(member, name)),
        OperatorTypeRefKind::Array { of } => names_variable(of, name),
        OperatorTypeRefKind::Tuple { items } => items.iter().any(|item| names_variable(item, name)),
        OperatorTypeRefKind::Map { key, value } => names_variable(key, name) || names_variable(value, name),
        OperatorTypeRefKind::Object { fields, .. } => fields.values().any(|field| names_variable(field, name)),
        OperatorTypeRefKind::Binding { shape } => shape.as_deref().is_some_and(|shape| names_variable(shape, name)),
        OperatorTypeRefKind::Sexpr { of } => of.as_deref().is_some_and(|of| names_variable(of, name)),
        OperatorTypeRefKind::Fn { params, returns } => {
            params.iter().any(|param| names_variable(param, name)) || names_variable(returns, name)
        }
        OperatorTypeRefKind::PickOf { of }
        | OperatorTypeRefKind::OmitOf { of }
        | OperatorTypeRefKind::MergeOf { of }
        | OperatorTypeRefKind::ChildrenOf { of, .. } => of == name,
        OperatorTypeRefKind::KeyOf { .. }
        | OperatorTypeRefKind::FieldOf { .. }
        | OperatorTypeRefKind::Literal { .. }
        | OperatorTypeRefKind::Entity { .. }
        | OperatorTypeRefKind::EntityRef
        | OperatorTypeRefKind::EventKey { .. }
        | OperatorTypeRefKind::UiSlot
        | OperatorTypeRefKind::PatternType
        | OperatorTypeRefKind::Record
        | OperatorTypeRefKind::Shape => false,
    }
}

/// Whether an object value at `index` can neither fail nor leave a trace in the call's bindings, so the binder is not run.
///
/// A plain `record` formal admits every object. A variable whose bound admits a record (`record`, or a union with
/// one) and that is not an `each` variable admits it too; the binding it would make, `{}` (an untyped object carries no
/// fields), is read only by another position that names the same variable, so it is skipped exactly when no other
/// position does. A `key-of` position reads nothing at run time (its argument has no text here) and a `field-of`
/// position resolves against `{}` to nothing, the same as against an unbound variable. A signature with return
/// semantics unifies bindings across positions and is left to the binder, as is a rest position, which binds the one
/// variable once per argument.
fn object_binds_nothing(signature: &Signature<'static>, index: usize) -> bool {
    let Some(formal) = signature.formal(index) else {
        return false;
    };
    let OperatorTypeRef::Tagged(kind) = formal else {
        return false;
    };
    let (name, bound) = match kind {
        OperatorTypeRefKind::Record => return true,
        OperatorTypeRefKind::Var { name, bound, each: false } => (name, bound.as_deref()),
        _ => return false,
    };
    if signature.semantics().is_some() {
        return false;
    }
    let admits_record = |ty: &OperatorTypeRef| matches!(ty, OperatorTypeRef::Tagged(OperatorTypeRefKind::Record));
    let admits = match bound {
        None => true,
        Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Union { of })) => of.iter().any(admits_record),
        Some(bound) => admits_record(bound),
    };
    if !admits {
        return false;
    }
    let same_as = |at: usize| signature.formal(at).is_some_and(|other| std::ptr::eq(other, formal));
    if same_as(index + 1) || (index > 0 && same_as(index - 1)) {
        return false;
    }
    let mut previous: Option<&OperatorTypeRef> = None;
    let mut position = 0;
    while let Some(other) = signature.formal(position) {
        if previous.is_some_and(|prior| std::ptr::eq(prior, other)) {
            break;
        }
        if position != index && names_variable(other, name) {
            return false;
        }
        previous = Some(other);
        position += 1;
    }
    true
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
        if matches!(value, Value::Object(_)) && object_binds_nothing(&signature, index) {
            return Ok(());
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
        let declaration = SExpression::string(shape.to_string());
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
            let parsed = JsonParseOp.evaluate(&[SExpression::string(value.to_string()), declaration.clone()], &context).unwrap();
            assert_eq!(!parsed.is_null(), accepted, "parse {value}: {parsed:?}");
        }
        let legacy = SExpression::string(serde_json::json!({"type":{"kind":"string"}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::Null, legacy], &context).is_ok(), "old unmarked null semantics remain");
        let strict = SExpression::string(serde_json::json!({"version":1,"type":{"kind":"string"}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::Null, strict], &context).is_err(), "new nonnull root refuses null");
    }

    #[test]
    fn malformed_quoted_enum_is_not_a_nominal_type_in_a_strict_descriptor() {
        use crate::evaluator::context::BindingContext;
        use crate::evaluator::operators::{Operator, cast::TypedOp, json::JsonParseOp};
        let context = BindingContext::new();
        for name in [r#""bad\q""#, r#""unterminated"#, r#""a" |"#] {
            let declaration = SExpression::string(serde_json::json!({"version":1,"type":{"kind":"enum","name":name}}).to_string());
            assert!(TypedOp.evaluate(&[SExpression::string("anything"), declaration.clone()], &context).is_err(), "{name}");
            let parsed = JsonParseOp.evaluate(&[SExpression::string("\"anything\""), declaration], &context).unwrap();
            assert!(parsed.is_null(), "{name}: {parsed:?}");
            let legacy = SExpression::string(serde_json::json!({"type":{"kind":"enum","name":name}}).to_string());
            assert!(TypedOp.evaluate(&[SExpression::string("anything"), legacy], &context).is_ok());
        }
        let nominal = SExpression::string(serde_json::json!({"version":1,"type":{"kind":"enum","name":"Status"}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::string("ready"), nominal], &context).is_ok());
        let old_quoted = SExpression::string(serde_json::json!({"version":1,"type":{"kind":"enum","name":"\"line1\nline2\" | \"ready\""}}).to_string());
        assert!(TypedOp.evaluate(&[SExpression::string("line1\nline2"), old_quoted], &context).is_ok());
    }

    #[test]
    fn unknown_descriptor_version_cannot_fall_back_to_a_bare_type() {
        for wire in [
            serde_json::json!({"version":2,"kind":"string"}),
            serde_json::json!({"version":2,"type":{"kind":"string"},"kind":"string"}),
        ] {
            assert!(declared_shape(&SExpression::string(wire.to_string())).is_err(), "{wire}");
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
        Value::string(t.to_string())
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
        assert!(value_of_declared(&Value::array(vec![s("AA==")]), &bytes, &named).is_ok());
        assert_eq!(
            misfit_of_declared(&Value::array(vec![s("AA=="), s("invalid bytes")]), &bytes, &named),
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
        assert!(check(&list_or_string, &[Value::array(vec![])]).is_ok());
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
        assert!(check(&op, &[Value::array(vec![num(1.0)]), num(2.0)]).is_ok());
        assert_eq!(
            check(&op, &[Value::array(vec![num(1.0)]), s("x")])
                .unwrap_err()
                .to_string(),
            "Evaluation error: Type mismatch: expected number, got string"
        );
        assert!(
            check(&op, &[Value::array(vec![]), s("x")]).is_ok(),
            "an empty list binds nothing"
        );
        assert!(check(&op, &[Value::array(vec![Value::Null, num(1.0)]), num(2.0)]).is_ok());
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
        Value::array(vec![a, b])
    }

    #[test]
    fn a_list_of_heterogeneous_tuples_is_a_list() {
        let op = list_of_var;
        let entries = Value::array(vec![
            pair(s("a"), num(1.0)),
            pair(s("b"), s("x")),
            pair(s("c"), num(2.0)),
        ]);
        assert_eq!(
            check_rt(op(), &[entries]).map_err(|e| e.to_string()),
            Ok(())
        );
        let same_shape = Value::array(vec![pair(s("a"), num(1.0)), pair(s("b"), num(2.0))]);
        assert!(check_rt(op(), &[same_shape]).is_ok());
        let records = Value::array(vec![
            Value::Object(ValueMap::default().into()),
            Value::Object(ValueMap::default().into()),
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
        let tuple = Value::array(vec![s("create"), s("Row"), num(1.0)]);
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
            items.push(Value::object(ValueMap::default()));
        }
        Value::array(items)
    }

    #[test]
    fn a_list_of_op_tuples_of_unlike_widths_is_a_list() {
        let ops = Value::array(vec![
            op_tuple("delete", 3),
            op_tuple("update", 4),
            op_tuple("delete", 3),
        ]);
        assert_eq!(
            check_rt(list_of_var(), &[ops.clone()]).map_err(|e| e.to_string()),
            Ok(())
        );
        let updates = Value::array(vec![op_tuple("update", 4), op_tuple("update", 4)]);
        assert_eq!(
            check_rt(append(), &[updates, op_tuple("delete", 3)]).map_err(|e| e.to_string()),
            Ok(())
        );
        let deletes = Value::array(vec![op_tuple("delete", 3)]);
        assert_eq!(
            check_rt(append(), &[deletes, op_tuple("update", 4)]).map_err(|e| e.to_string()),
            Ok(())
        );
    }

    #[test]
    fn groups_of_op_tuples_of_unlike_widths_flatten_to_one_list() {
        let flatten = || {
            operator(
                json!([{ "name": "arr", "type": { "kind": "array", "of": { "kind": "array", "of": { "kind": "var", "name": "T" } } } }]),
                Some(1),
            )
        };
        let create = Value::array(vec![s("create"), s("Row"), Value::Object(ValueMap::default().into())]);
        let rekey = Value::array(vec![op_tuple("delete", 3), create]);
        let set = Value::array(vec![op_tuple("update", 4)]);
        let groups = Value::array(vec![Value::Array(vec![rekey].into()), Value::Array(vec![set].into())]);
        assert_eq!(check_rt(flatten(), &[groups]).map_err(|e| e.to_string()), Ok(()));
        let unlike = Value::array(vec![
            Value::Array(vec![Value::Array(vec![num(1.0)].into())].into()),
            Value::Array(vec![Value::Array(vec![s("x")].into())].into()),
        ]);
        assert!(check_rt(flatten(), &[unlike]).is_err(), "control: lists of unlike scalars stay two types");
    }

    #[test]
    fn control_op_tuples_of_unlike_widths_do_not_join_a_list_of_scalars() {
        let numbers = Value::array(vec![num(1.0), num(2.0)]);
        assert!(check_rt(append(), &[numbers, op_tuple("update", 4)]).is_err());
        let mixed = Value::array(vec![op_tuple("update", 4), num(1.0)]);
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
        let entry = Value::array(vec![s("a"), Value::Object(ValueMap::default().into())]);
        assert_eq!(check_rt(nth(), &[entry, num(1.0)]).map_err(|e| e.to_string()), Ok(()));
        let entry = Value::array(vec![s("a"), num(1.0)]);
        assert_eq!(check_rt(nth(), &[entry, num(1.0)]).map_err(|e| e.to_string()), Ok(()));
        let numbers = Value::array(vec![num(1.0), num(2.0)]);
        assert_eq!(check_rt(nth(), &[numbers, num(0.0)]).map_err(|e| e.to_string()), Ok(()));
    }

    #[test]
    fn control_an_operator_without_the_each_position_still_refuses_an_unlike_pair() {
        let entry = Value::array(vec![s("a"), num(1.0)]);
        assert!(check_rt(list_of_var(), &[entry]).is_err());
        assert!(check_rt(nth(), &[Value::array(vec![s("a")]), s("0")]).is_err(), "a non-number index is refused");
    }

    #[test]
    fn control_a_mixed_scalar_list_is_still_refused() {
        let op = list_of_var;
        assert!(check_rt(op(), &[Value::array(vec![num(1.0), s("a")])]).is_err());
        let lists = Value::array(vec![
            Value::Array(vec![num(1.0)].into()),
            Value::Array(vec![s("a")].into()),
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
        assert!(check_rt(op(), &[Value::array(vec![pair(s("a"), num(1.0))])]).is_ok());
        let short = Value::array(vec![pair(s("a"), num(1.0)), Value::Array(vec![s("b")].into())]);
        assert!(check_rt(op(), &[short]).is_err());
        let mismatched = Value::array(vec![pair(s("a"), num(1.0)), pair(s("b"), s("x"))]);
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
        let mixed = Value::array(vec![
            pair(s("a"), num(1.0)),
            pair(s("b"), s("x")),
            pair(s("c"), Value::Bool(true)),
        ]);
        assert_eq!(
            check_rt(from_entries(), &[mixed]).map_err(|e| e.to_string()),
            Ok(())
        );
        let same = Value::array(vec![pair(s("a"), num(1.0)), pair(s("b"), num(2.0))]);
        assert!(check_rt(from_entries(), &[same]).is_ok());
    }

    #[test]
    fn control_pairs_whose_keys_differ_in_type_are_still_refused() {
        let keys = Value::array(vec![pair(s("a"), num(1.0)), pair(num(2.0), num(3.0))]);
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
        let acc = Value::array(vec![pair(s("a"), num(1.0))]);
        assert_eq!(
            check_rt(append(), &[acc.clone(), pair(s("b"), s("x"))]).map_err(|e| e.to_string()),
            Ok(())
        );
        assert!(check_rt(append(), &[acc, pair(s("b"), num(2.0))]).is_ok());
    }

    #[test]
    fn control_a_pair_does_not_append_to_a_list_of_numbers_or_of_triples() {
        let numbers = Value::array(vec![num(1.0), num(2.0)]);
        assert!(check_rt(append(), &[numbers, pair(s("b"), num(2.0))]).is_err());
        let strings = Value::array(vec![pair(s("a"), s("b"))]);
        assert!(check_rt(append(), &[strings, pair(num(1.0), num(2.0))]).is_err());
    }

    #[test]
    fn a_tuple_of_another_width_joins_a_list_of_tuples() {
        let pairs = Value::array(vec![pair(s("a"), num(1.0))]);
        let triple = Value::array(vec![s("b"), num(2.0), s("c")]);
        assert!(check_rt(append(), &[pairs, triple]).is_ok());
    }

    #[test]
    fn a_record_variable_takes_an_object_and_not_an_array() {
        let op = operator(
            json!([{ "name": "obj", "type": { "kind": "var", "name": "R", "bound": { "kind": "record" } } }]),
            Some(1),
        );
        assert!(check(&op, &[Value::object(ValueMap::default())]).is_ok());
        assert_eq!(
            check(&op, &[Value::array(vec![])]).unwrap_err().to_string(),
            "Evaluation error: Type mismatch: expected object, got array"
        );
    }

    #[test]
    fn a_long_homogeneous_list_of_each_scalar_kind_passes() {
        let strings = Value::Array((0..500).map(|i| s(&i.to_string())).collect());
        let numbers = Value::Array((0..500).map(|i| num(i as f64)).collect());
        let bools = Value::Array((0..500).map(|i| Value::Bool(i % 2 == 0)).collect());
        let records = Value::Array((0..500).map(|_| Value::object(ValueMap::default())).collect());
        for list in [strings, numbers, bools, records] {
            assert!(check_rt(list_of_var(), &[list]).is_ok());
        }
    }

    #[test]
    fn one_unlike_element_after_a_long_run_raises_the_same_error_as_in_a_short_list() {
        let short = Value::array(vec![s("a"), num(1.0)]);
        let mut long: Vec<Value> = (0..500).map(|i| s(&i.to_string())).collect();
        long.push(num(1.0));
        long.push(s("tail"));
        let expected = check_rt(list_of_var(), &[short]).unwrap_err().to_string();
        let got = check_rt(list_of_var(), &[Value::array(long)]).unwrap_err().to_string();
        assert_eq!(got, expected);
    }

    #[test]
    fn nulls_among_a_homogeneous_list_do_not_change_its_type() {
        let list = Value::array(vec![s("a"), Value::Null, s("b"), Value::Null]);
        assert!(check_rt(list_of_var(), &[list]).is_ok());
    }

    #[test]
    fn an_empty_list_and_a_list_of_nulls_are_empty_lists_that_agree_with_any_list() {
        let op = || {
            operator(
                json!([
                    { "name": "a", "type": { "kind": "array", "of": { "kind": "var", "name": "T" } } },
                    { "name": "b", "type": { "kind": "array", "of": { "kind": "var", "name": "T" } } }
                ]),
                Some(2),
            )
        };
        let strings = Value::array(vec![s("a"), s("b")]);
        let empty = Value::array(vec![]);
        let nulls = Value::array(vec![Value::Null, Value::Null]);
        assert!(check_rt(op(), &[strings.clone(), empty]).is_ok());
        assert!(check_rt(op(), &[nulls, strings]).is_ok());
    }

    #[test]
    fn repeated_rows_of_one_shape_settle_as_one_and_an_unlike_row_still_raises() {
        let row = || pair(s("a"), num(1.0));
        let rows = Value::Array((0..200).map(|_| row()).collect());
        assert!(check_rt(list_of_var(), &[rows]).is_ok());
        let with_null = Value::array(vec![pair(s("a"), Value::Null), pair(s("b"), Value::Null), pair(s("c"), Value::Null)]);
        assert!(check_rt(list_of_var(), &[with_null]).is_ok());
        let mut unlike: Vec<Value> = (0..200).map(|_| Value::array(vec![s("a")])).collect();
        unlike.push(Value::array(vec![num(1.0)]));
        assert!(check_rt(list_of_var(), &[Value::array(unlike)]).is_err());
    }

    #[test]
    fn rows_that_differ_only_after_many_repeats_are_still_tuples_of_one_list() {
        let mut rows: Vec<Value> = (0..200).map(|_| pair(s("a"), num(1.0))).collect();
        rows.push(pair(s("b"), s("x")));
        assert!(check_rt(list_of_var(), &[Value::array(rows)]).is_ok());
    }

    #[test]
    fn a_tuple_formal_still_checks_every_row_after_the_fast_path() {
        let op = || {
            operator(
                json!([{ "name": "entries", "type": { "kind": "array", "of": { "kind": "tuple", "items": ["string", "number"] } } }]),
                Some(1),
            )
        };
        let mut rows: Vec<Value> = (0..200).map(|_| pair(s("a"), num(1.0))).collect();
        assert!(check_rt(op(), &[Value::array(rows.clone())]).is_ok());
        rows.push(pair(s("b"), s("x")));
        assert!(check_rt(op(), &[Value::array(rows)]).is_err());
    }

    fn record_or_list_bound() -> serde_json::Value {
        json!({ "kind": "union", "of": [
            { "kind": "record" },
            { "kind": "array", "of": { "kind": "var", "name": "E", "each": true } }
        ] })
    }

    fn object_get() -> EmbeddedOperator {
        operator(
            json!([
                { "name": "obj", "type": { "kind": "var", "name": "R", "bound": record_or_list_bound() } },
                { "name": "path", "type": { "kind": "keyOf", "of": "R" } },
                { "name": "default", "optional": true, "type": { "kind": "fieldOf", "of": "R" } }
            ]),
            Some(3),
        )
    }

    fn object() -> Value {
        Value::object(ValueMap::default())
    }

    fn same_verdict(op: &EmbeddedOperator, args: &[Value]) {
        let bound = check(op, args).map_err(|e| e.to_string());
        let leaked: &'static EmbeddedOperator = Box::leak(Box::new(op.clone()));
        let exprs: Vec<SExpression> = args.iter().map(|_| SExpression::Null).collect();
        let _frame = CallFrame::enter(Signature::of(leaked), &exprs);
        let fast = args
            .iter()
            .enumerate()
            .try_for_each(|(i, v)| check_argument(i, v))
            .map_err(|e| e.to_string());
        assert_eq!(fast, bound, "{args:?}");
    }

    #[test]
    fn an_object_passes_the_record_or_list_position_of_object_get_with_its_key_and_default() {
        let op = object_get();
        assert!(check_rt(op.clone(), &[object(), s("a"), num(1.0)]).is_ok());
        assert!(check_rt(op.clone(), &[object(), s("a")]).is_ok());
        assert!(check_rt(op.clone(), &[Value::array(vec![num(1.0)]), s("0")]).is_ok());
        same_verdict(&op, &[object(), s("a"), s("fallback")]);
        same_verdict(&op, &[object(), num(3.0)]);
    }

    #[test]
    fn a_scalar_at_the_record_or_list_position_still_raises_the_binder_message() {
        let op = object_get();
        for value in [num(1.0), s("x"), Value::Bool(true)] {
            let err = check_rt(op.clone(), &[value.clone(), s("a")]).unwrap_err().to_string();
            assert!(err.contains("Type mismatch"), "{err}");
            same_verdict(&op, &[value, s("a")]);
        }
    }

    #[test]
    fn control_array_len_still_refuses_a_list_of_unlike_items() {
        let op = list_of_var();
        assert!(check_rt(op.clone(), &[Value::array(vec![num(1.0), s("b")])]).is_err());
        assert!(check_rt(op, &[Value::array(vec![num(1.0), num(2.0)])]).is_ok());
    }

    #[test]
    fn a_variable_named_by_another_position_still_binds_and_conflicts() {
        let op = operator(
            json!([
                { "name": "a", "type": { "kind": "var", "name": "R", "bound": { "kind": "record" } } },
                { "name": "b", "type": { "kind": "var", "name": "R" } }
            ]),
            Some(2),
        );
        assert!(check_rt(op.clone(), &[object(), object()]).is_ok());
        assert!(check_rt(op.clone(), &[object(), num(1.0)]).is_err());
        same_verdict(&op, &[object(), num(1.0)]);
        same_verdict(&op, &[object(), s("x")]);
        same_verdict(&op, &[object(), Value::array(vec![num(1.0)])]);
    }

    #[test]
    fn a_rest_position_of_a_record_variable_binds_each_argument_to_the_one_variable() {
        let op = operator(
            json!([{ "name": "...xs", "type": { "kind": "var", "name": "R", "bound": { "kind": "record" } } }]),
            None,
        );
        assert!(check_rt(op.clone(), &[object(), object(), object()]).is_ok());
        same_verdict(&op, &[object(), Value::array(vec![num(1.0)])]);
        same_verdict(&op, &[object(), num(1.0)]);
    }

    #[test]
    fn a_plain_record_position_takes_an_object_and_refuses_the_rest() {
        let op = operator(json!([{ "name": "r", "type": { "kind": "record" } }]), Some(1));
        assert!(check_rt(op.clone(), &[object()]).is_ok());
        same_verdict(&op, &[num(1.0)]);
        same_verdict(&op, &[Value::array(vec![])]);
    }

    #[test]
    fn an_object_variable_unbounded_by_a_record_binds_as_before() {
        let op = operator(
            json!([
                { "name": "a", "type": { "kind": "var", "name": "T" } },
                { "name": "b", "type": { "kind": "var", "name": "T" } }
            ]),
            Some(2),
        );
        same_verdict(&op, &[object(), object()]);
        same_verdict(&op, &[object(), num(1.0)]);
        same_verdict(&op, &[num(1.0), object()]);
    }
}
