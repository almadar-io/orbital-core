//! Walking field paths through a type and projecting its children.

use super::*;

pub(super) enum Path {
    Found(OirType),
    Opaque,
    Missing,
}

/// The type at a dot path through `ty`.
pub(super) fn walk_path(ty: &OirType, path: &str) -> Path {
    walk_path_in(ty, path, &|_| None)
}

/// [`walk_path`] where a segment's owner may be a declared name, read through `resolve`.
pub(super) fn walk_path_in(ty: &OirType, path: &str, resolve: &dyn Fn(&str) -> Option<OirType>) -> Path {
    let mut current = ty.clone();
    let mut may_be_absent = false;
    let segments = crate::value::split_path(path);
    for segment in segments.iter().map(|s| s.as_ref()) {
        let mut here = language_value(unwrap_optional(&current)).clone();
        let mut hops = 0;
        while let OirType::TypeRef { name } | OirType::Reference { entity: name } = &here {
            let Some(next) = resolve(name) else { break };
            hops += 1;
            if hops > 16 {
                break;
            }
            may_be_absent |= next.is_optional();
            here = language_value(unwrap_optional(&next)).clone();
        }
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
                    OirType::Union { variants: distinct.into() }
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
            | OirType::Reference { .. } => return Path::Opaque,
            _ => return Path::Missing,
        }
    }
    Path::Found(if may_be_absent && !current.is_optional() {
        OirType::optional(current)
    } else {
        current
    })
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
pub(super) fn children_of(
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
                .collect::<Option<Vec<_>>>()?.into(),
        }),
        OirType::Array { inner } if is_shape(inner) => Some(OirType::array(result.clone())),
        OirType::Map { key, value } if is_shape(value) => {
            Some(OirType::map((**key).clone(), result.clone()))
        }
        _ => Some(OirType::Struct { fields: crate::ir::types::no_fields(), additional: None }),
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
        _ => Some(OirType::Union { variants: kept.into() }),
    }
}
