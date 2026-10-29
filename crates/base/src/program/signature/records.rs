//! Concrete and language-record types an operator signature names.

use super::*;

/// The concrete type a declared name stands for in a result. `void` has none.
pub(super) fn checks(ty: &OperatorTypeRef) -> bool {
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

/// The concrete type a declared primitive name stands for (`"instant"`), or `None` for `void`.
pub fn concrete_type(name: &str) -> Option<OirType> {
    concrete(name)
}

pub(super) fn concrete(name: &str) -> Option<OirType> {
    Some(match name {
        "number" => OirType::Number,
        "string" => OirType::String,
        "boolean" => OirType::Boolean,
        "date" => OirType::Date,
        "datetime" => OirType::DateTime,
        "instant" => OirType::Instant,
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
pub(super) fn checkable(name: &str) -> Option<OirType> {
    match name {
        "number" => Some(OirType::Number),
        "string" => Some(OirType::String),
        "boolean" => Some(OirType::Boolean),
        "date" => Some(OirType::Date),
        "datetime" => Some(OirType::DateTime),
        "instant" => Some(OirType::Instant),
        _ => None,
    }
}

pub(super) fn unwrap_optional(ty: &OirType) -> &OirType {
    match ty {
        OirType::Optional { inner } => unwrap_optional(inner),
        other => other,
    }
}

/// A type whose value says nothing the checker can use.
pub(super) fn is_uninformative(ty: &OirType) -> bool {
    crate::type_compat::is_json_family(unwrap_optional(ty))
}

pub(super) enum Record<'t> {
    Fields(&'t [OirField]),
    Uniform(&'t OirType, &'t OirType),
    Opaque,
}

/// The name the installed language gives the record an orbital, trait or behavior value is.
pub(super) fn language_value_name(ty: &OirType) -> Option<&'static str> {
    match ty {
        OirType::Orbital => Some("OrbitalValue"),
        OirType::Trait => Some("TraitValue"),
        OirType::Behavior => Some("BehaviorRef"),
        _ => None,
    }
}

/// `ty`, or the record the language declares for it when it is an orbital, trait or behavior value.
pub(super) fn language_value(ty: &OirType) -> &OirType {
    language_value_name(ty)
        .and_then(crate::language::program_types::program_type)
        .unwrap_or(ty)
}

pub(super) fn language_record(name: &str) -> Record<'static> {
    match crate::language::program_types::program_type(name) {
        Some(OirType::Struct { fields, .. }) => Record::Fields(fields),
        _ => Record::Opaque,
    }
}

pub(super) fn record_of<'a>(ty: &'a OirType, bindings: &CallBindings) -> Option<Record<'a>> {
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
                &OirType::Struct { fields: crate::ir::types::no_fields(), additional: None },
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
