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
                        { "from": "describing", "to": "idle", "event": "DESCRIBED", "effects": [["set", "@entity.label", "drawn"], ["render-ui", "main", { "type": "typography", "content": "@entity.label" }]] },
                        { "from": "idle", "to": "idle", "event": "ADVANCE", "effects": [] }] } }
            ]
        }, {
            "name": "Views",
            "entity": { "name": "View", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "label", "type": "string", "default": "" }] },
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

/// The browser mounts the page (the Machine steps to `idle` on its own), then folds `answer` from a host that answered
/// the List's leg from its own copy of every trait: the in-process browser store, stateless per leg (EXEC-DIST-2).
fn mounted_then_folded(answer: serde_json::Value) -> String {
    let client = EmbeddedKernel::new(schema("optimistic"), "tab").unwrap();
    let mount = client.dispatch_mount(&[seed("List"), seed("Machine")], None, Value::Null).unwrap();
    assert!(mount.failed.is_none(), "{:?}", mount.failed);
    assert!(client.get_state("Machine", None).contains("idle"));
    let response: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(answer).unwrap();
    client.apply_server_response(&response, &HashSet::new()).unwrap();
    client.get_state("Machine", None)
}

fn list_hop() -> serde_json::Value {
    json!({ "trait": "List", "from": "loading", "to": "ready", "event": "LOADED", "effects": [] })
}

#[test]
fn a_hosts_report_of_an_optimistic_trait_it_did_not_run_is_ignored() {
    let state = mounted_then_folded(json!({ "success": true, "emittedEvents": [],
        "states": { "List": "ready", "Machine": "describing" }, "transitions": [list_hop()] }));
    assert!(state.contains("idle"), "the host's held copy moved the browser's committed step back: {state}");
}

#[test]
fn control_a_host_that_ran_the_optimistic_trait_from_its_current_state_is_adopted() {
    let state = mounted_then_folded(json!({ "success": true, "emittedEvents": [],
        "states": { "List": "ready", "Machine": "describing" },
        "transitions": [list_hop(), { "trait": "Machine", "from": "idle", "to": "describing", "event": "ADVANCE", "effects": [] }] }));
    assert!(state.contains("describing"), "{state}");
}

#[test]
fn edge_a_host_that_ran_the_optimistic_trait_from_a_state_the_browser_left_is_ignored() {
    let state = mounted_then_folded(json!({ "success": true, "emittedEvents": [],
        "states": { "List": "ready", "Machine": "describing" },
        "transitions": [list_hop(), { "trait": "Machine", "from": "describing", "to": "describing", "event": "INIT", "effects": [] }] }));
    assert!(state.contains("idle"), "{state}");
}

fn host_paints_after_mount(machine_hop_from: Option<&str>) -> Vec<String> {
    let client = EmbeddedKernel::new(schema("optimistic"), "tab").unwrap();
    client.dispatch_mount(&[seed("List"), seed("Machine")], None, Value::Null).unwrap();
    let mut hops = vec![list_hop()];
    hops.extend(machine_hop_from.map(|from| json!({ "trait": "Machine", "from": from, "to": "describing", "event": "INIT", "effects": [] })));
    let response: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "emittedEvents": [],
        "states": { "List": "ready", "Machine": "describing" },
        "transitions": hops,
        "clientEffectsByTrait": [
            { "traitName": "Machine", "effect": ["render-ui", "main", { "type": "typography", "content": "loading" }] },
            { "traitName": "List", "effect": ["render-ui", "main", { "type": "typography", "content": "rows" }] }]
    })).unwrap();
    let applied = client.apply_server_response(&response, &HashSet::new()).unwrap();
    applied.host_paints.iter().map(|p| format!("{}:{}", p.trait_name, serde_json::to_string(&p.effect).unwrap())).collect()
}

#[test]
fn a_host_paint_of_an_optimistic_trait_the_client_stepped_past_is_not_handed_to_the_painter() {
    let paints = host_paints_after_mount(Some("describing"));
    assert!(!paints.iter().any(|p| p.starts_with("Machine:")), "the host's replay of INIT would paint the diagram back to loading: {paints:?}");
    assert!(paints.iter().any(|p| p.starts_with("List:")), "{paints:?}");
}

#[test]
fn a_host_repaint_of_an_optimistic_trait_it_did_not_run_is_not_handed_to_the_painter() {
    let paints = host_paints_after_mount(None);
    assert!(!paints.iter().any(|p| p.starts_with("Machine:")), "the host's entity-bound repaint of its held render is its copy, not the client's: {paints:?}");
    assert!(paints.iter().any(|p| p.starts_with("List:")), "{paints:?}");
}

#[test]
fn control_a_host_paint_from_the_state_the_client_holds_is_handed_to_the_painter() {
    let paints = host_paints_after_mount(Some("idle"));
    assert!(paints.iter().any(|p| p.starts_with("Machine:")), "{paints:?}");
}

fn machine_paints_after_host_row(machine_hop_from: Option<&str>) -> Vec<String> {
    let client = EmbeddedKernel::new(schema("optimistic"), "tab").unwrap();
    client.dispatch_mount(&[seed("List"), seed("Machine")], None, Value::Null).unwrap();
    let mut hops = vec![list_hop()];
    hops.extend(machine_hop_from.map(|from| json!({ "trait": "Machine", "from": from, "to": "idle", "event": "ADVANCE", "effects": [] })));
    let response: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "emittedEvents": [], "states": { "List": "ready", "Machine": "describing" }, "transitions": hops,
        "frames": { "from": 0, "to": 0, "ops": [["set", { "Machine": { "id": "singleton", "label": "" } }]] }
    })).unwrap();
    let applied = client.apply_server_response(&response, &HashSet::new()).unwrap();
    applied.response.client_effects_by_trait.iter().flatten().filter(|p| p.trait_name == "Machine").map(|p| serde_json::to_string(&p.effect).unwrap()).collect()
}

#[test]
fn a_hosts_row_for_an_optimistic_trait_it_did_not_run_does_not_overwrite_the_clients() {
    let paints = machine_paints_after_host_row(None);
    assert!(paints.is_empty(), "the host's held row repainted the diagram empty: {paints:?}");
}

#[test]
fn control_a_hosts_row_for_an_optimistic_trait_it_ran_from_the_clients_state_is_folded() {
    let paints = machine_paints_after_host_row(Some("idle"));
    assert!(paints.iter().any(|p| p.contains(r#""content":"""#)), "{paints:?}");
}

fn machine_steps_after_host_moved(machine_hop: Option<serde_json::Value>) -> Vec<String> {
    let client = EmbeddedKernel::new(schema("optimistic"), "tab").unwrap();
    client.set_host_fans_out(true);
    client.dispatch_mount(&[seed("List"), seed("Machine")], None, Value::Null).unwrap();
    let machine_state = machine_hop.as_ref().map_or("describing", |_| "idle");
    let mut hops = vec![list_hop()];
    hops.extend(machine_hop);
    let response: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "states": { "List": "ready", "Machine": machine_state }, "transitions": hops,
        "emittedEvents": [{ "event": "MOVED", "payload": {}, "source": { "orbital": "Page", "trait": "List", "dispatched": true } }]
    })).unwrap();
    let applied = client.apply_server_response(&response, &HashSet::new()).unwrap();
    applied.applied.iter().map(|(t, s)| format!("{t}:{s}")).collect()
}

#[test]
fn a_listener_the_host_could_not_run_on_its_held_copy_runs_on_the_client() {
    let steps = machine_steps_after_host_moved(None);
    assert!(steps.iter().any(|s| s.starts_with("Machine:")), "the diagram never followed the demo: {steps:?}");
}

#[test]
fn control_a_listener_the_host_ran_is_not_run_twice() {
    let steps = machine_steps_after_host_moved(Some(json!({ "trait": "Machine", "from": "idle", "to": "idle", "event": "ADVANCE", "effects": [] })));
    assert!(!steps.iter().any(|s| s.starts_with("Machine:")), "{steps:?}");
}

#[test]
fn control_a_client_trait_that_stepped_locally_keeps_its_state() {
    let (state, host_paints) = mount_and_settle("client");
    assert!(state.contains("idle"), "{state}");
    assert!(!host_paints.iter().any(|p| p.contains("loading")), "{host_paints:?}");
}

// A page visited again: the optimistic trait sits where its last visit left it, and its lifecycle step moves it (a
// chart that goes back to loading to refetch). The leg carries the state before that step, so the host's cascade
// starts there and passes through the state the client holds now (Friday's Timeline, blank on every second visit).
fn revisit_schema() -> OrbitalSchema {
    from_json_value(&json!({
        "name": "plan",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Page",
            "entity": { "name": "Bar", "persistence": "persistent", "collection": "bars",
                "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [
                { "name": "Chart", "linkedEntity": "Bar", "dispatch": "optimistic",
                  "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }, { "name": "chart" }],
                    "transitions": [
                        { "from": "loading", "to": "loading", "event": "INIT", "effects": [["fetch", "Bar", { "emit": { "success": "LOADED" } }], ["render-ui", "main", { "type": "typography", "content": "skeleton" }]] },
                        { "from": "loading", "to": "chart", "event": "LOADED", "effects": [["render-ui", "main", { "type": "typography", "content": "bars" }]] },
                        { "from": "chart", "to": "loading", "event": "INIT", "effects": [["fetch", "Bar", { "emit": { "success": "LOADED" } }], ["render-ui", "main", { "type": "typography", "content": "skeleton" }]] }] } }
            ]
        }]
    }))
    .unwrap()
}

fn chart_answer(init_from: &str, loaded: bool) -> orbital_core_runtime::effects::response::EventResponse {
    let mut hops = vec![json!({ "trait": "Chart", "from": init_from, "to": "loading", "event": "INIT", "effects": [] })];
    let mut paints = vec![json!({ "traitName": "Chart", "event": "INIT", "fromState": init_from, "effect": ["render-ui", "main", { "type": "typography", "content": "skeleton" }] })];
    if loaded {
        hops.push(json!({ "trait": "Chart", "from": "loading", "to": "chart", "event": "LOADED", "effects": [] }));
        paints.push(json!({ "traitName": "Chart", "event": "LOADED", "fromState": "loading", "effect": ["render-ui", "main", { "type": "typography", "content": "bars" }] }));
    }
    serde_json::from_value(json!({
        "success": true, "emittedEvents": [],
        "states": { "Chart": if loaded { "chart" } else { "loading" } },
        "transitions": hops, "clientEffectsByTrait": paints
    })).unwrap()
}

/// The client mounts the page, folds the host's answer, mounts it again, then folds `second`. Answers the Chart's
/// state after the second mount, its state after the fold, and the Chart paints the fold handed the painter.
fn chart_revisited(second: orbital_core_runtime::effects::response::EventResponse) -> (String, String, Vec<String>) {
    let client = EmbeddedKernel::new(revisit_schema(), "tab").unwrap();
    client.dispatch_mount(&[seed("Chart")], None, Value::Null).unwrap();
    client.apply_server_response(&chart_answer("loading", true), &HashSet::new()).unwrap();
    assert!(client.get_state("Chart", None).contains("chart"), "the first visit ends on the chart: {}", client.get_state("Chart", None));
    client.dispatch_mount(&[seed("Chart")], None, Value::Null).unwrap();
    let mounted = client.get_state("Chart", None);
    let applied = client.apply_server_response(&second, &HashSet::new()).unwrap();
    let paints = applied.host_paints.iter().filter(|p| p.trait_name == "Chart").map(|p| serde_json::to_string(&p.effect).unwrap()).collect();
    (mounted, client.get_state("Chart", None), paints)
}

#[test]
fn a_second_visit_whose_lifecycle_step_moves_the_trait_still_takes_the_hosts_paint() {
    let (mounted, state, paints) = chart_revisited(chart_answer("chart", true));
    assert!(mounted.contains("loading"), "the second mount commits the lifecycle step here first: {mounted}");
    assert!(paints.last().is_some_and(|p| p.contains("bars")), "the host passed through the state the client holds, so its chart is the continuation: {paints:?}");
    assert!(state.contains("chart"), "{state}");
}

#[test]
fn control_a_first_visit_takes_the_hosts_paint() {
    let client = EmbeddedKernel::new(revisit_schema(), "tab").unwrap();
    client.dispatch_mount(&[seed("Chart")], None, Value::Null).unwrap();
    let applied = client.apply_server_response(&chart_answer("loading", true), &HashSet::new()).unwrap();
    assert!(applied.host_paints.iter().any(|p| p.trait_name == "Chart" && serde_json::to_string(&p.effect).unwrap().contains("bars")));
}

#[test]
fn control_a_second_visit_answer_that_never_reaches_the_clients_state_is_stale() {
    let (_, state, paints) = chart_revisited(chart_answer("chart", false));
    assert!(paints.is_empty(), "the host only replayed the step the client took itself: {paints:?}");
    assert!(state.contains("loading"), "{state}");
}

#[test]
fn edge_the_paint_of_a_step_before_the_clients_state_is_not_handed_to_the_painter() {
    let (_, _, paints) = chart_revisited(chart_answer("chart", true));
    assert!(!paints.iter().any(|p| p.contains("skeleton")), "the replayed lifecycle step would paint the skeleton over the client's own: {paints:?}");
}
