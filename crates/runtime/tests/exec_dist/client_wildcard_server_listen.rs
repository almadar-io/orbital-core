// EXEC-DIST-2: a `[client]` front that forwards an ask to a worker only the host runs (`*.FWD`, listed in
// `_metadata.serverListens` with an `any` source) leaves the forwarding step as a leg, also when the front was reached
// by another client trait's emit (std-kflow's dashboard → knowledge-paths-front → knowledge-paths).

use orbital_core_runtime::kernel::RuntimeKernel;
use serde_json::json;
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value};

fn kernel(server_source: serde_json::Value) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1,
        "_metadata": { "serverListens": [{ "orbital": "Paths", "listen": {
            "event": "FWD_LIST", "triggers": "LIST", "source": server_source
        } }] },
        "orbitals": [{
            "name": "Dash",
            "entity": { "name": "Board", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Grid", "linkedEntity": "Board", "dispatch": "client",
                "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }],
                    "transitions": [{ "from": "loading", "to": "loading", "event": "LOAD", "effects": [["emit", "ASK_LIST", { "key": "k1" }]] }]
                },
                "emits": [{ "event": "ASK_LIST", "scope": "external" }]
            }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Grid" }, { "ref": "Front" }] }]
        }, {
            "name": "PathFront",
            "entity": { "name": "FrontRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Front", "linkedEntity": "FrontRun", "dispatch": "client",
                "listens": [{ "event": "ASK_LIST", "triggers": "ASK_LIST", "source": { "kind": "any" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                    "transitions": [
                        { "from": "idle", "to": "busy", "event": "ASK_LIST", "effects": [["emit", "FWD_LIST", {}]] },
                        { "from": "idle", "to": "idle", "event": "POKE", "effects": [["emit", "FWD_LIST", {}]] }
                    ]
                },
                "emits": [{ "event": "FWD_LIST", "scope": "external" }]
            }]
        }]
    }))
    .unwrap();
    crate::support::TestKernel::client().register(schema)
}

fn any() -> serde_json::Value {
    json!({ "kind": "any" })
}

#[test]
fn a_client_front_seeded_directly_leaves_its_forward_as_a_leg() {
    let d = kernel(any())
        .dispatch_with_server_leg("PathFront", "Front", None, "POKE", Value::Null, Some("c1"))
        .unwrap();
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Front"));
}

#[test]
fn a_client_front_reached_by_another_client_traits_emit_leaves_its_forward_as_a_leg() {
    let d = kernel(any())
        .dispatch_with_server_leg("Dash", "Grid", None, "LOAD", Value::Null, Some("c1"))
        .unwrap();
    assert_eq!(d.server_legs.len(), 1, "{:?}", d.server_legs);
    assert_eq!(d.server_legs[0].target_trait.as_deref(), Some("Front"));
    assert_eq!(d.server_legs[0].event, "ASK_LIST");
}

#[test]
fn control_a_forward_no_server_orbital_hears_posts_nothing() {
    let d = kernel(json!({ "kind": "trait", "trait": "Elsewhere" }))
        .dispatch_with_server_leg("Dash", "Grid", None, "LOAD", Value::Null, Some("c1"))
        .unwrap();
    assert!(d.server_legs.is_empty(), "{:?}", d.server_legs);
}

/// The kflow shape at mount: `Board` (`[client]`) asks from its INIT before `Desk` (`[client]`) mounts, so the ask is held
/// for Desk's INIT (EXEC-SEED-2); Desk's answer to it is a forward only the host hears.
fn held_schema(server_source: serde_json::Value) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1,
        "_metadata": { "serverListens": [{ "orbital": "Paths", "listen": {
            "event": "FWD_MAP", "triggers": "MAP", "source": server_source
        } }] },
        "orbitals": [{
            "name": "Dash",
            "entity": { "name": "Board", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Board", "linkedEntity": "Board", "dispatch": "client",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "asking" }],
                    "transitions": [{ "from": "idle", "to": "asking", "event": "INIT", "effects": [["emit", "ASK_MAP", {}]] }]
                },
                "emits": [{ "event": "ASK_MAP", "scope": "external" }]
            }],
            "pages": [{ "name": "Home", "path": "/", "traits": [{ "ref": "Board" }] }]
        }, {
            "name": "DeskFront",
            "entity": { "name": "DeskRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Desk", "linkedEntity": "DeskRun", "dispatch": "client",
                "listens": [{ "event": "ASK_MAP", "triggers": "ASK_MAP", "source": { "kind": "any" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "busy", "event": "ASK_MAP", "effects": [["emit", "FWD_MAP", {}]] }
                    ]
                },
                "emits": [{ "event": "FWD_MAP", "scope": "external" }]
            }],
            "pages": [{ "name": "Desk", "path": "/desk", "traits": [{ "ref": "Desk" }] }]
        }]
    }))
    .unwrap()
}

fn held_kernel(server_source: serde_json::Value) -> RuntimeKernel {
    let kernel = crate::support::TestKernel::client().register(held_schema(server_source));
    kernel.mount_traits(["Board", "Desk"]);
    kernel
}

#[test]
fn a_held_ask_a_client_front_forwards_off_client_leaves_as_its_own_leg() {
    let kernel = held_kernel(any());
    let board = kernel
        .dispatch_with_server_leg("Dash", "Board", None, "INIT", Value::Null, Some("c1"))
        .unwrap();
    assert!(board.server_legs.is_empty(), "{:?}", board.server_legs);
    let desk = kernel
        .dispatch_with_server_leg("DeskFront", "Desk", None, "INIT", Value::Null, Some("c1"))
        .unwrap();
    let legs: Vec<(String, Option<String>, Option<String>)> = desk
        .server_legs
        .iter()
        .map(|l| {
            (
                l.event.clone(),
                l.target_trait.clone(),
                l.source_trait.clone(),
            )
        })
        .collect();
    assert!(
        legs.contains(&(
            "ASK_MAP".to_string(),
            Some("Desk".to_string()),
            Some("Board".to_string())
        )),
        "the forward must reach the host: {legs:?}"
    );
}

#[test]
fn control_a_held_ask_whose_forward_no_server_orbital_hears_posts_only_the_init_replay() {
    let kernel = held_kernel(json!({ "kind": "trait", "trait": "Elsewhere" }));
    kernel
        .dispatch_with_server_leg("Dash", "Board", None, "INIT", Value::Null, Some("c1"))
        .unwrap();
    let desk = kernel
        .dispatch_with_server_leg("DeskFront", "Desk", None, "INIT", Value::Null, Some("c1"))
        .unwrap();
    assert!(
        desk.server_legs.iter().all(|l| l.event != "ASK_MAP"),
        "{:?}",
        desk.server_legs
    );
}

fn mount_seed(t: &str) -> orbital_core_runtime::effects::MountSeed {
    orbital_core_runtime::effects::MountSeed {
        r#trait: t.to_string(),
        event: orbital_core_runtime::effects::LifecycleEvent::Init,
    }
}

fn mounted_desk_hops(server_source: serde_json::Value) -> Vec<(String, Option<String>)> {
    let k =
        orbital_core_runtime::embed::EmbeddedKernel::new(held_schema(server_source), "c1").unwrap();
    k.mount(vec!["Board".to_string(), "Desk".to_string()]);
    let mount = k
        .dispatch_mount(
            &[mount_seed("Board"), mount_seed("Desk")],
            None,
            Value::Null,
        )
        .unwrap();
    mount
        .hops
        .iter()
        .map(|h| (h.leg.event.clone(), h.leg.target_trait.clone()))
        .collect()
}

/// EXEC-DIST-2 at mount: the held ask a `[client]` front answers with an off-client forward is a hop of its own, never
/// taken for the front's INIT replay (the host never runs the held delivery).
#[test]
fn a_mount_posts_the_held_ask_a_client_front_forwards_off_client_as_a_hop() {
    let hops = mounted_desk_hops(any());
    assert!(
        hops.contains(&("ASK_MAP".to_string(), Some("Desk".to_string()))),
        "{hops:?}"
    );
}

#[test]
fn control_a_mount_whose_held_ask_reaches_no_server_orbital_posts_no_hop() {
    assert!(mounted_desk_hops(json!({ "kind": "trait", "trait": "Elsewhere" })).is_empty());
}

/// The kflow fold: the host answers the front's map ask (`MAP_RESULT`); the front's own `NEXT` then forwards its queued
/// list ask, which only the host hears.
fn fold_schema(server_source: serde_json::Value) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1,
        "_metadata": { "serverListens": [{ "orbital": "Paths", "listen": {
            "event": "FWD_LIST", "triggers": "LIST", "source": server_source
        } }] },
        "orbitals": [{
            "name": "DeskFront",
            "entity": { "name": "DeskRun", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Desk", "linkedEntity": "DeskRun", "dispatch": "client",
                "listens": [{ "event": "MAP_RESULT", "triggers": "DONE", "source": { "kind": "any" } }],
                "stateMachine": {
                    "states": [{ "name": "busy", "isInitial": true }, { "name": "idle" }],
                    "transitions": [
                        { "from": "busy", "to": "idle", "event": "DONE", "effects": [["emit", "NEXT", {}]] },
                        { "from": "idle", "to": "busy", "event": "NEXT", "effects": [["emit", "FWD_LIST", {}]] }
                    ]
                },
                "emits": [{ "event": "NEXT" }, { "event": "FWD_LIST", "scope": "external" }]
            }],
            "pages": [{ "name": "Desk", "path": "/desk", "traits": [{ "ref": "Desk" }] }]
        }]
    }))
    .unwrap()
}

fn folded_legs(server_source: serde_json::Value) -> Vec<(String, Option<String>)> {
    let k = orbital_core_runtime::embed::EmbeddedKernel::new(fold_schema(server_source), "c1").unwrap();
    k.mount(vec!["Desk".to_string()]);
    let answer = orbital_core_runtime::effects::response::EventResponse {
        success: true,
        transitioned: true,
        emitted_events: vec![orbital_core_base::bus_event::EmittedEvent {
            event: "MAP_RESULT".to_string(),
            payload: None,
            source: Some(orbital_core_base::bus_event::BusEventSource {
                r#trait: Some("Lister".to_string()),
                orbital: Some("Paths".to_string()),
                ..Default::default()
            }),
        }],
        ..Default::default()
    };
    let applied = k.apply_server_response(&answer, &std::collections::HashSet::new()).unwrap();
    applied.server_legs.iter().map(|r| (r.leg.event.clone(), r.leg.target_trait.clone())).collect()
}

/// EXEC-DIST-2 + EXEC-DIST-10: a fold step of a `[client]` trait whose own emit only the host hears is handed back as
/// a leg, or the queued ask never reaches the host.
#[test]
fn a_fold_hands_back_the_leg_a_client_fronts_own_forward_owes() {
    let legs = folded_legs(any());
    assert!(legs.contains(&("NEXT".to_string(), Some("Desk".to_string()))), "{legs:?}");
}

#[test]
fn control_a_fold_whose_forward_no_server_orbital_hears_hands_back_nothing() {
    assert!(folded_legs(json!({ "kind": "trait", "trait": "Elsewhere" })).is_empty());
}

fn wizard_server() -> orbital_core_runtime::embed::EmbeddedServer {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App", "typeContractVersion": 1,
        "orbitals": [{ "name": "Steps",
            "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Wizard", "linkedEntity": "Run",
                "stateMachine": { "states": [{ "name": "first", "isInitial": true }, { "name": "second" }, { "name": "done" }],
                    "transitions": [{ "from": "second", "to": "done", "event": "NEXT" }] } }] }]
    }))
    .unwrap();
    orbital_core_runtime::embed::EmbeddedServer::new(schema).unwrap()
}

fn carried_step(from: &str, event: &str) -> orbital_core_runtime::effects::response::TraitFrom {
    orbital_core_runtime::effects::response::TraitFrom { r#trait: "Wizard".to_string(), from: from.to_string(), event: Some(event.to_string()) }
}

fn replay(traits: Vec<orbital_core_runtime::effects::response::TraitFrom>) -> Option<String> {
    let request = orbital_core_runtime::effects::response::EventRequest {
        event: "NEXT".to_string(),
        target_trait: Some("Wizard".to_string()),
        traits: Some(traits),
        ..Default::default()
    };
    match wizard_server().start("Steps", request).unwrap() {
        orbital_core_runtime::embed::ServerProgress::Done(response) => response.states.get("Wizard").cloned(),
        _ => panic!("a lone request with no I/O settles at once"),
    }
}

/// EXEC-DIST-11: a client leg carries one entry per step a trait took; the replayed event starts from the entry of that
/// event, not from a later step's state.
#[test]
fn a_leg_replays_its_target_from_the_entry_of_the_replayed_event() {
    assert_eq!(replay(vec![carried_step("second", "NEXT"), carried_step("first", "LATER")]).as_deref(), Some("done"));
}

#[test]
fn control_a_leg_whose_only_entry_is_another_state_replays_from_it() {
    assert_eq!(replay(vec![carried_step("first", "NEXT")]).as_deref(), Some("first"));
}
