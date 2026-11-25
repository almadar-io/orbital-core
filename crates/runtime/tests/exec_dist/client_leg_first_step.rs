// A trait the browser stepped more than once in one cascade reaches a stateless host from its first step: the leg
// carries its pre-dispatch state and row (EXEC-DIST-2), so the host's replay is not refused as already running
// (G-STD-474, the launchpad's Stop preview: std-cli's CLI_RUN then CLI_ENV_NEXT in the browser).

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::kernel::RuntimeKernel;
use serde_json::json;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = from_json_value(&json!({
        "name": "board",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Fleet",
            "entity": { "name": "Run", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "stage", "type": "string", "default": "" }] },
            "traits": [
                { "name": "View", "linkedEntity": "Run", "dispatch": "client",
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "CLICK", "effects": [["emit", "GO", {}]] }] } },
                { "name": "Board", "linkedEntity": "Run",
                  "listens": [{ "event": "GO", "triggers": "GO", "source": { "kind": "trait", "trait": "View" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "RUN", {}]] }] } },
                { "name": "Runner", "linkedEntity": "Run",
                  "listens": [{ "event": "RUN", "triggers": "START", "source": { "kind": "trait", "trait": "Board" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "running" }],
                    "transitions": [
                        { "from": "idle", "to": "running", "event": "START", "effects": [["set", "@entity.stage", "stop"], ["emit", "NEXT", {}]] },
                        { "from": "running", "to": "running", "event": "NEXT", "effects": [["persist", "create", "Run", { "stage": "@entity.stage" }]] }] } }
            ]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
}

fn click() -> orbital_core_runtime::kernel::ClientDispatch {
    kernel()
        .dispatch_with_server_leg("Fleet", "View", None, "CLICK", Value::Null, Some("c1"))
        .unwrap()
}

#[test]
fn a_trait_stepped_twice_in_the_browser_is_carried_from_its_first_step() {
    let d = click();
    assert!(
        !d.server_legs.is_empty(),
        "the runner's persist owes the host a leg"
    );
    for leg in &d.server_legs {
        let carried: Vec<String> = leg
            .traits
            .iter()
            .flatten()
            .filter(|t| t.r#trait == "Runner")
            .map(|t| t.from.clone())
            .collect();
        assert!(
            carried.iter().all(|from| from == "idle"),
            "Runner carried from {carried:?}"
        );
        if let Some(row) = leg
            .entity_by_trait
            .as_ref()
            .and_then(|rows| rows.get("Runner"))
        {
            let stage = row
                .get("stage")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            assert_ne!(
                stage, "stop",
                "Runner's carried row is the one its first step wrote over: {row:?}"
            );
        }
    }
}

#[test]
fn control_a_trait_stepped_once_is_carried_from_its_pre_dispatch_state() {
    let d = click();
    let boards: Vec<String> = d
        .server_legs
        .iter()
        .flat_map(|leg| leg.traits.iter().flatten())
        .filter(|t| t.r#trait == "Board")
        .map(|t| t.from.clone())
        .collect();
    assert!(
        !boards.is_empty() && boards.iter().all(|from| from == "idle"),
        "Board carried from {boards:?}"
    );
}
