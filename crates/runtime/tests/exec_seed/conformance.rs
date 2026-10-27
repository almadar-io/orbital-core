//! EXEC-SEED conformance: the dispatch stamp, the mount hold and the mount batch.

use orbital_core_base::value::Value;
use orbital_core_runtime::effects::response::{LifecycleEvent, MountSeed};
use orbital_core_runtime::kernel::RuntimeKernel;
use orbital_core_runtime::process_result::ProcessEventResult;
use serde_json::json;
use crate::support::TestKernel;

type Outcomes = Vec<(String, ProcessEventResult)>;

fn stamp_kernel() -> RuntimeKernel {
    TestKernel::server().build(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Source", "linkedEntity": "Item",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [
                          ["set", "@entity.viaOperator", ["time/now"]],
                          ["set", "@entity.viaBinding", "@now"],
                          ["emit", "PING"]
                      ] }] } },
                { "name": "Listener", "linkedEntity": "Item",
                  "listens": [{ "event": "PING", "triggers": "PONG", "source": { "kind": "trait", "trait": "Source" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "PONG", "effects": [["set", "@entity.heardAt", "@now"]] }] } }
            ]
        }]
    }))
}

fn stamped(outcomes: &Outcomes, trait_name: &str, key: &str) -> f64 {
    let (_, result) = outcomes
        .iter()
        .find(|(t, r)| t == trait_name && r.executed)
        .unwrap_or_else(|| panic!("{trait_name} must execute"));
    result
        .entity_data
        .to_value()
        .get_path(key)
        .and_then(Value::as_number)
        .unwrap_or_else(|| panic!("{trait_name}.{key} must be a number, got {:?}", result.entity_data))
}

/// EXEC-SEED-1: `@now`, every `time/*` read and the cascaded listeners come from one stamp per dispatch.
#[test]
fn exec_seed_1_one_stamp_serves_the_operator_the_binding_and_the_cascaded_listener_of_a_dispatch() {
    let kernel = stamp_kernel();
    let go = || kernel.dispatch("Source", None, "GO", Value::Null).unwrap();
    let first = go();
    let stamp = stamped(&first, "Source", "viaOperator");
    assert_eq!(stamped(&first, "Source", "viaBinding"), stamp, "@now equals the time/now read of the same dispatch");
    assert_eq!(stamped(&first, "Listener", "heardAt"), stamp, "the cascaded listener reads the dispatch's stamp");
    std::thread::sleep(std::time::Duration::from_millis(5));
    let second = stamped(&go(), "Source", "viaOperator");
    assert!(second > stamp, "control: the next dispatch takes its own, later stamp ({second} vs {stamp})");
}

fn mount_kernel() -> RuntimeKernel {
    TestKernel::server().build(json!({
        "name": "Boot",
        "orbitals": [{
            "name": "Level",
            "entity": { "name": "World", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Seeder",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [
                          { "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "SEED", { "v": 1 }]] },
                          { "from": "idle", "to": "idle", "event": "PING" }
                      ] } },
                { "name": "Body",
                  "listens": [{ "event": "SEED", "triggers": "SET", "source": { "kind": "trait", "trait": "Seeder" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "ready" }, { "name": "seeded" }],
                      "transitions": [
                          { "from": "idle", "to": "ready", "event": "INIT" },
                          { "from": "ready", "to": "seeded", "event": "SET" }
                      ] } }
            ]
        }]
    }))
}

fn body_state(kernel: &RuntimeKernel) -> String {
    kernel
        .get_state("Body", None)
        .map_or_else(|| "idle".to_string(), |s| s.current_state)
}

fn leg(awaiting: &[&str]) -> Value {
    serde_json::from_value(json!({ "_awaitingInit": awaiting })).unwrap()
}

fn seeds(list: &[(&str, &str)]) -> Vec<MountSeed> {
    list.iter()
        .map(|(t, e)| MountSeed {
            r#trait: t.to_string(),
            event: LifecycleEvent::parse(e).expect("a lifecycle event"),
        })
        .collect()
}

fn ran(outcomes: &Outcomes) -> Vec<String> {
    outcomes
        .iter()
        .filter(|(_, r)| r.executed)
        .map(|(t, r)| format!("{t}:{}", r.event.clone().unwrap_or_default()))
        .collect()
}

/// EXEC-SEED-2: a routed delivery never reaches a trait before that trait's own lifecycle event has run on the current mount.
#[test]
fn exec_seed_2_a_routed_delivery_is_held_until_the_receivers_own_lifecycle_event_has_run() {
    let kernel = mount_kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    kernel.dispatch("Seeder", None, "INIT", Value::Null).unwrap();
    assert_eq!(body_state(&kernel), "idle", "mounted Body has not run INIT: the SEED is held");
    kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
    assert_eq!(body_state(&kernel), "seeded", "the held SEED is re-enqueued right after Body's INIT");

    let initialized = mount_kernel();
    initialized.mount_traits(["Seeder", "Body"]);
    initialized.dispatch("Body", None, "INIT", Value::Null).unwrap();
    initialized.dispatch("Seeder", None, "INIT", Value::Null).unwrap();
    assert_eq!(body_state(&initialized), "seeded", "control: Body already initialized, the SEED is delivered at once");

    let served = mount_kernel();
    served.dispatch_continuing("Seeder", None, "INIT", leg(&["Body"]), None, None).unwrap();
    assert_eq!(body_state(&served), "idle", "a server leg carrying the client's mount holds the seed for a trait the client still awaits");
    served.dispatch("Body", None, "INIT", Value::Null).unwrap();
    served.dispatch_continuing("Seeder", None, "PING", leg(&[]), None, None).unwrap();
    assert_eq!(body_state(&served), "seeded", "the client's mount releases what it has since initialized");
}

/// EXEC-SEED-4: a mount batch runs each seed's lifecycle in order, syncs the client's mount once, and skips an unregistered seed.
#[test]
fn exec_seed_4_a_mount_batch_runs_seeds_in_order_syncs_the_clients_mount_once_and_skips_unknown_traits() {
    let kernel = mount_kernel();
    kernel.mount_traits(["Seeder", "Body"]);
    let outcomes = kernel
        .dispatch_mount(&seeds(&[("Seeder", "INIT"), ("Nowhere", "INIT"), ("Body", "INIT")]), None, Value::Null)
        .unwrap();
    assert_eq!(ran(&outcomes), vec!["Seeder:INIT", "Body:INIT", "Body:SET"], "seeds in written order, the held SEED after Body's own INIT");
    assert!(!outcomes.iter().any(|(t, _)| t == "Nowhere"), "a seed naming no registered trait is skipped");

    let reversed = mount_kernel();
    reversed.mount_traits(["Seeder", "Body"]);
    let outcomes = reversed
        .dispatch_mount(&seeds(&[("Body", "INIT"), ("Seeder", "INIT")]), None, Value::Null)
        .unwrap();
    assert_eq!(ran(&outcomes), vec!["Body:INIT", "Seeder:INIT", "Body:SET"], "control: the order of the batch is the order of the lifecycle steps");

    let synced = mount_kernel();
    synced
        .dispatch_mount(&seeds(&[("Seeder", "INIT"), ("Body", "INIT")]), None, leg(&["Body"]))
        .unwrap();
    assert_eq!(body_state(&synced), "seeded", "the client's mount synced against every seed: Body's seed step releases the held SEED");
    let partial = mount_kernel();
    partial
        .dispatch_mount(&seeds(&[("Seeder", "INIT")]), None, leg(&["Body"]))
        .unwrap();
    assert_eq!(body_state(&partial), "idle", "control: with no seed for Body its held delivery stays held");
}
