//! The Rust half of the shared codec operator vectors (orbital-core
//! `fixtures/operators/codec-vectors.json`, mirrored byte-identically under
//! `tests/fixtures/operators/`; the TS twin in `@almadar/evaluator` reads the same file).

use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::path::PathBuf;

#[derive(serde::Deserialize)]
struct Case {
    op: String,
    args: Vec<Json>,
    #[serde(default)]
    expect: Option<Json>,
    #[serde(default)]
    error: bool,
    #[serde(default)]
    null: bool,
}

fn crate_path(rel: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join(rel)
}

fn eval(op: &str, args: &[Json]) -> Result<Json, String> {
    let mut expr = vec![Json::String(op.to_string())];
    expr.extend(args.iter().cloned());
    let sexpr = serde_json::from_value(Json::Array(expr)).map_err(|e| e.to_string())?;
    let ctx = BindingContextBuilder::new().build();
    Evaluator::new()
        .evaluate(&sexpr, &ctx)
        .map(|v: Value| serde_json::to_value(v).unwrap())
        .map_err(|e| e.to_string())
}

#[test]
fn every_shared_vector_matches() {
    let raw = std::fs::read_to_string(crate_path("tests/fixtures/operators/codec-vectors.json"))
        .unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(!cases.is_empty());
    for case in &cases {
        let got = eval(&case.op, &case.args);
        if case.null {
            assert_eq!(got, Ok(Json::Null), "{} {:?} must answer null", case.op, case.args);
        } else if case.error {
            assert!(got.is_err(), "{} {:?} must error, got {got:?}", case.op, case.args);
        } else {
            assert_eq!(
                got.as_ref().ok(),
                case.expect.as_ref(),
                "{} {:?}",
                case.op,
                case.args
            );
        }
    }
}

#[test]
fn an_unknown_digest_encoding_answers_null_for_hash_and_errors_for_hmac() {
    assert!(eval("hash/sha256", &[json!("abc"), json!("hex")]).is_ok_and(|v| v.is_string()));
    assert_eq!(eval("hash/sha256", &[json!("abc"), json!("octal")]), Ok(Json::Null));
    assert!(eval("hmac/sha256-bytes", &[json!("aw=="), json!("m"), json!("octal")]).is_err());
    let held = BindingContextBuilder::new().build().with_secret("K", "k".to_string());
    let expr = serde_json::from_value(json!(["hmac/sha256", "K", "m", "octal"])).unwrap();
    assert!(Evaluator::new().evaluate(&expr, &held).is_err());
}

#[test]
fn hmac_reads_its_key_from_a_held_credential_and_a_key_that_is_a_value_is_not_a_key() {
    let call = |ctx: &orbital_core_eval::evaluator::context::BindingContext| {
        let expr = serde_json::from_value(json!(["hmac/sha256", "K", "m", "hex"])).unwrap();
        Evaluator::new().evaluate(&expr, ctx)
    };
    let held = BindingContextBuilder::new().build().with_secret("K", "k".to_string());
    let signed = call(&held).unwrap();
    let by_value = eval("hmac/sha256-bytes", &[json!("aw=="), json!("m"), json!("hex")]);
    assert_eq!(by_value.map(|v| v.as_str().map(str::to_string)), Ok(signed.as_str().map(str::to_string)));
    let other = BindingContextBuilder::new().build().with_secret("K", "k2".to_string());
    assert_ne!(call(&other).unwrap(), signed, "another credential value signs differently");
    assert_eq!(call(&BindingContextBuilder::new().build()).unwrap(), Value::Null, "the name is never the key");
}

#[test]
fn missing_arguments_are_arity_errors() {
    assert!(eval("base64/encode", &[]).is_err());
    assert!(eval("hash/sha256", &[json!("abc")]).is_err());
    assert!(eval("hmac/sha256", &[json!("k"), json!("m")]).is_err());
    for op in ["hmac/sha256-bytes", "ecdsa/p256-sign", "rsa/sign", "ecdh/p256-ephemeral", "aes-gcm/seal", "aes-gcm/open"] {
        assert!(eval(op, &[]).is_err(), "{op}");
    }
}

#[test]
fn every_shared_regex_vector_matches() {
    let raw = std::fs::read_to_string(crate_path("tests/fixtures/operators/regex-vectors.json")).unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(cases.iter().any(|case| case.expect == Some(json!(true))));
    assert!(cases.iter().any(|case| case.expect == Some(json!(false))));
    assert!(cases.iter().any(|case| case.null));
    for case in &cases {
        let got = eval(&case.op, &case.args);
        if case.null {
            assert_eq!(got, Ok(Json::Null), "{} {:?} must answer null", case.op, case.args);
        } else if case.error {
            assert!(got.is_err(), "{} {:?} must error, got {got:?}", case.op, case.args);
        } else {
            assert_eq!(got.as_ref().ok(), case.expect.as_ref(), "{} {:?}", case.op, case.args);
        }
    }
}

