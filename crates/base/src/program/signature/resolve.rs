//! Resolving a formal type from a call's bindings.

use super::*;

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
                    // The key is a computed value: only a record whose every field has one type answers it. A key the
                    // map does not hold answers null, unless a default answers it (a list index reads like `array/nth`,
                    // which the owner ruled is not optional).
                    let missing = |found: &OirType| {
                        if b.defaulted.contains(of) || found.is_optional() {
                            found.clone()
                        } else {
                            OirType::optional(found.clone())
                        }
                    };
                    return match unwrap_optional(record) {
                        OirType::Map { value, .. } => Some(missing(value)),
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
                match walk_path_in(record, key, &|name| b.named_types.get(name).cloned()) {
                    // A read through a record that may be missing may be missing too, unless a default answers it.
                    Path::Found(found)
                        if matches!(record, OirType::Optional { .. })
                            && !found.is_optional()
                            && !b.defaulted.contains(of) =>
                    {
                        Some(OirType::optional(found))
                    }
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
