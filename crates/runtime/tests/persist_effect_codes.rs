// `ServerEffectResult.code` on a persist update/delete: `no-row-key` when no row key resolves,
// `not-found` when the keyed row is absent, none when it lands. One fixture, read by this suite and by
// `@almadar/runtime` `persist-effect-codes.test.ts` (canonical source `@almadar-io/parity`
// `fixtures/effect-codes`, mirrored into `tests/baked/`).

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize)]
struct Expect {
    success: bool,
    code: Option<String>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    event: String,
    payload: serde_json::Value,
    expect: Expect,
}

#[derive(Deserialize)]
struct Vector {
    schema: OrbitalSchema,
    cases: Vec<Case>,
}

fn vector() -> Vector {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/baked/effect-codes/persist-codes.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} unreadable: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("persist-codes.json malformed: {e}"))
}

fn run(vector: &Vector, case: &Case) -> EventResponse {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(vector.schema.clone()).unwrap();
    let payload: Value = serde_json::from_value(case.payload.clone()).unwrap();
    let outcomes = kernel
        .dispatch("Editor", None, &case.event, payload)
        .unwrap();
    EventResponse::from_process_results(&outcomes, "Editor", &case.event)
}

#[test]
fn every_case_reports_the_fixtures_success_and_code() {
    let vector = vector();
    assert!(vector.cases.len() >= 6);
    for case in &vector.cases {
        let response = run(&vector, case);
        let persist = response
            .effect_results
            .iter()
            .flatten()
            .find(|r| serde_json::to_value(r).unwrap()["effect"] == "persist")
            .unwrap_or_else(|| {
                panic!(
                    "{}: no persist result in {:?}",
                    case.name, response.effect_results
                )
            });
        let wire = serde_json::to_value(persist).unwrap();
        assert_eq!(
            wire["success"], case.expect.success,
            "{}: {wire}",
            case.name
        );
        assert_eq!(
            wire.get("code").and_then(serde_json::Value::as_str),
            case.expect.code.as_deref(),
            "{}: {wire}",
            case.name
        );
    }
}
