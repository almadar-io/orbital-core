//! A client program declaring `dispatchKernel`: each client dispatch runs through the named trait, which steps the
//! kernel's own traits with `kernel/step "self"` (EXEC-METAL-7), and answers as the native cascade does.

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::client::ClientKernel;
use serde_json::json;

/// A program whose Ui bumps a counter; with `kernel`, its dispatches go through K, a one-step kernel that hands the
/// event to `kernel/step "self"` and keeps the answer.
fn program(kernel: bool) -> OrbitalSchema {
    let mut program = json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{ "name": "Main",
            "entity": { "name": "Cell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number", "default": 0 }, { "name": "last", "type": "string" }] },
            "osGrants": { "kernel": [{ "target": "guest", "kinds": ["step"] }] },
            "traits": [
                { "name": "Ui", "linkedEntity": "Cell", "dispatch": "client", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "bumped" }],
                    "events": [{ "key": "BUMP", "name": "Bump" }],
                    "transitions": [{ "from": "idle", "to": "bumped", "event": "BUMP", "effects": [["set", "@entity.n", ["+", "@entity.n", 1]]] }] } },
                { "name": "K", "linkedEntity": "Cell", "dispatch": "client", "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }],
                    "events": [{ "key": "INIT", "name": "Init" }, { "key": "KERNEL_DISPATCH", "name": "Kernel dispatch", "payloadSchema": [{ "name": "trait", "type": "string", "required": true }, { "name": "event", "type": "string", "required": true }, { "name": "payload", "type": "object" }] }],
                    "transitions": [
                        { "from": "ready", "to": "ready", "event": "INIT" },
                        { "from": "ready", "to": "ready", "event": "KERNEL_DISPATCH", "effects": [
                            ["set", "@entity.last", ["json/stringify", ["kernel/step", "self", { "trait": "@payload.trait", "event": "@payload.event", "payload": {} }]]]] }] } }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Ui" }] }] }]
    });
    if kernel {
        program["dispatchKernel"] = json!({ "trait": "K", "mount": ["K"] });
    }
    from_json_value(&program).unwrap()
}

fn bumped(kernel: bool) -> (Vec<(String, String)>, String, serde_json::Value) {
    let client = ClientKernel::new(program(kernel), None).unwrap();
    let response = client.dispatch("Ui", None, "BUMP", Value::Null).unwrap();
    let response = serde_json::to_value(&response).unwrap();
    let moved = response["transitions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| (t["trait"].as_str().unwrap_or_default().to_string(), t["event"].as_str().unwrap_or_default().to_string()))
        .collect();
    let ui = client.snapshot_trait("Ui", None);
    (moved, ui.state, serde_json::to_value(&ui.entity).unwrap()["n"].clone())
}

#[test]
fn a_client_dispatch_through_its_lolo_kernel_moves_what_the_native_cascade_moves() {
    let native = bumped(false);
    assert_eq!(native.1, "bumped", "control: natively Ui moved");
    let through = bumped(true);
    assert_eq!(through, native, "the .lolo kernel's dispatch answers as the native cascade");
    assert!(!through.0.iter().any(|(t, _)| t == "K"), "the kernel's own step is not the dispatch's: {:?}", through.0);
}

#[test]
fn the_dispatch_ran_through_the_lolo_kernel() {
    let client = ClientKernel::new(program(true), None).unwrap();
    client.dispatch("Ui", None, "BUMP", Value::Null).unwrap();
    let last = serde_json::to_value(&client.snapshot_trait("K", None).entity).unwrap()["last"].clone();
    let stepped: serde_json::Value = serde_json::from_str(last.as_str().expect("K kept the step's answer")).unwrap();
    assert_eq!((stepped["fired"].clone(), stepped["to"].clone()), (json!(true), json!("bumped")), "{stepped}");
    let native = ClientKernel::new(program(false), None).unwrap();
    native.dispatch("Ui", None, "BUMP", Value::Null).unwrap();
    assert!(serde_json::to_value(&native.snapshot_trait("K", None).entity).unwrap()["last"].is_null(), "control: natively K never runs");
}

#[test]
fn the_dispatch_kernel_is_mounted_when_the_program_loads() {
    let client = ClientKernel::new(program(true), None).unwrap();
    assert_eq!(client.snapshot_trait("K", None).state, "ready", "K ran its INIT at load");
}

#[test]
fn edge_a_step_of_self_outside_a_client_dispatch_is_refused() {
    let client = ClientKernel::new(program(true), None).unwrap();
    let answer = client.dispatch("K", None, "KERNEL_DISPATCH", from_json_value(&json!({ "trait": "Ui", "event": "BUMP" })).unwrap()).unwrap();
    let text = serde_json::to_string(&answer).unwrap();
    assert!(text.contains("runs only inside a client dispatch"), "{text}");
}
