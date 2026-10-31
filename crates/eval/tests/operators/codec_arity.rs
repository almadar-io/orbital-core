use {orbital_core_base::error::{EvalError, OrbitalError}, orbital_core_base::schema::SExpression, orbital_core_eval::evaluator::context::{BindingContext, BindingContextBuilder}, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn context() -> BindingContext {
    BindingContextBuilder::new()
        .payload(serde_json::from_value(json!({ "invalidBase64": "!" })).unwrap())
        .build()
        .with_secret("Jefe", "Jefe".to_string())
}

fn evaluate(op: &str, args: &[Json], ctx: &BindingContext) -> Result<Value, OrbitalError> {
    let mut call = vec![json!(op)];
    call.extend(args.iter().cloned());
    let expression: SExpression = serde_json::from_value(Json::Array(call)).unwrap();
    Evaluator::new().evaluate(&expression, ctx)
}

fn assert_arity(op: &str, args: &[Json], expected: usize, ctx: &BindingContext) {
    match evaluate(op, args, ctx) {
        Err(OrbitalError::Eval(EvalError::ArityMismatch {
            operator,
            expected: actual_expected,
            actual,
        })) => {
            assert_eq!(operator, op);
            assert_eq!(actual_expected, expected, "{op}");
            assert_eq!(actual, args.len(), "{op}");
        }
        other => panic!(
            "{op}: expected arity {expected} rejection for {} arguments, got {other:?}",
            args.len()
        ),
    }
}

fn assert_extra_rejected(op: &str, args: Vec<Json>, expected: &str) {
    let ctx = context();
    assert_eq!(
        evaluate(op, &args, &ctx).unwrap(),
        Value::String(expected.into()),
        "{op}"
    );
    let arity = args.len();
    let mut extra = args;
    extra.push(json!("extra"));
    assert_arity(op, &extra, arity, &ctx);
}

/// Each codec's full call and its minimum arity (`hash/sha256` takes an optional third `input`, `hmac/sha256` an optional
/// literal `prefix`).
fn valid_calls() -> [(&'static str, Vec<Json>, usize); 5] {
    [
        ("str/encode-uri-component", vec![json!("a b&c")], 1),
        ("base64/encode", vec![json!("foobar")], 1),
        ("base64/decode", vec![json!("Zm9vYmFy")], 1),
        ("hash/sha256", vec![json!("abc"), json!("hex"), json!("text")], 2),
        (
            "hmac/sha256",
            vec![json!("Jefe"), json!("what do ya want for nothing?"), json!("hex"), json!("")],
            3,
        ),
    ]
}

fn failing_operand() -> Json {
    json!(["base64/decode", "@payload.invalidBase64", "extra"])
}

#[test]
fn uri_component_rejects_extra_arguments() {
    assert_extra_rejected(
        "str/encode-uri-component",
        vec![json!("a b&c")],
        "a%20b%26c",
    );
}

#[test]
fn base64_encode_rejects_extra_arguments() {
    assert_extra_rejected("base64/encode", vec![json!("foobar")], "Zm9vYmFy");
}

#[test]
fn base64_decode_rejects_extra_arguments() {
    assert_extra_rejected("base64/decode", vec![json!("Zm9vYmFy")], "foobar");
}

#[test]
fn hash_sha256_rejects_extra_arguments() {
    assert_extra_rejected(
        "hash/sha256",
        vec![json!("abc"), json!("hex"), json!("text")],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
}

#[test]
fn hmac_sha256_rejects_extra_arguments() {
    assert_extra_rejected(
        "hmac/sha256",
        vec![json!("Jefe"), json!("what do ya want for nothing?"), json!("hex"), json!("")],
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
    );
}

#[test]
fn missing_codec_arguments_are_typed_arity_errors() {
    let ctx = context();
    for (op, args, min) in valid_calls() {
        for count in 0..min {
            assert_arity(op, &args[..count], min, &ctx);
        }
    }
}

#[test]
fn extra_codec_arity_is_rejected_before_evaluating_failing_operands() {
    let ctx = context();
    for (op, mut args, _) in valid_calls() {
        let arity = args.len();
        args[0] = failing_operand();
        args.push(json!("extra"));
        assert_arity(op, &args, arity, &ctx);
    }
}

#[test]
fn missing_codec_arity_is_rejected_before_evaluating_failing_operands() {
    let ctx = context();
    for (op, mut args, min) in valid_calls() {
        if min == 1 {
            continue;
        }
        args[0] = failing_operand();
        args.truncate(min - 1);
        assert_arity(op, &args, min, &ctx);
    }
}

#[test]
fn valid_codec_arity_evaluates_failing_context_bound_operands() {
    let ctx = context();
    for (op, mut args, _) in valid_calls() {
        args[0] = failing_operand();
        match evaluate(op, &args, &ctx) {
            Err(OrbitalError::Eval(EvalError::ArityMismatch { operator, .. })) => {
                assert_eq!(operator, "base64/decode", "{op}");
            }
            other => panic!("{op}: expected the context-bound nested decoder to fail, got {other:?}"),
        }
    }
}

#[test]
fn an_invalid_base64_operand_answers_null_and_the_outer_codec_reads_it_as_a_missing_string() {
    let ctx = context();
    assert_eq!(evaluate("base64/decode", &[json!("@payload.invalidBase64")], &ctx).unwrap(), Value::Null);
    assert_eq!(evaluate("base64/decode", &[json!("Zm9vYmFy")], &ctx).unwrap(), Value::String("foobar".into()));
    match evaluate("base64/encode", &[json!(["base64/decode", "@payload.invalidBase64"])], &ctx) {
        Err(OrbitalError::Eval(EvalError::TypeMismatch { expected, actual })) => {
            assert_eq!((expected.as_str(), actual.as_str()), ("string", "null"));
        }
        other => panic!("expected the null operand to be refused, got {other:?}"),
    }
}
