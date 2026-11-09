// EXEC-TICK-2: a tick's emits enter the normal cascade, the emitter's own arm included (G-CROSS-539).

use orbital_core_base::bus_event::tick_event_key;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::effects::response::{EventRequest, EventResponse};
use orbital_core_runtime::effects::RunTick;
use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
use serde_json::{json, Value as Json};

fn schema(guard: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "Boot",
            "entity": { "name": "Flow", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" },
                { "name": "dueAt", "type": "string", "nullable": true },
                { "name": "started", "type": "boolean", "default": false }
            ] },
            "traits": [{
                "name": "Starter", "linkedEntity": "Flow",
                "stateMachine": {
                    "states": [{ "name": "ready", "isInitial": true }],
                    "events": [{ "key": "INIT", "name": "INIT" }, { "key": "DUE", "name": "DUE" }],
                    "transitions": [
                        { "from": "ready", "to": "ready", "event": "INIT", "effects": [["set", "@entity.dueAt", ["time/add", "@now", -1000, "ms"]]] },
                        { "from": "ready", "to": "ready", "event": "DUE", "effects": [
                            ["set", "@entity.dueAt", null],
                            ["set", "@entity.started", true],
                            ["emit", "STARTED", {}]
                        ] }
                    ]
                },
                "ticks": [{ "name": "beat", "interval": 10, "runsInBackground": true, "guard": guard, "effects": [["emit", "DUE", {}]] }],
                "emits": [{ "event": "DUE" }, { "event": "STARTED" }]
            }]
        }]
    }))
    .unwrap()
}

fn run(server: &EmbeddedServer, request: EventRequest) -> EventResponse {
    match server.start("Boot", request).unwrap() {
        ServerProgress::Done(response) => response,
        ServerProgress::Await { requests, .. } => panic!("the fixture asks the host for {requests:?}"),
        ServerProgress::Queued { ticket } => panic!("a lone request is never queued (ticket {ticket})"),
    }
}

fn init() -> EventRequest {
    EventRequest { event: "INIT".to_string(), target_trait: Some("Starter".to_string()), ..Default::default() }
}

fn tick() -> EventRequest {
    EventRequest {
        event: tick_event_key("beat"),
        run_tick: Some(RunTick { trait_name: "Starter".to_string(), tick: "beat".to_string() }),
        target_trait: Some("Starter".to_string()),
        ..Default::default()
    }
}

fn emitted(response: &EventResponse) -> Vec<&str> {
    response.emitted_events.iter().map(|e| e.event.as_str()).collect()
}


#[test]
fn a_tick_emit_reaches_the_emitter_s_own_arm() {
    let server = EmbeddedServer::new(schema(json!(["!=", "@entity.dueAt", null]))).unwrap();
    run(&server, init());
    let response = run(&server, tick());
    assert_eq!(emitted(&response), ["DUE", "STARTED"]);
    assert!(emitted(&run(&server, tick())).is_empty(), "the own arm cleared dueAt, so the next tick's guard fails");
}

#[test]
fn a_tick_guarded_on_a_due_instant_reaches_its_own_arm() {
    let guard = json!(["and", ["!=", "@entity.dueAt", null], ["not", ["time/isBefore", "@now", ["default", "@entity.dueAt", "@now"]]]]);
    let server = EmbeddedServer::new(schema(guard)).unwrap();
    run(&server, init());
    let response = run(&server, tick());
    assert_eq!(emitted(&response), ["DUE", "STARTED"]);
    assert!(emitted(&run(&server, tick())).is_empty(), "the own arm cleared dueAt, so the next tick's guard fails");
}

#[test]
fn control_a_tick_whose_guard_fails_emits_nothing() {
    let server = EmbeddedServer::new(schema(json!(["!=", "@entity.dueAt", null]))).unwrap();
    let response = run(&server, tick());
    assert!(emitted(&response).is_empty());
}
