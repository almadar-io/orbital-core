//! The Rust half of the shared mock `call-service` answer vector.
//!
//! `tests/fixtures/mock-answers/mock-answers.json` is the TS answer
//! (`mockServiceAnswer` in `@almadar/core`, `src/mock/serviceAnswer.ts`) for every
//! action of the services registry with empty params.
//! The TS function uses no randomness and no clock, so parity is exact.
//!
//! Re-record (after the registry changes):
//!
//! ```bash
//! cd packages/almadar-core && pnpm exec tsx scripts/record-mock-answers.mts \
//!   ../../orbital-rust/crates/orbital-core/tests/fixtures/mock-answers/mock-answers.json
//! ```

use orbital_core_base::language::registry::baked_service_registry;
use orbital_core_runtime::mock_answer::{mock_answer, MockAnswerError};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
struct Case {
    service: String,
    action: String,
    params: Map<String, Value>,
    data: Value,
}

#[derive(serde::Deserialize)]
struct Vector {
    cases: Vec<Case>,
}

fn vector() -> Vector {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/fixtures/mock-answers/mock-answers.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn every_recorded_ts_answer_is_reproduced() {
    let v = vector();
    assert!(
        v.cases.len() >= 14,
        "fixture looks truncated: {}",
        v.cases.len()
    );
    for c in &v.cases {
        let got = mock_answer(&c.service, &c.action)
            .unwrap_or_else(|e| panic!("{}.{}: {e}", c.service, c.action));
        assert_eq!(
            got,
            c.data,
            "{}.{} params={}",
            c.service,
            c.action,
            Value::Object(c.params.clone())
        );
    }
}

#[test]
fn every_registry_action_is_in_the_vector() {
    let v = vector();
    let recorded: BTreeSet<(String, String)> = v
        .cases
        .iter()
        .map(|c| (c.service.clone(), c.action.clone()))
        .collect();
    let registry = baked_service_registry();
    let mut missing_services = BTreeSet::new();
    for (service, actions) in &registry.actions {
        for action in actions {
            if !recorded.contains(&(service.clone(), action.clone())) {
                missing_services.insert(service.clone());
            }
        }
    }
    assert_eq!(missing_services, BTreeSet::new());
}

#[test]
fn typed_array_cases_from_the_ts_test() {
    assert_eq!(
        mock_answer("registry", "handles").unwrap(),
        json!({"handle": "mock_handle", "teams": [{"id": "mock_id", "name": "mock_name", "handle": "mock_handle"}]})
    );
    assert_eq!(
        mock_answer("registry", "removeTeamBehavior").unwrap(),
        json!({"removed": true})
    );
}

#[test]
fn unknown_service_and_action_are_errors_not_invented_answers() {
    assert!(matches!(
        mock_answer("no-such-service", "x"),
        Err(MockAnswerError::UnknownService(_))
    ));
    assert!(matches!(
        mock_answer("hosting", "no-such-action"),
        Err(MockAnswerError::UnknownAction { .. })
    ));
}
