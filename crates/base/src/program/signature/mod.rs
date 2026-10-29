//! Operator signatures: the one reader of the installed operator table's type language
//! (`@almadar/std` `OperatorTypeRef`) and the one resolver of its type variables.
//!
//! There are no generic types. An operator that works over many types declares TYPE VARIABLES in its
//! signature (`array/first : [T] -> T`, `= : T, T -> boolean`, `object/get : R, key-of R -> field-of R`); a
//! [`CallBindings`] binds them argument by argument, and [`Signature::result`] reads the call's concrete
//! result type off those bindings. The runtime check (`evaluator::operators::arg_types`, over the types of
//! evaluated values) and the compiler's static check and inference (over the types of expressions) both
//! drive this one resolver, so they cannot disagree on what a call means.
//!
//! What is checked is decided by the signature alone: a declared concrete type, the agreement of every
//! argument bound to one variable, a variable's bound, and — statically — that a `key-of R` argument names a
//! field of the record `R` is bound to. A static check ([`Mode::Proof`]) is total: an argument whose type is not
//! known or is loose — an unresolved expression, an expression value, json, a loose union — is an error
//! ([`Mismatch::Undetermined`]) at every position that declares a type, and `(as x T)` narrows it; an optional value
//! or the null literal is refused where the declared type excludes null.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::BTreeMap;
use std::fmt;

mod small;
use small::{SmallMap, SmallSet};

use crate::ir::types::{OirField, OirType};
use crate::language::registry::{
    EmbeddedOperator, EmbeddedOperatorParam, OperatorTypeRef, OperatorTypeRefKind,
};
use crate::type_compat::{oir_type_kind, sub};

/// The slot of a run-time tuple whose value is null: it is absent, so it passes its declared type and binds nothing.
pub fn absent_slot() -> OirType {
    OirType::absent_slot()
}

pub fn is_absent_slot(ty: &OirType) -> bool {
    ty.is_absent_slot()
}

/// What is known of one argument of a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Known {
    /// The argument has this type.
    Type(OirType),
    /// The argument is a list with no element: it is a list, and says nothing of the element type.
    EmptyList,
    /// The argument is an effect: it runs for what it does and produces no value.
    Void,
    /// The argument is the null value: it is absent, passes every declared type and binds nothing, but the
    /// variable it is passed to may then hold null.
    Absent,
}

/// A lambda's result type given its parameters' types. A lambda whose declared type has a `children-of` parameter
/// is read twice: first with that parameter untyped (a provisional reading, whose findings the caller drops), then
/// with it typed by the result the first reading gave.
pub type LambdaResult<'a> = &'a (dyn Fn(&[Option<OirType>]) -> Option<OirType> + 'a);

/// One argument of a call, as the checker sees it.
pub struct Actual<'a> {
    /// What is known of the argument's value; `None` when nothing is (an absent value, an unresolved binding).
    pub known: Option<Known>,
    /// The argument's text, when it is a literal string.
    pub text: Option<String>,
    /// The argument's strings, when it is a literal list of literal strings.
    pub texts: Option<Vec<String>>,
    /// The result type of the argument when it is a lambda, given the types of its parameters (`None` for a
    /// parameter whose type is not known). `Some` marks the argument a lambda.
    pub lambda: Option<LambdaResult<'a>>,
}

impl<'a> Actual<'a> {
    /// An argument of unknown type.
    pub fn unknown() -> Self {
        Self {
            known: None,
            text: None,
            texts: None,
            lambda: None,
        }
    }

    /// An argument of known type.
    pub fn of_type(ty: OirType) -> Self {
        Self {
            known: Some(Known::Type(ty)),
            ..Self::unknown()
        }
    }
}

/// How strictly a call is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The lenient reading over expression types: a `key-of` argument must name a field of the record it is bound
    /// to, and an argument of no informative type passes a concrete position. The compiler no longer uses it.
    Static,
    /// Over the types of evaluated values, which carry no field information: `key-of` is not checked.
    Runtime,
    /// The strict static check, and a proof (`proven`, G-CROSS-335) that every value of the argument's type fits: it
    /// fits only by subtyping (a date or semantic string still passes a string position at rest), a union fits a
    /// position that binds no variable only when each of its variants does, and an argument of no informative type
    /// (`json`, `sexpr`, a value of unknown type) is undetermined at every position that declares a type.
    Proof,
}

/// A call whose argument contradicts its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// The argument's type is not the declared one.
    Type { expected: String, actual: String },
    /// The argument's type is not the one an earlier argument bound the same type variable to.
    Conflict { expected: String, actual: String },
    /// A `key-of` argument names no field of the record.
    NoField { path: String, record: String },
    /// A value handed to a type variable has no determined type: what the variable stands for is unknown.
    Undetermined { reason: String },
    /// An ordering comparison (`<`, `<=`, `>`, `>=`) whose operand can be missing or null and is neither
    /// guarded by a presence check nor declared with a default. Both evaluators read a missing value as below
    /// any number while every push-down store fails the comparison, so the program must say which it means.
    MaybeAbsent { operator: String, operand: String },
    /// An operand that can be missing or null handed to a position the kernel raises on when it is null (the
    /// parameter's `rejectsNull`), with no presence check or default in front of it.
    NullOperand { operator: String, operand: String },
    /// An operand whose value an operator that can fail on a well-typed value produced (`source`, answering null
    /// then), handed to an operator that needs a value, with no default or presence check in front of it.
    MissingResult { operator: String, operand: String, source: String },
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mismatch::Type { expected, actual } | Mismatch::Conflict { expected, actual } => {
                write!(f, "expected {expected}, got {actual}")
            }
            Mismatch::NoField { path, record } => {
                write!(f, "`{path}` is not a field of {record}")
            }
            Mismatch::Undetermined { reason } => write!(f, "{reason}"),
            Mismatch::NullOperand { operator, operand } => {
                write!(
                    f,
                    "`{operand}` can be missing, and `{operator}` raises on a null operand"
                )
            }
            Mismatch::MaybeAbsent { operator, operand } => {
                write!(
                    f,
                    "`{operand}` can be missing, so `{operator}` over it is undecided"
                )
            }
            Mismatch::MissingResult { operator, operand, source } => {
                write!(f, "`{operand}` is missing when `{source}` fails, and `{operator}` needs a value")
            }
        }
    }
}

/// The program types a call's bindings read. A call that names none shares one empty map: binding runs once per checked
/// argument, so an allocation per call is most of what an unchecked-looking call costs.
#[derive(Debug, Clone)]
struct NamedTypes(std::sync::Arc<BTreeMap<String, OirType>>);

impl Default for NamedTypes {
    fn default() -> Self {
        static EMPTY: crate::sync::OnceLock<std::sync::Arc<BTreeMap<String, OirType>>> = crate::sync::OnceLock::new();
        NamedTypes(std::sync::Arc::clone(EMPTY.get_or_init(Default::default)))
    }
}

impl std::ops::Deref for NamedTypes {
    type Target = BTreeMap<String, OirType>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The variables a call has bound so far.
#[derive(Debug, Default, Clone)]
pub struct CallBindings {
    vars: SmallMap<Vec<OirType>>,
    keys: SmallMap<Vec<String>>,
    /// The key arguments that name no field of the record their variable is bound to: the argument position and
    /// the path. Reading such a field is an error only when the call supplies no default (see [`Signature::finish`]).
    absent: SmallMap<(usize, String)>,
    /// The type of the default supplied for a `field-of` parameter whose field the record does not declare.
    defaults: SmallMap<OirType>,
    /// The `field-of` variables the call supplies a never-null default for: a read through a record that may be
    /// missing is answered by it.
    defaulted: SmallSet,
    /// The variables the null value was passed to: they may hold null.
    nullable: SmallSet,
    /// The variables whose latest binding is an effect (it produces no value).
    voided: SmallSet,
    /// The variables holding only the empty list so far: a list with no element, whose type some sibling binding
    /// or the slot it fills must supply.
    empty_lists: SmallSet,
    /// The element variables of an empty list handed to a list position: nothing in the call has typed them yet.
    empty_elements: SmallSet,
    /// Whether the call is checked over expression types (an empty list nothing types is then an error).
    strict: bool,
    index: usize,
    shape: Option<String>,
    named_types: NamedTypes,
}

impl CallBindings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Empties the bindings for the next call, keeping their storage.
    pub fn clear(&mut self) {
        self.vars.clear();
        self.keys.clear();
        self.absent.clear();
        self.defaults.clear();
        self.defaulted.clear();
        self.nullable.clear();
        self.voided.clear();
        self.empty_lists.clear();
        self.empty_elements.clear();
        self.strict = false;
        self.index = 0;
        self.shape = None;
        self.named_types = NamedTypes::default();
    }

    pub fn use_named_types(&mut self, types: std::sync::Arc<BTreeMap<String, OirType>>) {
        self.named_types = NamedTypes(types);
    }

    fn agrees(&self, a: &OirType, b: &OirType) -> bool {
        let resolve = |name: &str| self.named_types.get(name).cloned();
        crate::type_compat::sub_with_named(a, b, &resolve).is_some()
            || crate::type_compat::sub_with_named(b, a, &resolve).is_some()
    }

    /// An empty list reached a list position whose element is `of`: the variables in it are typed by a sibling
    /// argument or not at all.
    fn note_empty_element(&mut self, of: &OperatorTypeRef) {
        let mut names = Vec::new();
        variables_in(of, &mut names);
        self.empty_elements.extend(names);
    }

    fn bound(&self, name: &str) -> Option<&OirType> {
        self.vars.get(name).and_then(|all| all.first())
    }

    fn last(&self, name: &str) -> Option<&OirType> {
        self.vars.get(name).and_then(|all| all.last())
    }
}

/// Declared argument positions for a unique field projection relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldProjection {
    pub source_arg: usize,
    pub key_arg: usize,
    pub default_arg: Option<usize>,
}

/// An operator's declared parameters and result.
#[derive(Clone, Copy)]
pub struct Signature<'a> {
    params: &'a [EmbeddedOperatorParam],
    required: usize,
    most: Option<usize>,
    unbounded: bool,
    checks: bool,
    returns: &'a OperatorTypeRef,
    semantics: Option<&'a str>,
}

impl<'a> Signature<'a> {
    pub fn of(op: &'a EmbeddedOperator) -> Self {
        Self {
            params: &op.params,
            required: op.min_arity as usize,
            most: op.max_arity.map(|m| m as usize),
            unbounded: op.max_arity.is_none(),
            checks: op.params.iter().any(|p| checks(&p.ty)),
            returns: &op.return_type,
            semantics: op.return_semantics.as_deref(),
        }
    }

    /// Whether the operand at `index` raises in the kernel when it is null: its declared type excludes null, unless it
    /// is an optional operand written before a later declared one (null then holds its place: `(array/sort xs null "desc")`).
    pub fn rejects_null(&self, index: usize) -> bool {
        let placeholder = index >= self.required
            && index + 1 < self.params.len()
            && !self.params[index].name.starts_with("...");
        !placeholder && self.formal(index).is_some_and(|ty| !ty.admits_null())
    }

    /// The declared arity a call of `count` arguments breaks: the bound it falls outside of, or `None` when it fits.
    pub fn arity_breach(&self, count: usize) -> Option<usize> {
        if count < self.required {
            return Some(self.required);
        }
        self.most.filter(|most| count > *most)
    }

    /// The declared type of the argument at `index`: its own parameter, or the rest parameter past the last.
    pub fn formal(&self, index: usize) -> Option<&'a OperatorTypeRef> {
        if let Some(param) = self.params.get(index) {
            return Some(&param.ty);
        }
        let last = self.params.last()?;
        (last.name.starts_with("...") || self.unbounded).then_some(&last.ty)
    }

    pub fn returns(&self) -> &'a OperatorTypeRef {
        self.returns
    }

    /// The declared parameters' count, and the index from which arguments share the rest parameter, when it has one.
    pub fn arity(&self) -> (usize, Option<usize>) {
        let rest = self.params.last().and_then(|last| {
            (last.name.starts_with("...") || self.unbounded).then_some(self.params.len() - 1)
        });
        (self.params.len(), rest)
    }

    /// The declared `returnSemantics` kind, when the operator has one.
    pub fn semantics(&self) -> Option<&'a str> {
        self.semantics
    }

    pub fn field_projection(&self) -> Option<FieldProjection> {
        use crate::language::registry::OperatorTypeRefKind as K;

        let OperatorTypeRef::Tagged(K::FieldOf { of }) = self.returns else {
            return None;
        };
        let mut source_arg = None;
        let mut key_arg = None;
        let mut default_arg = None;
        for (index, param) in self.params.iter().enumerate() {
            let OperatorTypeRef::Tagged(kind) = &param.ty else {
                continue;
            };
            let slot = match kind {
                K::Var { name, .. } if name == of => &mut source_arg,
                K::KeyOf { of: variable } if variable == of => &mut key_arg,
                K::FieldOf { of: variable } if variable == of => &mut default_arg,
                _ => continue,
            };
            if slot.replace(index).is_some() {
                return None;
            }
        }
        Some(FieldProjection {
            source_arg: source_arg?,
            key_arg: key_arg?,
            default_arg,
        })
    }

    /// The position of the parameter that names the result's declared shape (`json/parse`'s second).
    pub fn shape_position(&self) -> Option<usize> {
        self.params
            .iter()
            .position(|p| matches!(p.ty, OperatorTypeRef::Tagged(OperatorTypeRefKind::Shape)))
    }

    /// Whether the lambda at `index` is read twice, a provisional reading first (see [`LambdaResult`]).
    pub fn reads_lambda_twice(&self, index: usize) -> bool {
        matches!(
            self.formal(index),
            Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Fn { params, .. }))
                if params.iter().any(|p| matches!(p, OperatorTypeRef::Tagged(OperatorTypeRefKind::ChildrenOf { .. })))
        )
    }

    pub fn lambda_child_parameters(&self, index: usize) -> Vec<(usize, usize)> {
        use crate::language::registry::OperatorTypeRefKind as K;
        let Some(OperatorTypeRef::Tagged(K::Fn { params, .. })) = self.formal(index) else {
            return Vec::new();
        };
        params.iter().enumerate().filter_map(|(child, param)| {
            let OperatorTypeRef::Tagged(K::ChildrenOf { of, .. }) = param else { return None; };
            let node = params.iter().position(|param| matches!(param, OperatorTypeRef::Tagged(K::Var { name, .. }) if name == of))?;
            Some((node, child))
        }).collect()
    }

    pub fn bind_declared_node(&self, bindings: &mut CallBindings, index: usize, ty: OirType) {
        use crate::language::registry::OperatorTypeRefKind as K;
        let Some(OperatorTypeRef::Tagged(K::Fn { params, .. })) = self.formal(index) else {
            return;
        };
        for param in params {
            if let OperatorTypeRef::Tagged(K::ChildrenOf { of, .. }) = param {
                bindings.vars.insert(of.clone(), vec![ty.clone()]);
            }
        }
    }

    /// Whether any parameter declares a type a call is checked against. A signature of nominal std types and
    /// unevaluated expressions checks nothing, so a call to it needs no binding.
    pub fn checks_anything(&self) -> bool {
        self.checks
    }

    /// Binds argument `index` against its declared type. `Ok` when it fits, the variables it names are bound
    /// (or agree with their binding), and — statically — a `key-of` names a field.
    pub fn bind(
        &self,
        bindings: &mut CallBindings,
        index: usize,
        actual: &Actual<'_>,
        mode: Mode,
    ) -> Result<(), Mismatch> {
        let Some(formal) = self.formal(index) else {
            return Ok(());
        };
        if self.semantics == Some("union-of-args") {
            if let (
                OperatorTypeRef::Tagged(OperatorTypeRefKind::Array { of }),
                Some(Known::Type(OirType::Array { inner })),
            ) = (formal, &actual.known)
            {
                if let OperatorTypeRef::Tagged(OperatorTypeRefKind::Var {
                    name,
                    bound: None,
                    each: false,
                }) = &**of
                {
                    if let Some(prior) = bindings.bound(name) {
                        let records = [prior.clone(), (**inner).clone()];
                        if let Some(merged) = unify_structs(&records) {
                            bindings.vars.insert(name.clone(), vec![merged]);
                            return Ok(());
                        }
                        if is_typed_record_join(&records) {
                            return Err(undetermined("record elements have unknown additional values at a named field"));
                        }
                    }
                }
            }
        }
        if self.semantics == Some("branch-union") {
            if let (
                OperatorTypeRef::Tagged(OperatorTypeRefKind::Var { name, .. }),
                Some(Known::Type(actual)),
            ) = (formal, &actual.known)
            {
                if let Some(prior) = bindings.bound(name) {
                    let records = [prior.clone(), actual.clone()];
                    if let Some(merged) = unify_structs(&records) {
                        bindings.vars.insert(name.clone(), vec![merged]);
                        return Ok(());
                    }
                    if is_typed_record_join(&records) {
                        return Err(undetermined("record branches have unknown additional values at a named field"));
                    }
                }
            }
        }
        // A runtime mismatch fails the whole call and its bindings are dropped with it, so the binding is made in place.
        // A static check goes on to the next argument after a mismatch, so it binds a copy and keeps it only on success.
        if mode == Mode::Runtime {
            bindings.index = index;
            bindings.strict = false;
            return bind_type(formal, actual, bindings, mode);
        }
        let mut scratch = bindings.clone();
        scratch.index = index;
        scratch.strict = true;
        bind_type(formal, actual, &mut scratch, mode)?;
        *bindings = scratch;
        Ok(())
    }

    /// A literal object handed to a closed `shape` parameter, checked field by field: every declared field is
    /// present (a field whose type allows null may hold null), each value is of its field's type, and no field is
    /// undeclared. `None` when the parameter is not a closed shape; the caller binds the argument as a whole then.
    pub fn bind_shape_literal(
        &self,
        bindings: &mut CallBindings,
        index: usize,
        fields: &[(String, Actual<'_>)],
        mode: Mode,
    ) -> Option<Result<(), Mismatch>> {
        use crate::language::registry::OperatorTypeRefKind as K;
        let OperatorTypeRef::Tagged(K::Object {
            fields: declared,
            open,
            optional,
        }) = self.formal(index)?
        else {
            return None;
        };
        if *open == Some(true) || declared.is_empty() {
            return None;
        }
        let mut names: Vec<&String> = declared.keys().collect();
        names.sort();
        let mut scratch = bindings.clone();
        scratch.index = index;
        scratch.strict = mode != Mode::Runtime;
        for name in &names {
            let Some((_, actual)) = fields.iter().find(|(given, _)| given == *name) else {
                if optional.contains(name) {
                    continue;
                }
                return Some(Err(Mismatch::Type {
                    expected: format!(
                        "an object with `{name}` ({})",
                        expected_name(&declared[*name])
                    ),
                    actual: format!("an object where `{name}` is missing"),
                }));
            };
            if let Err(found) = bind_type(&declared[*name], actual, &mut scratch, mode) {
                return Some(Err(match found {
                    Mismatch::Type { expected, actual } => Mismatch::Type {
                        expected: format!("`{name}`: {expected}"),
                        actual,
                    },
                    other => other,
                }));
            }
        }
        if let Some((extra, _)) = fields
            .iter()
            .find(|(given, _)| !declared.contains_key(given))
        {
            let allowed: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
            return Some(Err(Mismatch::Type {
                expected: format!("an object of only {}", allowed.join(", ")),
                actual: format!("an object with `{extra}`, which the shape does not declare"),
            }));
        }
        *bindings = scratch;
        Some(Ok(()))
    }

    /// Checks what only the whole call decides, once `supplied` arguments are bound: a `key-of` argument that
    /// names no field of its record is an error when the call reads that field and supplies no default.
    pub fn finish(
        &self,
        bindings: &CallBindings,
        supplied: usize,
    ) -> Result<(), (usize, Mismatch)> {
        if bindings.strict {
            let untyped = |var: &String| {
                bindings.empty_lists.contains(var) || !bindings.vars.contains_key(var)
            };
            if bindings
                .empty_lists
                .iter()
                .chain(&bindings.empty_elements)
                .any(untyped)
            {
                return Err((bindings.index, undetermined(EMPTY_LIST)));
            }
        }
        let reads = |var: &str| matches!(self.returns, OperatorTypeRef::Tagged(OperatorTypeRefKind::FieldOf { of }) if of == var);
        for (var, (index, path)) in &bindings.absent {
            if !reads(var) {
                continue;
            }
            let default_supplied = self.params.iter().enumerate().any(|(i, p)| {
                i < supplied
                    && matches!(&p.ty, OperatorTypeRef::Tagged(OperatorTypeRefKind::FieldOf { of }) if of == var)
            });
            if !default_supplied {
                let record = bindings
                    .last(var)
                    .map(oir_type_kind)
                    .unwrap_or_else(|| "object".to_string());
                return Err((
                    *index,
                    Mismatch::NoField {
                        path: path.clone(),
                        record,
                    },
                ));
            }
        }
        Ok(())
    }

    /// The concrete type of the call's result under `bindings`, or `None` when it is not determined (a
    /// variable nothing bound, `void`, a function).
    pub fn result(&self, bindings: &CallBindings) -> Option<OirType> {
        resolve(self.returns, bindings)
    }

    /// The result with the declared null alternative left out (`T | null` read as the `T` the bindings give),
    /// for a call that cannot answer null; `None` when the declared result names no null alternative.
    pub fn result_without_null(&self, bindings: &CallBindings) -> Option<OirType> {
        let OperatorTypeRef::Tagged(OperatorTypeRefKind::Union { of }) = self.returns else {
            return None;
        };
        let mut members = of.iter().filter(
            |m| !matches!(m, OperatorTypeRef::Tagged(OperatorTypeRefKind::Literal { value }) if value.is_null()),
        );
        let only = members.next()?;
        if members.next().is_some() || of.len() < 2 {
            return None;
        }
        resolve(only, bindings)
    }

    pub fn result_with_options(
        &self,
        bindings: &CallBindings,
        option: impl Fn(usize, &str) -> Option<bool>,
    ) -> Option<OirType> {
        let Some((index, field)) = self
            .semantics
            .and_then(|sem| sem.strip_prefix("option-branch<"))
            .and_then(|sem| sem.strip_suffix('>'))
            .and_then(|sem| sem.split_once(','))
        else {
            return self.result(bindings);
        };
        let Ok(index) = index.parse::<usize>() else {
            return self.result(bindings);
        };
        let Some(selected) = option(index, field) else {
            return self.result(bindings);
        };
        let OperatorTypeRef::Tagged(OperatorTypeRefKind::Union { of }) = self.returns else {
            return self.result(bindings);
        };
        of.get(usize::from(selected))
            .and_then(|ty| resolve(ty, bindings))
    }

    /// Whether the operator is declared to return `void` (it is run for its effect).
    pub fn returns_void(&self) -> bool {
        matches!(self.returns, OperatorTypeRef::Primitive(name) if name == "void")
    }

    /// Whether the call's result is void under `bindings`: a result variable whose latest binding is an effect.
    pub fn result_is_void(&self, bindings: &CallBindings) -> bool {
        match self.returns {
            OperatorTypeRef::Tagged(OperatorTypeRefKind::Var { name, .. }) => {
                bindings.voided.contains(name)
            }
            other => matches!(other, OperatorTypeRef::Primitive(name) if name == "void"),
        }
    }
}

mod records;
mod paths;
mod describe;
mod shapes;
mod bind;
mod resolve;
use records::*;
use paths::*;
use describe::*;
use shapes::*;
use bind::*;
pub use records::concrete_type;
pub use paths::{project_children_type, type_at_path};
pub use describe::describe_type;
pub use bind::nominal_type;
pub use resolve::resolve;

#[cfg(test)]
mod tests;
