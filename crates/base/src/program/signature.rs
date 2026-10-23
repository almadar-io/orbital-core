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
//! field of the record `R` is bound to. An argument handed to a type variable whose type is not known — an unresolved expression, an expression value, json — is
//! an error ([`Mismatch::Undetermined`]): a variable never stands for an unknown. A static argument at a concrete
//! position binds nothing and is not checked.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ir::types::{OirField, OirType};
use crate::language::registry::{
    EmbeddedOperator, EmbeddedOperatorParam, OperatorTypeRef, OperatorTypeRefKind,
};
use crate::type_compat::{oir_type_kind, sub};

/// The slot of a run-time tuple whose value is null: it is absent, so it passes its declared type and binds nothing.
pub fn absent_slot() -> OirType {
    OirType::Optional {
        inner: Box::new(OirType::Json),
    }
}

fn is_absent_slot(ty: &OirType) -> bool {
    matches!(ty, OirType::Optional { inner } if matches!(**inner, OirType::Json))
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
    /// Over the types of expressions: a `key-of` argument must name a field of the record it is bound to.
    Static,
    /// Over the types of evaluated values, which carry no field information: `key-of` is not checked.
    Runtime,
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
        }
    }
}

/// The variables a call has bound so far.
#[derive(Debug, Default, Clone)]
pub struct CallBindings {
    vars: BTreeMap<String, Vec<OirType>>,
    keys: BTreeMap<String, Vec<String>>,
    /// The key arguments that name no field of the record their variable is bound to: the argument position and
    /// the path. Reading such a field is an error only when the call supplies no default (see [`Signature::finish`]).
    absent: BTreeMap<String, (usize, String)>,
    /// The type of the default supplied for a `field-of` parameter whose field the record does not declare.
    defaults: BTreeMap<String, OirType>,
    /// The variables the null value was passed to: they may hold null.
    nullable: BTreeSet<String>,
    /// The variables whose latest binding is an effect (it produces no value).
    voided: BTreeSet<String>,
    /// The variables holding only the empty list so far: a list with no element, whose type some sibling binding
    /// or the slot it fills must supply.
    empty_lists: BTreeSet<String>,
    /// The element variables of an empty list handed to a list position: nothing in the call has typed them yet.
    empty_elements: BTreeSet<String>,
    /// Whether the call is checked over expression types (an empty list nothing types is then an error).
    strict: bool,
    index: usize,
    shape: Option<String>,
    named_types: std::sync::Arc<BTreeMap<String, OirType>>,
}

impl CallBindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn use_named_types(&mut self, types: std::sync::Arc<BTreeMap<String, OirType>>) {
        self.named_types = types;
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
    unbounded: bool,
    checks: bool,
    returns: &'a OperatorTypeRef,
    semantics: Option<&'a str>,
}

impl<'a> Signature<'a> {
    pub fn of(op: &'a EmbeddedOperator) -> Self {
        Self {
            params: &op.params,
            unbounded: op.max_arity.is_none(),
            checks: op.params.iter().any(|p| checks(&p.ty)),
            returns: &op.return_type,
            semantics: op.return_semantics.as_deref(),
        }
    }

    /// Whether the operand at `index` raises in the kernel when it is null.
    pub fn rejects_null(&self, index: usize) -> bool {
        match self.params.get(index) {
            Some(param) => param.rejects_null,
            None => self.params.last().is_some_and(|last| {
                (last.name.starts_with("...") || self.unbounded) && last.rejects_null
            }),
        }
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
        scratch.strict = mode == Mode::Static;
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

/// The concrete type a declared name stands for in a result. `void` has none.
fn checks(ty: &OperatorTypeRef) -> bool {
    use crate::language::registry::OperatorTypeRefKind as K;
    match ty {
        OperatorTypeRef::Primitive(name) => checkable(name).is_some(),
        OperatorTypeRef::Tagged(kind) => match kind {
            K::Union { of } => of.iter().any(checks),
            K::Array { .. }
            | K::Tuple { .. }
            | K::Map { .. }
            | K::Object { .. }
            | K::Record
            | K::Var { .. }
            | K::KeyOf { .. }
            | K::Shape => true,
            K::Binding { shape } => shape.as_deref().is_some_and(checks),
            K::Sexpr { of } => of.as_deref().is_some_and(checks),
            K::Literal { .. }
            | K::Fn { .. }
            | K::Entity { .. }
            | K::EntityRef
            | K::EventKey { .. }
            | K::UiSlot
            | K::PatternType
            | K::FieldOf { .. }
            | K::PickOf { .. }
            | K::OmitOf { .. }
            | K::MergeOf { .. }
            | K::ChildrenOf { .. } => false,
        },
    }
}

fn concrete(name: &str) -> Option<OirType> {
    Some(match name {
        "number" => OirType::Number,
        "string" => OirType::String,
        "boolean" => OirType::Boolean,
        "date" => OirType::Date,
        "datetime" => OirType::DateTime,
        "bytes" => OirType::Bytes,
        "callkey" => OirType::CallKey,
        "sexpr" => OirType::SExpr,
        "trait" => OirType::Trait,
        "orbital" => OirType::Orbital,
        "behavior" => OirType::Behavior,
        "void" => return None,
        other => OirType::TypeRef {
            name: other.to_string(),
        },
    })
}

/// The types an argument is checked against: the primitives. Every other declared name (a std struct such as
/// `tensor`, a language value such as `trait`) is nominal and not checked.
fn checkable(name: &str) -> Option<OirType> {
    match name {
        "number" => Some(OirType::Number),
        "string" => Some(OirType::String),
        "boolean" => Some(OirType::Boolean),
        "date" => Some(OirType::Date),
        "datetime" => Some(OirType::DateTime),
        _ => None,
    }
}

fn unwrap_optional(ty: &OirType) -> &OirType {
    match ty {
        OirType::Optional { inner } => unwrap_optional(inner),
        other => other,
    }
}

/// A type whose value says nothing the checker can use.
fn is_uninformative(ty: &OirType) -> bool {
    matches!(unwrap_optional(ty), OirType::Json | OirType::SExpr)
}

enum Record<'t> {
    Fields(&'t [OirField]),
    Uniform(&'t OirType, &'t OirType),
    Opaque,
}

/// The name the installed language gives the record an orbital, trait or behavior value is.
fn language_value_name(ty: &OirType) -> Option<&'static str> {
    match ty {
        OirType::Orbital => Some("OrbitalValue"),
        OirType::Trait => Some("TraitValue"),
        OirType::Behavior => Some("BehaviorRef"),
        _ => None,
    }
}

/// `ty`, or the record the language declares for it when it is an orbital, trait or behavior value.
fn language_value(ty: &OirType) -> &OirType {
    language_value_name(ty)
        .and_then(crate::language::program_types::program_type)
        .unwrap_or(ty)
}

fn language_record(name: &str) -> Record<'static> {
    match crate::language::program_types::program_type(name) {
        Some(OirType::Struct { fields, .. }) => Record::Fields(fields),
        _ => Record::Opaque,
    }
}

fn record_of<'a>(ty: &'a OirType, bindings: &CallBindings) -> Option<Record<'a>> {
    match unwrap_optional(ty) {
        OirType::Struct { fields, .. } => Some(Record::Fields(fields)),
        OirType::Map { key, value } => Some(Record::Uniform(key, value)),
        // An orbital, trait or behavior value is the record the installed language declares for it.
        OirType::Orbital => Some(language_record("OrbitalValue")),
        OirType::Trait => Some(language_record("TraitValue")),
        OirType::Behavior => Some(language_record("BehaviorRef")),
        OirType::Union { variants } => variants
            .iter()
            .all(|variant| record_of(&variant.field_type, bindings).is_some())
            .then_some(Record::Opaque),
        OirType::TypeRef { name } | OirType::Reference { entity: name }
            if bindings.named_types.contains_key(name) =>
        {
            crate::type_compat::sub_with_named(
                ty,
                &OirType::Struct { fields: Vec::new(), additional: None },
                &|name| bindings.named_types.get(name).cloned(),
            )
            .map(|_| Record::Opaque)
        }
        OirType::Row { .. }
        | OirType::TypeRef { .. }
        | OirType::File
        | OirType::Reference { .. } => Some(Record::Opaque),
        _ => None,
    }
}

enum Path {
    Found(OirType),
    Opaque,
    Missing,
}

/// The type at a dot path through `ty`.
fn walk_path(ty: &OirType, path: &str) -> Path {
    let mut current = ty.clone();
    let mut may_be_absent = false;
    let segments = crate::value::split_path(path);
    for segment in segments.iter().map(|s| s.as_ref()) {
        let here = language_value(unwrap_optional(&current)).clone();
        match &here {
            OirType::Struct { fields, additional } => match fields.iter().find(|f| f.name == segment) {
                Some(field) => {
                    current = field.field_type.clone();
                    may_be_absent |= !field.required;
                }
                None => match additional.as_deref() {
                    Some(value) => {
                        current = value.clone();
                        may_be_absent = true;
                    }
                    None => return Path::Missing,
                },
            },
            OirType::Map { value, .. } => current = (**value).clone(),
            OirType::Array { inner } if segment.parse::<usize>().is_ok() => {
                current = (**inner).clone();
            }
            OirType::Tuple { items } => {
                match segment.parse::<usize>().ok().and_then(|i| items.get(i)) {
                    Some(item) => current = item.clone(),
                    None => return Path::Missing,
                }
            }
            // A tagged union's member read: the field of every variant that declares it. One type when they
            // agree; otherwise the union of the distinct types, so a read as one of them needs a guard.
            OirType::Union { variants } => {
                let declared: Vec<(&str, &OirType)> = variants
                    .iter()
                    .filter_map(|v| match &v.field_type {
                        OirType::Struct { fields, .. } => fields
                            .iter()
                            .find(|f| f.name == segment)
                            .map(|f| (v.name.as_str(), &f.field_type)),
                        _ => None,
                    })
                    .collect();
                let Some(((_, first), rest)) = declared.split_first() else {
                    return Path::Missing;
                };
                let joined = if rest.iter().all(|(_, t)| agrees(first, t)) {
                    (*first).clone()
                } else {
                    let mut distinct: Vec<OirField> = Vec::new();
                    for (name, ty) in &declared {
                        if !distinct.iter().any(|d| &d.field_type == *ty) {
                            distinct.push(OirField::new((*name).to_string(), (*ty).clone()));
                        }
                    }
                    OirType::Union { variants: distinct }
                };
                current = if declared.len() == variants.len() {
                    joined
                } else {
                    OirType::optional(joined)
                };
            }
            OirType::Row { .. }
            | OirType::TypeRef { .. }
            | OirType::File
            | OirType::Orbital
            | OirType::Behavior
            | OirType::Reference { .. }
            | OirType::Json => return Path::Opaque,
            _ => return Path::Missing,
        }
    }
    Path::Found(if may_be_absent && !current.is_optional() {
        OirType::optional(current)
    } else {
        current
    })
}

fn expected_name(formal: &OperatorTypeRef) -> String {
    match formal {
        OperatorTypeRef::Primitive(name) => name.clone(),
        OperatorTypeRef::Tagged(kind) => match kind {
            OperatorTypeRefKind::Union { of } => {
                let mut names: Vec<String> = Vec::new();
                for member in of {
                    if matches!(
                        member,
                        OperatorTypeRef::Tagged(OperatorTypeRefKind::Literal { value }) if value.is_null()
                    ) {
                        continue;
                    }
                    let name = expected_name(member);
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
                names.join(" | ")
            }
            OperatorTypeRefKind::Var {
                bound: Some(bound), ..
            } => expected_name(bound),
            OperatorTypeRefKind::Var { name, .. } => name.clone(),
            OperatorTypeRefKind::Record => "object".to_string(),
            _ => formal.kind_name().to_string(),
        },
    }
}

fn kind_of(known: &Option<Known>) -> String {
    match known {
        Some(Known::Type(ty)) => oir_type_kind(ty),
        Some(Known::EmptyList) => "array".to_string(),
        Some(Known::Absent) => "null".to_string(),
        Some(Known::Void) => "void".to_string(),
        None => "null".to_string(),
    }
}

fn type_mismatch(formal: &OperatorTypeRef, actual: &Actual<'_>) -> Mismatch {
    Mismatch::Type {
        expected: expected_name(formal),
        actual: kind_of(&actual.known),
    }
}

fn agrees(a: &OirType, b: &OirType) -> bool {
    sub(a, b).is_some() || sub(b, a).is_some()
}

fn fits(actual: &OirType, expected: &OirType, bindings: &CallBindings) -> bool {
    // A date is a string or a number at rest, depending on the host; a static date passes either.
    let date_at_rest = matches!(unwrap_optional(actual), OirType::Date | OirType::DateTime)
        && matches!(expected, OirType::Number | OirType::String);
    // A semantic string (email, url, ...) is a string, and a reference to another row is its id string.
    let text_at_rest = matches!(
        unwrap_optional(actual),
        OirType::Email
            | OirType::Url
            | OirType::Phone
            | OirType::Uuid
            | OirType::Image
            | OirType::Reference { .. }
    ) && matches!(expected, OirType::String);
    date_at_rest
        || text_at_rest
        || is_uninformative(actual)
        || crate::type_compat::sub_with_named(actual, expected, &|name| {
            bindings.named_types.get(name).cloned()
        })
        .is_some()
}

/// Whether `actual` is a record (a struct, a map, a row).
fn is_record(known: &Known, bindings: &CallBindings) -> bool {
    match known {
        Known::Type(ty) => record_of(ty, bindings).is_some() || is_uninformative(ty),
        Known::EmptyList | Known::Absent | Known::Void => false,
    }
}

/// Whether `formal` binds a type variable to the value handed to it.
fn binds_variable(formal: &OperatorTypeRef) -> bool {
    use crate::language::registry::OperatorTypeRefKind as K;
    let OperatorTypeRef::Tagged(kind) = formal else {
        return false;
    };
    match kind {
        K::Var { .. } => true,
        K::Array { of } => binds_variable(of),
        K::Tuple { items } => items.iter().any(binds_variable),
        K::Map { key, value } => binds_variable(key) || binds_variable(value),
        K::Union { of } => of.iter().any(binds_variable),
        K::Object { fields, .. } => fields.values().any(binds_variable),
        K::Binding { shape } => shape.as_deref().is_some_and(binds_variable),
        K::Sexpr { of } => of.as_deref().is_some_and(binds_variable),
        _ => false,
    }
}

/// The positions a list of tuples of one length shares, an absent slot where they differ; `None` unless every
/// entry is a tuple of the same non-zero length.
fn shared_tuple(entries: &[OirType]) -> Option<Vec<OirType>> {
    let mut merged: Option<Vec<OirType>> = None;
    for entry in entries {
        let OirType::Tuple { items } = unwrap_optional(entry) else {
            return None;
        };
        match &mut merged {
            None if items.is_empty() => return None,
            None => merged = Some(items.clone()),
            Some(kept) if kept.len() == items.len() => {
                for (slot, other) in kept.iter_mut().zip(items) {
                    if slot != other {
                        *slot = absent_slot();
                    }
                }
            }
            Some(_) => return None,
        }
    }
    merged
}

/// The one type two list-shaped bindings of a variable share: tuples of one length are a tuple of the positions
/// they share (an absent slot where they differ); a tuple whose items all share a type is a list of it, beside a
/// list or a tuple of another length. `None` when either is not list-shaped or the shapes do not meet.
fn merge_list_shapes(a: &OirType, b: &OirType) -> Option<OirType> {
    let (a, b) = (unwrap_optional(a), unwrap_optional(b));
    if is_empty_list(a) && matches!(b, OirType::Array { .. } | OirType::Tuple { .. }) {
        return Some(b.clone());
    }
    if is_empty_list(b) && matches!(a, OirType::Array { .. } | OirType::Tuple { .. }) {
        return Some(a.clone());
    }
    if let Some(shared) = shared_tuple(&[a.clone(), b.clone()]) {
        return Some(OirType::Tuple { items: shared });
    }
    let element_of = |ty: &OirType| -> Option<OirType> {
        match ty {
            OirType::Array { inner } => Some((**inner).clone()),
            OirType::Tuple { items } => {
                let mut present = items.iter().filter(|t| !is_absent_slot(t));
                let first = present.next()?;
                present.all(|t| t == first).then(|| first.clone())
            }
            _ => None,
        }
    };
    if let (Some(x), Some(y)) = (element_of(a), element_of(b)) {
        if agrees(&x, &y) {
            return Some(OirType::array(x));
        }
    }
    joined_tuples(&[a.clone(), b.clone()]).map(|items| OirType::Tuple { items })
}

fn fill_empty_list_slots(prior: &OirType, actual: &OirType) -> Option<OirType> {
    if is_empty_list(prior)
        && matches!(
            unwrap_optional(actual),
            OirType::Array { .. } | OirType::Tuple { .. }
        )
    {
        return (!is_empty_list(actual)).then(|| actual.clone());
    }
    match (prior, actual) {
        (
            OirType::Struct { fields, additional },
            OirType::Struct { fields: supplied, additional: next_additional },
        ) => {
            let mut fields = fields.clone();
            let mut additional = additional.clone();
            let mut changed = false;
            for field in &mut fields {
                if let Some(next) = supplied
                    .iter()
                    .find(|next| next.name == field.name)
                    .and_then(|next| fill_empty_list_slots(&field.field_type, &next.field_type))
                {
                    field.field_type = next;
                    changed = true;
                }
            }
            if let (Some(prior), Some(next)) = (&mut additional, next_additional) {
                if let Some(filled) = fill_empty_list_slots(prior, next) {
                    *prior = Box::new(filled);
                    changed = true;
                }
            }
            changed.then_some(OirType::Struct { fields, additional })
        }
        (OirType::Array { inner }, OirType::Array { inner: next }) => {
            fill_empty_list_slots(inner, next).map(OirType::array)
        }
        (OirType::Optional { inner }, next) => {
            fill_empty_list_slots(inner, unwrap_optional(next)).map(OirType::optional)
        }
        (OirType::Tuple { items }, OirType::Tuple { items: next }) if items.len() == next.len() => {
            let mut changed = false;
            let items = items
                .iter()
                .zip(next)
                .map(
                    |(prior, actual)| match fill_empty_list_slots(prior, actual) {
                        Some(next) => {
                            changed = true;
                            next
                        }
                        None => prior.clone(),
                    },
                )
                .collect();
            changed.then_some(OirType::Tuple { items })
        }
        _ => None,
    }
}

/// The positions of tuples of any lengths, an absent slot where they differ or one is shorter: the op tuples of a
/// `persist batch` (`create` 3 wide, `update` 4) are one list. A list of one type is a tuple of it (`["delete" "Row"
/// id]` is `[string]`), and joins when a tuple has a slot of that type. `None` unless one entry is a tuple that mixes
/// types, so lists of unlike scalars stay two types whatever their lengths.
fn joined_tuples(entries: &[OirType]) -> Option<Vec<OirType>> {
    let mut tuples: Vec<Vec<OirType>> = Vec::new();
    let mut lists: Vec<OirType> = Vec::new();
    for entry in entries {
        match unwrap_optional(entry) {
            OirType::Tuple { items } if !items.is_empty() => tuples.push(items.clone()),
            OirType::Array { inner } => lists.push((**inner).clone()),
            _ => return None,
        }
    }
    let mixes_types = tuples.iter().any(|items| {
        let mut present = items.iter().filter(|t| !is_absent_slot(t));
        present
            .next()
            .is_some_and(|first| present.any(|t| t != first))
    });
    if !mixes_types {
        return None;
    }
    let width = tuples.iter().map(|items| items.len()).max()?;
    for element in lists {
        if !tuples.iter().any(|items| items.contains(&element)) {
            return None;
        }
        tuples.push(vec![element; width]);
    }
    Some(
        (0..width)
            .map(|slot| {
                let mut at = tuples.iter().map(|items| items.get(slot));
                match at.next().flatten() {
                    Some(first) if at.all(|other| other == Some(first)) => first.clone(),
                    _ => absent_slot(),
                }
            })
            .collect(),
    )
}

/// Whether `ty` is the type of the empty list literal: a list with no slot.
fn is_empty_list(ty: &OirType) -> bool {
    matches!(unwrap_optional(ty), OirType::Tuple { items } if items.is_empty())
}

/// The type variables `formal` names.
fn variables_in(formal: &OperatorTypeRef, out: &mut Vec<String>) {
    use crate::language::registry::OperatorTypeRefKind as K;
    let OperatorTypeRef::Tagged(kind) = formal else {
        return;
    };
    match kind {
        K::Var { name, .. } => out.push(name.clone()),
        K::Array { of } => variables_in(of, out),
        K::Tuple { items } => items.iter().for_each(|i| variables_in(i, out)),
        K::Map { key, value } => {
            variables_in(key, out);
            variables_in(value, out);
        }
        K::Union { of } => of.iter().for_each(|m| variables_in(m, out)),
        K::Object { fields, .. } => fields.values().for_each(|f| variables_in(f, out)),
        K::Binding { shape } => shape
            .as_deref()
            .into_iter()
            .for_each(|s| variables_in(s, out)),
        K::Sexpr { of } => of.as_deref().into_iter().for_each(|s| variables_in(s, out)),
        _ => {}
    }
}

fn undetermined(reason: &str) -> Mismatch {
    Mismatch::Undetermined {
        reason: reason.to_string(),
    }
}

const NO_TYPE: &str = "its type is not determined";
const IS_EXPRESSION: &str = "it is an expression value, which has no type of its own";
const IS_JSON: &str = "it is json, which has no type";
const NO_ELEMENT: &str = "its items do not share one type";
const EMPTY_LIST: &str = "an empty list takes its element type from the slot it fills or a sibling argument, and nothing in this call types it";

fn why_uninformative(ty: &OirType) -> &'static str {
    match unwrap_optional(ty) {
        OirType::SExpr => IS_EXPRESSION,
        _ => IS_JSON,
    }
}

fn bind_type(
    formal: &OperatorTypeRef,
    actual: &Actual<'_>,
    b: &mut CallBindings,
    mode: Mode,
) -> Result<(), Mismatch> {
    use crate::language::registry::OperatorTypeRefKind as K;
    // An absent value passes every declared type (EXEC-EVAL-2).
    let Some(known) = &actual.known else {
        return bind_unknown(formal, actual, b, mode);
    };
    if matches!(known, Known::Absent) {
        note_absent(formal, b);
        return Ok(());
    }
    if matches!(known, Known::Void) {
        return bind_void(formal, actual, b);
    }
    match formal {
        OperatorTypeRef::Primitive(name) => {
            let Some(expected) = checkable(name) else {
                return Ok(());
            };
            match known {
                Known::Type(ty) if fits(ty, &expected, b) => Ok(()),
                _ => Err(type_mismatch(formal, actual)),
            }
        }
        OperatorTypeRef::Tagged(kind) => match kind {
            K::Union { of } => {
                if let Known::Type(OirType::Union { variants }) = known {
                    if !variants.is_empty() {
                        let mut trial = b.clone();
                        for variant in variants {
                            bind_type(formal, &element(&variant.field_type), &mut trial, mode)?;
                        }
                        *b = trial;
                        return Ok(());
                    }
                }
                let mut undetermined = None;
                for member in of {
                    let mut trial = b.clone();
                    match bind_type(member, actual, &mut trial, mode) {
                        Ok(()) => {
                            *b = trial;
                            return Ok(());
                        }
                        Err(found @ Mismatch::Undetermined { .. }) => {
                            undetermined.get_or_insert(found);
                        }
                        Err(_) => {}
                    }
                }
                // A member that fits in kind but whose type variables cannot be bound is the finding, not "no
                // member fits".
                Err(undetermined.unwrap_or_else(|| type_mismatch(formal, actual)))
            }
            K::Literal { value } => match (value, known) {
                (serde_json::Value::Null, _) => Err(type_mismatch(formal, actual)),
                // A literal string fits only itself; a string whose text is not known here could be it.
                (serde_json::Value::String(want), Known::Type(ty))
                    if matches!(unwrap_optional(ty), OirType::String) =>
                {
                    match &actual.text {
                        Some(text) if text != want => Err(type_mismatch(formal, actual)),
                        _ => Ok(()),
                    }
                }
                (serde_json::Value::String(_), Known::Type(ty)) if !is_uninformative(ty) => {
                    Err(type_mismatch(formal, actual))
                }
                (serde_json::Value::Bool(_), Known::Type(ty))
                    if !is_uninformative(ty)
                        && !matches!(unwrap_optional(ty), OirType::Boolean) =>
                {
                    Err(type_mismatch(formal, actual))
                }
                (serde_json::Value::Number(_), Known::Type(ty))
                    if !is_uninformative(ty) && !matches!(unwrap_optional(ty), OirType::Number) =>
                {
                    Err(type_mismatch(formal, actual))
                }
                _ => Ok(()),
            },
            K::Array { of } => match known {
                Known::Absent | Known::Void => Ok(()),
                Known::EmptyList => {
                    b.note_empty_element(of);
                    Ok(())
                }
                Known::Type(ty) => match unwrap_optional(ty) {
                    OirType::Array { inner } => {
                        bind_type(of, &element(inner), b, mode)?;
                        bind_literal_keys(of, actual, b, mode)
                    }
                    // A list position that binds each item afresh (`[E each]`) takes a tuple of unlike types item by item.
                    OirType::Tuple { items }
                        if !items.is_empty()
                            && matches!(
                                &**of,
                                OperatorTypeRef::Tagged(K::Var { each: true, .. })
                            ) =>
                    {
                        for item in items {
                            bind_type(of, &element(item), b, mode)?;
                        }
                        Ok(())
                    }
                    // A tuple of one type is a list of it; one of many types has no element type to bind.
                    OirType::Tuple { items } => match items.split_first() {
                        Some((first, rest)) if rest.iter().all(|t| b.agrees(first, t)) => {
                            bind_type(of, &element(first), b, mode)?;
                            bind_literal_keys(of, actual, b, mode)
                        }
                        // Lists of one length, one of them a tuple, are one element type: a tuple is heterogeneous by
                        // design, so a list of unlike tuples is a list.
                        Some(_) if shared_tuple(items).is_some() => {
                            let merged = shared_tuple(items).unwrap_or_default();
                            bind_type(of, &element(&OirType::Tuple { items: merged }), b, mode)?;
                            bind_literal_keys(of, actual, b, mode)
                        }
                        Some(_) if joined_tuples(items).is_some() => {
                            let merged = joined_tuples(items).unwrap_or_default();
                            bind_type(of, &element(&OirType::Tuple { items: merged }), b, mode)?;
                            bind_literal_keys(of, actual, b, mode)
                        }
                        // The empty list has no element to bind.
                        None => {
                            b.note_empty_element(of);
                            Ok(())
                        }
                        _ => Err(undetermined(NO_ELEMENT)),
                    },
                    other if is_uninformative(other) => Err(undetermined(why_uninformative(other))),
                    _ => Err(type_mismatch(formal, actual)),
                },
            },
            K::Tuple { items } => match known {
                Known::EmptyList | Known::Absent | Known::Void => Ok(()),
                Known::Type(ty) => match unwrap_optional(ty) {
                    OirType::Tuple { items: slots } if slots.is_empty() => Ok(()),
                    OirType::Tuple { items: slots } if slots.len() == items.len() => {
                        for (slot_formal, slot) in items.iter().zip(slots) {
                            if is_absent_slot(slot) {
                                continue;
                            }
                            bind_type(slot_formal, &element(slot), b, mode)?;
                        }
                        Ok(())
                    }
                    OirType::Array { inner } => {
                        for slot_formal in items {
                            bind_type(slot_formal, &element(inner), b, mode)?;
                        }
                        Ok(())
                    }
                    other if is_uninformative(other) => Err(undetermined(why_uninformative(other))),
                    _ => Err(type_mismatch(formal, actual)),
                },
            },
            K::Map { key, value } => match known {
                Known::Type(ty) => match record_of(ty, b) {
                    Some(Record::Uniform(k, v)) => {
                        bind_type(key, &element(k), b, mode)?;
                        bind_type(value, &element(v), b, mode)
                    }
                    Some(Record::Fields(fields)) => {
                        bind_type(key, &element(&OirType::String), b, mode)?;
                        for field in fields {
                            bind_type(value, &element(&field.field_type), b, mode)?;
                        }
                        Ok(())
                    }
                    Some(Record::Opaque) => Err(undetermined(
                        "it is a record whose fields the call cannot see",
                    )),
                    None if is_uninformative(ty) => Err(undetermined(why_uninformative(ty))),
                    None => Err(type_mismatch(formal, actual)),
                },
                Known::EmptyList | Known::Absent | Known::Void => {
                    Err(type_mismatch(formal, actual))
                }
            },
            K::Object { fields, .. } => match known {
                Known::Type(ty) => match record_of(ty, b) {
                    Some(Record::Fields(have)) => {
                        for (name, declared) in fields {
                            if let Some(field) = have.iter().find(|f| &f.name == name) {
                                bind_type(declared, &element(&field.field_type), b, mode)?;
                            }
                        }
                        Ok(())
                    }
                    Some(_) => Ok(()),
                    None if is_uninformative(ty) => Ok(()),
                    None => Err(type_mismatch(formal, actual)),
                },
                Known::EmptyList | Known::Absent | Known::Void => {
                    Err(type_mismatch(formal, actual))
                }
            },
            K::Record => {
                if is_record(known, b) {
                    Ok(())
                } else {
                    Err(type_mismatch(formal, actual))
                }
            }
            K::Var { name, bound, each } => {
                if let Some(bound) = bound {
                    bind_bound(bound, known, formal, actual, b)?;
                }
                let empty_list = OirType::Tuple { items: Vec::new() };
                let ty = match known {
                    Known::Type(ty) => ty,
                    Known::EmptyList => &empty_list,
                    _ => return Ok(()),
                };
                // A held program is a type a free variable binds (a list of them, the one a lookup returns); a variable
                // bounded to a readable kind never takes one, because nothing can be read out of it.
                let holds_program =
                    matches!(unwrap_optional(ty), OirType::SExpr) && bound.is_none();
                if is_uninformative(ty) && !holds_program {
                    return Err(undetermined(why_uninformative(ty)));
                }
                let is_empty = is_empty_list(ty);
                if *each {
                    if is_empty {
                        if !b.vars.contains_key(name) {
                            b.empty_lists.insert(name.clone());
                        }
                    } else {
                        b.empty_lists.remove(name);
                    }
                    b.vars.entry(name.clone()).or_default().push(ty.clone());
                    b.voided.remove(name);
                    return Ok(());
                }
                if b.voided.contains(name) {
                    return Err(Mismatch::Conflict {
                        expected: "void".to_string(),
                        actual: oir_type_kind(ty),
                    });
                }
                // Two lists that differ in the type of an item are still lists of one shape: pairs of unlike types are
                // pairs, and a tuple whose items share a type is a list of it.
                if let Some(prior) = b.bound(name) {
                    if !b.agrees(prior, ty) {
                        if let Some(merged) = merge_list_shapes(prior, ty) {
                            b.vars.insert(name.clone(), vec![merged]);
                            return Ok(());
                        }
                    }
                }
                match b.bound(name) {
                    Some(prior) if !b.agrees(prior, ty) => Err(Mismatch::Conflict {
                        expected: oir_type_kind(prior),
                        actual: oir_type_kind(ty),
                    }),
                    // A list that turns out to have elements types the empty list a variable first held.
                    Some(prior) if is_empty_list(prior) && !is_empty => {
                        b.vars.insert(name.clone(), vec![ty.clone()]);
                        b.empty_lists.remove(name);
                        Ok(())
                    }
                    Some(prior) => {
                        if let Some(filled) = fill_empty_list_slots(prior, ty) {
                            b.vars.insert(name.clone(), vec![filled]);
                        }
                        Ok(())
                    }
                    None => {
                        b.vars.insert(name.clone(), vec![ty.clone()]);
                        if is_empty {
                            b.empty_lists.insert(name.clone());
                        }
                        Ok(())
                    }
                }
            }
            K::Fn { .. }
            | K::Entity { .. }
            | K::EntityRef
            | K::EventKey { .. }
            | K::UiSlot
            | K::PatternType => Ok(()),
            K::Binding { shape } => match shape {
                Some(shape) => bind_type(shape, actual, b, mode),
                None => Ok(()),
            },
            K::Sexpr { of } => match of {
                Some(of) => bind_type(of, actual, b, mode),
                None => Ok(()),
            },
            K::KeyOf { of } => bind_key(of, actual, formal, b, mode),
            K::FieldOf { of } => {
                if b.absent.contains_key(of) {
                    match known {
                        Known::Type(ty) => {
                            b.defaults.insert(of.clone(), ty.clone());
                        }
                        // The read is the empty list: nothing types it but what it is handed to.
                        Known::EmptyList => {
                            b.defaults
                                .insert(of.clone(), OirType::Tuple { items: Vec::new() });
                        }
                        Known::Absent | Known::Void => {}
                    }
                    return Ok(());
                }
                if let (Some(declared), Known::Type(ty)) = (resolve(formal, b), known) {
                    if !is_uninformative(ty)
                        && !is_uninformative(&declared)
                        && !fits(ty, &declared, b)
                    {
                        return Err(Mismatch::Type {
                            expected: oir_type_kind(&declared),
                            actual: oir_type_kind(ty),
                        });
                    }
                }
                Ok(())
            }
            K::PickOf { .. } | K::OmitOf { .. } | K::MergeOf { .. } | K::ChildrenOf { .. } => {
                Ok(())
            }
            K::Shape => match (&actual.text, mode) {
                (Some(text), _) => {
                    b.shape = Some(crate::ir::types::DeclaredShape::name_of(text).into_owned());
                    Ok(())
                }
                (None, Mode::Runtime) => match known {
                    Known::Type(OirType::String) => Ok(()),
                    _ => Err(Mismatch::Type {
                        expected: "string".to_string(),
                        actual: kind_of(&actual.known),
                    }),
                },
                (None, Mode::Static) => Err(Mismatch::Type {
                    expected: "a type name".to_string(),
                    actual: kind_of(&actual.known),
                }),
            },
        },
    }
}

/// The checks that need no value: a lambda's result, a literal key, a declared type name.
fn bind_unknown(
    formal: &OperatorTypeRef,
    actual: &Actual<'_>,
    b: &mut CallBindings,
    mode: Mode,
) -> Result<(), Mismatch> {
    use crate::language::registry::OperatorTypeRefKind as K;
    let OperatorTypeRef::Tagged(kind) = formal else {
        return Ok(());
    };
    // At run time a value always has a kind (null is absent and passes); only a static argument can have no type.
    let valueless = actual.lambda.is_none() && actual.text.is_none() && actual.texts.is_none();
    if mode == Mode::Static && valueless && binds_variable(formal) {
        return Err(undetermined(NO_TYPE));
    }
    match kind {
        K::Fn { params, returns } => {
            let Some(lambda) = actual.lambda else {
                return Ok(());
            };
            let inputs: Vec<Option<OirType>> = params.iter().map(|p| resolve(p, b)).collect();
            let first = lambda(&inputs);
            if let Some(result) = &first {
                bind_type(returns, &Actual::of_type(result.clone()), b, mode)?;
            }
            if !params
                .iter()
                .any(|p| matches!(p, OperatorTypeRef::Tagged(K::ChildrenOf { .. })))
            {
                return Ok(());
            }
            // The children's results are the step's own result: read with them typed so, it must give that type.
            let typed: Vec<Option<OirType>> = params.iter().map(|p| resolve(p, b)).collect();
            match (first, lambda(&typed)) {
                (Some(before), Some(after)) if !b.agrees(&before, &after) => Err(Mismatch::Type {
                    expected: format!(
                        "a step whose result is the type of its childResults entries ({})",
                        oir_type_kind(&before)
                    ),
                    actual: format!(
                        "a step returning {} when its childResults entries are {}",
                        oir_type_kind(&after),
                        oir_type_kind(&before)
                    ),
                }),
                (None, Some(after)) => bind_type(returns, &Actual::of_type(after), b, mode),
                _ => Ok(()),
            }
        }
        K::Union { of } => {
            for member in of {
                let mut trial = b.clone();
                if bind_unknown(member, actual, &mut trial, mode).is_ok() {
                    *b = trial;
                    return Ok(());
                }
            }
            Ok(())
        }
        K::KeyOf { of } => bind_key(of, actual, formal, b, mode),
        K::Array { of } => {
            if let Some(texts) = &actual.texts {
                for text in texts {
                    let item = Actual {
                        known: None,
                        text: Some(text.clone()),
                        texts: None,
                        lambda: None,
                    };
                    bind_unknown(of, &item, b, mode)?;
                }
            }
            Ok(())
        }
        K::Shape => {
            if let Some(text) = &actual.text {
                b.shape = Some(crate::ir::types::DeclaredShape::name_of(text).into_owned());
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The built-in nominal types a type-name argument can name: a cast to one yields that type, not a by-name struct.
pub fn nominal_type(name: &str) -> Option<OirType> {
    match name {
        "CallKey" => Some(OirType::CallKey),
        _ => None,
    }
}

/// An effect reached `formal`: only a variable or an expression position takes one.
fn bind_void(
    formal: &OperatorTypeRef,
    actual: &Actual<'_>,
    b: &mut CallBindings,
) -> Result<(), Mismatch> {
    use crate::language::registry::OperatorTypeRefKind as K;
    let OperatorTypeRef::Tagged(kind) = formal else {
        return Err(type_mismatch(formal, actual));
    };
    match kind {
        K::Var { name, each, .. } => {
            if !*each && b.vars.contains_key(name) {
                return Err(Mismatch::Conflict {
                    expected: b.bound(name).map(oir_type_kind).unwrap_or_default(),
                    actual: "void".to_string(),
                });
            }
            b.voided.insert(name.clone());
            Ok(())
        }
        K::Sexpr { of: Some(of) } | K::Binding { shape: Some(of) } => bind_void(of, actual, b),
        K::Sexpr { of: None } | K::Binding { shape: None } => Ok(()),
        K::Union { of } => {
            for member in of {
                let mut trial = b.clone();
                if bind_void(member, actual, &mut trial).is_ok() {
                    *b = trial;
                    return Ok(());
                }
            }
            Err(type_mismatch(formal, actual))
        }
        K::Fn { .. }
        | K::Entity { .. }
        | K::EntityRef
        | K::EventKey { .. }
        | K::UiSlot
        | K::PatternType => Ok(()),
        _ => Err(type_mismatch(formal, actual)),
    }
}

/// The null value reached `formal`: every variable it names may hold null.
fn note_absent(formal: &OperatorTypeRef, b: &mut CallBindings) {
    use crate::language::registry::OperatorTypeRefKind as K;
    let OperatorTypeRef::Tagged(kind) = formal else {
        return;
    };
    match kind {
        K::Var { name, .. } => {
            b.nullable.insert(name.clone());
        }
        K::Union { of } => of.iter().for_each(|m| note_absent(m, b)),
        _ => {}
    }
}

fn element(ty: &OirType) -> Actual<'static> {
    Actual::of_type(ty.clone())
}

fn bind_bound(
    bound: &OperatorTypeRef,
    known: &Known,
    formal: &OperatorTypeRef,
    actual: &Actual<'_>,
    bindings: &CallBindings,
) -> Result<(), Mismatch> {
    if let OperatorTypeRef::Tagged(OperatorTypeRefKind::Record) = bound {
        return if is_record(known, bindings) {
            Ok(())
        } else {
            Err(Mismatch::Type {
                expected: "object".to_string(),
                actual: kind_of(&actual.known),
            })
        };
    }
    let mut scratch = CallBindings::new();
    scratch.use_named_types(std::sync::Arc::clone(&bindings.named_types));
    match bind_type(bound, actual, &mut scratch, Mode::Runtime) {
        Ok(()) => Ok(()),
        // A bound is a constraint on the kind: the elements of a list that fits it are the variable's own business.
        Err(Mismatch::Undetermined { .. }) => Ok(()),
        Err(Mismatch::Type { actual, .. }) => Err(Mismatch::Type {
            expected: expected_name(bound),
            actual,
        }),
        Err(other) => {
            let _ = formal;
            Err(other)
        }
    }
}

/// A list of literal strings handed to a list of `key-of R`: each string names a key, whatever type the list
/// itself was also given.
fn bind_literal_keys(
    element_formal: &OperatorTypeRef,
    actual: &Actual<'_>,
    b: &mut CallBindings,
    mode: Mode,
) -> Result<(), Mismatch> {
    let OperatorTypeRef::Tagged(OperatorTypeRefKind::KeyOf { of }) = element_formal else {
        return Ok(());
    };
    for text in actual.texts.iter().flatten() {
        let item = Actual {
            known: Some(Known::Type(OirType::String)),
            text: Some(text.clone()),
            texts: None,
            lambda: None,
        };
        bind_key(of, &item, element_formal, b, mode)?;
    }
    Ok(())
}

fn bind_key(
    record_var: &str,
    actual: &Actual<'_>,
    formal: &OperatorTypeRef,
    b: &mut CallBindings,
    mode: Mode,
) -> Result<(), Mismatch> {
    let paths: Vec<&String> = actual
        .text
        .iter()
        .chain(actual.texts.iter().flatten())
        .collect();
    if let Some(Known::Type(ty)) = &actual.known {
        let known_text = !matches!(unwrap_optional(ty), OirType::String | OirType::Enum { .. });
        if known_text && !is_uninformative(ty) && paths.is_empty() {
            return Err(Mismatch::Type {
                expected: "string".to_string(),
                actual: oir_type_kind(ty),
            });
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    let Some(record) = b.last(record_var).cloned() else {
        return Ok(());
    };
    let indexed = matches!(
        unwrap_optional(&record),
        OirType::Array { .. } | OirType::Tuple { .. }
    );
    if record_of(&record, b).is_none() && !indexed && !is_uninformative(&record) {
        // A key over a number or a string (an element variable of `array/sort`) is only valid when absent.
        return Err(Mismatch::Type {
            expected: "object".to_string(),
            actual: oir_type_kind(&record),
        });
    }
    for path in paths {
        if mode == Mode::Static {
            if let Path::Missing = walk_path(&record, path) {
                b.absent
                    .insert(record_var.to_string(), (b.index, path.clone()));
            }
        }
        b.keys
            .entry(record_var.to_string())
            .or_default()
            .push(path.clone());
    }
    let _ = formal;
    Ok(())
}

fn has_typed_additional(ty: &OirType) -> bool {
    match ty {
        OirType::Struct { fields, additional } => additional.is_some()
            || fields.iter().any(|field| has_typed_additional(&field.field_type)),
        OirType::Array { inner } | OirType::Optional { inner } => has_typed_additional(inner),
        OirType::Map { key, value } => has_typed_additional(key) || has_typed_additional(value),
        OirType::Tuple { items } => items.iter().any(has_typed_additional),
        OirType::Union { variants } => variants.iter().any(|field| has_typed_additional(&field.field_type)),
        _ => false,
    }
}

fn is_typed_record_join(all: &[OirType]) -> bool {
    all.iter().all(|ty| matches!(unwrap_optional(ty), OirType::Struct { .. }))
        && all.iter().any(has_typed_additional)
}

fn join_struct_member_types(types: Vec<OirType>) -> Option<OirType> {
    let first = types.first()?.clone();
    if types.len() == 1 {
        return Some(first);
    }
    let list = types[1..].iter().try_fold(first, |prior, next| match (&prior, next) {
        (OirType::Array { inner: left }, OirType::Array { inner: right }) => {
            let records = [(**left).clone(), (**right).clone()];
            unify_structs(&records).map(OirType::array).or_else(|| {
                (!is_typed_record_join(&records)).then(|| merge_list_shapes(&prior, next)).flatten()
            })
        }
        _ => merge_list_shapes(&prior, next),
    });
    Some(list.unwrap_or_else(|| OirType::Union {
        variants: types.into_iter().enumerate()
            .map(|(index, ty)| OirField::new(index.to_string(), ty)).collect(),
    }))
}

/// One struct carrying the fields of every struct in `all`, a field missing from some optional; `None` unless
/// every member is a struct.
fn unify_structs(all: &[OirType]) -> Option<OirType> {
    let mut merged: Vec<OirField> = Vec::new();
    for ty in all {
        let OirType::Struct { fields, .. } = unwrap_optional(ty) else {
            return None;
        };
        for field in fields {
            if !merged.iter().any(|m| m.name == field.name) {
                merged.push(field.clone());
            }
        }
    }
    if is_typed_record_join(all) && all.iter().any(|ty| match unwrap_optional(ty) {
        OirType::Struct { fields, additional: None } => merged.iter()
            .any(|field| !fields.iter().any(|known| known.name == field.name)),
        _ => false,
    }) {
        return None;
    }
    for m in &mut merged {
        m.required = all.iter().all(|ty| matches!(unwrap_optional(ty), OirType::Struct { fields, .. } if fields.iter().any(|field| field.name == m.name && field.required)));
        let mut types = Vec::new();
        for ty in all {
            if let OirType::Struct { fields, additional } = unwrap_optional(ty) {
                let member = fields.iter().find(|field| field.name == m.name)
                    .map(|field| &field.field_type).or(additional.as_deref());
                if let Some(member) = member {
                    if !types.contains(member) {
                        types.push(member.clone());
                    }
                }
            }
        }
        m.field_type = join_struct_member_types(types)?;
    }
    let mut additional_types = Vec::new();
    let all_additional = all.iter().all(|ty| {
        let OirType::Struct { additional: Some(value), .. } = unwrap_optional(ty) else {
            return false;
        };
        if !additional_types.contains(value.as_ref()) {
            additional_types.push((**value).clone());
        }
        true
    });
    let additional = if all_additional {
        join_struct_member_types(additional_types).map(Box::new)
    } else {
        None
    };
    Some(OirType::Struct { fields: merged, additional })
}

/// The type at `segments` below `ty` (a field, a map value, a list or tuple index), when it is determined.
pub fn type_at_path(ty: &OirType, segments: &[String]) -> Option<OirType> {
    if segments.is_empty() {
        return Some(ty.clone());
    }
    match walk_path(ty, &segments.join(".")) {
        Path::Found(found) => Some(found),
        Path::Opaque | Path::Missing => None,
    }
}

/// `children-of`: the fields of `ty` declared `shape`, `[shape]` or `Map K shape`, with `shape` replaced by
/// `result`; each is present only when the node holds it. A tagged union maps each variant.
fn children_of(
    ty: &OirType,
    shape: &str,
    result: &OirType,
    bindings: &CallBindings,
) -> Option<OirType> {
    let identity = |name: &str| {
        let mut name = name.to_string();
        let mut visited = std::collections::BTreeSet::new();
        let mut nullable = false;
        while visited.insert(name.clone()) {
            let declared = bindings.named_types.get(&name);
            nullable |= declared.is_some_and(OirType::is_optional);
            match declared.map(unwrap_optional) {
                Some(OirType::TypeRef { name: next }) => name = next.clone(),
                Some(OirType::Reference { entity })
                    if bindings.named_types.contains_key(entity) =>
                {
                    name = entity.clone()
                }
                _ => return Some((name, nullable)),
            }
        }
        None
    };
    let shape_identity = identity(shape).map(|(name, _)| name);
    let variant_names = |variants: &[OirField]| {
        let mut names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
        names.sort_unstable();
        names
    };
    // The `types` section expands a recursive alias one level, so a field of the shape can arrive as the shape's own
    // tagged union written out: the same named variants.
    let shape_variants = shape_identity
        .as_ref()
        .and_then(|name| bindings.named_types.get(name))
        .and_then(|declared| match unwrap_optional(declared) {
            OirType::Union { variants } => Some(variant_names(variants)),
            _ => None,
        });
    let is_shape = |t: &OirType| match unwrap_optional(t) {
        OirType::Union { variants } => shape_variants.as_ref().is_some_and(|names| *names == variant_names(variants)),
        OirType::TypeRef { name } => {
            shape_identity.is_some() && identity(name).map(|(name, _)| name) == shape_identity
        }
        OirType::Reference { entity } if bindings.named_types.contains_key(entity) => {
            shape_identity.is_some() && identity(entity).map(|(name, _)| name) == shape_identity
        }
        _ => false,
    };
    let slot = |t: &OirType| match unwrap_optional(t) {
        own if is_shape(own) => Some(result.clone()),
        OirType::Array { inner } if is_shape(inner) => Some(OirType::array(result.clone())),
        OirType::Map { key, value } if is_shape(value) => {
            Some(OirType::map((**key).clone(), result.clone()))
        }
        _ => None,
    };
    match unwrap_optional(ty) {
        OirType::Struct { fields, additional } => Some(OirType::Struct {
            additional: additional.as_deref().and_then(slot).map(Box::new),
            fields: fields
                .iter()
                .filter_map(|f| {
                    let mut child = OirField::new(f.name.clone(), slot(&f.field_type)?);
                    let nullable = f.field_type.is_optional()
                        || match &f.field_type {
                            OirType::TypeRef { name } | OirType::Reference { entity: name } => {
                                identity(name).is_some_and(|(_, nullable)| nullable)
                            }
                            _ => false,
                        };
                    child.required = f.required && !nullable;
                    Some(child)
                })
                .collect(),
        }),
        OirType::Union { variants } => Some(OirType::Union {
            variants: variants
                .iter()
                .map(|v| {
                    Some(OirField {
                        field_type: children_of(&v.field_type, shape, result, bindings)?,
                        ..v.clone()
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        }),
        OirType::Array { inner } if is_shape(inner) => Some(OirType::array(result.clone())),
        OirType::Map { key, value } if is_shape(value) => {
            Some(OirType::map((**key).clone(), result.clone()))
        }
        _ => Some(OirType::Struct { fields: Vec::new(), additional: None }),
    }
}

pub fn project_children_type(
    node: &OirType,
    children: &OirType,
    refined: &OirType,
) -> Option<OirType> {
    let (OirType::Union { variants: nodes }, OirType::Union { variants: results }) =
        (unwrap_optional(node), unwrap_optional(children))
    else {
        return (node == refined).then(|| children.clone());
    };
    if nodes.len() != results.len() {
        return None;
    }
    let selected = |ty: &OirType| match unwrap_optional(refined) {
        OirType::Union { variants } => variants.iter().any(|variant| &variant.field_type == ty),
        other => other == ty,
    };
    let kept: Vec<_> = nodes
        .iter()
        .zip(results)
        .filter(|(node, _)| selected(&node.field_type))
        .map(|(_, result)| result.clone())
        .collect();
    match kept.as_slice() {
        [] => None,
        [only] => Some(only.field_type.clone()),
        _ => Some(OirType::Union { variants: kept }),
    }
}

/// The concrete type a signature type stands for under `b`, or `None` when it is not determined.
pub fn resolve(ty: &OperatorTypeRef, b: &CallBindings) -> Option<OirType> {
    use crate::language::registry::OperatorTypeRefKind as K;
    match ty {
        OperatorTypeRef::Primitive(name) => concrete(name),
        OperatorTypeRef::Tagged(kind) => match kind {
            K::Union { of } => {
                let mut members = of.iter().filter(|m| {
                    !matches!(m, OperatorTypeRef::Tagged(K::Literal { value }) if value.is_null())
                });
                let nullable = members.clone().count() < of.len();
                let only = members.next()?;
                if members.next().is_some() {
                    return None;
                }
                let inner = resolve(only, b)?;
                Some(if nullable {
                    OirType::optional(inner)
                } else {
                    inner
                })
            }
            // A literal sequence (`…items: T each`) is a list when its items share a type, else a tuple of them.
            K::Array { of } => match &**of {
                OperatorTypeRef::Tagged(K::Var {
                    name, each: true, ..
                }) => {
                    let all = b.vars.get(name)?;
                    let first = all.first()?;
                    if all.iter().all(|t| b.agrees(first, t)) {
                        let joined = unify_structs(all);
                        if joined.is_none() && is_typed_record_join(all) {
                            return None;
                        }
                        Some(OirType::array(joined.unwrap_or_else(|| first.clone())))
                    } else {
                        Some(OirType::Tuple { items: all.clone() })
                    }
                }
                other => Some(OirType::array(resolve(other, b)?)),
            },
            K::Tuple { items } => Some(OirType::Tuple {
                items: items
                    .iter()
                    .map(|i| resolve(i, b))
                    .collect::<Option<Vec<_>>>()?,
            }),
            // A map whose values differ in type is a record.
            K::Map { key, value } => {
                if let OperatorTypeRef::Tagged(K::Var { name, .. }) = &**value {
                    if let Some(all) = b.vars.get(name) {
                        if all
                            .first()
                            .is_some_and(|first| !all.iter().all(|t| b.agrees(first, t)))
                        {
                            return Some(OirType::Struct { fields: Vec::new(), additional: None });
                        }
                    }
                }
                Some(OirType::map(resolve(key, b)?, resolve(value, b)?))
            }
            K::Object {
                fields, optional, ..
            } => {
                let mut names: Vec<&String> = fields.keys().collect();
                names.sort();
                let mut out = Vec::new();
                for name in names {
                    let mut field = OirField::new(name.clone(), resolve(&fields[name], b)?);
                    if optional.contains(name) {
                        field.required = false;
                        field.field_type = OirType::Optional {
                            inner: Box::new(field.field_type),
                        };
                    }
                    out.push(field);
                }
                Some(OirType::Struct { fields: out, additional: None })
            }
            K::Var { name, .. } => {
                if b.voided.contains(name) {
                    return None;
                }
                let bound = b.last(name).cloned()?;
                Some(
                    if b.nullable.contains(name) && !matches!(bound, OirType::Optional { .. }) {
                        OirType::optional(bound)
                    } else {
                        bound
                    },
                )
            }
            K::FieldOf { of } => {
                let record = b.bound(of)?;
                let Some(key) = b.keys.get(of).and_then(|keys| keys.first()) else {
                    // The key is a computed value: only a record whose every field has one type answers it.
                    return match unwrap_optional(record) {
                        OirType::Map { value, .. } => Some((**value).clone()),
                        OirType::Array { inner } => Some((**inner).clone()),
                        // A record whose members are alike: whichever one the key names has their join.
                        OirType::Struct { fields, .. } => {
                            let members: Vec<OirType> =
                                fields.iter().map(|f| f.field_type.clone()).collect();
                            let first = members.first()?;
                            if !members.iter().all(|m| b.agrees(first, m)) {
                                return None;
                            }
                            let joined = unify_structs(&members);
                            if joined.is_none() && is_typed_record_join(&members) {
                                return None;
                            }
                            Some(joined.unwrap_or_else(|| first.clone()))
                        }
                        _ => None,
                    };
                };
                match walk_path(record, key) {
                    Path::Found(found) => Some(found),
                    Path::Missing => b.defaults.get(of).cloned(),
                    Path::Opaque => None,
                }
            }
            K::PickOf { of } | K::OmitOf { of } => {
                let record = b.bound(of)?;
                // A map keeps its one value type whichever keys go.
                if let map @ OirType::Map { .. } = unwrap_optional(record) {
                    return Some(map.clone());
                }
                let keys = b.keys.get(of)?;
                let OirType::Struct { fields, additional } = unwrap_optional(record) else {
                    return None;
                };
                let pick = matches!(kind, K::PickOf { .. });
                let mut projected: Vec<OirField> = fields
                    .iter()
                    .filter(|f| keys.contains(&f.name) == pick)
                    .cloned()
                    .collect();
                if pick {
                    if let Some(value) = additional.as_deref() {
                        for key in keys {
                            if !projected.iter().any(|field| field.name == *key) {
                                let mut field = OirField::new(key.clone(), value.clone());
                                field.required = false;
                                projected.push(field);
                            }
                        }
                    }
                }
                Some(OirType::Struct {
                    additional: if pick { None } else { additional.clone() },
                    fields: projected,
                })
            }
            K::MergeOf { of } => {
                let all = b.vars.get(of)?;
                let typed_additional = all.iter().any(has_typed_additional);
                // Maps merge into a map of the one value type the operands share; a record among them adds keys
                // whose members are that type.
                if all
                    .iter()
                    .any(|record| matches!(unwrap_optional(record), OirType::Map { .. }))
                {
                    let mut shared: Option<&OirType> = None;
                    let mut key: Option<&OirType> = None;
                    for record in all {
                        match unwrap_optional(record) {
                            OirType::Map { key: k, value } => {
                                if key.is_some_and(|prior| !b.agrees(prior, k)) {
                                    return None;
                                }
                                key = Some(k);
                                if shared.is_some_and(|prior| !b.agrees(prior, value)) {
                                    return None;
                                }
                                shared = shared.or(Some(value));
                            }
                            OirType::Struct { fields, additional } => {
                                if typed_additional && additional.is_none() {
                                    return None;
                                }
                                if let Some(value) = additional.as_deref() {
                                    if shared.is_some_and(|prior| !b.agrees(prior, value)) {
                                        return None;
                                    }
                                    shared = shared.or(Some(value));
                                }
                                for field in fields {
                                    if shared
                                        .is_some_and(|prior| !b.agrees(prior, &field.field_type))
                                    {
                                        return None;
                                    }
                                    shared = shared.or(Some(&field.field_type));
                                }
                            }
                            _ => return None,
                        }
                    }
                    return Some(OirType::map(key?.clone(), shared?.clone()));
                }
                let mut merged: Vec<OirField> = Vec::new();
                let mut additional_types = Vec::new();
                let mut all_additional = true;
                for (index, record) in all.iter().enumerate() {
                    match unwrap_optional(record) {
                        OirType::Struct { fields, additional } => {
                            let prior_additional = all_additional.then(||
                                join_struct_member_types(additional_types.clone())
                            ).flatten();
                            if typed_additional && additional.is_none() && merged.iter()
                                .any(|prior| !fields.iter().any(|field| field.name == prior.name))
                            {
                                return None;
                            }
                            if let Some(value) = additional.as_deref() {
                                for prior in &mut merged {
                                    if !fields.iter().any(|field| field.name == prior.name)
                                        && prior.field_type != *value
                                    {
                                        prior.field_type = join_struct_member_types(vec![prior.field_type.clone(), value.clone()])?;
                                    }
                                }
                                if !additional_types.contains(value) {
                                    additional_types.push(value.clone());
                                }
                            } else {
                                all_additional = false;
                            }
                            for field in fields {
                                match merged.iter_mut().find(|f| f.name == field.name) {
                                    Some(slot) if typed_additional && !field.required => {
                                        let required = slot.required;
                                        let previous = slot.field_type.clone();
                                        *slot = field.clone();
                                        slot.required = required;
                                        if previous != slot.field_type {
                                            slot.field_type = join_struct_member_types(vec![previous, slot.field_type.clone()])?;
                                        }
                                    }
                                    Some(slot) => *slot = field.clone(),
                                    None if typed_additional && !field.required && index > 0 => {
                                        let previous = prior_additional.as_ref()?;
                                        let mut field = field.clone();
                                        if field.field_type != *previous {
                                            field.field_type = join_struct_member_types(vec![previous.clone(), field.field_type])?;
                                        }
                                        merged.push(field);
                                    }
                                    None => merged.push(field.clone()),
                                }
                            }
                        }
                        _ => return None,
                    }
                }
                let additional = if all_additional {
                    join_struct_member_types(additional_types).map(Box::new)
                } else {
                    None
                };
                Some(OirType::Struct { fields: merged, additional })
            }
            K::Shape => b
                .shape
                .clone()
                .map(|name| nominal_type(&name).unwrap_or(OirType::TypeRef { name })),
            K::ChildrenOf { of, result } => {
                children_of(b.bound(of)?, b.shape.as_deref()?, b.last(result)?, b)
            }
            K::Sexpr { .. } => Some(OirType::SExpr),
            K::Literal { .. }
            | K::Record
            | K::Fn { .. }
            | K::Entity { .. }
            | K::EntityRef
            | K::EventKey { .. }
            | K::UiSlot
            | K::PatternType
            | K::Binding { .. }
            | K::KeyOf { .. } => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(
        params: serde_json::Value,
        returns: serde_json::Value,
        max_arity: Option<u32>,
    ) -> EmbeddedOperator {
        serde_json::from_value(json!({
            "category": "std-array",
            "minArity": 0,
            "maxArity": max_arity,
            "params": params,
            "returnType": returns,
        }))
        .unwrap()
    }

    #[test]
    fn literal_option_selects_a_declared_return_branch_but_dynamic_keeps_the_union() {
        let mut operator = op(
            json!([]),
            json!({ "kind": "union", "of": ["bytes", "number"] }),
            None,
        );
        operator.return_semantics = Some("option-branch<1,result>".into());
        let signature = Signature::of(&operator);
        let bindings = CallBindings::new();
        assert_eq!(
            signature.result_with_options(&bindings, |index, field| {
                assert_eq!((index, field), (1, "result"));
                Some(true)
            }),
            Some(OirType::Number)
        );
        assert_eq!(
            signature.result_with_options(&bindings, |_, _| Some(false)),
            Some(OirType::Bytes)
        );
        assert_eq!(
            signature.result_with_options(&bindings, |_, _| None),
            signature.result(&bindings)
        );
    }

    #[test]
    fn closed_optional_options_allow_omission_without_weakening_field_types() {
        let operator = op(
            json!([p(
                "options",
                json!({ "kind": "object", "fields": { "result": "boolean", "level": "number" }, "optional": ["result", "level"] })
            )]),
            json!("bytes"),
            None,
        );
        let signature = Signature::of(&operator);
        let check = |fields: Vec<(String, Actual<'_>)>| {
            signature
                .bind_shape_literal(&mut CallBindings::new(), 0, &fields, Mode::Static)
                .unwrap()
        };
        assert!(check(vec![]).is_ok());
        assert!(check(vec![("result".into(), Actual::of_type(OirType::Boolean))]).is_ok());
        assert!(check(vec![("result".into(), Actual::of_type(OirType::String))]).is_err());
        assert!(check(vec![(
            "undeclared".into(),
            Actual::of_type(OirType::Number)
        )])
        .is_err());
    }

    fn var(name: &str) -> serde_json::Value {
        json!({ "kind": "var", "name": name })
    }

    fn record(name: &str) -> serde_json::Value {
        json!({ "kind": "var", "name": name, "bound": { "kind": "record" } })
    }

    fn list(of: serde_json::Value) -> serde_json::Value {
        json!({ "kind": "array", "of": of })
    }

    fn p(name: &str, ty: serde_json::Value) -> serde_json::Value {
        json!({ "name": name, "type": ty })
    }

    fn s_arr(of: OirType) -> OirType {
        OirType::array(of)
    }

    fn spawn_config() -> EmbeddedOperator {
        let mode = json!({ "kind": "union", "of": [{ "kind": "literal", "value": "pipe" }, { "kind": "literal", "value": "null" }] });
        op(
            json!([p(
                "config",
                json!({ "kind": "object", "fields": { "stdout": mode, "detached": "boolean", "cwd": { "kind": "union", "of": ["string", { "kind": "literal", "value": null }] } } })
            )]),
            json!("void"),
            None,
        )
    }

    fn shape_check(fields: Vec<(&str, Actual<'static>)>) -> Option<Result<(), Mismatch>> {
        let op = spawn_config();
        let sig = Signature::of(&op);
        let mut b = CallBindings::new();
        let fields: Vec<(String, Actual<'static>)> = fields
            .into_iter()
            .map(|(n, a)| (n.to_string(), a))
            .collect();
        sig.bind_shape_literal(&mut b, 0, &fields, Mode::Static)
    }

    fn literal_text(v: &str) -> Actual<'static> {
        Actual {
            text: Some(v.to_string()),
            ..Actual::of_type(OirType::String)
        }
    }

    fn null() -> Actual<'static> {
        Actual {
            known: Some(Known::Absent),
            ..Actual::unknown()
        }
    }

    fn boolean() -> Actual<'static> {
        Actual::of_type(OirType::Boolean)
    }

    #[test]
    fn a_literal_object_for_a_closed_shape_is_checked_field_by_field() {
        let good = vec![
            ("stdout", literal_text("pipe")),
            ("detached", boolean()),
            ("cwd", null()),
        ];
        assert_eq!(
            shape_check(good),
            Some(Ok(())),
            "control: every field, each of its type, null where the type allows it"
        );
        let wrong = shape_check(vec![
            ("stdout", literal_text("pipe")),
            ("detached", literal_text("yes")),
            ("cwd", null()),
        ]);
        let Some(Err(Mismatch::Type { expected, .. })) = wrong else {
            panic!("{wrong:?}")
        };
        assert!(
            expected.contains("`detached`") && expected.contains("boolean"),
            "{expected}"
        );
        let number = shape_check(vec![
            ("stdout", Actual::of_type(OirType::Number)),
            ("detached", boolean()),
            ("cwd", null()),
        ]);
        assert!(
            matches!(number, Some(Err(Mismatch::Type { .. }))),
            "a number is none of the literals: {number:?}"
        );
        let other = shape_check(vec![
            ("stdout", literal_text("inherit")),
            ("detached", boolean()),
            ("cwd", null()),
        ]);
        assert!(
            matches!(other, Some(Err(Mismatch::Type { .. }))),
            "a literal string the union does not list: {other:?}"
        );
    }

    #[test]
    fn a_literal_object_for_a_closed_shape_names_a_missing_and_an_unknown_field() {
        let missing = shape_check(vec![("stdout", literal_text("pipe")), ("cwd", null())]);
        let Some(Err(Mismatch::Type { actual, .. })) = missing else {
            panic!("{missing:?}")
        };
        assert!(
            actual.contains("`detached`") && actual.contains("missing"),
            "{actual}"
        );
        let extra = shape_check(vec![
            ("stdout", literal_text("pipe")),
            ("detached", boolean()),
            ("cwd", null()),
            ("shell", Actual::unknown()),
        ]);
        let Some(Err(Mismatch::Type { actual, .. })) = extra else {
            panic!("{extra:?}")
        };
        assert!(actual.contains("`shell`"), "{actual}");
    }

    #[test]
    fn a_parameter_that_is_not_a_closed_shape_is_left_to_bind() {
        let any = op(json!([p("x", json!("object"))]), json!("void"), None);
        let open = op(
            json!([p(
                "x",
                json!({ "kind": "object", "fields": { "a": "string" }, "open": true })
            )]),
            json!("void"),
            None,
        );
        for o in [&any, &open] {
            let mut b = CallBindings::new();
            assert_eq!(
                Signature::of(o).bind_shape_literal(
                    &mut b,
                    0,
                    &[("z".to_string(), Actual::unknown())],
                    Mode::Static
                ),
                None
            );
        }
    }

    fn struct_of(fields: &[(&str, OirType)]) -> OirType {
        OirType::Struct {
            additional: None,
            fields: fields
                .iter()
                .map(|(n, t)| OirField::new((*n).to_string(), t.clone()))
                .collect(),
        }
    }

    /// Binds every argument in order and returns the result type, or the first mismatch.
    fn call(
        operator: &EmbeddedOperator,
        args: &[Actual<'_>],
        mode: Mode,
    ) -> Result<Option<OirType>, Mismatch> {
        let sig = Signature::of(operator);
        let mut b = CallBindings::new();
        for (i, a) in args.iter().enumerate() {
            sig.bind(&mut b, i, a, mode)?;
        }
        sig.finish(&b, args.len())
            .map_err(|(_, mismatch)| mismatch)?;
        Ok(sig.result(&b))
    }

    fn ty(t: OirType) -> Actual<'static> {
        Actual::of_type(t)
    }

    fn text(t: &str) -> Actual<'static> {
        Actual {
            known: Some(Known::Type(OirType::String)),
            text: Some(t.to_string()),
            texts: None,
            lambda: None,
        }
    }

    #[test]
    fn two_arguments_bound_to_one_variable_must_agree() {
        let eq = op(
            json!([p("a", var("T")), p("b", var("T"))]),
            json!("boolean"),
            Some(2),
        );
        assert_eq!(
            call(
                &eq,
                &[ty(OirType::Number), ty(OirType::Number)],
                Mode::Static
            ),
            Ok(Some(OirType::Boolean))
        );
        assert_eq!(
            call(
                &eq,
                &[ty(OirType::Number), ty(OirType::String)],
                Mode::Static
            ),
            Err(Mismatch::Conflict {
                expected: "number".into(),
                actual: "string".into()
            })
        );
    }

    fn binary_var_operator() -> EmbeddedOperator {
        op(
            json!([p("a", var("T")), p("b", var("T"))]),
            var("T"),
            Some(2),
        )
    }

    #[test]
    fn pairs_of_unlike_value_types_bind_one_variable() {
        let pair = |a: OirType, b: OirType| OirType::Tuple { items: vec![a, b] };
        let eq = binary_var_operator();
        let merged = call(
            &eq,
            &[
                ty(pair(OirType::String, OirType::Number)),
                ty(pair(OirType::String, OirType::String)),
            ],
            Mode::Runtime,
        );
        assert_eq!(merged, Ok(Some(pair(OirType::String, absent_slot()))));
        assert!(
            call(
                &eq,
                &[
                    ty(pair(OirType::String, OirType::Number)),
                    ty(OirType::Number)
                ],
                Mode::Runtime
            )
            .is_err(),
            "control: a pair is not a number"
        );
        let triple = OirType::Tuple {
            items: vec![OirType::String, OirType::Number, OirType::String],
        };
        assert_eq!(
            call(
                &eq,
                &[ty(pair(OirType::String, OirType::Number)), ty(triple)],
                Mode::Runtime
            ),
            Ok(Some(OirType::Tuple {
                items: vec![OirType::String, OirType::Number, absent_slot()]
            })),
            "tuples of unlike widths are one shape: the positions they share"
        );
        let word = OirType::Tuple {
            items: vec![OirType::String],
        };
        let digits = OirType::Tuple {
            items: vec![OirType::Number, OirType::Number],
        };
        assert!(
            call(&eq, &[ty(word), ty(digits)], Mode::Runtime).is_err(),
            "control: lists of unlike scalars are not one shape, whatever their lengths"
        );
        let numbers = OirType::Tuple {
            items: vec![OirType::Number, OirType::Number],
        };
        assert_eq!(
            call(
                &eq,
                &[ty(s_arr(OirType::Number)), ty(numbers)],
                Mode::Runtime
            ),
            Ok(Some(s_arr(OirType::Number))),
            "a tuple of one type is a list of it"
        );
    }

    #[test]
    fn a_map_whose_values_differ_in_type_is_a_record() {
        let entries = op(
            json!([p(
                "entries",
                list(
                    json!({ "kind": "tuple", "items": [var("K"), { "kind": "var", "name": "V", "each": true }] })
                )
            )]),
            json!({ "kind": "map", "key": var("K"), "value": var("V") }),
            Some(1),
        );
        let pair = |a: OirType, b: OirType| OirType::Tuple { items: vec![a, b] };
        let mixed = call(
            &entries,
            &[ty(s_arr(pair(OirType::String, OirType::Number)))],
            Mode::Runtime,
        );
        assert_eq!(
            mixed,
            Ok(Some(OirType::map(OirType::String, OirType::Number))),
            "control: one value type is a map"
        );
        let sig = Signature::of(&entries);
        let mut b = CallBindings::new();
        sig.bind(
            &mut b,
            0,
            &ty(s_arr(pair(OirType::String, OirType::Number))),
            Mode::Runtime,
        )
        .unwrap();
        sig.bind(
            &mut b,
            0,
            &ty(s_arr(pair(OirType::String, OirType::String))),
            Mode::Runtime,
        )
        .unwrap();
        assert_eq!(sig.result(&b), Some(OirType::Struct { fields: Vec::new(), additional: None }));
    }

    #[test]
    fn a_list_literal_of_unlike_tuples_is_a_list_of_one_element_type() {
        let count = op(json!([p("arr", list(var("T")))]), json!("number"), Some(1));
        let pair = |a: OirType, b: OirType| OirType::Tuple { items: vec![a, b] };
        let unlike = OirType::Tuple {
            items: vec![
                pair(OirType::String, OirType::Number),
                pair(OirType::String, OirType::String),
            ],
        };
        assert_eq!(
            call(&count, &[ty(unlike)], Mode::Static),
            Ok(Some(OirType::Number))
        );
        let lengths = OirType::Tuple {
            items: vec![
                pair(OirType::String, OirType::Number),
                OirType::Tuple {
                    items: vec![OirType::String],
                },
            ],
        };
        assert_eq!(
            call(&count, &[ty(lengths)], Mode::Static),
            Ok(Some(OirType::Number)),
            "tuples of different widths"
        );
        let uniform = OirType::Tuple {
            items: vec![
                OirType::Tuple {
                    items: vec![OirType::Number, OirType::Number],
                },
                OirType::Tuple {
                    items: vec![OirType::String],
                },
            ],
        };
        assert!(
            call(&count, &[ty(uniform)], Mode::Static).is_err(),
            "control: lists of unlike scalars of different lengths"
        );
        let scalars = OirType::Tuple {
            items: vec![OirType::Number, OirType::String],
        };
        assert!(
            call(&count, &[ty(scalars)], Mode::Static).is_err(),
            "control: unlike scalars"
        );
    }

    #[test]
    fn an_absent_argument_binds_nothing_and_passes() {
        let eq = op(
            json!([p("a", var("T")), p("b", var("T"))]),
            json!("boolean"),
            Some(2),
        );
        assert!(call(&eq, &[absent(), ty(OirType::String)], Mode::Static).is_ok());
        assert!(call(&eq, &[ty(OirType::String), absent()], Mode::Static).is_ok());
        assert!(call(
            &eq,
            &[Actual::unknown(), ty(OirType::String)],
            Mode::Runtime
        )
        .is_ok());
    }

    #[test]
    fn a_list_element_and_a_bare_argument_share_a_variable() {
        let append = op(
            json!([p("arr", list(var("T"))), p("item", var("T"))]),
            list(var("T")),
            Some(2),
        );
        assert_eq!(
            call(
                &append,
                &[ty(s_arr(OirType::String)), ty(OirType::String)],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::String)))
        );
        assert_eq!(
            call(
                &append,
                &[ty(s_arr(OirType::Number)), ty(OirType::String)],
                Mode::Static
            ),
            Err(Mismatch::Conflict {
                expected: "number".into(),
                actual: "string".into()
            })
        );
        assert_eq!(
            call(
                &append,
                &[
                    Actual {
                        known: Some(Known::EmptyList),
                        ..Actual::unknown()
                    },
                    ty(OirType::String)
                ],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::String))),
            "an empty list takes its element type from the sibling that binds it"
        );
        assert!(
            is_undetermined(call(
                &append,
                &[
                    Actual {
                        known: Some(Known::EmptyList),
                        ..Actual::unknown()
                    },
                    Actual::unknown()
                ],
                Mode::Static
            )),
            "control: an empty list beside an undetermined item has no element type"
        );
        assert_eq!(
            call(
                &append,
                &[ty(OirType::Number), ty(OirType::Number)],
                Mode::Static
            ),
            Err(Mismatch::Type {
                expected: "array".into(),
                actual: "number".into()
            })
        );
    }

    #[test]
    fn the_result_of_a_polymorphic_call_is_the_concrete_type() {
        let first = op(json!([p("arr", list(var("T")))]), var("T"), Some(1));
        assert_eq!(
            call(&first, &[ty(s_arr(OirType::String))], Mode::Static),
            Ok(Some(OirType::String))
        );
        assert!(
            is_undetermined(call(&first, &[Actual::unknown()], Mode::Static)),
            "control: an undetermined list types nothing and is an error"
        );
        assert!(
            is_undetermined(call(
                &first,
                &[Actual {
                    known: Some(Known::EmptyList),
                    ..Actual::unknown()
                }],
                Mode::Static
            )),
            "an empty list with nothing to type it is an error, not a list of any element"
        );
        assert_eq!(
            call(
                &first,
                &[Actual {
                    known: Some(Known::EmptyList),
                    ..Actual::unknown()
                }],
                Mode::Runtime
            ),
            Ok(None),
            "at run time an empty list is a value; the rule is a static one"
        );
    }

    #[test]
    fn accumulator_record_empty_list_fields_take_the_callbacks_element_type() {
        let reduce = op(
            json!([
                p("items", list(var("T"))),
                p("seed", var("A")),
                p(
                    "step",
                    json!({"kind":"fn", "params":[var("A"), var("T")], "returns":var("A")})
                )
            ]),
            var("A"),
            Some(3),
        );
        let seed = struct_of(&[("cells", OirType::Tuple { items: Vec::new() })]);
        let filled = struct_of(&[(
            "cells",
            OirType::array(struct_of(&[("kind", OirType::String)])),
        )]);
        let step = |_: &[Option<OirType>]| Some(filled.clone());
        let callback = Actual {
            known: None,
            text: None,
            texts: None,
            lambda: Some(&step),
        };
        assert_eq!(
            call(
                &reduce,
                &[ty(s_arr(OirType::Number)), ty(seed), callback],
                Mode::Static
            ),
            Ok(Some(filled))
        );
        let wrong = |_: &[Option<OirType>]| Some(struct_of(&[("cells", s_arr(OirType::Number))]));
        let callback = Actual {
            known: None,
            text: None,
            texts: None,
            lambda: Some(&wrong),
        };
        assert!(
            call(
                &reduce,
                &[
                    ty(s_arr(OirType::Number)),
                    ty(struct_of(&[("cells", s_arr(OirType::String))])),
                    callback
                ],
                Mode::Static
            )
            .is_err(),
            "a known nonempty field cannot change its declared element type"
        );
    }

    #[test]
    fn record_bounds_require_every_declared_union_member_to_be_a_record() {
        let operator = op(json!([p("value", record("R"))]), var("R"), Some(1));
        let shape = struct_of(&[("name", OirType::String)]);
        let union = |other: OirType| OirType::Union {
            variants: vec![
                OirField::new("record", shape.clone()),
                OirField::new("other", other),
            ],
        };
        assert!(call(
            &operator,
            &[ty(union(OirType::map(OirType::String, OirType::Number)))],
            Mode::Static
        )
        .is_ok());
        assert!(
            call(&operator, &[ty(union(OirType::String))], Mode::Static).is_err(),
            "a possible scalar cannot satisfy a record bound"
        );
        let sig = Signature::of(&operator);
        let check_alias = |name: &str, definition: Option<OirType>| {
            let mut bindings = CallBindings::new();
            if let Some(definition) = definition {
                bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([(
                    "Value".into(),
                    definition,
                )])));
            }
            sig.bind(
                &mut bindings,
                0,
                &ty(OirType::TypeRef { name: name.into() }),
                Mode::Static,
            )
        };
        assert!(check_alias("Value", Some(union(shape.clone()))).is_ok());
        assert!(check_alias("Value", Some(union(OirType::String))).is_err());
        let mut bindings = CallBindings::new();
        bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([(
            "Value".into(),
            union(OirType::String),
        )])));
        assert!(sig
            .bind(
                &mut bindings,
                0,
                &ty(OirType::reference("Value")),
                Mode::Static
            )
            .is_err());
        bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([(
            "Value".into(),
            OirType::TypeRef {
                name: "Value".into(),
            },
        )])));
        assert!(sig
            .bind(
                &mut bindings,
                0,
                &ty(OirType::TypeRef {
                    name: "Value".into()
                }),
                Mode::Static
            )
            .is_err());
        assert!(
            check_alias("ExternalRow", None).is_ok(),
            "an undeclared external row retains its existing opaque contract"
        );
    }

    #[test]
    fn a_bound_variable_restricts_what_it_binds() {
        let lt = op(
            json!([
                p(
                    "a",
                    json!({ "kind": "var", "name": "T", "bound": { "kind": "union", "of": ["number", "string"] } })
                ),
                p(
                    "b",
                    json!({ "kind": "var", "name": "T", "bound": { "kind": "union", "of": ["number", "string"] } })
                ),
            ]),
            json!("boolean"),
            Some(2),
        );
        assert!(call(
            &lt,
            &[ty(OirType::String), ty(OirType::String)],
            Mode::Static
        )
        .is_ok());
        assert_eq!(
            call(
                &lt,
                &[ty(OirType::Boolean), ty(OirType::Boolean)],
                Mode::Static
            ),
            Err(Mismatch::Type {
                expected: "number | string".into(),
                actual: "boolean".into()
            })
        );
        assert_eq!(
            call(
                &lt,
                &[ty(OirType::Number), ty(OirType::String)],
                Mode::Static
            ),
            Err(Mismatch::Conflict {
                expected: "number".into(),
                actual: "string".into()
            })
        );
    }

    #[test]
    fn a_function_parameter_binds_the_lambda_result() {
        let map = op(
            json!([
                p("arr", list(var("T"))),
                p(
                    "fn",
                    json!({ "kind": "fn", "params": [var("T")], "returns": var("U") })
                ),
            ]),
            list(var("U")),
            Some(2),
        );
        let seen = std::cell::RefCell::new(Vec::new());
        let body = |params: &[Option<OirType>]| {
            seen.borrow_mut().push(params.to_vec());
            Some(OirType::Boolean)
        };
        let lambda = Actual {
            known: None,
            text: None,
            texts: None,
            lambda: Some(&body),
        };
        let sig = Signature::of(&map);
        let mut b = CallBindings::new();
        sig.bind(&mut b, 0, &ty(s_arr(OirType::Number)), Mode::Static)
            .unwrap();
        sig.bind(&mut b, 1, &lambda, Mode::Static).unwrap();
        assert_eq!(sig.result(&b), Some(s_arr(OirType::Boolean)));
        assert_eq!(seen.borrow().as_slice(), &[vec![Some(OirType::Number)]]);
    }

    fn tree_fold_op() -> EmbeddedOperator {
        let kids = json!({ "kind": "childrenOf", "of": "T", "result": "R" });
        op(
            json!([
                p("value", var("T")),
                p(
                    "step",
                    json!({ "kind": "fn", "params": [var("T"), kids], "returns": var("R") })
                ),
                p("shape", json!({ "kind": "shape" })),
            ]),
            var("R"),
            Some(4),
        )
    }

    fn tree_type() -> OirType {
        let own = || OirType::TypeRef {
            name: "Tree".to_string(),
        };
        struct_of(&[
            ("label", OirType::String),
            ("kids", s_arr(own())),
            ("parent", own()),
            ("named", OirType::map(OirType::String, own())),
            ("tags", s_arr(OirType::String)),
        ])
    }

    /// Binds the shape first (as the checker does), then the value and the step.
    fn fold(
        t: OirType,
        shape: Option<&str>,
        step: LambdaResult<'_>,
    ) -> Result<Option<OirType>, Mismatch> {
        let operator = tree_fold_op();
        let sig = Signature::of(&operator);
        let mut b = CallBindings::new();
        if let Some(shape) = shape {
            sig.bind(&mut b, 2, &text(shape), Mode::Static)?;
        }
        sig.bind(&mut b, 0, &ty(t), Mode::Static)?;
        let lambda = Actual {
            known: None,
            text: None,
            texts: None,
            lambda: Some(step),
        };
        sig.bind(&mut b, 1, &lambda, Mode::Static)?;
        Ok(sig.result(&b))
    }

    fn optional_fields(fields: &[(&str, OirType)]) -> OirType {
        let OirType::Struct { mut fields, additional } = struct_of(fields) else {
            unreachable!()
        };
        fields.iter_mut().for_each(|f| f.required = false);
        OirType::Struct { fields, additional }
    }

    #[test]
    fn children_reference_identity_requires_a_declared_alias() {
        let node = OirType::array(OirType::reference("Tree"));
        let mut bindings = CallBindings::new();
        assert_eq!(
            children_of(&node, "Tree", &OirType::String, &bindings),
            Some(OirType::Struct { fields: Vec::new(), additional: None })
        );
        let required = struct_of(&[(
            "kids",
            OirType::array(OirType::TypeRef {
                name: "Tree".into(),
            }),
        )]);
        let Some(OirType::Struct { fields, .. }) =
            children_of(&required, "Tree", &OirType::String, &bindings)
        else {
            panic!("child fields")
        };
        assert!(
            fields[0].required,
            "a required nonnullable child container is always in childResults"
        );
        for optional in [
            optional_fields(&[(
                "kids",
                OirType::array(OirType::TypeRef {
                    name: "Tree".into(),
                }),
            )]),
            struct_of(&[(
                "kids",
                OirType::optional(OirType::array(OirType::TypeRef {
                    name: "Tree".into(),
                })),
            )]),
        ] {
            let Some(OirType::Struct { fields, .. }) =
                children_of(&optional, "Tree", &OirType::String, &bindings)
            else {
                panic!("child fields")
            };
            assert!(
                !fields[0].required,
                "optional or nullable children may be absent"
            );
        }
        bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([
            ("Tree".into(), tree_type()),
            (
                "Alias".into(),
                OirType::TypeRef {
                    name: "Tree".into(),
                },
            ),
            (
                "MaybeTree".into(),
                OirType::optional(OirType::TypeRef {
                    name: "Tree".into(),
                }),
            ),
        ])));
        assert_eq!(
            children_of(&node, "Alias", &OirType::String, &bindings),
            Some(OirType::array(OirType::String))
        );
        let nullable_alias = struct_of(&[(
            "parent",
            OirType::TypeRef {
                name: "MaybeTree".into(),
            },
        )]);
        let Some(OirType::Struct { fields, .. }) =
            children_of(&nullable_alias, "Tree", &OirType::String, &bindings)
        else {
            panic!("nullable alias child")
        };
        assert!(
            !fields[0].required,
            "nullable aliases do not prove child presence"
        );
        bindings.use_named_types(std::sync::Arc::new(BTreeMap::from([
            (
                "Tree".into(),
                OirType::TypeRef {
                    name: "Alias".into(),
                },
            ),
            (
                "Alias".into(),
                OirType::TypeRef {
                    name: "Tree".into(),
                },
            ),
        ])));
        assert_eq!(
            children_of(&node, "Alias", &OirType::String, &bindings),
            Some(OirType::Struct { fields: Vec::new(), additional: None })
        );
    }

    #[test]
    fn a_fold_step_reads_its_children_typed_by_its_own_result() {
        let seen = std::cell::RefCell::new(Vec::new());
        let step = |params: &[Option<OirType>]| {
            seen.borrow_mut().push(params.to_vec());
            Some(OirType::String)
        };
        assert_eq!(
            fold(tree_type(), Some("Tree"), &step),
            Ok(Some(OirType::String))
        );
        let kids = struct_of(&[
            ("kids", s_arr(OirType::String)),
            ("parent", OirType::String),
            ("named", OirType::map(OirType::String, OirType::String)),
        ]);
        assert_eq!(
            seen.borrow().as_slice(),
            &[
                vec![Some(tree_type()), None],
                vec![Some(tree_type()), Some(kids)]
            ],
            "a provisional reading, then one with the children typed; `tags` is no child"
        );
    }

    #[test]
    fn a_fold_step_whose_result_changes_with_its_children_is_a_mismatch() {
        let step = |params: &[Option<OirType>]| {
            Some(if params[1].is_some() {
                OirType::Number
            } else {
                OirType::String
            })
        };
        let Err(Mismatch::Type { expected, actual }) = fold(tree_type(), Some("Tree"), &step)
        else {
            panic!("a step returning number over string children is refused");
        };
        assert!(
            expected.contains("childResults") && expected.contains("string"),
            "{expected}"
        );
        assert!(actual.contains("number"), "{actual}");
    }

    #[test]
    fn control_a_fold_step_over_a_non_recursive_type_gets_no_children() {
        let seen = std::cell::RefCell::new(Vec::new());
        let step = |params: &[Option<OirType>]| {
            seen.borrow_mut().push(params[1].clone());
            Some(OirType::Boolean)
        };
        let note = struct_of(&[("of", s_arr(OirType::String)), ("Note", OirType::String)]);
        assert_eq!(fold(note, Some("Note"), &step), Ok(Some(OirType::Boolean)));
        assert_eq!(seen.borrow().last(), Some(&Some(optional_fields(&[]))));
    }

    #[test]
    fn a_fold_over_a_tagged_union_types_the_children_of_every_variant() {
        let own = || OirType::TypeRef {
            name: "Pred".to_string(),
        };
        let union = OirType::Union {
            variants: vec![
                OirField::new(
                    "All",
                    struct_of(&[("kind", OirType::String), ("of", s_arr(own()))]),
                ),
                OirField::new(
                    "Not",
                    struct_of(&[("kind", OirType::String), ("of", own())]),
                ),
            ],
        };
        let last = std::cell::RefCell::new(None);
        let step = |params: &[Option<OirType>]| {
            *last.borrow_mut() = params[1].clone();
            Some(OirType::String)
        };
        assert_eq!(fold(union, Some("Pred"), &step), Ok(Some(OirType::String)));
        let expected = OirType::Union {
            variants: vec![
                OirField::new("All", struct_of(&[("of", s_arr(OirType::String))])),
                OirField::new("Not", struct_of(&[("of", OirType::String)])),
            ],
        };
        assert_eq!(last.borrow().clone(), Some(expected));
    }

    fn pred_children() -> OirType {
        OirType::Union {
            variants: vec![
                OirField::new("All", struct_of(&[("of", s_arr(OirType::String))])),
                OirField::new("Not", struct_of(&[("of", OirType::String)])),
            ],
        }
    }

    #[test]
    fn a_union_member_read_whose_variants_differ_is_their_union_not_one_variants_type() {
        let Path::Found(OirType::Union { variants }) = walk_path(&pred_children(), "of") else {
            panic!("a member declared with different types reads as the union of them")
        };
        let types: Vec<_> = variants.iter().map(|v| v.field_type.clone()).collect();
        assert_eq!(types, vec![s_arr(OirType::String), OirType::String]);
    }

    #[test]
    fn control_a_union_member_read_whose_variants_agree_is_one_type() {
        let alike = OirType::Union {
            variants: vec![
                OirField::new("A", struct_of(&[("of", s_arr(OirType::String))])),
                OirField::new("B", struct_of(&[("of", s_arr(OirType::String))])),
            ],
        };
        let Path::Found(found) = walk_path(&alike, "of") else {
            panic!("agreeing variants read as one type")
        };
        assert_eq!(found, s_arr(OirType::String));
    }

    #[test]
    fn control_without_the_shape_the_children_stay_untyped() {
        let seen = std::cell::RefCell::new(Vec::new());
        let step = |params: &[Option<OirType>]| {
            seen.borrow_mut().push(params[1].clone());
            Some(OirType::String)
        };
        assert_eq!(fold(tree_type(), None, &step), Ok(Some(OirType::String)));
        assert_eq!(seen.borrow().as_slice(), &[None, None]);
    }

    #[test]
    fn a_predicate_lambda_must_return_a_boolean() {
        let filter = op(
            json!([
                p("arr", list(var("T"))),
                p(
                    "pred",
                    json!({ "kind": "fn", "params": [var("T")], "returns": "boolean" })
                ),
            ]),
            list(var("T")),
            Some(2),
        );
        let returns_number = |_: &[Option<OirType>]| Some(OirType::Number);
        let lambda = Actual {
            known: None,
            text: None,
            texts: None,
            lambda: Some(&returns_number),
        };
        let sig = Signature::of(&filter);
        let mut b = CallBindings::new();
        sig.bind(&mut b, 0, &ty(s_arr(OirType::String)), Mode::Static)
            .unwrap();
        assert_eq!(
            sig.bind(&mut b, 1, &lambda, Mode::Static),
            Err(Mismatch::Type {
                expected: "boolean".into(),
                actual: "number".into()
            })
        );
    }

    fn get() -> EmbeddedOperator {
        op(
            json!([
                p("obj", record("R")),
                p("path", json!({ "kind": "keyOf", "of": "R" })),
                p("default", json!({ "kind": "fieldOf", "of": "R" })),
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(3),
        )
    }

    #[test]
    fn a_key_names_a_field_and_the_result_is_that_fields_type() {
        let row = struct_of(&[("name", OirType::String), ("count", OirType::Number)]);
        assert_eq!(
            call(&get(), &[ty(row.clone()), text("count")], Mode::Static),
            Ok(Some(OirType::Number))
        );
        assert_eq!(
            call(&get(), &[ty(row.clone()), text("missing")], Mode::Static),
            Err(Mismatch::NoField {
                path: "missing".into(),
                record: "object".into()
            })
        );
    }

    fn get_of_record_or_list() -> EmbeddedOperator {
        let bound = json!({ "kind": "union", "of": [{ "kind": "record" }, list(var("E"))] });
        op(
            json!([
                p("obj", json!({ "kind": "var", "name": "R", "bound": bound })),
                p("path", json!({ "kind": "keyOf", "of": "R" })),
                p("default", json!({ "kind": "fieldOf", "of": "R" })),
            ]),
            json!({ "kind": "fieldOf", "of": "R" }),
            Some(3),
        )
    }

    #[test]
    fn a_tuple_is_addressed_by_index_whatever_its_items_are() {
        let get = get_of_record_or_list();
        let pair = OirType::Tuple {
            items: vec![OirType::Number, OirType::String],
        };
        assert_eq!(
            call(&get, &[ty(pair.clone()), text("0")], Mode::Static),
            Ok(Some(OirType::Number))
        );
        assert_eq!(
            call(&get, &[ty(pair.clone()), text("1")], Mode::Static),
            Ok(Some(OirType::String))
        );
        assert!(
            call(&get, &[ty(pair), text("2")], Mode::Static).is_err(),
            "control: a tuple of two has no third item"
        );
        assert_eq!(
            call(&get, &[ty(s_arr(OirType::Number)), text("3")], Mode::Static),
            Ok(Some(OirType::Number)),
            "control: a list of one type"
        );
        assert!(
            call(&get, &[ty(OirType::Number), text("0")], Mode::Static).is_err(),
            "control: a number is neither a record nor a list"
        );
    }

    #[test]
    fn a_tagged_union_member_read_is_the_field_its_variants_declare() {
        let variant = |name: &str, fields: &[(&str, OirType)]| {
            OirField::new(name.to_string(), struct_of(fields))
        };
        let union = OirType::Union {
            variants: vec![
                variant(
                    "shape",
                    &[("type", OirType::String), ("radius", OirType::Number)],
                ),
                variant(
                    "text",
                    &[("type", OirType::String), ("content", OirType::String)],
                ),
            ],
        };
        assert_eq!(
            type_at_path(&union, &["type".to_string()]),
            Some(OirType::String),
            "declared by every variant"
        );
        assert_eq!(
            type_at_path(&union, &["radius".to_string()]),
            Some(OirType::optional(OirType::Number)),
            "declared by one of two"
        );
        assert_eq!(
            type_at_path(&union, &["nope".to_string()]),
            None,
            "control: declared by none"
        );
    }

    #[test]
    fn a_default_stands_in_for_a_field_the_record_does_not_declare() {
        let row = struct_of(&[("count", OirType::Number)]);
        assert_eq!(
            call(
                &get(),
                &[ty(row.clone()), text("roles"), ty(s_arr(OirType::String))],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::String)))
        );
        assert_eq!(
            call(&get(), &[ty(row.clone()), text("roles")], Mode::Static),
            Err(Mismatch::NoField {
                path: "roles".into(),
                record: "object".into()
            }),
            "control: without a default the missing field is an error"
        );
        let has = op(
            json!([
                p("obj", record("R")),
                p("path", json!({ "kind": "keyOf", "of": "R" }))
            ]),
            json!("boolean"),
            Some(2),
        );
        assert!(
            call(&has, &[ty(row), text("roles")], Mode::Static).is_ok(),
            "asking whether a field exists is not reading it"
        );
    }

    #[test]
    fn a_default_must_have_the_fields_type() {
        let row = struct_of(&[("count", OirType::Number)]);
        assert!(call(
            &get(),
            &[ty(row.clone()), text("count"), ty(OirType::Number)],
            Mode::Static
        )
        .is_ok());
        assert_eq!(
            call(
                &get(),
                &[ty(row), text("count"), ty(OirType::String)],
                Mode::Static
            ),
            Err(Mismatch::Type {
                expected: "number".into(),
                actual: "string".into()
            })
        );
    }

    #[test]
    fn a_default_for_an_expression_field_is_any_literal_and_a_typed_field_still_checks() {
        let row = struct_of(&[("default", OirType::SExpr), ("count", OirType::Number)]);
        assert!(call(
            &get(),
            &[ty(row.clone()), text("default"), ty(OirType::String)],
            Mode::Static
        )
        .is_ok());
        assert!(call(
            &get(),
            &[ty(row.clone()), text("default"), ty(OirType::Number)],
            Mode::Static
        )
        .is_ok());
        assert!(
            call(
                &get(),
                &[ty(row), text("count"), ty(OirType::String)],
                Mode::Static
            )
            .is_err(),
            "control: a number field still rejects a string default"
        );
    }

    #[test]
    fn a_dot_path_walks_nested_records_and_a_map_takes_any_key() {
        let nested = struct_of(&[("owner", struct_of(&[("name", OirType::String)]))]);
        assert_eq!(
            call(&get(), &[ty(nested), text("owner.name")], Mode::Static),
            Ok(Some(OirType::String))
        );
        let map = OirType::map(OirType::String, OirType::Number);
        assert_eq!(
            call(&get(), &[ty(map), text("anything")], Mode::Static),
            Ok(Some(OirType::Number))
        );
    }

    #[test]
    fn branch_record_list_fields_keep_members_from_both_element_shapes() {
        let mut pick = op(
            json!([
                p("condition", json!("boolean")),
                p("yes", var("T")),
                p("no", var("T"))
            ]),
            var("T"),
            Some(3),
        );
        pick.return_semantics = Some("branch-union".into());
        let left = struct_of(&[("cells", s_arr(struct_of(&[("kind", OirType::String)])))]);
        let right = struct_of(&[(
            "cells",
            s_arr(struct_of(&[
                ("kind", OirType::String),
                ("kids", s_arr(OirType::String)),
            ])),
        )]);
        let joined = call(
            &pick,
            &[ty(OirType::Boolean), ty(left), ty(right)],
            Mode::Static,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            type_at_path(&joined, &["cells".into(), "0".into(), "kids".into()]),
            Some(OirType::optional(s_arr(OirType::String))),
            "a member only one element shape declares may be absent, so its read is optional"
        );
        assert_eq!(
            type_at_path(&joined, &["cells".into(), "0".into(), "kind".into()]),
            Some(OirType::String),
            "control: a member both shapes require reads non-optional"
        );
        let optional = optional_fields(&[("note", OirType::String)]);
        let joined = call(
            &pick,
            &[ty(OirType::Boolean), ty(optional.clone()), ty(optional)],
            Mode::Static,
        )
        .unwrap()
        .unwrap();
        let OirType::Struct { fields, .. } = joined else {
            panic!("record")
        };
        assert!(
            !fields[0].required,
            "a member optional in both branches remains optional"
        );
    }

    #[test]
    fn lists_of_record_elements_share_the_canonical_optional_field_join() {
        let mut concat = op(
            json!([p("left", list(var("E"))), p("right", list(var("E")))]),
            list(var("E")),
            Some(2),
        );
        concat.return_semantics = Some("union-of-args".into());
        let left = struct_of(&[("kind", OirType::String), ("kids", s_arr(OirType::String))]);
        let right = struct_of(&[("kind", OirType::String), ("val", OirType::Number)]);
        let actual = call(&concat, &[ty(s_arr(left)), ty(s_arr(right))], Mode::Static);
        let expected = OirType::array(optional_fields(&[
            ("kind", OirType::String),
            ("kids", s_arr(OirType::String)),
            ("val", OirType::Number),
        ]));
        let Ok(Some(OirType::Array { inner })) = actual else {
            panic!("record lists must join: {actual:?}")
        };
        let OirType::Array { inner: expected } = expected else {
            unreachable!()
        };
        let OirType::Struct { fields, .. } = &*inner else {
            panic!("record element")
        };
        assert!(fields.iter().find(|f| f.name == "kind").unwrap().required);
        assert!(!fields.iter().find(|f| f.name == "kids").unwrap().required);
        assert!(!fields.iter().find(|f| f.name == "val").unwrap().required);
        assert_eq!(
            fields
                .iter()
                .map(|f| (&f.name, &f.field_type))
                .collect::<Vec<_>>(),
            match &*expected {
                OirType::Struct { fields, .. } => fields
                    .iter()
                    .map(|f| (&f.name, &f.field_type))
                    .collect::<Vec<_>>(),
                _ => unreachable!(),
            }
        );
        assert!(call(
            &concat,
            &[ty(s_arr(OirType::String)), ty(s_arr(OirType::Number))],
            Mode::Static
        )
        .is_err());
    }

    #[test]
    fn a_computed_key_over_a_record_of_alike_members_is_their_join() {
        let stat = |hp: OirType| struct_of(&[("hp", hp), ("name", OirType::String)]);
        let roster = struct_of(&[
            ("goblin", stat(OirType::Number)),
            ("orc", stat(OirType::Number)),
        ]);
        let key = || Actual::of_type(OirType::String);
        assert_eq!(
            call(&get(), &[ty(roster), key()], Mode::Static),
            Ok(Some(stat(OirType::Number))),
            "whichever member the key names has the members' shape"
        );
        // members that differ in a field: it is optional in the join.
        let mixed = struct_of(&[
            ("a", struct_of(&[("hp", OirType::Number)])),
            (
                "b",
                struct_of(&[("hp", OirType::Number), ("tag", OirType::String)]),
            ),
        ]);
        let joined = call(&get(), &[ty(mixed), key()], Mode::Static)
            .unwrap()
            .unwrap();
        let OirType::Struct { fields, .. } = joined else {
            panic!("a struct")
        };
        assert!(fields.iter().find(|f| f.name == "hp").unwrap().required);
        assert!(!fields.iter().find(|f| f.name == "tag").unwrap().required);
        // controls: members of different kinds have no join; neither has an empty record.
        let unlike = struct_of(&[("a", OirType::Number), ("b", OirType::String)]);
        assert_eq!(call(&get(), &[ty(unlike), key()], Mode::Static), Ok(None));
        assert_eq!(
            call(&get(), &[ty(struct_of(&[])), key()], Mode::Static),
            Ok(None)
        );
        // a scalar member list is the member type.
        let uniform = struct_of(&[("a", OirType::Number), ("b", OirType::Number)]);
        assert_eq!(
            call(&get(), &[ty(uniform), key()], Mode::Static),
            Ok(Some(OirType::Number))
        );
    }

    #[test]
    fn a_missing_field_read_with_the_empty_list_default_is_typed_by_what_it_is_handed_to() {
        let row = struct_of(&[("name", OirType::String)]);
        let includes = op(
            json!([p("arr", list(var("T"))), p("item", var("T"))]),
            json!("boolean"),
            Some(2),
        );
        let getter = get();
        let sig_get = Signature::of(&getter);
        let mut b = CallBindings::new();
        sig_get.bind(&mut b, 0, &ty(row), Mode::Static).unwrap();
        sig_get
            .bind(&mut b, 1, &text("roles"), Mode::Static)
            .unwrap();
        sig_get.bind(&mut b, 2, &empty(), Mode::Static).unwrap();
        sig_get.finish(&b, 3).unwrap();
        let read = sig_get.result(&b);
        assert_eq!(
            read,
            Some(OirType::Tuple { items: Vec::new() }),
            "the default is the read"
        );
        // handed to a list operator with a typed sibling, it is that list; with none, it is untyped.
        assert_eq!(
            call(
                &includes,
                &[
                    Actual {
                        known: read.clone().map(Known::Type),
                        ..Actual::unknown()
                    },
                    ty(OirType::String)
                ],
                Mode::Static
            ),
            Ok(Some(OirType::Boolean))
        );
        assert!(is_undetermined(call(
            &op(json!([p("arr", list(var("T")))]), json!("number"), Some(1)),
            &[Actual {
                known: read.map(Known::Type),
                ..Actual::unknown()
            }],
            Mode::Static
        )));
    }

    #[test]
    fn a_key_read_from_a_value_is_the_value_type_of_a_map_and_nothing_of_a_struct() {
        // The key is an expression (a payload field), not a literal: a map answers its value type whatever key.
        let dynamic = Actual::of_type(OirType::String);
        let map = OirType::map(OirType::String, OirType::Number);
        assert_eq!(
            call(&get(), &[ty(map), dynamic], Mode::Static),
            Ok(Some(OirType::Number)),
            "a map holds one value type under every key"
        );
        let row = struct_of(&[("name", OirType::String), ("count", OirType::Number)]);
        assert_eq!(
            call(&get(), &[ty(row), Actual::of_type(OirType::String)], Mode::Static),
            Ok(None),
            "control: which field of a struct a computed key names is not known, so the read has no type"
        );
        let list = s_arr(OirType::Number);
        assert_eq!(
            call(
                &get_of_record_or_list(),
                &[ty(list), Actual::of_type(OirType::String)],
                Mode::Static
            ),
            Ok(Some(OirType::Number)),
            "a list answers its element type at a computed index"
        );
    }

    #[test]
    fn at_run_time_a_key_is_not_checked_but_the_record_is() {
        let row = struct_of(&[]);
        assert!(call(&get(), &[ty(row), text("whatever")], Mode::Runtime).is_ok());
        assert_eq!(
            call(&get(), &[ty(OirType::Number), text("x")], Mode::Runtime),
            Err(Mismatch::Type {
                expected: "object".into(),
                actual: "number".into()
            })
        );
    }

    #[test]
    fn additional_records_transforms_preserve_the_same_value_slot() {
        let prior = OirType::Struct {
            fields: vec![],
            additional: Some(Box::new(OirType::Tuple { items: vec![] })),
        };
        let actual = OirType::Struct {
            fields: vec![],
            additional: Some(Box::new(OirType::array(OirType::Number))),
        };
        assert_eq!(fill_empty_list_slots(&prior, &actual), Some(actual));
        let node = OirType::Struct {
            fields: vec![],
            additional: Some(Box::new(OirType::TypeRef { name: "Node".into() })),
        };
        assert_eq!(
            children_of(&node, "Node", &OirType::String, &CallBindings::new()),
            Some(OirType::Struct {
                fields: vec![],
                additional: Some(Box::new(OirType::String)),
            })
        );
    }

    #[test]
    fn additional_records_paths_and_pick_omit_keep_presence_distinct() {
        let optional = optional_fields(&[("maybe", OirType::String)]);
        assert_eq!(type_at_path(&optional, &["maybe".into()]), Some(OirType::optional(OirType::String)));
        let row = OirType::Struct {
            fields: vec![OirField::new("fixed", OirType::String)],
            additional: Some(Box::new(OirType::Number)),
        };
        assert_eq!(type_at_path(&row, &["extra".into()]), Some(OirType::optional(OirType::Number)));
        let operator = |kind: &str| op(
            json!([p("obj", record("R")), p("keys", list(json!({ "kind": "keyOf", "of": "R" })))]),
            json!({ "kind": kind, "of": "R" }), Some(2),
        );
        let keys = |name: &str| Actual {
            known: None, text: None, texts: Some(vec![name.into()]), lambda: None,
        };
        let mut extra = OirField::new("extra", OirType::Number);
        extra.required = false;
        assert_eq!(call(&operator("pickOf"), &[ty(row.clone()), keys("extra")], Mode::Static),
            Ok(Some(OirType::Struct { fields: vec![extra], additional: None })));
        assert_eq!(call(&operator("omitOf"), &[ty(row), keys("fixed")], Mode::Static),
            Ok(Some(OirType::Struct { fields: vec![], additional: Some(Box::new(OirType::Number)) })));
    }

    #[test]
    fn additional_records_branch_join_covers_both_extra_value_types() {
        let branch = |additional| OirType::Struct {
            fields: vec![OirField::new("fixed", OirType::String)],
            additional: Some(Box::new(additional)),
        };
        let joined = unify_structs(&[branch(OirType::Number), branch(OirType::Boolean)]).unwrap();
        let OirType::Struct { additional: Some(additional), .. } = joined else { panic!("typed extras retained") };
        assert!(crate::type_compat::sub(&OirType::Number, &additional).is_some());
        assert!(crate::type_compat::sub(&OirType::Boolean, &additional).is_some());
        let mixed = unify_structs(&[branch(OirType::Number), struct_of(&[("fixed", OirType::String)])]).unwrap();
        assert!(matches!(mixed, OirType::Struct { additional: None, .. }));
    }

    #[test]
    fn additional_records_merge_retains_typed_extras_and_right_known_fields() {
        let merge = op(json!([p("...objs", json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true }))]),
            json!({ "kind": "mergeOf", "of": "R" }), None);
        let left = OirType::Struct { fields: vec![OirField::new("left", OirType::Number)], additional: Some(Box::new(OirType::Number)) };
        let right = OirType::Struct { fields: vec![OirField::new("right", OirType::String)], additional: Some(Box::new(OirType::Number)) };
        assert_eq!(call(&merge, &[ty(left), ty(right)], Mode::Static), Ok(Some(OirType::Struct {
            fields: vec![OirField::new("left", OirType::Number), OirField::new("right", OirType::String)],
            additional: Some(Box::new(OirType::Number)),
        })));
    }

    #[test]
    fn additional_records_unknown_branch_does_not_claim_a_missing_named_field() {
        let mut choose = op(json!([p("condition", json!("boolean")), p("yes", var("T")), p("no", var("T"))]),
            var("T"), Some(3));
        choose.return_semantics = Some("branch-union".into());
        let typed = OirType::Struct {
            fields: vec![OirField::new("amount", OirType::Number)],
            additional: Some(Box::new(OirType::Number)),
        };
        for operands in [[typed.clone(), struct_of(&[])], [struct_of(&[]), typed.clone()]] {
            assert!(matches!(call(&choose,
                &[ty(OirType::Boolean), ty(operands[0].clone()), ty(operands[1].clone())],
                Mode::Static), Err(Mismatch::Undetermined { .. })));
        }
        let mut concat = op(json!([p("left", list(var("E"))), p("right", list(var("E")))]),
            list(var("E")), Some(2));
        concat.return_semantics = Some("union-of-args".into());
        assert!(matches!(call(&concat,
            &[ty(s_arr(typed.clone())), ty(s_arr(struct_of(&[])))], Mode::Static),
            Err(Mismatch::Undetermined { .. })));
        let array = op(json!([]), list(json!({ "kind": "var", "name": "T", "each": true })), None);
        let mut bindings = CallBindings::new();
        bindings.vars.insert("T".into(), vec![typed.clone(), struct_of(&[])]);
        assert_eq!(Signature::of(&array).result(&bindings), None);
        let roster = struct_of(&[("typed", typed), ("unknown", struct_of(&[]))]);
        assert_eq!(call(&get(), &[ty(roster), ty(OirType::String)], Mode::Static), Ok(None));
    }

    #[test]
    fn additional_records_unknown_merge_cannot_overwrite_known_members_unnoticed() {
        let merge = op(json!([p("...objs", json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true }))]),
            json!({ "kind": "mergeOf", "of": "R" }), None);
        let typed = OirType::Struct {
            fields: vec![OirField::new("amount", OirType::Number)],
            additional: Some(Box::new(OirType::Number)),
        };
        assert_eq!(call(&merge, &[ty(typed.clone()), ty(struct_of(&[]))], Mode::Static), Ok(None));
        let mut optional = typed.clone();
        if let OirType::Struct { fields, .. } = &mut optional { fields[0].required = false; }
        assert_eq!(call(&merge, &[ty(struct_of(&[])), ty(optional)], Mode::Static), Ok(None));
        assert_eq!(call(&merge, &[ty(struct_of(&[])), ty(typed)], Mode::Static),
            Ok(Some(struct_of(&[("amount", OirType::Number)]))));
        let ordinary = struct_of(&[("amount", OirType::Number)]);
        assert_eq!(call(&merge, &[ty(ordinary.clone()), ty(struct_of(&[]))], Mode::Static), Ok(Some(ordinary)));
        let left = OirType::Struct { fields: vec![], additional: Some(Box::new(OirType::Number)) };
        let mut amount = OirField::new("amount", OirType::String);
        amount.required = false;
        let right = OirType::Struct { fields: vec![amount], additional: Some(Box::new(OirType::Number)) };
        let merged = call(&merge, &[ty(left), ty(right)], Mode::Static).unwrap().unwrap();
        let OirType::Struct { fields, additional: Some(_), .. } = merged else { panic!("typed merge retained") };
        assert!(!fields[0].required);
        assert!(crate::type_compat::sub(&OirType::Number, &fields[0].field_type).is_some());
        assert!(crate::type_compat::sub(&OirType::String, &fields[0].field_type).is_some());
    }

    #[test]
    fn pick_and_omit_keep_and_drop_the_named_fields() {
        let pick = op(
            json!([
                p("obj", record("R")),
                p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
            ]),
            json!({ "kind": "pickOf", "of": "R" }),
            Some(2),
        );
        let omit = op(
            json!([
                p("obj", record("R")),
                p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
            ]),
            json!({ "kind": "omitOf", "of": "R" }),
            Some(2),
        );
        let row = struct_of(&[("a", OirType::Number), ("b", OirType::String)]);
        let keys = Actual {
            known: None,
            text: None,
            texts: Some(vec!["a".to_string()]),
            lambda: None,
        };
        assert_eq!(
            call(&pick, &[ty(row.clone()), keys], Mode::Static),
            Ok(Some(struct_of(&[("a", OirType::Number)])))
        );
        let keys = Actual {
            known: None,
            text: None,
            texts: Some(vec!["a".to_string()]),
            lambda: None,
        };
        assert_eq!(
            call(&omit, &[ty(row.clone()), keys], Mode::Static),
            Ok(Some(struct_of(&[("b", OirType::String)])))
        );
        let unknown = Actual {
            known: None,
            text: None,
            texts: Some(vec!["zzz".to_string()]),
            lambda: None,
        };
        assert_eq!(
            call(&pick, &[ty(row), unknown], Mode::Static),
            Ok(Some(struct_of(&[])))
        );
    }

    #[test]
    fn omitting_or_picking_keys_of_a_map_is_the_map() {
        let omit = op(
            json!([
                p("obj", record("R")),
                p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
            ]),
            json!({ "kind": "omitOf", "of": "R" }),
            Some(2),
        );
        let pick = op(
            json!([
                p("obj", record("R")),
                p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
            ]),
            json!({ "kind": "pickOf", "of": "R" }),
            Some(2),
        );
        let bag = OirType::map(OirType::String, OirType::Number);
        let computed = || ty(s_arr(OirType::String));
        assert_eq!(
            call(&omit, &[ty(bag.clone()), computed()], Mode::Static),
            Ok(Some(bag.clone()))
        );
        assert_eq!(
            call(&pick, &[ty(bag.clone()), computed()], Mode::Static),
            Ok(Some(bag.clone()))
        );
        assert_eq!(
            call(
                &omit,
                &[ty(OirType::optional(bag.clone())), computed()],
                Mode::Static
            ),
            Ok(Some(bag)),
            "an optional map is read as the map"
        );
        let row = struct_of(&[("a", OirType::Number)]);
        assert_eq!(
            call(&omit, &[ty(row), computed()], Mode::Static),
            Ok(None),
            "control: keys computed from data say nothing about which fields of a record go"
        );
    }

    #[test]
    fn a_literal_key_list_that_also_has_a_type_still_names_its_keys() {
        let pick = op(
            json!([
                p("obj", record("R")),
                p("keys", list(json!({ "kind": "keyOf", "of": "R" })))
            ]),
            json!({ "kind": "pickOf", "of": "R" }),
            Some(2),
        );
        let row = struct_of(&[("a", OirType::Number), ("b", OirType::String)]);
        let keys = Actual {
            known: Some(Known::Type(OirType::array(OirType::String))),
            text: None,
            texts: Some(vec!["a".to_string()]),
            lambda: None,
        };
        assert_eq!(
            call(&pick, &[ty(row.clone()), keys], Mode::Static),
            Ok(Some(struct_of(&[("a", OirType::Number)])))
        );
        let control = Actual {
            known: Some(Known::Type(OirType::array(OirType::String))),
            text: None,
            texts: None,
            lambda: None,
        };
        assert_eq!(
            call(&pick, &[ty(row), control], Mode::Static),
            Ok(None),
            "computed keys name nothing"
        );
    }

    #[test]
    fn a_merge_is_the_fields_of_every_operand_later_winning() {
        let merge = op(
            json!([p(
                "...objs",
                json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true })
            )]),
            json!({ "kind": "mergeOf", "of": "R" }),
            None,
        );
        let a = struct_of(&[("a", OirType::Number), ("b", OirType::Number)]);
        let b = struct_of(&[("b", OirType::String)]);
        assert_eq!(
            call(&merge, &[ty(a), ty(b)], Mode::Static),
            Ok(Some(struct_of(&[
                ("a", OirType::Number),
                ("b", OirType::String)
            ])))
        );
        assert!(call(
            &merge,
            &[ty(OirType::Number), ty(struct_of(&[]))],
            Mode::Static
        )
        .is_err());
    }

    #[test]
    fn a_merge_of_maps_is_a_map_of_the_value_type_they_share() {
        let merge = op(
            json!([p(
                "...objs",
                json!({ "kind": "var", "name": "R", "bound": { "kind": "record" }, "each": true })
            )]),
            json!({ "kind": "mergeOf", "of": "R" }),
            None,
        );
        let words = OirType::map(OirType::String, OirType::String);
        assert_eq!(
            call(
                &merge,
                &[ty(words.clone()), ty(words.clone())],
                Mode::Static
            ),
            Ok(Some(words.clone()))
        );
        let optional = OirType::optional(words.clone());
        assert_eq!(
            call(&merge, &[ty(optional), ty(words.clone())], Mode::Static),
            Ok(Some(words.clone())),
            "an optional map merges like a map"
        );
        let named = struct_of(&[("a", OirType::String), ("b", OirType::String)]);
        assert_eq!(
            call(&merge, &[ty(words.clone()), ty(named)], Mode::Static),
            Ok(Some(words.clone())),
            "a record whose members are the map's value type adds its keys to the map"
        );
        let counts = OirType::map(OirType::String, OirType::Number);
        assert_eq!(
            call(&merge, &[ty(words.clone()), ty(counts)], Mode::Static),
            Ok(None),
            "control: maps of unlike values have no one value type"
        );
        let mixed = struct_of(&[("a", OirType::String), ("n", OirType::Number)]);
        assert_eq!(
            call(&merge, &[ty(words), ty(mixed)], Mode::Static),
            Ok(None),
            "control: a member of another type"
        );
    }

    #[test]
    fn a_rest_parameter_covers_every_further_argument() {
        let concat = op(json!([p("...arrs", list(var("T")))]), list(var("T")), None);
        assert!(call(
            &concat,
            &[
                ty(s_arr(OirType::Number)),
                ty(s_arr(OirType::Number)),
                ty(s_arr(OirType::Number))
            ],
            Mode::Static
        )
        .is_ok());
        assert!(call(
            &concat,
            &[ty(s_arr(OirType::Number)), ty(s_arr(OirType::String))],
            Mode::Static
        )
        .is_err());
    }

    #[test]
    fn a_shape_argument_names_the_result_type() {
        let parse = op(
            json!([
                p("s", json!("string")),
                p("shape", json!({ "kind": "shape" }))
            ]),
            json!({ "kind": "shape" }),
            Some(2),
        );
        assert_eq!(
            call(
                &parse,
                &[ty(OirType::String), text("Program")],
                Mode::Static
            ),
            Ok(Some(OirType::TypeRef {
                name: "Program".into()
            }))
        );
        assert!(call(
            &parse,
            &[ty(OirType::String), ty(OirType::Number)],
            Mode::Static
        )
        .is_err());
    }

    #[test]
    fn at_run_time_a_shape_argument_is_a_string() {
        let parse = op(
            json!([
                p("s", json!("string")),
                p("shape", json!({ "kind": "shape" }))
            ]),
            json!({ "kind": "shape" }),
            Some(2),
        );
        assert!(call(
            &parse,
            &[ty(OirType::String), ty(OirType::String)],
            Mode::Runtime
        )
        .is_ok());
        assert_eq!(
            call(
                &parse,
                &[ty(OirType::String), ty(OirType::Number)],
                Mode::Runtime
            ),
            Err(Mismatch::Type {
                expected: "string".into(),
                actual: "number".into()
            })
        );
    }

    #[test]
    fn a_nominal_std_type_is_not_checked_and_a_date_takes_a_string_or_a_number() {
        let nominal = op(
            json!([p("x", json!("tensor")), p("d", json!("date"))]),
            json!("boolean"),
            Some(2),
        );
        assert!(call(
            &nominal,
            &[ty(OirType::Number), ty(OirType::String)],
            Mode::Static
        )
        .is_ok());
        assert!(call(
            &nominal,
            &[ty(OirType::Boolean), ty(OirType::Number)],
            Mode::Static
        )
        .is_ok());
        assert!(call(
            &nominal,
            &[ty(OirType::Boolean), ty(OirType::Boolean)],
            Mode::Static
        )
        .is_err());
    }

    #[test]
    fn a_union_parameter_binds_the_member_that_fits() {
        let len = op(
            json!([p(
                "value",
                json!({ "kind": "union", "of": ["string", list(var("T"))] })
            )]),
            json!("boolean"),
            Some(1),
        );
        assert!(call(&len, &[ty(OirType::String)], Mode::Static).is_ok());
        assert!(call(&len, &[ty(s_arr(OirType::Number))], Mode::Static).is_ok());
        assert_eq!(
            call(&len, &[ty(OirType::Number)], Mode::Static),
            Err(Mismatch::Type {
                expected: "string | array".into(),
                actual: "number".into()
            })
        );
    }

    #[test]
    fn a_union_argument_binds_when_every_variant_fits_a_union_parameter() {
        let spawn = op(
            json!([p("command", json!({ "kind": "union", "of": ["string", "object"] }))]),
            json!("boolean"),
            Some(1),
        );
        let config = struct_of(&[("cmd", OirType::String)]);
        let string_or_config = OirType::Union {
            variants: vec![
                OirField::new("string", OirType::String),
                OirField::new("config", config),
            ],
        };
        assert!(call(&spawn, &[ty(string_or_config)], Mode::Static).is_ok());
    }

    #[test]
    fn control_a_union_argument_with_a_variant_no_member_fits_is_refused() {
        let show = op(
            json!([p("v", json!({ "kind": "union", "of": ["string", "number"] }))]),
            json!("boolean"),
            Some(1),
        );
        let string_or_boolean = OirType::Union {
            variants: vec![
                OirField::new("string", OirType::String),
                OirField::new("boolean", OirType::Boolean),
            ],
        };
        assert!(call(&show, &[ty(string_or_boolean)], Mode::Static).is_err());
    }

    #[test]
    fn a_nullable_result_is_optional() {
        let max = op(
            json!([p("arr", list("number".into()))]),
            json!({ "kind": "union", "of": ["number", { "kind": "literal", "value": null }] }),
            Some(1),
        );
        assert_eq!(
            call(&max, &[ty(s_arr(OirType::Number))], Mode::Static),
            Ok(Some(OirType::optional(OirType::Number)))
        );
    }

    #[test]
    fn a_failed_bind_leaves_the_bindings_untouched() {
        let eq = op(
            json!([p("a", var("T")), p("b", var("T"))]),
            json!("boolean"),
            Some(2),
        );
        let sig = Signature::of(&eq);
        let mut b = CallBindings::new();
        sig.bind(&mut b, 0, &ty(OirType::Number), Mode::Static)
            .unwrap();
        assert!(sig
            .bind(&mut b, 1, &ty(OirType::String), Mode::Static)
            .is_err());
        assert!(sig
            .bind(&mut b, 1, &ty(OirType::Number), Mode::Static)
            .is_ok());
    }

    #[test]
    fn a_literal_sequence_is_a_list_of_one_type_or_a_tuple_of_many() {
        let each = json!({ "kind": "var", "name": "T", "each": true });
        let list = op(json!([p("...items", each.clone())]), list(each), None);
        assert_eq!(
            call(
                &list,
                &[ty(OirType::Number), ty(OirType::Number)],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::Number)))
        );
        assert_eq!(
            call(
                &list,
                &[ty(OirType::Number), ty(OirType::String)],
                Mode::Static
            ),
            Ok(Some(OirType::Tuple {
                items: vec![OirType::Number, OirType::String]
            }))
        );
    }

    fn undetermined() -> Actual<'static> {
        Actual {
            known: None,
            text: None,
            texts: None,
            lambda: None,
        }
    }

    fn absent() -> Actual<'static> {
        Actual {
            known: Some(Known::Absent),
            text: None,
            texts: None,
            lambda: None,
        }
    }

    fn is_undetermined(r: Result<Option<OirType>, Mismatch>) -> bool {
        matches!(r, Err(Mismatch::Undetermined { .. }))
    }

    #[test]
    fn a_literal_sequence_with_an_undetermined_item_is_an_error() {
        let each = json!({ "kind": "var", "name": "T", "each": true });
        let list = op(json!([p("...items", each.clone())]), list(each), None);
        assert!(is_undetermined(call(
            &list,
            &[undetermined(), ty(OirType::Number)],
            Mode::Static
        )));
        assert!(is_undetermined(call(
            &list,
            &[ty(OirType::Number), undetermined()],
            Mode::Static
        )));
        assert_eq!(
            call(&list, &[ty(OirType::Number)], Mode::Static),
            Ok(Some(s_arr(OirType::Number))),
            "control: with every item determined it is typed"
        );
        assert!(
            is_undetermined(call(&list, &[ty(OirType::Json)], Mode::Static)),
            "json is no type"
        );
    }

    #[test]
    fn a_variable_handed_an_undetermined_or_opaque_value_is_an_error() {
        let prepend = op(
            json!([p("arr", list(var("T"))), p("item", var("T"))]),
            list(var("T")),
            Some(2),
        );
        assert_eq!(
            call(
                &prepend,
                &[ty(s_arr(OirType::String)), ty(OirType::String)],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::String))),
            "control: a list of strings and a string is a list of strings"
        );
        assert!(is_undetermined(call(
            &prepend,
            &[ty(s_arr(OirType::Json)), ty(OirType::String)],
            Mode::Static
        )));
        assert!(is_undetermined(call(
            &prepend,
            &[undetermined(), ty(OirType::String)],
            Mode::Static
        )));
        assert!(is_undetermined(call(
            &prepend,
            &[ty(s_arr(OirType::String)), undetermined()],
            Mode::Static
        )));
        let pick = op(
            json!([p("c", json!("boolean")), p("a", var("T")), p("b", var("T"))]),
            var("T"),
            Some(3),
        );
        assert!(is_undetermined(call(
            &pick,
            &[ty(OirType::Boolean), undetermined(), ty(OirType::Number)],
            Mode::Static
        )));
        assert_eq!(
            call(
                &pick,
                &[
                    ty(OirType::Boolean),
                    ty(OirType::Number),
                    ty(OirType::Number)
                ],
                Mode::Static
            ),
            Ok(Some(OirType::Number)),
            "control"
        );
        let heterogeneous = OirType::Tuple {
            items: vec![OirType::Number, OirType::String],
        };
        assert!(is_undetermined(call(
            &prepend,
            &[ty(heterogeneous), ty(OirType::String)],
            Mode::Static
        )));
    }

    fn nth_operator() -> EmbeddedOperator {
        let each = json!({ "kind": "var", "name": "E", "each": true });
        op(
            json!([
                p(
                    "arr",
                    json!({ "kind": "union", "of": [list(var("T")), list(each)] })
                ),
                p("index", json!("number"))
            ]),
            var("T"),
            Some(2),
        )
    }

    #[test]
    fn a_pair_of_unlike_types_is_indexed_by_array_nth_and_a_list_still_types_its_element() {
        let nth = nth_operator();
        let pair = OirType::Tuple {
            items: vec![OirType::String, OirType::Number],
        };
        assert!(
            call(&nth, &[ty(pair), ty(OirType::Number)], Mode::Static).is_ok(),
            "an unlike pair is addressable"
        );
        assert_eq!(
            call(
                &nth,
                &[ty(s_arr(OirType::Number)), ty(OirType::Number)],
                Mode::Static
            ),
            Ok(Some(OirType::Number)),
            "control: a list of one type still gives its element type"
        );
        assert!(
            call(
                &nth,
                &[ty(OirType::Number), ty(OirType::Number)],
                Mode::Static
            )
            .is_err(),
            "control: a number is no list"
        );
    }

    #[test]
    fn tuples_of_unlike_widths_are_one_list() {
        let prepend = op(
            json!([p("arr", list(var("T"))), p("item", var("T"))]),
            list(var("T")),
            Some(2),
        );
        let tuple = |items: Vec<OirType>| OirType::Tuple { items };
        let delete = tuple(vec![OirType::String, OirType::String, OirType::String]);
        let update = tuple(vec![
            OirType::String,
            OirType::String,
            OirType::String,
            OirType::Number,
        ]);
        assert!(call(
            &prepend,
            &[ty(s_arr(update.clone())), ty(delete.clone())],
            Mode::Static
        )
        .is_ok());
        assert!(call(
            &prepend,
            &[ty(s_arr(delete.clone())), ty(update.clone())],
            Mode::Static
        )
        .is_ok());
        assert!(
            call(
                &prepend,
                &[ty(s_arr(update.clone())), ty(s_arr(OirType::String))],
                Mode::Static
            )
            .is_ok(),
            "a list of strings is the delete tuple"
        );
        let list = op(json!([p("arr", list(var("T")))]), list(var("T")), Some(1));
        assert!(call(
            &list,
            &[ty(tuple(vec![delete.clone(), update.clone(), delete]))],
            Mode::Static
        )
        .is_ok());
        assert!(
            call(
                &prepend,
                &[ty(s_arr(OirType::Boolean)), ty(update)],
                Mode::Static
            )
            .is_err(),
            "control: a list of booleans takes no tuple with no boolean slot"
        );
    }

    #[test]
    fn a_held_program_is_a_type_a_variable_binds_and_nothing_can_be_read_from() {
        let items = json!({ "kind": "var", "name": "T", "each": true });
        let literal = op(json!([p("...items", items.clone())]), list(items), None);
        assert_eq!(
            call(
                &literal,
                &[ty(OirType::SExpr), ty(OirType::SExpr)],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::SExpr))),
            "a list of held programs is a list of them"
        );
        assert!(
            is_undetermined(call(
                &literal,
                &[ty(OirType::SExpr), ty(OirType::Json)],
                Mode::Static
            )),
            "control: json beside it is still no type"
        );
        let prepend = op(
            json!([p("arr", list(var("T"))), p("item", var("T"))]),
            list(var("T")),
            Some(2),
        );
        assert_eq!(
            call(
                &prepend,
                &[ty(s_arr(OirType::SExpr)), ty(OirType::SExpr)],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::SExpr)))
        );
        assert_eq!(
            call(
                &prepend,
                &[ty(s_arr(OirType::SExpr)), ty(OirType::String)],
                Mode::Static
            ),
            Ok(Some(s_arr(OirType::SExpr))),
            "an atom is a program too"
        );
        assert!(
            call(
                &prepend,
                &[ty(s_arr(OirType::SExpr)), ty(OirType::Date)],
                Mode::Static
            )
            .is_err(),
            "control: a date is not program data"
        );
        let show = op(json!([p("v", var("T"))]), json!("string"), Some(1));
        assert_eq!(
            call(&show, &[ty(OirType::SExpr)], Mode::Static),
            Ok(Some(OirType::String))
        );
        assert!(
            is_undetermined(call(&show, &[ty(OirType::Json)], Mode::Static)),
            "control: json"
        );
        assert!(
            is_undetermined(call(
                &get(),
                &[ty(OirType::SExpr), text("name")],
                Mode::Static
            )),
            "a field cannot be read out of a held program"
        );
        assert!(
            is_undetermined(call(
                &get(),
                &[ty(OirType::SExpr), text("name"), ty(OirType::String)],
                Mode::Static
            )),
            "nor with a default"
        );
        let bag = OirType::map(OirType::String, OirType::SExpr);
        assert!(
            call(&get(), &[ty(bag), text("name")], Mode::Static).is_ok(),
            "control: a map of held programs is read by key"
        );
    }

    #[test]
    fn a_concrete_position_and_a_lambda_are_not_undetermined() {
        let upper = op(json!([p("s", json!("string"))]), json!("string"), Some(1));
        assert!(
            call(&upper, &[undetermined()], Mode::Static).is_ok(),
            "no variable is bound by a concrete position"
        );
    }

    #[test]
    fn the_null_value_binds_nothing_and_makes_the_result_optional() {
        let eq = op(
            json!([p("a", var("T")), p("b", var("T"))]),
            json!("boolean"),
            Some(2),
        );
        assert!(call(&eq, &[ty(OirType::String), absent()], Mode::Static).is_ok());
        assert!(call(&eq, &[absent(), absent()], Mode::Static).is_ok());
        let pick = op(
            json!([p("c", json!("boolean")), p("a", var("T")), p("b", var("T"))]),
            var("T"),
            Some(3),
        );
        assert_eq!(
            call(
                &pick,
                &[ty(OirType::Boolean), ty(OirType::Number), absent()],
                Mode::Static
            ),
            Ok(Some(OirType::optional(OirType::Number)))
        );
    }

    fn effect() -> Actual<'static> {
        Actual {
            known: Some(Known::Void),
            text: None,
            texts: None,
            lambda: None,
        }
    }

    /// The result type and whether the result is void, after binding every argument.
    fn run(
        operator: &EmbeddedOperator,
        args: &[Actual<'_>],
    ) -> Result<(Option<OirType>, bool), Mismatch> {
        let sig = Signature::of(operator);
        let mut b = CallBindings::new();
        for (i, a) in args.iter().enumerate() {
            sig.bind(&mut b, i, a, Mode::Static)?;
        }
        Ok((sig.result(&b), sig.result_is_void(&b)))
    }

    #[test]
    fn an_effect_is_void_and_a_conditional_of_effects_is_void() {
        let cond = op(
            json!([
                p("c", json!("boolean")),
                p("then", var("T")),
                p("else", var("T"))
            ]),
            var("T"),
            Some(3),
        );
        assert_eq!(
            run(&cond, &[ty(OirType::Boolean), effect(), effect()]),
            Ok((None, true))
        );
        assert_eq!(
            run(&cond, &[ty(OirType::Boolean), effect(), absent()]),
            Ok((None, true)),
            "an effect and null"
        );
        assert_eq!(
            run(
                &cond,
                &[
                    ty(OirType::Boolean),
                    ty(OirType::Number),
                    ty(OirType::Number)
                ]
            ),
            Ok((Some(OirType::Number), false)),
            "control: two numbers are a number"
        );
        assert!(
            matches!(
                run(
                    &cond,
                    &[ty(OirType::Boolean), effect(), ty(OirType::Number)]
                ),
                Err(Mismatch::Conflict { .. })
            ),
            "an effect and a value are not one type"
        );
        assert!(
            matches!(
                run(
                    &cond,
                    &[ty(OirType::Boolean), ty(OirType::Number), effect()]
                ),
                Err(Mismatch::Conflict { .. })
            ),
            "a value and an effect are not one type"
        );
    }

    #[test]
    fn a_sequence_is_the_type_of_its_last_expression() {
        let each = json!({ "kind": "var", "name": "T", "each": true });
        let seq = op(
            json!([p("...exprs", json!({ "kind": "sexpr", "of": each }))]),
            var("T"),
            None,
        );
        assert_eq!(run(&seq, &[effect(), effect()]), Ok((None, true)));
        assert_eq!(
            run(&seq, &[effect(), ty(OirType::Number)]),
            Ok((Some(OirType::Number), false))
        );
        assert_eq!(
            run(&seq, &[ty(OirType::Number), effect()]),
            Ok((None, true))
        );
    }

    #[test]
    fn an_effect_is_not_a_value_at_a_typed_position_and_an_operator_can_return_void() {
        let upper = op(json!([p("s", json!("string"))]), json!("string"), Some(1));
        assert_eq!(
            call(&upper, &[effect()], Mode::Static),
            Err(Mismatch::Type {
                expected: "string".into(),
                actual: "void".into()
            })
        );
        let when = op(
            json!([p("c", json!("boolean")), p("e", json!({ "kind": "sexpr" }))]),
            json!("void"),
            Some(2),
        );
        assert!(
            call(&when, &[ty(OirType::Boolean), effect()], Mode::Static).is_ok(),
            "an effect position takes an effect"
        );
        assert!(Signature::of(&when).returns_void());
        assert!(!Signature::of(&upper).returns_void());
    }

    fn empty() -> Actual<'static> {
        Actual {
            known: Some(Known::EmptyList),
            text: None,
            texts: None,
            lambda: None,
        }
    }

    #[test]
    fn an_empty_list_takes_its_type_from_a_sibling_binding_and_never_from_nothing() {
        let or = op(json!([p("...xs", var("T"))]), var("T"), None);
        assert_eq!(
            call(&or, &[ty(s_arr(OirType::String)), empty()], Mode::Static),
            Ok(Some(s_arr(OirType::String))),
            "a list of strings or the empty list is a list of strings"
        );
        assert_eq!(
            call(&or, &[empty(), ty(s_arr(OirType::String))], Mode::Static),
            Ok(Some(s_arr(OirType::String))),
            "the empty list met by a list of strings is that list"
        );
        assert!(
            is_undetermined(call(&or, &[empty(), empty()], Mode::Static)),
            "only empty lists: nothing types them"
        );
        assert!(
            call(&or, &[ty(OirType::Number), empty()], Mode::Static).is_err(),
            "control: a number is not a list"
        );
        assert!(
            call(&or, &[empty(), ty(OirType::Number)], Mode::Static).is_err(),
            "control: the empty list does not take a number's place either"
        );
        assert_eq!(
            call(&or, &[empty(), empty()], Mode::Runtime),
            Ok(Some(OirType::Tuple { items: Vec::new() })),
            "at run time two empty lists are two values"
        );

        let length = op(json!([p("arr", list(var("T")))]), json!("number"), Some(1));
        assert!(
            is_undetermined(call(&length, &[empty()], Mode::Static)),
            "the length of an empty list has no element type to bind"
        );
        assert_eq!(
            call(&length, &[ty(s_arr(OirType::String))], Mode::Static),
            Ok(Some(OirType::Number)),
            "control: the length of a typed list"
        );
        let strings = op(
            json!([p("arr", list(json!("string")))]),
            json!("number"),
            Some(1),
        );
        assert_eq!(
            call(&strings, &[empty()], Mode::Static),
            Ok(Some(OirType::Number)),
            "an empty list in a slot that declares its element needs no sibling"
        );

        let reduce = op(
            json!([
                p("arr", list(var("T"))),
                p("init", var("A")),
                p(
                    "fn",
                    json!({ "kind": "fn", "params": [var("A"), var("T")], "returns": var("A") })
                ),
            ]),
            var("A"),
            Some(3),
        );
        let fold = |returned: Option<OirType>| {
            let sig = Signature::of(&reduce);
            let mut b = CallBindings::new();
            sig.bind(&mut b, 0, &ty(s_arr(OirType::String)), Mode::Static)
                .unwrap();
            sig.bind(&mut b, 1, &empty(), Mode::Static).unwrap();
            let body = |_: &[Option<OirType>]| returned.clone();
            let lambda = Actual {
                known: None,
                text: None,
                texts: None,
                lambda: Some(&body),
            };
            sig.bind(&mut b, 2, &lambda, Mode::Static).unwrap();
            sig.finish(&b, 3)
                .map(|()| sig.result(&b))
                .map_err(|(_, m)| m)
        };
        assert_eq!(
            fold(Some(s_arr(OirType::String))),
            Ok(Some(s_arr(OirType::String))),
            "a fold from the empty list is the list its body builds"
        );
        assert!(
            is_undetermined(fold(Some(OirType::Tuple { items: Vec::new() }))),
            "control: a fold whose body also builds only empty lists is untyped"
        );
        assert!(
            is_undetermined(fold(None)),
            "control: a fold whose body is undetermined is untyped"
        );
    }

    #[test]
    fn at_run_time_a_value_always_has_a_kind() {
        let eq = op(
            json!([p("a", var("T")), p("b", var("T"))]),
            json!("boolean"),
            Some(2),
        );
        assert!(
            call(&eq, &[undetermined(), ty(OirType::String)], Mode::Runtime).is_ok(),
            "an absent value passes"
        );
    }

    #[test]
    fn a_semantic_string_and_a_reference_are_strings_and_a_scalar_is_checked() {
        let upper = op(json!([p("s", json!("string"))]), json!("string"), Some(1));
        for text in [
            OirType::Email,
            OirType::Url,
            OirType::Uuid,
            OirType::Reference {
                entity: "User".into(),
            },
        ] {
            assert!(
                call(&upper, &[ty(text.clone())], Mode::Static).is_ok(),
                "{text:?}"
            );
        }
        assert!(
            call(&upper, &[ty(OirType::Number)], Mode::Static).is_err(),
            "control"
        );
        let show = op(
            json!([p(
                "v",
                json!({ "kind": "union", "of": ["string", "number"] })
            )]),
            json!("string"),
            Some(1),
        );
        assert!(call(&show, &[ty(OirType::Number)], Mode::Static).is_ok());
        assert!(call(&show, &[ty(OirType::Boolean)], Mode::Static).is_err());
        assert!(call(&show, &[ty(OirType::array(OirType::String))], Mode::Static).is_err());
    }

    #[test]
    fn an_orbital_trait_and_behavior_value_are_the_records_the_language_declares() {
        let read = |value: OirType, key: &str| call(&get(), &[ty(value), text(key)], Mode::Static);
        assert_eq!(
            read(OirType::Orbital, "behavior"),
            Ok(Some(OirType::String))
        );
        assert_eq!(
            read(OirType::Orbital, "pages"),
            Ok(Some(OirType::optional(OirType::map(
                OirType::String,
                OirType::String
            )))),
            "pages is declared optional, so it may be absent"
        );
        assert_eq!(read(OirType::Trait, "trait"), Ok(Some(OirType::String)));
        assert_eq!(
            read(OirType::Behavior, "behavior"),
            Ok(Some(OirType::String))
        );
        assert_eq!(
            read(OirType::optional(OirType::Orbital), "orbital"),
            Ok(Some(OirType::String)),
            "an unbound value reads like a bound one"
        );
        assert!(
            read(OirType::Orbital, "nonsense").is_err(),
            "control: an orbital value declares no such member"
        );
        assert!(
            read(OirType::Behavior, "orbital").is_err(),
            "control: a behavior reference has no orbital"
        );
        assert!(
            read(OirType::Number, "name").is_err(),
            "control: a number is no record"
        );
        let with_default = call(
            &get(),
            &[ty(OirType::Orbital), text("nonsense"), ty(OirType::String)],
            Mode::Static,
        );
        assert_eq!(
            with_default,
            Ok(Some(OirType::String)),
            "a default stands in for a member the value does not declare"
        );
    }

    #[test]
    fn a_member_of_a_value_that_is_program_data_is_held_as_such() {
        let app = call(&get(), &[ty(OirType::Orbital), text("app")], Mode::Static).unwrap();
        assert_eq!(
            app,
            Some(OirType::optional(OirType::map(OirType::String, OirType::SExpr)))
        );
    }

    mod field_projection_contract {
        use super::*;

        fn projection(operator: &EmbeddedOperator) -> Option<(usize, usize, Option<usize>)> {
            Signature::of(operator)
                .field_projection()
                .map(|projection| {
                    (
                        projection.source_arg,
                        projection.key_arg,
                        projection.default_arg,
                    )
                })
        }

        #[test]
        fn field_projection_uses_the_existing_declared_get_relationship() {
            assert_eq!(projection(&get()), Some((0, 1, Some(2))));
            assert_eq!(projection(&get_of_record_or_list()), Some((0, 1, Some(2))));
        }

        #[test]
        fn field_projection_can_have_no_default_formal() {
            let operator = op(
                json!([
                    p("value", record("R")),
                    p("selector", json!({ "kind": "keyOf", "of": "R" }))
                ]),
                json!({ "kind": "fieldOf", "of": "R" }),
                Some(2),
            );
            assert_eq!(projection(&operator), Some((0, 1, None)));
        }

        #[test]
        fn field_projection_ignores_names_categories_and_parameter_positions() {
            for name in ["object/get", "domain/project-member"] {
                let mut operator = op(
                    json!([
                        p("chosen", json!({ "kind": "keyOf", "of": "LedgerRow" })),
                        p("otherwise", json!({ "kind": "fieldOf", "of": "LedgerRow" })),
                        p("unrelated", json!("boolean")),
                        p("document", record("LedgerRow")),
                    ]),
                    json!({ "kind": "fieldOf", "of": "LedgerRow" }),
                    Some(4),
                );
                operator.category = format!("custom-{name}");
                operator.description = name.into();
                assert_eq!(projection(&operator), Some((3, 0, Some(1))), "{name}");
            }
        }

        #[test]
        fn field_projection_requires_a_direct_field_of_return() {
            for returns in [
                json!("string"),
                var("R"),
                json!({ "kind": "keyOf", "of": "R" }),
                list(json!({ "kind": "fieldOf", "of": "R" })),
            ] {
                let mut operator = get();
                operator.return_type = serde_json::from_value(returns).unwrap();
                assert_eq!(projection(&operator), None);
            }
        }

        #[test]
        fn field_projection_refuses_a_mismatched_source_variable() {
            let operator = op(
                json!([
                    p("obj", record("Other")),
                    p("key", json!({ "kind": "keyOf", "of": "R" }))
                ]),
                json!({ "kind": "fieldOf", "of": "R" }),
                Some(2),
            );
            assert_eq!(projection(&operator), None);
        }

        #[test]
        fn field_projection_refuses_a_mismatched_key_variable() {
            let operator = op(
                json!([
                    p("obj", record("R")),
                    p("key", json!({ "kind": "keyOf", "of": "Other" }))
                ]),
                json!({ "kind": "fieldOf", "of": "R" }),
                Some(2),
            );
            assert_eq!(projection(&operator), None);
        }

        #[test]
        fn field_projection_refuses_a_mismatched_return_variable() {
            let mut operator = get();
            operator.return_type =
                serde_json::from_value(json!({ "kind": "fieldOf", "of": "Other" })).unwrap();
            assert_eq!(projection(&operator), None);
        }

        #[test]
        fn field_projection_refuses_ambiguous_sources() {
            let operator = op(
                json!([
                    p("first", record("R")),
                    p("second", record("R")),
                    p("key", json!({ "kind": "keyOf", "of": "R" }))
                ]),
                json!({ "kind": "fieldOf", "of": "R" }),
                Some(3),
            );
            assert_eq!(projection(&operator), None);
        }

        #[test]
        fn field_projection_refuses_ambiguous_keys() {
            let operator = op(
                json!([
                    p("obj", record("R")),
                    p("first", json!({ "kind": "keyOf", "of": "R" })),
                    p("second", json!({ "kind": "keyOf", "of": "R" }))
                ]),
                json!({ "kind": "fieldOf", "of": "R" }),
                Some(3),
            );
            assert_eq!(projection(&operator), None);
        }

        #[test]
        fn field_projection_refuses_ambiguous_defaults() {
            let operator = op(
                json!([
                    p("obj", record("R")),
                    p("key", json!({ "kind": "keyOf", "of": "R" })),
                    p("first", json!({ "kind": "fieldOf", "of": "R" })),
                    p("second", json!({ "kind": "fieldOf", "of": "R" }))
                ]),
                json!({ "kind": "fieldOf", "of": "R" }),
                Some(4),
            );
            assert_eq!(projection(&operator), None);
        }

        #[test]
        fn field_projection_only_counts_relationships_for_the_return_variable() {
            let operator = op(
                json!([
                    p("other", record("Other")),
                    p("otherKey", json!({ "kind": "keyOf", "of": "Other" })),
                    p("obj", record("R")),
                    p("key", json!({ "kind": "keyOf", "of": "R" })),
                    p("unrelated", json!({ "kind": "fieldOf", "of": "Other" })),
                ]),
                json!({ "kind": "fieldOf", "of": "R" }),
                Some(5),
            );
            assert_eq!(projection(&operator), Some((2, 3, None)));
        }

        #[test]
        fn field_projection_does_not_unwrap_nested_formals() {
            for params in [
                json!([
                    p("obj", list(record("R"))),
                    p("key", json!({ "kind": "keyOf", "of": "R" }))
                ]),
                json!([
                    p("obj", record("R")),
                    p("key", list(json!({ "kind": "keyOf", "of": "R" })))
                ]),
            ] {
                let operator = op(params, json!({ "kind": "fieldOf", "of": "R" }), Some(2));
                assert_eq!(projection(&operator), None);
            }
        }
    }
}
