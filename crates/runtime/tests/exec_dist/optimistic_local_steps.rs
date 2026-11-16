// An optimistic trait whose step needs no server posts no leg (EXEC-DIST-2), so the host's copy of it stays where it
// was; a later host answer must not move the browser's copy back (orb.almadar.io: a live-machine diagram whose
// build-time-answered describe left it on its loading render once the demo beside it loaded its rows).

use orbital_core_base::json_value::from_json_value;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use orbital_core_runtime::effects::{LifecycleEvent, MountSeed};
use orbital_core_runtime::embed::{EmbeddedKernel, EmbeddedServer, ServerProgress};
use serde_json::json;
use std::collections::HashSet;

fn schema(machine_dispatch: &str) -> OrbitalSchema {
    from_json_value(&json!({
        "name": "docs",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Page",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [
                { "name": "List", "linkedEntity": "Note", "dispatch": "client",
                  "emits": [{ "event": "MOVED", "scope": "external" }],
                  "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }, { "name": "ready" }],
                    "transitions": [
                        { "from": "loading", "to": "loading", "event": "INIT", "effects": [["fetch", "Note", { "emit": { "success": "LOADED" } }]] },
                        { "from": "loading", "to": "ready", "event": "LOADED", "effects": [["emit", "MOVED", {}]] }] } },
                { "name": "Machine", "linkedEntity": "View", "dispatch": machine_dispatch,
                  "emits": [{ "event": "DESCRIBED", "scope": "internal" }],
                  "listens": [{ "event": "MOVED", "triggers": "ADVANCE", "scope": "external", "source": { "kind": "trait", "trait": "List" } }],
                  "stateMachine": {
                    "states": [{ "name": "describing", "isInitial": true }, { "name": "idle" }],
                    "transitions": [
                        { "from": "describing", "to": "describing", "event": "INIT", "effects": [["render-ui", "main", { "type": "typography", "content": "loading" }], ["emit", "DESCRIBED", {}]] },
                        { "from": "describing", "to": "idle", "event": "DESCRIBED", "effects": [["render-ui", "main", { "type": "typography", "content": "ready" }]] },
                        { "from": "idle", "to": "idle", "event": "ADVANCE", "effects": [] }] } }
            ]
        }, {
            "name": "Views",
            "entity": { "name": "View", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": []
        }]
    }))
    .unwrap()
}

fn seed(t: &str) -> MountSeed {
    MountSeed { r#trait: t.to_string(), event: LifecycleEvent::Init }
}

/// The browser mounts the page against a host that holds the circuit state (the web shell's default topology), posts
/// every leg the mount and its folds produce, in order, and folds each answer.
fn mount_and_settle(machine_dispatch: &str) -> (String, Vec<String>) {
    let host = EmbeddedServer::new(schema(machine_dispatch)).unwrap();
    let client = EmbeddedKernel::new(schema(machine_dispatch), "tab").unwrap();
    let mount = client.dispatch_mount(&[seed("List"), seed("Machine")], None, Value::Null).unwrap();
    assert!(mount.failed.is_none(), "{:?}", mount.failed);
    let mut legs: std::collections::VecDeque<(String, orbital_core_runtime::effects::response::EventRequest)> =
        mount.stateful_legs.iter().map(|leg| (leg.orbital.clone(), leg.leg.clone())).collect();
    legs.extend(mount.hops.iter().map(|hop| (hop.orbital.clone(), hop.leg.clone())));
    let mut host_paints = Vec::new();
    let mut rounds = 0;
    while let Some((orbital, leg)) = legs.pop_front() {
        rounds += 1;
        assert!(rounds < 10, "the legs settle");
        let response = match host.start(&orbital, leg).unwrap() {
            ServerProgress::Done(response) => response,
            _ => panic!("the host answers at once"),
        };
        let applied = client.apply_server_response(&response, &HashSet::new()).unwrap();
        for painted in applied.response.client_effects_by_trait.iter().flatten().filter(|p| p.trait_name == "Machine") {
            host_paints.push(serde_json::to_string(&painted.effect).unwrap());
        }
        legs.extend(applied.server_legs.iter().map(|wire| (wire.orbital.clone(), wire.leg.clone())));
    }
    (client.get_state("Machine", None), host_paints)
}

#[test]
fn an_optimistic_step_that_needed_no_host_is_not_undone_by_the_hosts_answer_to_another_trait() {
    let (state, host_paints) = mount_and_settle("optimistic");
    assert!(state.contains("idle"), "{state}");
    assert!(!host_paints.iter().any(|p| p.contains("loading")), "a host answer painted the diagram back to its loading render after the browser moved it on: {host_paints:?}");
}

#[test]
fn control_a_client_trait_that_stepped_locally_keeps_its_state() {
    let (state, host_paints) = mount_and_settle("client");
    assert!(state.contains("idle"), "{state}");
    assert!(!host_paints.iter().any(|p| p.contains("loading")), "{host_paints:?}");
}
