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
        "typeContractVersion": 1, "orbitals": [{
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

fn stamped(outcomes: &Outcomes, trait_name: &str, key: &str) -> String {
    let (_, result) = outcomes
        .iter()
        .find(|(t, r)| t == trait_name && r.executed)
        .unwrap_or_else(|| panic!("{trait_name} must execute"));
    result
        .entity_data
        .to_value()
        .get_path(key)
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("{trait_name}.{key} must be an ISO instant, got {:?}", result.entity_data))
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
        "typeContractVersion": 1, "orbitals": [{
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

/// EXEC-SEED-2 for a mount no viewer declared first (a host's boot mount, G-CROSS-540): the batch declares its own
/// seeds, so an earlier seed's emit waits for a later seed's lifecycle step.
#[test]
fn exec_seed_2_a_mount_batch_declares_its_own_seeds_so_an_earlier_seeds_emit_waits_for_a_later_seeds_lifecycle() {
    let kernel = mount_kernel();
    let outcomes = kernel
        .dispatch_mount(&seeds(&[("Seeder", "INIT"), ("Body", "INIT")]), None, Value::Null)
        .unwrap();
    assert_eq!(ran(&outcomes), vec!["Seeder:INIT", "Body:INIT", "Body:SET"]);
    assert_eq!(body_state(&kernel), "seeded");

    let reversed = mount_kernel();
    let outcomes = reversed
        .dispatch_mount(&seeds(&[("Body", "INIT"), ("Seeder", "INIT")]), None, Value::Null)
        .unwrap();
    assert_eq!(ran(&outcomes), vec!["Body:INIT", "Seeder:INIT", "Body:SET"], "control: a receiver already initialized hears it at once");
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

fn figures_kernel() -> RuntimeKernel {
    TestKernel::server().build(json!({
        "name": "Home",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Home",
            "entity": { "name": "Figure", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "count", "type": "number" }] },
            "traits": [
                { "name": "Figures", "linkedEntity": "Figure", "dispatch": "client",
                  "listens": [{ "event": "FIGURES_READ", "triggers": "ITEMS_LOADED", "source": { "kind": "trait", "trait": "Fetcher" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "zeroed" }, { "name": "loaded" }],
                      "transitions": [
                          { "from": "idle", "to": "zeroed", "event": "INIT", "effects": [["set", "@entity.count", 0]] },
                          { "from": "loaded", "to": "zeroed", "event": "INIT", "effects": [["set", "@entity.count", 0]] },
                          { "from": "idle", "to": "loaded", "event": "ITEMS_LOADED", "effects": [["set", "@entity.count", "@payload.count"]] },
                          { "from": "zeroed", "to": "loaded", "event": "ITEMS_LOADED", "effects": [["set", "@entity.count", "@payload.count"]] }
                      ] } },
                { "name": "Fetcher", "linkedEntity": "Figure",
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "FIGURES_READ", { "count": 3 }]] }] } }
            ]
        }]
    }))
}

fn figures_count(outcomes: &Outcomes) -> Option<f64> {
    outcomes
        .iter()
        .rev()
        .find(|(t, r)| t == "Figures" && r.executed)
        .and_then(|(_, r)| r.entity_data.to_value().get_path("count").and_then(|v| v.as_number()))
}

/// EXEC-SEED-2 (G-CROSS-438): a `[client]` trait whose INIT zeroes a field never has the loaded count overwritten, whichever seed the
/// batch lists first: the emitting server trait's event is held until the receiver's own INIT has run.
#[test]
fn exec_seed_2_a_client_trait_keeps_the_count_a_server_trait_emits_during_the_mount_whichever_seed_runs_first() {
    let emitter_first = figures_kernel();
    let outcomes = emitter_first
        .dispatch_mount(&seeds(&[("Fetcher", "INIT"), ("Figures", "INIT")]), None, Value::Null)
        .unwrap();
    assert_eq!(ran(&outcomes), vec!["Fetcher:INIT", "Figures:INIT", "Figures:ITEMS_LOADED"], "the emit waits for Figures' INIT");
    assert_eq!(figures_count(&outcomes), Some(3.0));
    assert_eq!(emitter_first.get_state("Figures", None).map(|s| s.current_state).as_deref(), Some("loaded"));

    let receiver_first = figures_kernel();
    let outcomes = receiver_first
        .dispatch_mount(&seeds(&[("Figures", "INIT"), ("Fetcher", "INIT")]), None, Value::Null)
        .unwrap();
    assert_eq!(ran(&outcomes), vec!["Figures:INIT", "Fetcher:INIT", "Figures:ITEMS_LOADED"], "control: the receiver's INIT already ran, the emit is delivered at once");
    assert_eq!(figures_count(&outcomes), Some(3.0));
}
