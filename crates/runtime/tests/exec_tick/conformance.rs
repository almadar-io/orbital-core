//! EXEC-TICK conformance: where a tick's emits go.


use orbital_core_base::value::Value;
use serde_json::json;
use crate::support::TestKernel;

#[test]
fn exec_tick_2_a_sibling_with_a_same_named_arm_and_no_listens_does_not_hear_a_tick_emit() {
    let kernel = TestKernel::client().build(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }, { "name": "beats", "type": "number" }] },
            "traits": [
                { "name": "Ticker", "linkedEntity": "Note", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "running", "isInitial": true }],
                      "transitions": [{ "from": "running", "to": "running", "event": "INIT", "effects": [] }] },
                  "ticks": [{ "name": "beat", "interval": 1000, "effects": [["emit", "BEAT", { "n": 1 }]] }] },
                { "name": "Listener", "linkedEntity": "Note", "dispatch": "client",
                  "listens": [{ "event": "BEAT", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Ticker" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                      "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD", "effects": [] }] } },
                { "name": "Sibling", "linkedEntity": "Note", "dispatch": "client",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "overheard" }],
                      "transitions": [{ "from": "idle", "to": "overheard", "event": "BEAT", "effects": [] }] } }
            ]
        }]
    }));
    kernel.dispatch_with_server_leg("App", "Ticker", None, "INIT", Value::Null, None).unwrap();

    let tick = kernel.dispatch_tick("Main", "Ticker", None, "beat", None).unwrap();
    assert_eq!(tick.emits.len(), 1);
    let states = &tick.emits[0].dispatch.response.states;
    assert_eq!(states.get("Listener").map(String::as_str), Some("heard"), "control: the declared listener hears it");
    assert_ne!(states.get("Sibling").map(String::as_str), Some("overheard"), "no bare-key broadcast of a tick emit");
    assert_ne!(kernel.snapshot_trait("Sibling", None).state, "overheard");
}

use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::embed::EmbeddedKernel;
use orbital_core_runtime::kernel::TickDispatch;
use serde_json::Value as Json;

fn client_tick(effects: Json) -> TickDispatch {
    let kernel = TestKernel::client().build(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Draft", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "saved", "type": "number" }] },
            "auxiliaryEntities": [
                { "name": "Notes", "persistence": "persistent", "collection": "notes", "browser": true,
                  "fields": [{ "name": "id", "type": "string" }, { "name": "text", "type": "string" }] }
            ],
            "traits": [{
                "name": "Editor", "linkedEntity": "Draft", "dispatch": "client",
                "stateMachine": { "states": [{ "name": "editing", "isInitial": true }], "transitions": [] },
                "ticks": [{ "name": "autosave", "interval": 1000, "effects": effects }]
            }]
        }]
    }));
    kernel.dispatch_tick("Main", "Editor", None, "autosave", Some("tab-1")).unwrap()
}

/// EXEC-TICK-3: a client tick's browser-store writes leave as one tick leg carrying the pre-tick state and row.
#[test]
fn exec_tick_3_a_browser_stored_write_leaves_as_one_tick_leg_with_the_pre_tick_state_and_row() {
    let out = client_tick(json!([["set", "@entity.saved", 1], ["persist", "create", "Notes", { "text": "x" }]]));
    let leg = out.browser_leg.expect("the browser-stored write leaves as a tick leg");
    let run = leg.run_tick.as_ref().expect("the leg is a run-tick leg");
    assert_eq!((run.trait_name.as_str(), run.tick.as_str()), ("Editor", "autosave"));
    let from = leg.traits.as_ref().and_then(|t| t.first()).map(|t| t.from.as_str());
    assert_eq!(from, Some("editing"), "the leg carries the trait's pre-tick state");
    let row = serde_json::to_value(leg.entity_by_trait.as_ref().and_then(|rows| rows.get("Editor"))).expect("row serializes");
    assert_ne!(row.get("saved"), Some(&json!(1.0)), "the leg carries the pre-tick row, not the tick's own write: {row}");
}

#[test]
fn exec_tick_3_control_a_tick_with_only_frame_writes_sends_no_leg() {
    let out = client_tick(json!([["set", "@entity.saved", 1]]));
    assert!(out.browser_leg.is_none(), "no browser-stored write, no leg");
}

fn visible_kernel() -> EmbeddedKernel {
    let tick = |name: &str, visible: bool| {
        json!({
            "name": name, "linkedEntity": "E", "dispatch": "client",
            "stateMachine": { "states": [{ "name": "on", "isInitial": true }], "transitions": [] },
            "ticks": [{ "name": "beat", "interval": 0, "whileVisible": visible, "effects": [["set", "@entity.n", 1]] }]
        })
    };
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number" }] },
            "traits": [tick("V", true), tick("P", false), tick("U", true)]
        }]
    }))
    .unwrap();
    EmbeddedKernel::new(schema, "c").unwrap()
}

fn fired_at(k: &EmbeddedKernel, now_ms: u64) -> Vec<String> {
    let mut names: Vec<String> = k
        .frame(now_ms)
        .unwrap()
        .map(|f| f.fired.into_iter().map(|t| t.trait_name).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// EXEC-TICK-4: a `[visible]` tick skips its firings while its trait is off screen; skipped firings are dropped.
#[test]
fn exec_tick_4_a_visible_tick_is_skipped_off_screen_and_resumes_without_a_burst() {
    let k = visible_kernel();
    k.set_trait_on_screen("V", false);
    k.set_trait_on_screen("P", false);
    assert_eq!(fired_at(&k, 0), ["P", "U"], "V is skipped off screen; the plain tick and the unreported visible tick fire");
    assert_eq!(fired_at(&k, 16), ["P", "U"], "still skipped");
    k.set_trait_on_screen("V", true);
    let resumed = fired_at(&k, 32);
    assert_eq!(resumed.iter().filter(|n| n.as_str() == "V").count(), 1, "back on screen it fires once, no burst of the dropped firings: {resumed:?}");
}

#[test]
fn exec_tick_4_control_a_plain_tick_off_screen_and_an_unreported_visible_tick_still_fire() {
    let k = visible_kernel();
    k.set_trait_on_screen("P", false);
    let fired = fired_at(&k, 0);
    assert!(fired.contains(&"P".to_string()), "a tick without [visible] ignores presence: {fired:?}");
    assert!(fired.contains(&"U".to_string()), "an unreported trait counts as on screen: {fired:?}");
}
