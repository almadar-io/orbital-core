

/// Shallow primitive-kind of a **literal** `SExpression` value, in the same
/// lowercase-kind vocabulary `set_value_type_compatible` speaks. This is the
/// single source both the lolo L1 emit-payload / `uses`-config-override check
/// and the compiler L2 emit-payload backstop call, so the two layers can't
/// drift (the same discipline as `set_value_type_compatible`).
///
/// Returns `None` for values whose type only the expression/binding resolver
/// can know — the caller must defer these to inference rather than compare a
/// literal kind:
/// - `@`-bindings and interpolated strings (`@entity.x`, `"pre@x"`), and
/// - operator **applications** (`(array/find …)`, `(object/get …)`, `(math/max …)`).
///
/// The `Array` arm mirrors `eval_expr`'s own call-vs-literal discrimination via
/// `get_operator`, so this check and the runtime never disagree on what is a
/// computed expression versus a literal array: an `Array` whose head is a known
/// operator is a call (defer, `None`); anything else — empty, or a head that is
/// not a known operator (e.g. `["red", "blue"]`) — is a literal array (`"array"`).
pub fn literal_value_type(value: &orbital_core_base::schema::sexpr::SExpression) -> Option<&'static str> {
    use orbital_core_base::schema::sexpr::SExpression;
    match value {
        SExpression::Number(_) => Some("number"),
        SExpression::Bool(_) => Some("boolean"),
        SExpression::String(s) => {
            if s.starts_with('@') || s.contains('@') {
                None
            } else {
                Some("string")
            }
        }
        SExpression::Null => None,
        SExpression::Object(_) => Some("object"),
        SExpression::Array(_) => {
            if value
                .operator()
                .and_then(crate::evaluator::operators::get_operator)
                .is_some()
            {
                None
            } else {
                Some("array")
            }
        }
    }
}