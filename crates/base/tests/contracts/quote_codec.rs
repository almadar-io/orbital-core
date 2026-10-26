//! G-CROSS-041: the `(quote x)` body codec, against the fixture `@almadar/core`
//! owns (`fixtures/quote/cases.json`) — byte parity with the TS twin.

use orbital_core_base::fixtures::quote_cases;
use orbital_core_base::schema::quote::{decode_quote_body, encode_quote_body, quote_body_of, quote_expr, QUOTE_HEAD};
use orbital_core_base::schema::SExpression;

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    expr: SExpression,
    body: String,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(quote_cases()).expect("quote fixture parses")
}

#[test]
fn every_case_encodes_to_the_fixture_body_and_decodes_back() {
    for c in cases() {
        assert_eq!(encode_quote_body(&c.expr), c.body, "encode `{}`", c.name);
        assert_eq!(decode_quote_body(&c.body), Some(c.expr), "decode `{}`", c.name);
    }
}

#[test]
fn a_body_never_carries_a_raw_at_sign() {
    for c in cases() {
        assert!(!encode_quote_body(&c.expr).contains('@'), "`{}` leaked an @", c.name);
    }
}

#[test]
fn quote_expr_builds_the_ir_call_and_quote_body_of_reads_it() {
    let guard = SExpression::Array(vec![
        SExpression::String(">".into()),
        SExpression::String("@payload.amount".into()),
        SExpression::Number(0.0),
    ]);
    let q = quote_expr(&guard);
    let SExpression::Array(items) = &q else { panic!("quote is a call") };
    assert_eq!(items[0], SExpression::String(QUOTE_HEAD.into()));
    assert_eq!(quote_body_of(&q), Some(encode_quote_body(&guard).as_str()));
}

#[test]
fn control_quote_body_of_rejects_non_quote_shapes() {
    let not_quote = SExpression::Array(vec![SExpression::String("quote".into())]);
    assert_eq!(quote_body_of(&not_quote), None);
    let nested = SExpression::Array(vec![
        SExpression::String("quote".into()),
        SExpression::Array(vec![SExpression::String(">".into())]),
    ]);
    assert_eq!(quote_body_of(&nested), None);
    assert_eq!(quote_body_of(&SExpression::String("quote".into())), None);
}

#[test]
fn edge_a_malformed_body_does_not_decode() {
    assert_eq!(decode_quote_body("[unterminated"), None);
}
