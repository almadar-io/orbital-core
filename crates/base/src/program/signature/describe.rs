//! Type labels and the text of a mismatch.

use super::*;

pub(super) fn expected_name(formal: &OperatorTypeRef) -> String {
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

pub(super) fn kind_of(known: &Option<Known>) -> String {
    match known {
        Some(Known::Type(ty)) => oir_type_kind(ty),
        Some(Known::EmptyList) => "array".to_string(),
        Some(Known::Absent) => "null".to_string(),
        Some(Known::Void) => "void".to_string(),
        None => "null".to_string(),
    }
}

pub(super) fn type_mismatch(formal: &OperatorTypeRef, actual: &Actual<'_>) -> Mismatch {
    Mismatch::Type {
        expected: expected_name(formal),
        actual: kind_of(&actual.known),
    }
}

/// A type as a finding names it: the kind at run time, where values carry no declared shape, and the shape
/// (`[{ id: string, text?: string }]`) over expression types, so two lists that differ only in their items read
/// as different.
pub(super) fn type_label(ty: &OirType, mode: Mode) -> String {
    match mode {
        Mode::Runtime => oir_type_kind(ty),
        Mode::Static | Mode::Proof => describe_type(ty),
    }
}

/// A type as a reader writes it: `[{ id: string, text?: string }]`, `Map string number`, `string?`.
pub fn describe_type(ty: &OirType) -> String {
    if is_absent_slot(ty) {
        return "_".to_string();
    }
    match ty {
        OirType::Array { inner } => format!("[{}]", describe_type(inner)),
        OirType::Tuple { items } => format!(
            "[{}]",
            items.iter().map(describe_type).collect::<Vec<_>>().join(", ")
        ),
        OirType::Optional { inner } => format!("{}?", describe_type(inner)),
        OirType::Struct { fields, additional } => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|field| {
                    format!(
                        "{}{}: {}",
                        field.name,
                        if field.required { "" } else { "?" },
                        describe_type(&field.field_type)
                    )
                })
                .collect();
            if let Some(additional) = additional {
                parts.push(format!("*: {}", describe_type(additional)));
            }
            format!("{{ {} }}", parts.join(", "))
        }
        OirType::Map { key, value } => {
            format!("Map {} {}", describe_type(key), describe_type(value))
        }
        OirType::Union { variants } => variants
            .iter()
            .map(|variant| describe_type(&variant.field_type))
            .collect::<Vec<_>>()
            .join(" | "),
        OirType::TypeRef { name } => crate::type_compat::describe_type_name(name),
        OirType::Enum { name } => crate::type_compat::literal_union_members(name)
            .map(|members| members.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(" | "))
            .unwrap_or_else(|| oir_type_kind(ty)),
        OirType::FieldRef { record, value: Some(value) } => {
            format!("field {} {}", describe_type(record), describe_type(value))
        }
        OirType::FieldRef { record, value: None } => format!("field {}", describe_type(record)),
        other => oir_type_kind(other),
    }
}


pub(super) fn undetermined(reason: &str) -> Mismatch {
    Mismatch::Undetermined {
        reason: reason.to_string(),
    }
}

/// A json or expression value at a position that declares a type: it is shown to have that type by `(as x T)`.
pub(super) fn loose(ty: &OirType, formal: &OperatorTypeRef) -> Mismatch {
    undetermined(&format!("{}, and `{}` is declared", why_uninformative(ty), expected_name(formal)))
}

pub(super) const NO_TYPE: &str = "its type is not determined";
pub(super) const IS_EXPRESSION: &str = "it is an expression value, which has no type of its own";
pub(super) const IS_JSON: &str = "it is json, which has no type";
pub(super) const NO_ELEMENT: &str = "its items do not share one type";
pub(super) const EMPTY_LIST: &str = "an empty list takes its element type from the slot it fills or a sibling argument, and nothing in this call types it";

pub(super) fn why_uninformative(ty: &OirType) -> &'static str {
    match unwrap_optional(ty) {
        OirType::SExpr => IS_EXPRESSION,
        _ => IS_JSON,
    }
}
