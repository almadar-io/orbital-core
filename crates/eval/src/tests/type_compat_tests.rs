#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

#[test]
fn literal_value_type_defers_operator_calls_but_types_literals() {
    use crate::literal_type::literal_value_type;
    use orbital_core_base::schema::sexpr::SExpression;
    let s = |x: &str| SExpression::string(x.to_string());

    // Operator applications are computed expressions → defer (None), so the
    // caller routes them to the expression inferencer instead of comparing a
    // literal kind. (CG-EMIT-PAYLOAD-SEXPR: these used to mis-type as "array".)
    let array_find = SExpression::Array(vec![s("array/find"), s("@entity.units")].into());
    assert_eq!(literal_value_type(&array_find), None);
    let object_get_find = SExpression::Array(vec![
        s("object/get"),
        SExpression::Array(vec![s("array/find"), s("@entity.units")].into()),
        s("id"),
    ].into());
    assert_eq!(literal_value_type(&object_get_find), None);

    // Genuine literal arrays still type as "array" (head not a known operator,
    // or empty) — matching eval_expr's own literal-vs-call discrimination.
    assert_eq!(
        literal_value_type(&SExpression::Array(vec![s("red"), s("blue")].into())),
        Some("array")
    );
    assert_eq!(
        literal_value_type(&SExpression::Array(vec![].into())),
        Some("array")
    );

    // Scalars + bindings unchanged.
    assert_eq!(
        literal_value_type(&SExpression::Number(1.0)),
        Some("number")
    );
    assert_eq!(
        literal_value_type(&SExpression::Bool(true)),
        Some("boolean")
    );
    assert_eq!(literal_value_type(&s("hello")), Some("string"));
    assert_eq!(literal_value_type(&s("@entity.x")), None);
    assert_eq!(literal_value_type(&SExpression::Null), None);
}
