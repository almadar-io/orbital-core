//! The Rust half of the shared mock `call-service` answer vector.
//!
//! `tests/fixtures/mock-answers/mock-answers.json` is the TS answer
//! (`RuntimeIntegrationManager`'s `mockValueForDescriptor` in
//! `@almadar/integrations`, reached through a mock-mode `factory.execute`) for every
//! action of the installed services registry with empty params, plus the
//! typed-array / nested-shape / `shapedBy` cases of `mock-shape-typed-arrays.test.ts`.
//! The TS function uses no randomness and no clock, so parity is exact.
//!
//! Re-record (after the registry changes):
//!
//! ```bash
//! cd packages/almadar-integrations && pnpm exec tsx \
//!   scripts/record-mock-answers.mts \
//!   ../../orbital-rust/crates/orbital-core/tests/fixtures/mock-answers/mock-answers.json
//! ```
//!
//! The script (`@almadar/integrations` `scripts/record-mock-answers.mts`, monorepo only) builds a
//! `RuntimeIntegrationManager`, `configureFromEnv({ALMADAR_INTEGRATIONS_MODE:'mock'})`,
//! and for every non-`unmockedServices` action runs `getFactory().execute(service, action, {})`,
//! then the extra shaped cases listed in `extras`, writing `{cases:[{service,action,params,data}]}`.

use orbital_core::language::registry::baked_service_registry;
use orbital_core::runtime::mock_answer::{mock_answer, MockAnswerError};
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mock-answers/mock-answers.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn every_recorded_ts_answer_is_reproduced() {
    let v = vector();
    assert!(
        v.cases.len() > 200,
        "fixture looks truncated: {}",
        v.cases.len()
    );
    for c in &v.cases {
        let got = mock_answer(&c.service, &c.action, &c.params)
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
fn every_registry_action_is_in_the_vector_except_the_unmocked_services() {
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
    let expected: BTreeSet<String> = ["credentials", "workspace"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(missing_services, expected);
}

#[test]
fn typed_array_and_shaped_cases_from_the_ts_test() {
    let none = Map::new();
    assert_eq!(
        mock_answer("knowledge", "currentTopics", &none).unwrap(),
        json!({"topics": [{"label": "mock_label", "keywords": [], "weight": 42}]})
    );
    assert_eq!(
        mock_answer("knowledge", "status", &none).unwrap(),
        json!({"connected": true, "account": "mock_account"})
    );
    assert_eq!(
        mock_answer("llm", "extract", json!({"text": "x"}).as_object().unwrap()).unwrap(),
        json!({"data": {}, "confidence": 42})
    );
}

#[test]
fn unknown_service_and_action_are_errors_not_invented_answers() {
    let none = Map::new();
    assert!(matches!(
        mock_answer("no-such-service", "x", &none),
        Err(MockAnswerError::UnknownService(_))
    ));
    assert!(matches!(
        mock_answer("knowledge", "no-such-action", &none),
        Err(MockAnswerError::UnknownAction { .. })
    ));
}
