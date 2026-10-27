//! Binding a call's arguments to the signature's formals.

use super::*;

pub(super) fn bind_type(
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
    if let (Mode::Proof, Known::Type(ty)) = (mode, known) {
        if is_uninformative(ty) {
            return Err(undetermined(why_uninformative(ty)));
        }
        if let OirType::Union { variants } = unwrap_optional(ty) {
            if !variants.is_empty() {
                let mut trial = b.clone();
                for variant in variants {
                    bind_type(formal, &element(&variant.field_type), &mut trial, mode)?;
                }
                *b = trial;
                return Ok(());
            }
        }
    }
    match formal {
        OperatorTypeRef::Primitive(name) => {
            let Some(expected) = checkable(name) else {
                return Ok(());
            };
            match known {
                Known::Type(ty) if fits(ty, &expected, b, mode) => Ok(()),
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
                    bind_bound(bound, known, formal, actual, b, mode)?;
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
                        expected: type_label(prior, mode),
                        actual: type_label(ty, mode),
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
                if matches!(known, Known::Type(ty) if !ty.is_optional()) || matches!(known, Known::EmptyList) {
                    b.defaulted.insert(of.clone());
                }
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
                        && !fits(ty, &declared, b, mode)
                    {
                        return Err(Mismatch::Type {
                            expected: type_label(&declared, mode),
                            actual: type_label(ty, mode),
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
                (None, Mode::Static | Mode::Proof) => Err(Mismatch::Type {
                    expected: "a type name".to_string(),
                    actual: kind_of(&actual.known),
                }),
            },
        },
    }
}

/// The checks that need no value: a lambda's result, a literal key, a declared type name.
pub(super) fn bind_unknown(
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
    if mode != Mode::Runtime && valueless && binds_variable(formal) {
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
pub(super) fn bind_void(
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
pub(super) fn note_absent(formal: &OperatorTypeRef, b: &mut CallBindings) {
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

pub(super) fn element(ty: &OirType) -> Actual<'static> {
    Actual::of_type(ty.clone())
}

pub(super) fn bind_bound(
    bound: &OperatorTypeRef,
    known: &Known,
    formal: &OperatorTypeRef,
    actual: &Actual<'_>,
    bindings: &CallBindings,
    mode: Mode,
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
    scratch.use_named_types(std::sync::Arc::clone(&bindings.named_types.0));
    let checked = if mode == Mode::Proof { Mode::Proof } else { Mode::Runtime };
    match bind_type(bound, actual, &mut scratch, checked) {
        Ok(()) => Ok(()),
        // A bound is a constraint on the kind: the elements of a list that fits it are the variable's own business.
        Err(Mismatch::Undetermined { .. }) if mode != Mode::Proof => Ok(()),
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
pub(super) fn bind_literal_keys(
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

pub(super) fn bind_key(
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
        let known_text = !matches!(
            unwrap_optional(ty),
            OirType::String | OirType::Enum { .. } | OirType::FieldRef { .. }
        );
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
        if mode != Mode::Runtime {
            let walked = walk_path_in(&record, path, &|name| b.named_types.get(name).cloned());
            if let Path::Missing = walked {
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
