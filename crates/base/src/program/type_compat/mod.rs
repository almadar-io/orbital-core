//! Canonical type-compatibility predicate shared across the Orbital crates.
//!
//! Single source of truth for "is this value's declared type assignable to the
//! declared entity-field type" in a `(set @entity.X <value>)` check. Both the
//! lolo L1 parse-time check and the compiler L2 post-resolve backstop call
//! this one function so the two layers can never drift.
//!
//! Phase 4.1 (spec: Almadar_Compiler_Spec.md Phase 4): the acceptance set is
//! now owned by the `sub`/`Coercion` relation — a structurally recursive,
//! exhaustive-by-construction function over all `OirType` variants, so adding
//! a variant forces a decision here instead of letting a string comparator
//! quietly answer `false`. `set_value_type_compatible` is a thin wrapper that
//! parses the legacy kind-string vocabulary into the relation. The relation
//! checks known structural members recursively. Schemaless records and opaque
//! recursive references remain undecidable; remaining leniency is enumerated
//! in `mod compat_tests`, not implicit.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::ir::types::OirType;

/// The semantic string domain a declared type names — the payload of a
/// [`Coercion::Refine`] (String → Email/Url/… is runtime-validated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticStringKind {
    Email,
    Url,
    Phone,
    Uuid,
    Image,
}

/// What a [`Coercion::Widen`] widens. The names describe the ASSIGNMENT
/// direction (src → dst); several cover both directions because the legacy
/// string predicate collapsed the numeric kinds and admitted both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidenKind {
    /// Integer ↔ Number (the legacy predicate erased both to `"number"`).
    IntegerNumber,
    /// Number/Integer ↔ Money (Money is numeric at rest).
    MoneyNumber,
    /// A value of one of a primitive union's member kinds joins the union.
    ToUnionMember,
    /// String-kind value parsed into a Date.
    StringToDate,
    /// String-kind value parsed into a DateTime.
    StringToDateTime,
    /// Any scalar renders as a node ("any rendered value is a ReactNode").
    ScalarToNode,
    /// A semantic string domain (email, url, phone, uuid, image) is a string.
    SemanticToString,
}

/// The witness for an accepted assignment `sub(src, dst) = Some(c)`: WHAT the
/// assignment costs, so widening becomes countable instead of invisible
/// (Type_Preservation §4.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Coercion {
    /// Structurally equal types (also kind-level equalities the legacy
    /// predicate could not distinguish, e.g. two different `Enum` names).
    Identity,
    Widen(WidenKind),
    /// T → Optional<T> (Phase 4.3), wrapping the inner witness.
    Wrap(Box<Coercion>),
    /// Optional<T> → T, unwrapping the inner witness: legal only through the run-time-checked `as` cast.
    Unwrap { inner: Box<Coercion> },
    /// Lift through Array value (structural since Phase 4.2 — the inner
    /// coercion is the element witness).
    Element(Box<Coercion>),
    /// Reference assignment: id string (`from_id: true`) or loaded row.
    Row {
        from_id: bool,
    },
    /// String-kind value entering a runtime-validated semantic domain.
    Refine(SemanticStringKind),
    /// Assignment that erases static information; `from` records what was
    /// lost (→ Json, cross-variant object family, → node content, → any).
    Erase {
        from: OirType,
    },
    /// A conversion that is legal only when spelled with the `as` cast
    /// (String → CallKey): plain assignment rejects it.
    Explicit,
}

impl Coercion {
    /// True when the conversion, or one nested in it, is legal only through `as`.
    pub fn is_explicit(&self) -> bool {
        match self {
            Coercion::Explicit => true,
            Coercion::Wrap(inner) | Coercion::Element(inner) | Coercion::Unwrap { inner } => inner.is_explicit(),
            _ => false,
        }
    }

    /// True when the value already has the target type: no unwrap, refinement, row load or cast is needed at run time.
    pub fn is_static(&self) -> bool {
        match self {
            Coercion::Identity | Coercion::Widen(_) | Coercion::Erase { .. } => true,
            Coercion::Wrap(inner) | Coercion::Element(inner) => inner.is_static(),
            Coercion::Unwrap { .. } | Coercion::Row { .. } | Coercion::Refine(_) | Coercion::Explicit => false,
        }
    }

    /// True when plain assignment refuses the conversion: one legal only through `as`, or an unwrap.
    pub fn requires_cast(&self) -> bool {
        match self {
            Coercion::Explicit => true,
            Coercion::Wrap(inner) | Coercion::Element(inner) => inner.requires_cast(),
            Coercion::Unwrap { .. } => true,
            _ => false,
        }
    }
}

/// Plain assignment: `src` converts to `dst` without a cast.
pub fn assignable(src: &OirType, dst: &OirType) -> bool {
    sub(src, dst).is_some_and(|c| !c.requires_cast())
}

/// The `as` cast: `src` converts to `dst`, with or without a cast.
pub fn castable(src: &OirType, dst: &OirType) -> bool {
    sub(src, dst).is_some()
}

pub fn is_object_family(t: &OirType) -> bool {
    matches!(
        t,
        OirType::Struct { .. }
            | OirType::Union { .. }
            | OirType::Map { .. }
            | OirType::File
            | OirType::TypeRef { .. }
            | OirType::Row { .. }
    )
}

/// What an S-expression can be besides an options object: an atom, or a list of S-expressions.
pub fn is_program_data(t: &OirType) -> bool {
    match t {
        OirType::Optional { inner } => is_program_data(inner),
        OirType::Boolean => true,
        OirType::Array { inner } => {
            is_program_data(inner) || is_object_family(inner) || is_json_family(inner)
        }
        OirType::Tuple { items } => items
            .iter()
            .all(|i| is_program_data(i) || is_object_family(i) || is_json_family(i)),
        OirType::Instant => true,
        _ => is_string_kind(t) || is_number_kind(t),
    }
}

/// The types that say nothing a check can use: the expression type and the erased variable.
pub fn is_json_family(t: &OirType) -> bool {
    matches!(t, OirType::SExpr) || t.is_erased()
}

/// Kinds the legacy predicate erased to `"string"`: String itself, and the
/// string-valued Enum/Trait (via `oir_type_kind`).
pub fn is_string_kind(t: &OirType) -> bool {
    matches!(
        t,
        OirType::String | OirType::Enum { .. } | OirType::Trait | OirType::CallKey
    )
}

/// Kinds erased to `"number"`: Number, Integer, and Money (numeric at rest).
pub fn is_number_kind(t: &OirType) -> bool {
    matches!(t, OirType::Number | OirType::Integer | OirType::Money)
}

pub fn semantic_kind(t: &OirType) -> Option<SemanticStringKind> {
    match t {
        OirType::Email => Some(SemanticStringKind::Email),
        OirType::Url => Some(SemanticStringKind::Url),
        OirType::Phone => Some(SemanticStringKind::Phone),
        OirType::Uuid => Some(SemanticStringKind::Uuid),
        OirType::Image => Some(SemanticStringKind::Image),
        _ => None,
    }
}

/// True when `dst` is a union of primitive kinds (`string | number`) and `src` fits one member.
pub fn primitive_union_member(dst: &OirType, src: &OirType) -> bool {
    let OirType::Union { variants } = dst else {
        return false;
    };
    variants.iter().any(|v| {
        !matches!(
            v.field_type,
            OirType::Struct { .. } | OirType::Union { .. } | OirType::Map { .. }
        ) && assignable(src, &v.field_type)
    })
}

/// The type relation: `Some(coercion)` when a value of type `src` may be
/// assigned to a slot of type `dst`, `None` on a clear kind contradiction.
///
/// Known container members compare recursively. Nullable sources require an explicit unwrap,
/// numeric kinds widen without narrowing, semantic strings refine at runtime,
/// references accept rows or id strings, and Node accepts renderable content.
/// Compare types with declared aliases resolved only when their shape is needed.
pub fn sub_with_named(
    src: &OirType,
    dst: &OirType,
    resolve: &dyn Fn(&str) -> Option<OirType>,
) -> Option<Coercion> {
    Relation {
        resolve: Some(resolve),
        aliases: Vec::new(),
        checked: false,
    }
    .sub(src, dst, 0)
}

/// An assignment whose declaration supplies no runtime conversion or check.
pub fn checked_sub_with_named(
    src: &OirType,
    dst: &OirType,
    resolve: &dyn Fn(&str) -> Option<OirType>,
) -> Option<Coercion> {
    Relation {
        resolve: Some(resolve),
        aliases: Vec::new(),
        checked: true,
    }
    .sub(src, dst, 0)
}

pub fn sub(src: &OirType, dst: &OirType) -> Option<Coercion> {
    Relation {
        resolve: None,
        aliases: Vec::new(),
        checked: false,
    }
    .sub(src, dst, 0)
}

mod relation;
mod compat_type;
mod oir_kind;
mod record_binding;
mod qualified;
use relation::*;
pub use qualified::{describe_type_name, qualified_type_name, rename_type_names, short_type_name, split_qualified_type_name, TYPE_QUALIFIER};
pub use compat_type::{
    decode_literal_union, literal_union_members, literal_union_text, parse_compat_type,
    set_value_type_compatible, sub_compat, tuple_type_slots, CompatType,
};
pub use oir_kind::{
    compat_kind_oir, enum_value_is_member, is_unknowable, oir_type_accepts_kind, oir_type_kind,
    oir_type_satisfies, oir_type_satisfies_field, oir_type_satisfies_with_named,
    semantic_string_kind,
};
pub use record_binding::{
    bound_shape_of_payload_field, bound_shape_of_type_str, record_prop_binding_mismatch,
    BoundShape, RecordBindingMismatch,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod compat_tests;

#[cfg(test)]
mod record_binding_tests;

#[cfg(test)]
mod tuple_type_tests;
