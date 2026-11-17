//! EXEC-DIST-13: in a browser kernel a `kv/` call is the browser host's (IndexedDB), handed to it as a `host-ask`
//! with its completion; the kernel answers whether the program's `kv` grant covers it.

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use serde_json::json;

fn kv_program() -> OrbitalSchema {
    from_json_value(&json!({
        "name": "App", "typeContractVersion": 1,
        "orbitals": [{ "name": "Shelf",
            "osGrants": { "kv": { "answers": ["read", "write"] } },
            "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Kept", "linkedEntity": "Run", "dispatch": "client",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [
                    { "from": "idle", "to": "idle", "event": "INIT",
                      "effects": [["kv/get", "answers", "k1", { "emit": { "success": "READ", "failure": "MISSED", "key": "k1" } }]] },
                    { "from": "idle", "to": "idle", "event": "READ" },
                    { "from": "idle", "to": "idle", "event": "MISSED" }] } }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Kept" }] }] }]
    }))
    .unwrap()
}

fn host_asks(wire: &orbital_core_runtime::embed::wire::DispatchWire) -> Vec<String> {
    let response = serde_json::to_value(&wire.response).unwrap();
    response["clientEffectsByTrait"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["effect"][0] == "host-ask")
        .map(|e| e["effect"][1].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_browser_kernel_hands_a_kv_call_to_its_host_as_a_host_ask() {
    let k = orbital_core_runtime::embed::EmbeddedKernel::new(kv_program(), "c1").unwrap();
    let wire = k.dispatch("Kept", None, "INIT", Value::Null).unwrap();
    assert_eq!(host_asks(&wire), vec!["kv/get".to_string()], "{:?}", serde_json::to_value(&wire.response).unwrap());
}

#[test]
fn control_a_kv_call_on_a_store_the_orbital_is_not_granted_is_refused_by_the_kernel_never_asked() {
    let mut program = serde_json::to_value(kv_program()).unwrap();
    program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"][0]["effects"][0][1] = json!("other");
    let k = orbital_core_runtime::embed::EmbeddedKernel::new(from_json_value(&program).unwrap(), "c1").unwrap();
    let wire = k.dispatch("Kept", None, "INIT", Value::Null).unwrap();
    assert!(host_asks(&wire).is_empty());
    let response = serde_json::to_value(&wire.response).unwrap();
    let missed = response["emittedEvents"].as_array().into_iter().flatten().find(|e| e["event"] == "MISSED").expect("the failure route");
    assert!(missed["payload"]["error"].as_str().unwrap_or_default().contains("no kv grant"), "{missed}");
}

#[test]
fn a_kv_call_whose_store_is_a_config_knob_is_checked_against_the_store_it_names() {
    let mut program = serde_json::to_value(kv_program()).unwrap();
    let t = &mut program["orbitals"][0]["traits"][0];
    t["config"] = json!({ "store": { "type": "string", "default": "answers" } });
    t["stateMachine"]["transitions"][0]["effects"][0][1] = json!(["let", [["knob_store", "answers", "{\"version\":1,\"type\":{\"kind\":\"string\"}}"]], "@knob_store"]);
    let k = orbital_core_runtime::embed::EmbeddedKernel::new(from_json_value(&program).unwrap(), "c1").unwrap();
    let wire = k.dispatch("Kept", None, "INIT", Value::Null).unwrap();
    assert_eq!(host_asks(&wire), vec!["kv/get".to_string()], "{}", serde_json::to_value(&wire.response).unwrap());
}
