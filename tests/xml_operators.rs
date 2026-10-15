//! The Rust half of the shared `xml/parse` vectors (`@almadar-io/parity` `fixtures/operators/xml-vectors.json`,
//! mirrored byte-identically under `tests/fixtures/operators/`; the TS twin in `@almadar/evaluator` reads the
//! same file).

use orbital_core::{BindingContextBuilder, Evaluator, Value};
use serde_json::{json, Value as Json};
use std::path::PathBuf;

#[derive(serde::Deserialize)]
struct Case {
    op: String,
    args: Vec<Json>,
    #[serde(default)]
    expect: Json,
    #[serde(default)]
    error: bool,
}

fn crate_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
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

fn cases() -> Vec<Case> {
    let raw = std::fs::read_to_string(crate_path("tests/fixtures/operators/xml-vectors.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn every_shared_vector_matches() {
    let cases = cases();
    assert!(cases.iter().any(|c| c.error) && cases.iter().any(|c| !c.error));
    for case in &cases {
        let got = eval(&case.op, &case.args);
        if case.error {
            assert!(got.is_err(), "{} {:?} must error, got {got:?}", case.op, case.args);
        } else {
            assert_eq!(got.as_ref().ok(), Some(&case.expect), "{} {:?}", case.op, case.args);
        }
    }
}

#[test]
fn the_mirror_is_byte_identical_to_the_parity_fixture() {
    let source = crate_path("../../../packages/almadar-parity/fixtures/operators/xml-vectors.json");
    if !source.exists() {
        return;
    }
    assert_eq!(
        std::fs::read(source).unwrap(),
        std::fs::read(crate_path("tests/fixtures/operators/xml-vectors.json")).unwrap(),
        "re-copy packages/almadar-parity/fixtures/operators/xml-vectors.json"
    );
}

#[test]
fn a_non_string_input_is_an_error_and_a_missing_shape_is_an_arity_error() {
    assert!(eval("xml/parse", &[json!(42), json!("Row")]).is_err());
    assert!(eval("xml/parse", &[json!("<a/>")]).is_err());
    assert_eq!(eval("xml/parse", &[json!("<a/>"), json!("Row")]).unwrap(), json!(""));
}

#[test]
fn the_arxiv_feed_vector_maps_to_what_the_arxiv_provider_returns() {
    let fixture = crate_path("../orbital-integrations/fixtures/arxiv/search.json");
    if !fixture.exists() {
        return;
    }
    let fixture: Json = serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    let body = fixture["exchanges"][0]["response"]["body"].clone();
    let expected = fixture["outcome"]["data"]["results"].clone();
    let feed = eval("xml/parse", &[body, json!("AtomFeed")]).unwrap();
    let results: Vec<Json> = feed["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let authors: Vec<Json> = match &entry["author"] {
                Json::Null => vec![],
                Json::Array(list) => list.iter().map(|a| a["name"].clone()).collect(),
                one => vec![one["name"].clone()],
            };
            let links = match &entry["link"] {
                Json::Array(list) => list.clone(),
                Json::Null => vec![],
                one => vec![one.clone()],
            };
            let url = links
                .iter()
                .find(|l| l["@rel"] == "alternate")
                .map_or(entry["id"].clone(), |l| l["@href"].clone());
            json!({
                "id": entry["id"], "title": entry["title"], "summary": entry["summary"],
                "authors": authors, "published": entry["published"], "url": url,
            })
        })
        .collect();
    assert_eq!(Json::Array(results), expected);
}
