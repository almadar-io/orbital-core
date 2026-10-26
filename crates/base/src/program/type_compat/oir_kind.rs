//! OIR type kinds and whether a type satisfies a declared field.

use super::*;

/// The semantic domain a declared type-name names, if it names one. The single
/// owner: L1 (lolo), L2 (validator) and `orbital-verify` all route through this
/// so no layer can invent its own vocabulary.
pub fn semantic_string_kind(declared: &str) -> Option<&'static str> {
    match declared.trim_end_matches('!') {
        "email" => Some("email"),
        "url" => Some("url"),
        "phone" => Some("phone"),
        "uuid" => Some("uuid"),
        "image" => Some("image"),
        _ => None,
    }
}

/// Returns `true` when `value` is one of the declared union `members`.
/// The single source for enum/union membership (Gap G-LOLO-ENUM-UNENFORCED) —
/// both the lolo author-time check and the compiler validator call this, so the
/// two layers cannot drift.
pub fn enum_value_is_member(value: &str, members: &[String]) -> bool {
    members.iter().any(|m| m == value)
}

/// Map an inferred `OirType` to the lowercase kind-string vocabulary that
/// `set_value_type_compatible` speaks (`string`/`number`/`boolean`/`date`/
/// `array`/`object`/`json`, or a PascalCase entity name for references).
/// `Optional` is transparent (nullability is not a kind); `Json`/`SExpr` are the
/// "unknowable" kinds.
pub fn oir_type_kind(t: &crate::ir::types::OirType) -> String {
    use crate::ir::types::OirType;
    match t {
        OirType::String => "string".to_string(),
        OirType::Number | OirType::Integer => "number".to_string(),
        OirType::Boolean => "boolean".to_string(),
        OirType::Date => "date".to_string(),
        OirType::DateTime => "datetime".to_string(),
        OirType::Instant => "instant".to_string(),
        OirType::Json | OirType::SExpr => "json".to_string(),
        OirType::Array { .. } | OirType::Tuple { .. } => "array".to_string(),
        OirType::Map { .. } | OirType::Struct { .. } | OirType::Row { .. } => "object".to_string(),
        // Every variant of a struct union is an object, so the KIND is object;
        // variant selection happens at the shape check, not the kind lattice.
        OirType::Union { .. } => "object".to_string(),
        OirType::Optional { inner } => oir_type_kind(inner),
        OirType::Reference { entity } => entity.clone(),
        OirType::Email => "email".to_string(),
        OirType::Url => "url".to_string(),
        OirType::Phone => "phone".to_string(),
        OirType::Uuid => "uuid".to_string(),
        OirType::Image => "image".to_string(),
        OirType::Bytes => "bytes".to_string(),
        OirType::CallKey => "callkey".to_string(),
        // A field name is a string at run time.
        OirType::FieldRef { .. } => "string".to_string(),
        // Money is numeric at rest; File is the canonical struct value.
        OirType::Money => "number".to_string(),
        OirType::File => "object".to_string(),
        OirType::Enum { .. } => "string".to_string(),
        OirType::Trait => "string".to_string(),
        OirType::Orbital => "orbital".to_string(),
        OirType::Behavior => "behavior".to_string(),
        OirType::Node => "node".to_string(),
        // A by-name reference to a named struct type — same wire shape as the
        // struct it names, so it reports the same KIND as `Struct`/`Union`.
        OirType::TypeRef { .. } => "object".to_string(),
        OirType::Var { name } => format!("${name}"),
    }
}

/// An inferred type carrying no shape information is undecidable. An array
/// retains its outer kind even when its element type is unknown.
pub fn is_unknowable(t: &OirType) -> bool {
    let t = match t {
        OirType::Optional { inner } => inner.as_ref(),
        _ => t,
    };
    is_json_family(t)
}

/// Returns `true` when a value whose *inferred* type is `inferred` may be
/// assigned to a field of declared type-name `declared_field`.
///
/// The companion to `set_value_type_compatible` for the `(set @field <expr>)`
/// path where the value's type is *inferred* through the SExpression
/// (Gap G-LOLO-SET-OP-RETURN). **Conservative on unknowable inferred types**:
/// an open `Json`/`SExpr` never rejects. An array with unknown elements still
/// checks its outer kind; matching array/tuple slots supply its element type.
/// A concrete inferred type — including a concrete Array element — compares
/// structurally through `sub`, passed by its real `OirType`, not an erased
/// kind string.
pub fn oir_type_satisfies_field(
    inferred: &crate::ir::types::OirType,
    declared_field: &str,
) -> bool {
    if is_unknowable(inferred) {
        return true;
    }
    sub_compat(
        &CompatType::Ty(inferred.clone()),
        &parse_compat_type(declared_field),
    )
    .is_some_and(|c| !c.requires_cast())
}

/// Structured sibling of [`oir_type_satisfies_field`] for callers that
/// already hold the declared side as a real `OirType` (an entity field's
/// resolved [`crate::field_shape`]) rather than a kind-string. Same
/// unknowable-inferred leniency, compared through `sub` directly — no
/// string round trip on either side.
pub fn oir_type_satisfies(inferred: &OirType, declared: &OirType) -> bool {
    is_unknowable(inferred) || assignable(inferred, declared)
}

/// [`oir_type_satisfies`] with declared aliases resolved through `resolve`, so a
/// `TypeRef` on either side compares by the shape it names instead of by spelling.
pub fn oir_type_satisfies_with_named(
    inferred: &OirType,
    declared: &OirType,
    resolve: &dyn Fn(&str) -> Option<OirType>,
) -> bool {
    is_unknowable(inferred)
        || sub_with_named(inferred, declared, resolve).is_some_and(|c| !c.requires_cast())
}

/// Structured sibling of [`set_value_type_compatible`]: `declared` is a real
/// `OirType`, `actual_kind` stays the legacy marker-string vocabulary parsed
/// through [`parse_compat_type`]. An erased-element actual (a bracket
/// spelling the parser can't resolve, e.g. `[Foo]`) gets the same
/// [`is_unknowable`] leniency the inferred side already has.
pub fn oir_type_accepts_kind(declared: &OirType, actual_kind: &str) -> bool {
    let actual = parse_compat_type(actual_kind);
    if let CompatType::Ty(t) = &actual {
        if is_unknowable(t) {
            return true;
        }
    }
    sub_compat(&actual, &CompatType::Ty(declared.clone())).is_some_and(|c| !c.requires_cast())
}

/// The legacy kind-string vocabulary's representative `OirType`, for callers
/// that hold a source spelling (a payload's declared kind) but want to
/// compare structurally rather than through the string-only relation.
/// Marker spellings with no real `OirType` (`any`, `entity`, raw
/// `int`/`float`/`null`, `Map<...>`, PascalCase named types) get their
/// [`CompatType::witness_type`] stand-in.
pub fn compat_kind_oir(kind: &str) -> OirType {
    parse_compat_type(kind).witness_type()
}
