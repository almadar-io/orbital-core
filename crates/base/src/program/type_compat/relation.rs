//! The structural `Relation` that decides `sub` over every `OirType` variant.

use super::*;

pub(super) type NamedTypeResolver<'a> = &'a dyn Fn(&str) -> Option<OirType>;

pub(super) struct Relation<'a> {
    pub(super) resolve: Option<NamedTypeResolver<'a>>,
    pub(super) aliases: Vec<(OirType, OirType, usize)>,
    pub(super) checked: bool,
}

impl Relation<'_> {
    pub(super) fn sub(&mut self, src: &OirType, dst: &OirType, progress: usize) -> Option<Coercion> {
        let witness = self.sub_inner(src, dst, progress)?;
        if self.checked
            && matches!(
                witness,
                Coercion::Explicit
                    | Coercion::Refine(_)
                    | Coercion::Unwrap { checked: false, .. }
                    | Coercion::Widen(
                        WidenKind::StringToDate
                            | WidenKind::StringToDateTime
                    )
            )
        {
            return None;
        }
        Some(witness)
    }

    pub(super) fn sub_inner(&mut self, src: &OirType, dst: &OirType, progress: usize) -> Option<Coercion> {
        if let (OirType::Optional { inner: source }, OirType::Optional { inner: target }) = (src, dst) {
            return self.sub(source, target, progress).map(|c| Coercion::Wrap(Box::new(c)));
        }
        // An unrefined nullable source needs an explicit unwrap.
        if let OirType::Optional { inner } = dst {
            return self
                .sub(src, inner, progress)
                .map(|c| Coercion::Wrap(Box::new(c)));
        }
        if let OirType::Optional { inner } = src {
            return self.sub(inner, dst, progress).map(|c| Coercion::Unwrap {
                checked: false,
                inner: Box::new(c),
            });
        }
        if src == dst
            && (!self.checked
                || !matches!(
                    src,
                    OirType::Array { .. }
                        | OirType::Tuple { .. }
                        | OirType::Map { .. }
                        | OirType::Struct { .. }
                        | OirType::Union { .. }
                        | OirType::TypeRef { .. }
                        | OirType::Reference { .. }
                ))
        {
            return Some(Coercion::Identity);
        }
        // A relation read is the related row's id at rest (owner ruling 2026-10-25), so it fits a string.
        if matches!((src, dst), (OirType::Reference { .. }, OirType::String)) {
            return Some(Coercion::Erase { from: src.clone() });
        }
        let expanded_src = match (self.resolve, src) {
            (Some(resolve), OirType::TypeRef { name } | OirType::Reference { entity: name }) => {
                resolve(name)
            }
            _ => None,
        };
        let expanded_dst = match (self.resolve, dst) {
            (Some(resolve), OirType::TypeRef { name } | OirType::Reference { entity: name }) => {
                resolve(name)
            }
            _ => None,
        };
        if expanded_src.is_some() || expanded_dst.is_some() {
            if let Some((_, _, previous)) = self
                .aliases
                .iter()
                .rev()
                .find(|(s, d, _)| s == src && d == dst)
            {
                // Alias expansion alone never establishes a recursive structural match.
                return (progress > *previous).then_some(Coercion::Identity);
            }
            self.aliases.push((src.clone(), dst.clone(), progress));
            let result = self.sub(
                expanded_src.as_ref().unwrap_or(src),
                expanded_dst.as_ref().unwrap_or(dst),
                progress,
            );
            self.aliases.pop();
            return result;
        }
        if let (OirType::Enum { name: source }, OirType::Enum { name: target }) = (src, dst) {
            if let (Some(source), Some(target)) = (literal_union_members(source), literal_union_members(target)) {
                return source.iter().all(|member| target.contains(member)).then_some(Coercion::Identity);
            }
        }
        if self.checked
            && (matches!(src, OirType::TypeRef { .. }) || matches!(dst, OirType::TypeRef { .. }))
        {
            return None;
        }
        if self.checked
            && src == dst
            && (matches!(src, OirType::Reference { .. })
                || matches!(src, OirType::Struct { fields, additional: None } if fields.is_empty()))
        {
            return Some(Coercion::Identity);
        }
        if let OirType::Union { variants } = src {
            if !variants.is_empty() {
                return variants
                    .iter()
                    .map(|variant| self.sub(&variant.field_type, dst, progress))
                    .collect::<Option<Vec<_>>>()
                    .map(|coercions| {
                        if coercions.iter().any(Coercion::requires_cast) {
                            Coercion::Explicit
                        } else {
                            Coercion::Erase { from: src.clone() }
                        }
                    });
            }
        }
        if let OirType::Union { variants } = dst {
            if !variants.is_empty() {
                let mut explicit = false;
                for variant in variants {
                    if let Some(coercion) = self.sub(src, &variant.field_type, progress) {
                        if !coercion.requires_cast() {
                            return Some(Coercion::Widen(WidenKind::ToUnionMember));
                        }
                        explicit = true;
                    }
                }
                return explicit.then_some(Coercion::Explicit);
            }
        }
        // A field reference is a string at run time. The call site decides which name it holds.
        match (src, dst) {
            (
                OirType::FieldRef { record: source_record, value: source },
                OirType::FieldRef { record: target_record, value: target },
            ) => {
                self.sub(source_record, target_record, progress + 1)?;
                return match (source, target) {
                    (_, None) => Some(Coercion::Identity),
                    (Some(source), Some(target)) => self.sub(source, target, progress + 1),
                    (None, Some(_)) => None,
                };
            }
            (OirType::FieldRef { .. }, OirType::String) | (OirType::String, OirType::FieldRef { .. }) => {
                return Some(Coercion::Identity);
            }
            _ => {}
        }
        match (src, dst) {
            (
                OirType::Struct { fields: actual, additional: actual_additional },
                OirType::Struct { fields: expected, additional: expected_additional },
            ) if !expected.is_empty() || expected_additional.is_some() => {
                if self.checked && actual.is_empty() && actual_additional.is_none() && expected_additional.is_some() {
                    return None;
                }
                let mut explicit = false;
                for field in expected {
                    match actual.iter().find(|a| a.name == field.name) {
                        Some(found) => {
                            if field.required && !found.required && found.default.is_none() {
                                return None;
                            }
                            // A member neither side requires may be missing either way, whether it reads as `T?` or `T`.
                            let found_type = match (&found.field_type, field.required || found.required) {
                                (OirType::Optional { inner }, false) => inner.as_ref(),
                                (other, _) => other,
                            };
                            if self.checked || !is_unknowable(found_type) {
                                explicit |= self.sub(found_type, &field.field_type, progress + 1)?.requires_cast();
                            }
                        }
                        None if field.required => return None,
                        None => {
                            if let Some(additional) = actual_additional {
                                explicit |= self.sub(additional, &field.field_type, progress + 1)?.requires_cast();
                            }
                        }
                    }
                }
                if let Some(additional) = expected_additional {
                    for field in actual.iter().filter(|field| !expected.iter().any(|target| target.name == field.name)) {
                        explicit |= self.sub(&field.field_type, additional, progress + 1)?.requires_cast();
                    }
                    if let Some(actual_additional) = actual_additional {
                        explicit |= self.sub(actual_additional, additional, progress + 1)?.requires_cast();
                    }
                }
                return Some(if explicit { Coercion::Explicit } else { Coercion::Erase { from: src.clone() } });
            }
            (
                OirType::Map {
                    key: actual_key,
                    value: actual_value,
                },
                OirType::Map {
                    key: expected_key,
                    value: expected_value,
                },
            ) => {
                let mut explicit = self
                    .sub(actual_key, expected_key, progress + 1)?
                    .requires_cast();
                if self.checked || !is_unknowable(actual_value) && !is_json_family(expected_value) {
                    explicit |= self
                        .sub(actual_value, expected_value, progress + 1)?
                        .requires_cast();
                }
                return Some(if explicit {
                    Coercion::Explicit
                } else {
                    Coercion::Erase { from: src.clone() }
                });
            }
            (OirType::Struct { fields, additional }, OirType::Map { key, value }) if !fields.is_empty() || additional.is_some() => {
                let mut explicit = self
                    .sub(&OirType::String, key, progress + 1)?
                    .requires_cast();
                if let Some(additional) = additional {
                    explicit |= self.sub(additional, value, progress + 1)?.requires_cast();
                }
                for field in fields {
                    if self.checked || !is_unknowable(&field.field_type) && !is_json_family(value) {
                        explicit |= self
                            .sub(&field.field_type, value, progress + 1)?
                            .requires_cast();
                    }
                }
                return Some(if explicit {
                    Coercion::Explicit
                } else {
                    Coercion::Erase { from: src.clone() }
                });
            }
            (OirType::Map { value, .. }, OirType::Struct { fields, additional }) if !fields.is_empty() || additional.is_some() => {
                if self.checked && fields.iter().any(|f| f.required && f.default.is_none()) {
                    return None;
                }
                let mut explicit = false;
                if let Some(additional) = additional {
                    explicit |= self.sub(value, additional, progress + 1)?.requires_cast();
                }
                if self.checked || !is_unknowable(value) {
                    for field in fields {
                        explicit |= self
                            .sub(value, &field.field_type, progress + 1)?
                            .requires_cast();
                    }
                }
                return Some(if explicit {
                    Coercion::Explicit
                } else {
                    Coercion::Erase { from: src.clone() }
                });
            }
            _ => {}
        }
        if self.checked {
            if matches!(dst, OirType::Enum { .. }) {
                return None;
            }
            if matches!(dst, OirType::Struct { fields, additional: None } if fields.is_empty())
                && is_object_family(src)
            {
                return Some(Coercion::Erase { from: src.clone() });
            }
            if matches!(
                dst,
                OirType::Struct { .. }
                    | OirType::Map { .. }
                    | OirType::Union { .. }
                    | OirType::File
                    | OirType::Row { .. }
            ) {
                return None;
            }
        }
        match dst {
            OirType::Optional { .. } => unreachable!("Optional stripped above"),
            // Enum/Trait are erased to "string"; a string-kind source is the
            // kind-level equality the legacy predicate admitted.
            OirType::Enum { .. } | OirType::Trait => match src {
                _ if is_string_kind(src) => Some(Coercion::Identity),
                _ => None,
            },
            // An orbital value is only ever an orbital value (identity is caught above).
            OirType::Orbital => None,
            OirType::Behavior => None,
            OirType::String => match src {
                _ if is_string_kind(src) => Some(Coercion::Identity),
                _ if semantic_kind(src).is_some() => Some(Coercion::Widen(WidenKind::SemanticToString)),
                _ => None,
            },
            OirType::Number => match src {
                OirType::Integer => Some(Coercion::Widen(WidenKind::IntegerNumber)),
                OirType::Money => Some(Coercion::Widen(WidenKind::MoneyNumber)),
                _ => None,
            },
            // Integer is the narrower numeric kind: Number/Money do not narrow
            // back down to Integer (4.4); only Integer == Integer, caught above.
            OirType::Integer => None,
            // Only the clock makes an instant (identity, caught above).
            OirType::Instant => None,
            OirType::Boolean => None,
            OirType::Date => match src {
                _ if is_string_kind(src) => Some(Coercion::Widen(WidenKind::StringToDate)),
                _ => None,
            },
            OirType::DateTime => match src {
                _ if is_string_kind(src) => Some(Coercion::Widen(WidenKind::StringToDateTime)),
                _ => None,
            },
            OirType::Email | OirType::Url | OirType::Phone | OirType::Uuid | OirType::Image => {
                match src {
                    _ if is_string_kind(src) => {
                        Some(Coercion::Refine(semantic_kind(dst).expect("semantic dst")))
                    }
                    _ => None,
                }
            }
            OirType::Money => match src {
                OirType::Number | OirType::Integer => Some(Coercion::Widen(WidenKind::MoneyNumber)),
                _ => None,
            },
            // Bytes is an opaque base64 string at rest: only bytes (identity, caught above) fits.
            OirType::Bytes => None,
            // A call key is a string at rest; a string-kind source converts only through `as`.
            OirType::CallKey => match src {
                _ if is_string_kind(src) => Some(Coercion::Explicit),
                _ => None,
            },
            // Field names meet strings and each other before this match; nothing else is a field name.
            OirType::FieldRef { .. } => None,
            // The object family (Struct/Union/Map/File/TypeRef) and the json family
            // (Json/SExpr) are mutually compatible.
            OirType::Struct { .. }
            | OirType::Union { .. }
            | OirType::Map { .. }
            | OirType::File
            | OirType::TypeRef { .. }
            | OirType::Row { .. }
            | OirType::Json
            | OirType::SExpr => match src {
                _ if is_object_family(src) || is_json_family(src) => {
                    Some(Coercion::Erase { from: src.clone() })
                }
                OirType::Var { .. } if matches!(dst, OirType::Json) => {
                    Some(Coercion::Erase { from: src.clone() })
                }
                _ if matches!(dst, OirType::SExpr) && is_program_data(src) => {
                    Some(Coercion::Erase { from: src.clone() })
                }
                _ if primitive_union_member(dst, src) => {
                    Some(Coercion::Widen(WidenKind::ToUnionMember))
                }
                _ => None,
            },
            // Renderable content: object-family pattern trees, arrays of them,
            // string leaves — plus any scalar, which React paints as text (the
            // arm moved in from config_type.rs's file-local special case).
            OirType::Node => match src {
                _ if is_string_kind(src)
                    || is_number_kind(src)
                    || matches!(src, OirType::Boolean | OirType::Instant) =>
                {
                    Some(Coercion::Widen(WidenKind::ScalarToNode))
                }
                OirType::Array { .. } => Some(Coercion::Erase { from: src.clone() }),
                _ if is_object_family(src) => Some(Coercion::Erase { from: src.clone() }),
                _ => None,
            },
            // Structural (Phase 4.2): Array<T> ≤ Array<U> iff T ≤ U. Element
            // types that admit no coercion reject the assignment outright.
            OirType::Array { inner: d_inner } => match src {
                OirType::Array { inner: s_inner } => {
                    if !self.checked && is_unknowable(s_inner) {
                        Some(Coercion::Element(Box::new(Coercion::Erase {
                            from: (**s_inner).clone(),
                        })))
                    } else {
                        self.sub(s_inner, d_inner, progress + 1)
                            .map(|element| Coercion::Element(Box::new(element)))
                    }
                }
                // The empty list is typed by the list slot it fills: the slot supplies the element.
                OirType::Tuple { items } if items.is_empty() => Some(Coercion::Identity),
                // A tuple is an array of its slots: every slot must fit the element.
                OirType::Tuple { items } => items
                    .iter()
                    .map(|slot| self.sub(slot, d_inner, progress + 1))
                    .collect::<Option<Vec<_>>>()
                    .and_then(|coercions| {
                        if coercions.iter().any(Coercion::requires_cast) {
                            Some(Coercion::Explicit)
                        } else {
                            coercions
                                .into_iter()
                                .next()
                                .map(|element| Coercion::Element(Box::new(element)))
                        }
                    }),
                _ => None,
            },
            // A tuple takes a same-arity tuple slot by slot, or an array whose element fits
            // every slot (a literal's arity is checked where the literal is validated).
            OirType::Tuple { items: d_items } => match src {
                OirType::Tuple { items: s_items } if s_items.len() == d_items.len() => s_items
                    .iter()
                    .zip(d_items)
                    .map(|(s, d)| self.sub(s, d, progress + 1))
                    .collect::<Option<Vec<_>>>()
                    .map(|coercions| {
                        if coercions.iter().any(Coercion::requires_cast) {
                            Coercion::Explicit
                        } else {
                            Coercion::Identity
                        }
                    }),
                // An array of unknowable elements (a literal's erased `array` kind) erases.
                OirType::Array { inner } if !self.checked && is_unknowable(inner) => {
                    Some(Coercion::Erase { from: src.clone() })
                }
                OirType::Array { inner: s_inner } if !self.checked => d_items
                    .iter()
                    .map(|d| self.sub(s_inner, d, progress + 1))
                    .collect::<Option<Vec<_>>>()
                    .map(|coercions| {
                        if coercions.iter().any(Coercion::requires_cast) {
                            Coercion::Explicit
                        } else {
                            Coercion::Identity
                        }
                    }),
                _ => None,
            },
            // An entity field takes a loaded row (object family) or an id string.
            OirType::Reference { .. } => match src {
                _ if is_object_family(src) => Some(Coercion::Row { from_id: false }),
                _ if is_string_kind(src) => Some(Coercion::Row { from_id: true }),
                _ => None,
            },
            // Opaque: only the same variable (identity, caught above) or an unknowable value.
            OirType::Var { .. } => match src {
                OirType::Json => Some(Coercion::Erase { from: src.clone() }),
                _ => None,
            },
        }
    }
}
