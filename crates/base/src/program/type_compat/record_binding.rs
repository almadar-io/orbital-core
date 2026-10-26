//! What a render-ui prop binding carries and when it mismatches a single-record prop.

/// What a render-ui prop binding carries, as far as a single-record prop is
/// concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundShape {
    /// A list of rows (`[T]`, `array`).
    Collection,
    /// One row: of the named type (`Order`), or an unnamed `object`.
    Record(Option<String>),
    /// Anything a record check has no opinion on (scalars, …).
    Other,
}

/// Why a binding cannot fill a single-record prop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordBindingMismatch {
    CollectionIntoRecord,
    WrongEntity { expected: String, actual: String },
}

/// Classify a declared payload/field type string. `None` = undecidable here:
/// `@entity` / `$E` sentinels are fixed only once a consumer binds the trait.
pub fn bound_shape_of_type_str(declared: &str) -> Option<BoundShape> {
    let t = declared.trim().trim_end_matches('!');
    if t.starts_with('@') || t.starts_with('$') || t.starts_with("[@") || t.starts_with("[$") {
        return None;
    }
    if t == "array" || (t.starts_with('[') && t.ends_with(']')) {
        return Some(BoundShape::Collection);
    }
    if t == "object" {
        return Some(BoundShape::Record(None));
    }
    if t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return Some(BoundShape::Record(Some(t.to_string())));
    }
    Some(BoundShape::Other)
}

/// Classify a LOWERED payload field: a record/collection of an entity keeps the
/// entity name in its own `entity` slot (`"object"` + `Staff`), not in the type.
pub fn bound_shape_of_payload_field(field_type: &str, entity: Option<&str>) -> Option<BoundShape> {
    if entity.is_some_and(|e| e.starts_with('@') || e.starts_with('$')) {
        return None;
    }
    match (bound_shape_of_type_str(field_type)?, entity) {
        (BoundShape::Record(None), Some(e)) => Some(BoundShape::Record(Some(e.to_string()))),
        (shape, _) => Some(shape),
    }
}

/// A single-record prop (declared object-only) bound to `bound`. `expected`
/// is the entity the prop renders (the trait's linked entity) when known;
/// `is_entity` limits the identity compare to declared entities, so a struct
/// alias is never mistaken for a row of the wrong entity.
pub fn record_prop_binding_mismatch(
    bound: &BoundShape,
    expected: Option<&str>,
    is_entity: impl Fn(&str) -> bool,
) -> Option<RecordBindingMismatch> {
    match bound {
        BoundShape::Collection => Some(RecordBindingMismatch::CollectionIntoRecord),
        BoundShape::Record(Some(actual)) => match expected {
            Some(exp) if actual != exp && is_entity(actual) && is_entity(exp) => {
                Some(RecordBindingMismatch::WrongEntity {
                    expected: exp.to_string(),
                    actual: actual.clone(),
                })
            }
            _ => None,
        },
        BoundShape::Record(None) | BoundShape::Other => None,
    }
}
