// A frame keeps each fired tick's `runTick` leg for browser-stored data (`TickDispatch.browser_leg`), so the host that owns the store receives the write (ui `tick-browser-store`).

use orbital_core_runtime::embed::EmbeddedKernel;
use orbital_core_base::schema::types::OrbitalSchema;
use serde_json::{json, Value};

fn kernel(effects: Value) -> EmbeddedKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Draft", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "saved", "type": "number" }] },
            "auxiliaryEntities": [
                { "name": "Notes", "persistence": "persistent", "collection": "notes", "browser": true,
                  "fields": [{ "name": "id", "type": "string" }, { "name": "text", "type": "string" }] }
            ],
            "traits": [{
                "name": "Editor", "linkedEntity": "Draft", "dispatch": "client",
                "stateMachine": { "states": [{ "name": "editing", "isInitial": true }], "transitions": [] },
                "ticks": [{ "name": "autosave", "interval": 0, "effects": effects }]
            }]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "tab-1").unwrap()
}

fn frame_json(k: &EmbeddedKernel) -> Value {
    serde_json::to_value(k.frame(0).unwrap().expect("a tick fired")).unwrap()
}

#[test]
fn a_fired_tick_that_writes_browser_data_surfaces_its_leg_on_the_frame() {
    let frame = frame_json(&kernel(
        json!([["persist", "create", "Notes", { "text": "x" }]]),
    ));
    let legs = frame["browserLegs"].as_array().expect("browserLegs");
    assert_eq!(legs.len(), 1, "{frame}");
    assert_eq!(legs[0]["orbital"], "Main");
    assert_eq!(
        legs[0]["leg"]["runTick"],
        json!({ "trait": "Editor", "tick": "autosave" })
    );
    assert_eq!(legs[0]["leg"]["targetTrait"], "Editor");
    assert_eq!(legs[0]["leg"]["clientId"], "tab-1");
}

#[test]
fn control_a_tick_with_only_frame_writes_has_no_leg() {
    let frame = frame_json(&kernel(json!([["set", "@entity.saved", 1]])));
    assert_eq!(frame["browserLegs"], json!([]));
}
