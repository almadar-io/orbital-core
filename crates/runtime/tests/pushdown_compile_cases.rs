//! The shared compile-decision fixture (`tests/baked/pushdown/compile-cases.json`, mirrored from
//! `packages/almadar-parity/fixtures/pushdown`): each case is one fetch's filter and/or `@read` policy
//! compiled for the entity's required fields, expecting the compiled slots or the interpreted reason.

use orbital_core_eval::evaluator::BindingContext;
use orbital_core_runtime::row_query::{compile_predicate, PredicateRole};
use orbital_core_base::value::Value;
use serde_json::{json, Map, Value as Json};

fn v(j: &Json) -> Value {
    serde_json::from_value(j.clone()).expect("a value")
}

fn ctx_of(bindings: &Json) -> BindingContext {
    let mut ctx = BindingContext::new();
    if let Some(user) = bindings.get("user") {
        ctx.set_user(v(user));
    }
    if let Some(payload) = bindings.get("payload") {
        ctx.set_payload(v(payload));
    }
    ctx
}

/// JSON with every number as a float: the fixture writes `3` where the serialised predicate writes `3.0`.
fn norm(j: &Json) -> Json {
    match j {
        Json::Number(n) => json!(n.as_f64()),
        Json::Array(a) => Json::Array(a.iter().map(norm).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, x)| (k.clone(), norm(x))).collect()),
        other => other.clone(),
    }
}

#[test]
fn every_compile_case_matches_the_shared_decision() {
    let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/baked/pushdown/compile-cases.json");
    let f: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let required: Vec<String> = f["requiredFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    let is_required = |field: &str| required.iter().any(|r| r == field);
    let mut disagreements = Vec::new();
    for case in f["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let ctx = ctx_of(&case["bindings"]);
        let mut got = Map::new();
        let mut interpreted: Option<&'static str> = None;
        for (slot, role) in [
            ("readPolicy", PredicateRole::Policy),
            ("filter", PredicateRole::Filter),
        ] {
            let Some(expr) = case["sexpr"].get(slot) else {
                continue;
            };
            match compile_predicate(&v(expr), &ctx, role, &is_required) {
                Ok(p) => {
                    let key = if slot == "readPolicy" {
                        "policy"
                    } else {
                        "filter"
                    };
                    got.insert(key.to_string(), serde_json::to_value(&p).unwrap());
                }
                Err(why) => interpreted = Some(why.reason()),
            }
        }
        let got = match interpreted {
            Some(reason) => json!({ "interpreted": reason }),
            None => json!({ "compiled": Json::Object(got) }),
        };
        if norm(&got) != norm(&case["expect"]) {
            disagreements.push(format!("{name}: expected {} got {got}", case["expect"]));
        }
    }
    assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
}
