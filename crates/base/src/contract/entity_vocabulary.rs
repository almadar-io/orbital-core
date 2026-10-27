#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use crate::schema::types::EntityDefinition;
use crate::schema::types::FieldDefinition;


/// The bare sigil root this module's `@user.<f>` readers are about.
pub const USER_SIGIL: &str = "@user.";

/// Comparison operators whose two operands are compared for equality. `=` and
/// `==` both occur in committed `.orb` files. Shared with `orbital-compiler`'s
/// `phases::validation::user_identity::check_comparison` (which re-exports
/// this constant rather than declaring its own) and the orbital-import
/// role-literal rewrite (`inline/rewrite.rs::rewrite_role_literals`, which
/// must stay shape-identical to `check_comparison`).
pub const EQUALITY_OPS: &[&str] = &["=", "==", "!="];

/// The declared vocabulary for an identity field, following `items` for the
/// array form. `roles : ["a" | "b"]` keeps the union on the ELEMENT, so a
/// multi-valued role field must be read through `items`. Moved here from
/// `orbital-compiler`'s `user_identity` module (re-exported there) so
/// [`role_satisfying_policy`] and `check_comparison` share one reader.
pub fn vocabulary(field: &FieldDefinition) -> Option<&Vec<String>> {
    if let Some(values) = field.values.as_ref().filter(|v| !v.is_empty()) {
        return Some(values);
    }
    field
        .items
        .as_ref()
        .and_then(|item| item.values.as_ref())
        .filter(|v| !v.is_empty())
}

/// Look a field up on the identity entity by name. Moved here from
/// `orbital-compiler`'s `user_identity` module (re-exported there).
pub fn identity_field<'a>(
    identity: &'a EntityDefinition,
    name: &str,
) -> Option<&'a FieldDefinition> {
    identity
        .fields
        .iter()
        .find(|f| f.name.as_deref() == Some(name))
}

/// `@user.role` → `Some("role")`. Only a direct field read; `@user` alone and
/// deeper paths (`@user.profile.x`) are not field claims this rule can judge.
/// Moved here from `orbital-compiler`'s `user_identity` module (re-exported
/// there).
pub fn user_field_of(s: &str) -> Option<&str> {
    let rest = s.strip_prefix(USER_SIGIL)?;
    if rest.is_empty() || rest.contains('.') || rest.contains('[') {
        return None;
    }
    Some(rest)
}