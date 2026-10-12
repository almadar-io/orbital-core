//! A browser-run tick's data effects on browser-stored entities (`[persistent: x, local]`) leave
//! as one `runTick` leg to the data authority instead of being discarded — a tick has no event
//! for an event leg to replay under (owner ruling 2026-10-12, drift D-1). Server-owned data in a
//! browser-run tick is an `orb validate` error (`ORB_T_CLIENT_TICK_SERVER_EFFECT`).

use orbital_core::effects::RunTick;
use orbital_core::runtime::{
    EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel, TickDispatch,
};
use orbital_core::ServerLegCollector;
use orbital_core::{EffectDelegate, OrbitalSchema, RuntimeEnvironment};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn tick_of(effects: Json) -> TickDispatch {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Draft", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "saved", "type": "number" }] },
            "auxiliaryEntities": [
                { "name": "Notes", "persistence": "persistent", "collection": "notes", "local": true,
                  "fields": [{ "name": "id", "type": "string" }, { "name": "text", "type": "string" }] },
                { "name": "Ledger", "persistence": "persistent", "collection": "ledgers",
                  "fields": [{ "name": "id", "type": "string" }] }
            ],
            "traits": [{
                "name": "Editor", "linkedEntity": "Draft",
                "stateMachine": { "states": [{ "name": "editing", "isInitial": true }], "transitions": [] },
                "ticks": [{ "name": "autosave", "interval": 1000, "effects": effects }]
            }]
        }]
    }))
    .unwrap();
    let collector = Arc::new(ServerLegCollector::new());
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        KernelConfig {
            environment: RuntimeEnvironment::Client,
            delegate: Some(collector as Arc<dyn EffectDelegate>),
            ..KernelConfig::default()
        },
    );
    kernel.register_schema(schema).unwrap();
    kernel
        .dispatch_tick("Main", "Editor", None, "autosave", Some("tab-1"))
        .unwrap()
}

#[test]
fn a_tick_writing_browser_stored_data_sends_one_run_tick_leg() {
    let out = tick_of(json!([["persist", "create", "Notes", { "text": "x" }]]));
    let leg = out
        .browser_leg
        .expect("a runTick leg for the browser-stored write");
    assert_eq!(
        leg.run_tick,
        Some(RunTick {
            trait_name: "Editor".to_string(),
            tick: "autosave".to_string()
        })
    );
    assert_eq!(leg.event, "$TICK_autosave");
    assert_eq!(leg.target_trait.as_deref(), Some("Editor"));
    assert_eq!(leg.client_id.as_deref(), Some("tab-1"));
    assert_eq!(out.discarded_server_effects, 0, "nothing is dropped");
}

#[test]
fn the_leg_carries_the_pre_tick_row_so_the_store_replays_the_tick_once() {
    let out = tick_of(json!([
        ["set", "@entity.saved", 1],
        ["persist", "create", "Notes", { "text": "x" }]
    ]));
    let leg = out.browser_leg.expect("a runTick leg");
    let row: Json = serde_json::to_value(
        leg.entity_by_trait
            .as_ref()
            .and_then(|rows| rows.get("Editor")),
    )
    .expect("the row serializes");
    let after: Json =
        serde_json::to_value(&out.outcome.entity_data).expect("the outcome row serializes");
    assert_eq!(
        after.get("saved"),
        Some(&json!(1.0)),
        "the tick wrote: {after}"
    );
    assert_ne!(
        row.get("saved"),
        Some(&json!(1.0)),
        "the leg must not carry the tick's own write: {row}"
    );
}

#[test]
fn control_a_tick_with_only_frame_writes_sends_no_leg() {
    let out = tick_of(json!([["set", "@entity.saved", 1]]));
    assert!(out.browser_leg.is_none());
    assert_eq!(out.discarded_server_effects, 0);
}

#[test]
fn edge_server_owned_data_in_a_browser_tick_takes_no_leg_and_is_reported_discarded() {
    let out = tick_of(json!([["persist", "create", "Ledger", {}]]));
    assert!(
        out.browser_leg.is_none(),
        "server-owned data never rides a browser leg"
    );
    assert_eq!(out.discarded_server_effects, 1);
}
