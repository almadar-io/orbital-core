//! F1, the native floor of the embedded-kernel benchmark: the server-role `RuntimeKernel` driven through
//! `dispatch` and nothing else. Each step's payload is decoded once when the scenario opens, and the dispatch
//! outcome is dropped unread: no `EventResponse`, no trace step, no `getState`, no serialization. R0 minus F1 is
//! what response building and serialization cost; F1 is the bound a faster kernel is measured against.
//!
//! The fixtures folder is `ALMADAR_PARITY_FIXTURES` (required) and the JSON row goes to `ALMADAR_BENCH_OUT`
//! (required), in the shape `@almadar-io/kernel-bench-tool`'s `bench-report` merges. `ALMADAR_BENCH_WARMUP`,
//! `ALMADAR_BENCH_RUNS` and `ALMADAR_BENCH_MEMORY_EVENTS` shorten the run for the 100 000-row tables; the row
//! has no reference to compare, so its recorded traces are empty.

#[path = "../parity_fixtures/scenario_bench.rs"]
#[allow(dead_code)]
mod scenario_bench;

use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use scenario_bench::*;
use serde_json::{json, Value as Json};
use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn default_viewer() -> Value {
    serde_json::from_value(json!({
        "id": "viewer-1", "name": "Dev Viewer", "email": "viewer@example.com", "role": ""
    }))
    .expect("default viewer is a Value")
}

struct DecodedStep {
    trait_name: String,
    event: String,
    payload: Value,
}

struct FloorSession {
    kernel: RuntimeKernel,
    steps: Vec<DecodedStep>,
    cursor: Cell<usize>,
}

struct FloorDriver;

impl Driver for FloorDriver {
    type Open = FloorSession;

    fn open(&self, fixture: &Fixture, scenario: &Scenario) -> Result<FloorSession, String> {
        let schema: OrbitalSchema = serde_json::from_value(fixture.schema.clone())
            .map_err(|e| format!("{}: schema: {e}", fixture.name))?;
        let steps = scenario
            .steps
            .iter()
            .map(|step| {
                let payload = match &step.payload {
                    Some(p) => serde_json::from_value(p.clone()).map_err(|e| format!("payload: {e}"))?,
                    None => Value::Object(Default::default()),
                };
                Ok(DecodedStep {
                    trait_name: step.trait_name.clone(),
                    event: step.event.clone(),
                    payload,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            Default::default(),
        );
        kernel
            .register_schema(schema)
            .map_err(|e| format!("{}: register_schema: {e}", fixture.name))?;
        kernel.set_current_user(default_viewer());
        Ok(FloorSession {
            kernel,
            steps,
            cursor: Cell::new(0),
        })
    }
}

impl Session for FloorSession {
    fn dispatch(&self, _step: &Step) -> Result<Json, String> {
        let at = self.cursor.get();
        self.cursor.set((at + 1) % self.steps.len());
        let step = &self.steps[at];
        let outcomes = self
            .kernel
            .dispatch(&step.trait_name, None, &step.event, step.payload.clone())
            .map_err(|e| format!("{}.{}: {e}", step.trait_name, step.event))?;
        std::hint::black_box(&outcomes);
        Ok(Json::Null)
    }
}

fn env_count(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .map(|v| v.parse().unwrap_or_else(|_| panic!("{name} must be a positive integer")))
        .unwrap_or(default)
}

fn main() {
    let fixtures_dir = PathBuf::from(
        std::env::var("ALMADAR_PARITY_FIXTURES")
            .expect("ALMADAR_PARITY_FIXTURES must name the fixtures folder"),
    );
    let out = std::env::var("ALMADAR_BENCH_OUT").expect("ALMADAR_BENCH_OUT must name the JSON row to write");
    let fixtures = load_fixtures(&fixtures_dir).expect("fixtures");
    let sha = command_output("git", &["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"]);
    let sampler = || (0, rss_bytes());
    let options = BenchOptions {
        driver: &FloorDriver,
        row: "F1",
        row_name: "floor: orbital-core RuntimeKernel dispatch only (in-process, server role)",
        reference_label: "none (floor row)",
        references: None,
        commits: json!({ "orbital-core": { "sha": sha } }),
        environment: machine_environment(),
        warmups: env_count("ALMADAR_BENCH_WARMUP", WARMUP_RUNS),
        measured: env_count("ALMADAR_BENCH_RUNS", MEASURED_RUNS),
        memory_events: env_count("ALMADAR_BENCH_MEMORY_EVENTS", MEMORY_EVENTS),
        sampler: &sampler,
    };
    let result = run_bench(&options, &fixtures);
    std::fs::write(out, serde_json::to_string_pretty(&result).unwrap()).expect("write bench json");
}
