// `appliesTo` lists the trait states a tick runs in (G-ORB-181); an absent or empty list runs it in every state.

use orbital_core_runtime::kernel::RuntimeKernel;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn kernel(applies_to: Json) -> RuntimeKernel {
    let mut tick =
        json!({ "name": "beat", "interval": 1000, "runsInBackground": true, "effects": [["set", "@entity.beats", 1]] });
    if !applies_to.is_null() {
        tick["appliesTo"] = applies_to;
    }
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [
                { "name": "id", "type": "string" },
                { "name": "beats", "type": "number" }
            ] },
            "traits": [{
                "name": "Clock", "linkedEntity": "Item",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "running" }],
                    "transitions": [{ "from": "idle", "to": "running", "event": "START" }]
                },
                "ticks": [tick]
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::server().register(schema)
}

fn start(kernel: &RuntimeKernel) {
    kernel
        .dispatch("Clock", None, "START", Value::Null)
        .unwrap();
}

fn beats(kernel: &RuntimeKernel) -> Option<f64> {
    kernel
        .process_tick("Clock", None, "beat")
        .unwrap()
        .entity_data
        .to_value()
        .get_path("beats")
        .and_then(Value::as_number)
}

#[test]
fn a_tick_runs_while_its_trait_is_in_a_listed_state() {
    let kernel = kernel(json!(["running"]));
    start(&kernel);
    let result = kernel.process_tick("Clock", None, "beat").unwrap();
    assert!(result.executed);
    assert_eq!(beats(&kernel), Some(1.0));
}

// EXEC-TICK-1
#[test]
fn a_tick_is_skipped_while_its_trait_is_in_another_state() {
    let kernel = kernel(json!(["running"]));
    let result = kernel.process_tick("Clock", None, "beat").unwrap();
    assert!(!result.executed);
    assert_eq!(result.previous_state, "idle");
    assert_eq!(result.new_state, "idle");
    assert!(result.effect_results.is_empty());
}

#[test]
fn the_tick_resumes_when_the_trait_enters_a_listed_state() {
    let kernel = kernel(json!(["running"]));
    assert!(!kernel.process_tick("Clock", None, "beat").unwrap().executed);
    start(&kernel);
    assert!(kernel.process_tick("Clock", None, "beat").unwrap().executed);
}

#[test]
fn several_listed_states_each_admit_the_tick() {
    let kernel = kernel(json!(["idle", "running"]));
    assert!(kernel.process_tick("Clock", None, "beat").unwrap().executed);
    start(&kernel);
    assert!(kernel.process_tick("Clock", None, "beat").unwrap().executed);
}

#[test]
fn control_an_absent_list_runs_in_every_state() {
    let kernel = kernel(Json::Null);
    assert!(kernel.process_tick("Clock", None, "beat").unwrap().executed);
    start(&kernel);
    assert!(kernel.process_tick("Clock", None, "beat").unwrap().executed);
}

#[test]
fn control_an_empty_list_runs_in_every_state() {
    let kernel = kernel(json!([]));
    assert!(kernel.process_tick("Clock", None, "beat").unwrap().executed);
    start(&kernel);
    assert!(kernel.process_tick("Clock", None, "beat").unwrap().executed);
}

#[test]
fn an_entity_id_in_the_list_no_longer_admits_the_tick() {
    let kernel = kernel(json!(["singleton"]));
    assert!(!kernel.process_tick("Clock", None, "beat").unwrap().executed);
}

#[test]
fn a_skipped_tick_dispatches_no_emits() {
    let kernel = kernel(json!(["running"]));
    let dispatched = kernel
        .dispatch_tick("App", "Clock", None, "beat", None)
        .unwrap();
    assert!(!dispatched.outcome.executed);
    assert!(dispatched.emits.is_empty());
}

// EXEC-INBOUND awaited answer: a `[background]` tick waiting in a state it names is a pending timer (G-STD-249).
#[test]
fn g_std_249_a_background_tick_is_pending_while_its_trait_is_in_a_state_it_names() {
    let kernel = kernel(json!(["running"]));
    assert!(kernel.pending_background_ticks().is_empty(), "control: the trait has not entered the named state");
    start(&kernel);
    assert_eq!(kernel.pending_background_ticks(), [("Clock".to_string(), "beat".to_string())]);
}

#[test]
fn g_std_249_a_tick_naming_no_state_is_never_pending() {
    let kernel = kernel(Json::Null);
    start(&kernel);
    assert!(kernel.pending_background_ticks().is_empty(), "a tick that runs in every state is not a scheduled wait");
}
