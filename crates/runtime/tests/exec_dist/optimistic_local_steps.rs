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

// A pushed dispatch (a deferred completion the host ran on its own) whose optimistic listener sits on a route this tab
// has not mounted: the host ran the listener and says so; the tab must not run it again (The Orb on a served host,
// G-CROSS-768: the generator's catalog command was started twice).
fn runner_schema() -> OrbitalSchema {
    from_json_value(&json!({
        "name": "runs",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Work",
            "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "runs", "type": "number", "default": 0 }] },
            "traits": [
                { "name": "Planner", "linkedEntity": "Job", "dispatch": "optimistic",
                  "emits": [{ "event": "PLAN_RUN", "scope": "external" }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "searching" }],
                    "transitions": [
                        { "from": "idle", "to": "searching", "event": "PLAN", "effects": [] },
                        { "from": "searching", "to": "searching", "event": "FOUND", "effects": [["emit", "PLAN_RUN", {}]] }] } },
                { "name": "Runner", "linkedEntity": "Run", "dispatch": "optimistic",
                  "listens": [{ "event": "PLAN_RUN", "triggers": "RUN", "scope": "external", "source": { "kind": "trait", "trait": "Planner" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "running" }],
                    "transitions": [
                        { "from": "idle", "to": "running", "event": "RUN", "effects": [["set", "@entity.started", 1]] }] } }
            ]
        }, {
            "name": "Runs",
            "entity": { "name": "Run", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "started", "type": "number", "default": 0 }] },
            "traits": []
        }]
    }))
    .unwrap()
}

fn runner_steps_after_a_push(runner_hop: bool, mounted: bool) -> Vec<String> {
    let client = EmbeddedKernel::new(runner_schema(), "tab").unwrap();
    client.set_host_fans_out(true);
    if mounted {
        client.dispatch_mount(&[seed("Planner"), seed("Runner")], None, Value::Null).unwrap();
    }
    let mut hops = vec![json!({ "trait": "Planner", "from": "searching", "to": "searching", "event": "FOUND", "effects": [] })];
    if runner_hop {
        hops.push(json!({ "trait": "Runner", "from": "idle", "to": "running", "event": "RUN", "effects": [] }));
    }
    let response: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "states": { "Planner": "searching", "Runner": if runner_hop { "running" } else { "idle" } }, "transitions": hops,
        "emittedEvents": [{ "event": "PLAN_RUN", "payload": {}, "source": { "orbital": "Work", "trait": "Planner", "dispatched": true } }]
    })).unwrap();
    let applied = client.apply_server_response(&response, &HashSet::new()).unwrap();
    applied.applied.iter().map(|(t, s)| format!("{t}:{s}")).collect()
}

#[test]
fn a_pushed_listener_on_an_unmounted_route_the_host_ran_is_not_run_again() {
    let steps = runner_steps_after_a_push(true, false);
    assert!(!steps.iter().any(|s| s.starts_with("Runner:")), "the tab ran the host's step a second time: {steps:?}");
}

#[test]
fn control_a_pushed_listener_on_a_mounted_route_the_host_ran_is_not_run_again() {
    let steps = runner_steps_after_a_push(true, true);
    assert!(!steps.iter().any(|s| s.starts_with("Runner:")), "{steps:?}");
}

#[test]
fn control_a_pushed_listener_the_host_did_not_run_runs_on_the_tab() {
    let steps = runner_steps_after_a_push(false, true);
    assert!(steps.iter().any(|s| s.starts_with("Runner:")), "{steps:?}");
}

// A host that lists no steps at all (an emit folded on its own) gives no evidence against its `dispatched` stamp.
fn runner_steps_after_a_stamped_emit(dispatched: bool) -> Vec<String> {
    let client = EmbeddedKernel::new(runner_schema(), "tab").unwrap();
    client.set_host_fans_out(true);
    let mut source = json!({ "orbital": "Work", "trait": "Planner" });
    if dispatched {
        source["dispatched"] = json!(true);
    }
    let response: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "states": {}, "emittedEvents": [{ "event": "PLAN_RUN", "payload": {}, "source": source }]
    })).unwrap();
    let applied = client.apply_server_response(&response, &HashSet::new()).unwrap();
    applied.applied.iter().map(|(t, s)| format!("{t}:{s}")).collect()
}

#[test]
fn a_delivered_emit_folded_with_no_step_list_does_not_run_its_optimistic_listener_again() {
    let steps = runner_steps_after_a_stamped_emit(true);
    assert!(!steps.iter().any(|s| s.starts_with("Runner:")), "the tab ran a listener the host said it delivered: {steps:?}");
}

#[test]
fn control_an_undelivered_emit_folded_with_no_step_list_runs_its_optimistic_listener() {
    let steps = runner_steps_after_a_stamped_emit(false);
    assert!(steps.iter().any(|s| s.starts_with("Runner:")), "{steps:?}");
}

// A snapshot answered to a frames-since request (the tab was pushed a delta past its version, G-ORBITAL-SHELL-RUST-052)
// is the session's record as the host holds it and lists no steps: it replaces an optimistic trait's frame too, or the
// rows of the steps the tab never saw never arrive (The Orb's document tool lost its request key, G-CROSS-767).
fn machine_paints_after_frames(frames: serde_json::Value) -> Vec<String> {
    let client = EmbeddedKernel::new(schema("optimistic"), "tab").unwrap();
    client.dispatch_mount(&[seed("List"), seed("Machine")], None, Value::Null).unwrap();
    let response: orbital_core_runtime::effects::response::EventResponse =
        serde_json::from_value(json!({ "success": true, "emittedEvents": [], "states": {}, "frames": frames })).unwrap();
    let applied = client.apply_server_response(&response, &HashSet::new()).unwrap();
    applied.response.client_effects_by_trait.iter().flatten().filter(|p| p.trait_name == "Machine").map(|p| serde_json::to_string(&p.effect).unwrap()).collect()
}

#[test]
fn a_resync_snapshot_replaces_an_optimistic_traits_frame() {
    let paints = machine_paints_after_frames(json!({ "session": "tab-session", "from": 0, "to": 6, "ops": [["clear"], ["set", { "Machine": { "id": "singleton", "label": "from the host" } }]] }));
    assert!(paints.iter().any(|p| p.contains("from the host")), "the snapshot's row never reached the optimistic trait: {paints:?}");
}

#[test]
fn control_a_delta_that_lists_no_steps_does_not_overwrite_an_optimistic_traits_frame() {
    let paints = machine_paints_after_frames(json!({ "session": "tab-session", "from": 0, "to": 1, "ops": [["set", { "Machine": { "id": "singleton", "label": "from the host" } }]] }));
    assert!(paints.is_empty(), "{paints:?}");
}

// The Orb's document tool on a served host (G-CROSS-767): the host ran the tool's steps in two dispatches, the tab was
// pushed the second first (a gap), resynced, and then hears the program-wide reader's answer as a live message. The
// tab's own copy of the tool must be at the host's state with the host's row for its guard to pass.
/// `loop_in_client`: the trait that hears the tool's result is in the client's copy of the program (a server-awaited
/// page trait); otherwise only the host has it.
fn tool_schema(loop_in_client: bool) -> OrbitalSchema {
    let mut program = json!({
        "name": "tools",
        "_metadata": { "serverListens": [{ "orbital": "Work", "listen": { "event": "TOOL_DONE", "triggers": "DONE", "source": { "kind": "any" } } }] },
        "typeContractVersion": 1, "orbitals": [{
            "name": "Desk",
            "entity": { "name": "Call", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "reqKey", "type": "string", "default": "" }, { "name": "path", "type": "string", "default": "" }] },
            "traits": [
                { "name": "Tool", "linkedEntity": "Call", "dispatch": "optimistic",
                  "emits": [{ "event": "READ_ASKED", "scope": "external" }, { "event": "TOOL_DONE", "scope": "external" }],
                  "listens": [{ "event": "TEXT", "triggers": "TEXT", "scope": "external", "source": { "kind": "any" } }],
                  "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }, { "name": "waiting" }],
                    "transitions": [
                        { "from": "ready", "to": "ready", "event": "INIT", "effects": [] },
                        { "from": "ready", "to": "ready", "event": "PENDING", "effects": [["set", "@entity.reqKey", "@payload.key"]] },
                        { "from": "ready", "to": "waiting", "event": "ADD", "effects": [["set", "@entity.path", "@payload.path"], ["emit", "READ_ASKED", { "key": "@entity.reqKey" }]] },
                        { "from": "waiting", "to": "ready", "event": "TEXT", "guard": ["=", "@payload.key", "@entity.reqKey"], "effects": [["emit", "TOOL_DONE", {}]] }] } }
            ],
            "pages": [{ "name": "DeskPage", "path": "/", "traits": [{ "ref": "Tool" }] }]
        }]
    });
    if loop_in_client {
        program.as_object_mut().unwrap().remove("_metadata");
        program["orbitals"][0]["traits"].as_array_mut().unwrap().push(json!({
            "name": "Loop", "linkedEntity": "Call", "dispatch": "server",
            "listens": [{ "event": "TOOL_DONE", "triggers": "DONE", "scope": "external", "source": { "kind": "trait", "trait": "Tool" } }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "DONE", "effects": [] }] }
        }));
        program["orbitals"][0]["pages"][0]["traits"].as_array_mut().unwrap().push(json!({ "ref": "Loop" }));
    }
    from_json_value(&program).unwrap()
}

fn tool_steps_after_a_gap(resync: bool) -> Vec<String> {
    tool_steps_after_a_gap_in(resync, false)
}

fn tool_steps_after_a_gap_in(resync: bool, loop_in_client: bool) -> Vec<String> {
    let client = EmbeddedKernel::new(tool_schema(loop_in_client), "tab").unwrap();
    client.set_host_fans_out(true);
    client.dispatch_mount(&[seed("Tool")], None, Value::Null).unwrap();
    let ahead: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "emittedEvents": [], "states": { "Tool": "waiting" },
        "transitions": [{ "trait": "Tool", "from": "ready", "to": "waiting", "event": "ADD", "effects": [] }],
        "frames": { "session": "tab-session", "from": 5, "to": 6, "ops": [["set", { "Tool": { "path": "terms.txt" } }]] }
    })).unwrap();
    let folded = client.apply_server_response(&ahead, &HashSet::new()).unwrap();
    assert!(folded.resync.is_some(), "a delta past the held version asks for a resync");
    if resync {
        let snapshot: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
            "success": true, "emittedEvents": [], "states": {},
            "frames": { "session": "tab-session", "from": 0, "to": 6, "ops": [["clear"], ["put", "Tool", { "reqKey": "agent:c1", "path": "terms.txt" }]] }
        })).unwrap();
        client.apply_server_response(&snapshot, &HashSet::new()).unwrap();
    }
    let flight = client.open_flight();
    let heard = client.deliver_emit_to_listeners(flight, serde_json::from_value(json!({
        "event": "TEXT", "payload": { "key": "agent:c1" }, "source": { "orbital": "Reader", "trait": "Reader", "dispatched": true }
    })).unwrap()).unwrap();
    client.end_flight(flight);
    match heard {
        orbital_core_runtime::embed::FoldProgress::Applied(fold) => fold
            .applied
            .iter()
            .map(|(t, s)| format!("{t}:{s}"))
            .chain(fold.server_legs.iter().map(|l| format!("leg {}.{}", l.leg.target_trait.clone().unwrap_or_default(), l.leg.event)))
            .collect(),
        orbital_core_runtime::embed::FoldProgress::Queued { .. } => vec!["queued".to_string()],
    }
}

#[test]
fn after_a_gap_and_its_resync_the_tabs_tool_hears_the_live_answer() {
    let steps = tool_steps_after_a_gap(true);
    assert!(steps.iter().any(|s| s == "Tool:ready"), "the tool never heard the reader's answer: {steps:?}");
}

#[test]
fn control_without_the_resync_the_tools_guard_fails_on_its_empty_key() {
    let steps = tool_steps_after_a_gap(false);
    assert!(!steps.iter().any(|s| s == "Tool:ready"), "{steps:?}");
}

// The host never ran that listener (the emit was made in the program session, the tool is the viewer's): the step the
// tab ran on the live message is the only run of it, so the server work it reaches is the tab's to post.
#[test]
fn a_live_messages_optimistic_listener_posts_the_server_work_its_step_reaches() {
    let steps = tool_steps_after_a_gap(true);
    assert!(steps.iter().any(|s| s == "leg Tool.TEXT"), "the tool's result never left the tab: {steps:?}");
}

#[test]
fn control_a_listener_whose_guard_refused_posts_nothing() {
    let steps = tool_steps_after_a_gap(false);
    assert!(!steps.iter().any(|s| s.starts_with("leg ")), "{steps:?}");
}

// The host keeps its own copy of an optimistic trait, so the step the tab ran on a live message is posted even when
// everything it emits is heard in the tab: otherwise the host's copy never leaves the state it waited in (The Orb's
// document tool refused the next lookup of the same turn).
#[test]
fn a_live_messages_optimistic_listener_posts_its_own_step_so_the_hosts_copy_follows() {
    let steps = tool_steps_after_a_gap_in(true, true);
    assert!(steps.iter().any(|s| s == "leg Tool.TEXT"), "the host's copy of the tool never heard the answer: {steps:?}");
    assert!(!steps.iter().any(|s| s == "leg Loop.DONE"), "the host's cascade from the tool's step reaches the loop: {steps:?}");
}

// The reader's answer can come back inside the answer to the tab's own leg, when the host had it at once. The host
// lists a step of the tool there (the leg's), which is not the step the answer triggers: the tab still runs that one,
// and posts it.
fn tool_steps_after_an_answer_carrying_the_emit(listed_event: &str) -> Vec<String> {
    let client = EmbeddedKernel::new(tool_schema(true), "tab").unwrap();
    client.set_host_fans_out(true);
    client.dispatch_mount(&[seed("Tool")], None, Value::Null).unwrap();
    let pending: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "emittedEvents": [], "states": { "Tool": "waiting" },
        "transitions": [{ "trait": "Tool", "from": "ready", "to": "waiting", "event": "ADD", "effects": [] }],
        "frames": { "session": "tab-session", "from": 0, "to": 1, "ops": [["set", { "Tool": { "reqKey": "agent:c1", "path": "terms.txt" } }]] }
    })).unwrap();
    client.apply_server_response(&pending, &HashSet::new()).unwrap();
    let answer: orbital_core_runtime::effects::response::EventResponse = serde_json::from_value(json!({
        "success": true, "states": {},
        "transitions": [{ "trait": "Tool", "from": "waiting", "to": "waiting", "event": listed_event, "effects": [] }],
        "emittedEvents": [{ "event": "TEXT", "payload": { "key": "agent:c1" }, "source": { "orbital": "Reader", "trait": "Reader", "dispatched": true } }]
    })).unwrap();
    let fold = client.apply_server_response(&answer, &HashSet::new()).unwrap();
    fold.applied
        .iter()
        .map(|(t, s)| format!("{t}:{s}"))
        .chain(fold.server_legs.iter().map(|l| format!("leg {}.{}", l.leg.target_trait.clone().unwrap_or_default(), l.leg.event)))
        .collect()
}

#[test]
fn an_emit_in_a_leg_answer_runs_the_listener_step_the_host_did_not_list_and_posts_it() {
    let steps = tool_steps_after_an_answer_carrying_the_emit("NOTED");
    assert!(steps.iter().any(|s| s == "Tool:ready"), "the tool never heard the reader's answer: {steps:?}");
    assert!(steps.iter().any(|s| s == "leg Tool.TEXT"), "the host's copy of the tool never heard the answer: {steps:?}");
}

#[test]
fn control_an_emit_in_a_leg_answer_whose_listener_step_the_host_listed_is_not_run_again() {
    let steps = tool_steps_after_an_answer_carrying_the_emit("TEXT");
    assert!(!steps.iter().any(|s| s == "Tool:ready" || s.starts_with("leg ")), "{steps:?}");
}
