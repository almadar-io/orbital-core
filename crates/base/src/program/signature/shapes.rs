//! Type agreement and the joins of lists, tuples and structs.

use super::*;

pub(super) fn agrees(a: &OirType, b: &OirType) -> bool {
    sub(a, b).is_some() || sub(b, a).is_some()
}

pub(super) fn fits(actual: &OirType, expected: &OirType, bindings: &CallBindings, mode: Mode) -> bool {
    // A strict call refuses an optional value where a present one is declared.
    if mode == Mode::Proof
        && matches!(actual, OirType::Optional { .. })
        && !matches!(expected, OirType::Optional { .. })
    {
        return false;
    }
    // A plain string is not a date: the program parses it (`time/parse`) or reads a value declared `date`.
    if mode == Mode::Proof
        && matches!(unwrap_optional(actual), OirType::String)
        && matches!(expected, OirType::Date | OirType::DateTime | OirType::Instant)
    {
        return false;
    }
    let date_at_rest = matches!(unwrap_optional(actual), OirType::Date | OirType::DateTime | OirType::Instant)
        && matches!(expected, OirType::String);
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
        || (mode != Mode::Proof && is_uninformative(actual))
        || crate::type_compat::sub_with_named(actual, expected, &|name| {
            bindings.named_types.get(name).cloned()
        })
        .is_some()
}

/// Whether `actual` is a record (a struct, a map, a row).
pub(super) fn is_record(known: &Known, bindings: &CallBindings) -> bool {
    match known {
        Known::Type(ty) => record_of(ty, bindings).is_some() || is_uninformative(ty),
        Known::EmptyList | Known::Absent | Known::Void => false,
    }
}

/// Whether a value of no known type cannot be handed to `formal`: it declares a type the value must be shown to have.
pub(super) fn demands_type(formal: &OperatorTypeRef) -> bool {
    use crate::language::registry::OperatorTypeRefKind as K;
    match formal {
        OperatorTypeRef::Primitive(name) => {
            checkable(name).is_some()
                || matches!(name.as_str(), "bytes" | "vector" | "cell" | "rect" | "circle" | "point" | "segment")
        }
        OperatorTypeRef::Tagged(kind) => match kind {
            K::Array { .. }
            | K::Tuple { .. }
            | K::Map { .. }
            | K::Object { .. }
            | K::Record
            | K::Var { .. }
            | K::Literal { .. }
            | K::Entity { .. }
            | K::EntityRef
            | K::FieldOf { .. } => true,
            K::Union { of } => of.iter().all(demands_type),
            K::Binding { shape } => shape.as_deref().is_some_and(demands_type),
            K::Sexpr { of } => of.as_deref().is_some_and(demands_type),
            _ => false,
        },
    }
}

/// Whether `formal` binds a type variable to the value handed to it.
pub(super) fn binds_variable(formal: &OperatorTypeRef) -> bool {
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
pub(super) fn shared_tuple(entries: &[OirType]) -> Option<Vec<OirType>> {
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
pub(super) fn merge_list_shapes(a: &OirType, b: &OirType) -> Option<OirType> {
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
    if let (OirType::Array { inner: x }, OirType::Array { inner: y }) = (a, b) {
        if let Some(element) = merge_list_shapes(x, y) {
            return Some(OirType::array(element));
        }
    }
    joined_tuples(&[a.clone(), b.clone()]).map(|items| OirType::Tuple { items })
}

pub(super) fn fill_empty_list_slots(prior: &OirType, actual: &OirType) -> Option<OirType> {
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
                    **prior = filled;
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
pub(super) fn joined_tuples(entries: &[OirType]) -> Option<Vec<OirType>> {
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
pub(super) fn is_empty_list(ty: &OirType) -> bool {
    matches!(unwrap_optional(ty), OirType::Tuple { items } if items.is_empty())
}

/// The type variables `formal` names.
pub(super) fn variables_in(formal: &OperatorTypeRef, out: &mut Vec<String>) {
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


pub(super) fn has_typed_additional(ty: &OirType) -> bool {
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

pub(super) fn is_typed_record_join(all: &[OirType]) -> bool {
    all.iter().all(|ty| matches!(unwrap_optional(ty), OirType::Struct { .. }))
        && all.iter().any(has_typed_additional)
}

pub(super) fn join_struct_member_types(types: Vec<OirType>) -> Option<OirType> {
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
pub(super) fn unify_structs(all: &[OirType]) -> Option<OirType> {
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
    Some(OirType::Struct { fields: merged.into(), additional })
}
