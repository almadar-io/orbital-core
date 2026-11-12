// EXEC-TICK-8: a tick with `afterEntryMs` runs only once its row has been in its state that long (a state's `for Ns`).

use orbital_core_runtime::config::KernelConfig;
use orbital_core_runtime::kernel::RuntimeKernel;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::json;

fn kernel(clock: Arc<AtomicU64>) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [
                { "name": "id", "type": "string" },
                { "name": "waited", "type": "number" }
            ] },
            "traits": [{
                "name": "Ask", "linkedEntity": "Item",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "asking", "timeBoundMs": 1000 }],
                    "transitions": [
                        { "from": "idle", "to": "asking", "event": "ASK" },
                        { "from": "asking", "to": "asking", "event": "ASK" }
                    ]
                },
                "ticks": [{ "name": "askingStateTimeout", "interval": 100, "runsInBackground": true, "appliesTo": ["asking"],
                            "afterEntryMs": 1000, "effects": [["set", "@entity.waited", 1]] }]
            }]
        }]
    }))
    .unwrap();
    let now = Arc::clone(&clock);
    crate::support::TestKernel::server()
        .config(KernelConfig {
            clock: Some(Arc::new(move || now.load(Ordering::SeqCst) as f64)),
            ..KernelConfig::default()
        })
        .register(schema)
}

fn ask(kernel: &RuntimeKernel) {
    kernel.dispatch("Ask", None, "ASK", Value::Null).unwrap();
}

fn runs(kernel: &RuntimeKernel) -> bool {
    kernel.process_tick("Ask", None, "askingStateTimeout").unwrap().executed
}

#[test]
fn a_bounded_tick_waits_until_its_row_has_been_in_the_state_long_enough() {
    let clock = Arc::new(AtomicU64::new(10_000));
    let kernel = kernel(Arc::clone(&clock));
    ask(&kernel);
    clock.store(10_999, Ordering::SeqCst);
    assert!(!runs(&kernel), "999 ms in: too soon");
    clock.store(11_000, Ordering::SeqCst);
    assert!(runs(&kernel), "1000 ms in: it runs");
}

#[test]
fn control_re_entering_the_state_starts_the_wait_again() {
    let clock = Arc::new(AtomicU64::new(10_000));
    let kernel = kernel(Arc::clone(&clock));
    ask(&kernel);
    clock.store(10_900, Ordering::SeqCst);
    ask(&kernel);
    clock.store(11_500, Ordering::SeqCst);
    assert!(!runs(&kernel), "600 ms since the second entry");
    clock.store(11_900, Ordering::SeqCst);
    assert!(runs(&kernel));
}

#[test]
fn control_outside_its_state_the_bounded_tick_never_runs() {
    let clock = Arc::new(AtomicU64::new(10_000));
    let kernel = kernel(Arc::clone(&clock));
    clock.store(99_000, Ordering::SeqCst);
    assert!(!runs(&kernel), "the row is idle, not asking");
}
