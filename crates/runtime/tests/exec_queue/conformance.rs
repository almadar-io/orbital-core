//! EXEC-QUEUE conformance: who hears an emit, and what a listener's guard reads.


use orbital_core_base::value::Value;
use serde_json::{json, Value as Json};
use crate::support::TestKernel;

fn emitter() -> Json {
    json!({ "name": "Play", "linkedEntity": "S",
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "UP", "effects": [["emit", "TURN", { "raw": "up" }]] }] } })
}

fn heard_by(traits: Vec<Json>, who: &str) -> bool {
    let kernel = TestKernel::server().build(json!({
        "name": "Queue",
        "orbitals": [{
            "name": "QueueOrbital",
            "entity": { "name": "S", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "dir", "type": "string" }] },
            "traits": traits
        }]
    }));
    let outcomes = kernel.dispatch("Play", None, "UP", Value::Null).unwrap();
    outcomes.iter().any(|(t, r)| t == who && r.executed)
}

fn listener(guard: Json) -> Json {
    json!({ "name": "Mechanic", "linkedEntity": "S",
        "listens": [{ "event": "Play.TURN", "triggers": "STEER", "guard": guard,
            "payloadMapping": { "mapped": "@payload.raw" } }],
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "STEER", "effects": [["set", "@entity.dir", "@payload.mapped"]] }] } })
}

#[test]
fn exec_queue_3_a_listen_guard_reads_the_raw_source_payload_before_the_with_mapping() {
    assert!(heard_by(vec![emitter(), listener(json!(["=", "@payload.raw", "up"]))], "Mechanic"));
}

#[test]
fn exec_queue_3_control_a_listen_guard_never_sees_the_mapped_field() {
    assert!(!heard_by(vec![emitter(), listener(json!(["=", "@payload.mapped", "up"]))], "Mechanic"));
}

fn bystander() -> Json {
    json!({ "name": "Bystander", "linkedEntity": "S",
        "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
            "transitions": [{ "from": "on", "to": "on", "event": "TURN", "effects": [["set", "@entity.dir", "overheard"]] }] } })
}

#[test]
fn exec_queue_7_a_transition_named_like_the_emit_is_not_a_listen() {
    assert!(!heard_by(vec![emitter(), bystander()], "Bystander"));
}

#[test]
fn exec_queue_7_control_a_declared_listens_edge_hears_the_emit() {
    assert!(heard_by(vec![emitter(), bystander(), listener(json!(true))], "Mechanic"));
}

fn name_only_listener_hears(source: Json) -> bool {
    use orbital_core_base::bus_event::BusEventSource;
    use orbital_core_base::events::types::SubscriptionId;
    use orbital_core_base::schema::types::ListenSource;
    use orbital_core_runtime::listener::ListenerInfo;
    let listener = ListenerInfo {
        event_pattern: "TURN".into(),
        trait_name: "Mechanic".into(),
        orbital: "Main".into(),
        triggers: "STEER".into(),
        source: Some(ListenSource::Trait { trait_name: "Play".into(), trait_id: None }),
        payload_mapping: None,
        guard: None,
        subscription_id: SubscriptionId(0),
    };
    let source: BusEventSource = serde_json::from_value(source).unwrap();
    listener.matches_source(&source)
}

#[test]
fn exec_queue_6_an_emit_with_no_stamped_orbital_matches_no_name_only_source() {
    assert!(!name_only_listener_hears(json!({ "trait": "Play" })));
}

#[test]
fn exec_queue_6_control_the_same_emit_stamped_with_the_listeners_orbital_matches() {
    assert!(name_only_listener_hears(json!({ "orbital": "Main", "trait": "Play" })));
}

fn diamond(first: Json, second: Json) -> Json {
    let relay = |name: &str, payload: Json| {
        json!({ "name": name, "linkedEntity": "S",
            "listens": [{ "event": "TURN", "triggers": "RELAY", "source": { "kind": "trait", "trait": "Play" } }],
            "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                "transitions": [{ "from": "on", "to": "on", "event": "RELAY", "effects": [["emit", "DONE", payload]] }] } })
    };
    json!({
        "name": "Queue",
        "orbitals": [{
            "name": "QueueOrbital",
            "entity": { "name": "S", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                emitter(),
                relay("Left", first),
                relay("Right", second),
                { "name": "Sink", "linkedEntity": "S",
                    "listens": [
                        { "event": "DONE", "triggers": "FIN", "source": { "kind": "trait", "trait": "Left" } },
                        { "event": "DONE", "triggers": "FIN", "source": { "kind": "trait", "trait": "Right" } }
                    ],
                    "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                        "transitions": [{ "from": "on", "to": "on", "event": "FIN" }] } }
            ]
        }]
    })
}

fn sink_runs_in_dispatch(first: Json, second: Json) -> usize {
    let kernel = TestKernel::server().build(diamond(first, second));
    let outcomes = kernel.dispatch("Play", None, "UP", Value::Null).unwrap();
    outcomes.iter().filter(|(t, r)| t == "Sink" && r.executed).count()
}

fn sink_runs_in_fold(first: Json, second: Json) -> usize {
    use orbital_core_base::bus_event::{BusEventSource, EmittedEvent};
    use orbital_core_runtime::effects::response::EventResponse;
    use std::collections::HashSet;
    let kernel = TestKernel::client().build(diamond(json!({}), json!({})));
    let done = |who: &str, payload: Json| EmittedEvent {
        event: "DONE".to_string(),
        payload: Some(serde_json::from_value::<Value>(payload).unwrap()),
        source: Some(BusEventSource {
            orbital: Some("QueueOrbital".to_string()),
            r#trait: Some(who.to_string()),
            ..Default::default()
        }),
    };
    let response = EventResponse {
        success: true,
        emitted_events: vec![done("Left", first), done("Right", second)],
        ..Default::default()
    };
    let outcomes = kernel.apply_server_response(&response, &HashSet::new()).unwrap();
    outcomes.iter().filter(|(t, r)| t == "Sink" && r.executed).count()
}

/// EXEC-QUEUE-1: one worklist per dispatch, shared by direct dispatches and by folding a server response.
#[test]
fn exec_queue_1_one_worklist_collapses_an_identical_delivery_reached_by_two_paths_in_a_dispatch_and_in_a_fold() {
    let same = || json!({ "who": "x" });
    assert_eq!(sink_runs_in_dispatch(same(), same()), 1, "dispatch: the second identical delivery is one worklist's repeat");
    assert_eq!(sink_runs_in_fold(same(), same()), 1, "fold: the response is folded through the same worklist");
    assert_eq!(sink_runs_in_dispatch(json!({ "who": "a" }), json!({ "who": "b" })), 2, "control (dispatch): distinct payloads are distinct deliveries");
    assert_eq!(sink_runs_in_fold(json!({ "who": "a" }), json!({ "who": "b" })), 2, "control (fold): distinct payloads are distinct deliveries");
}

fn job_kernel() -> orbital_core_runtime::kernel::RuntimeKernel {
    use orbital_core_base::schema::types::OrbitalSchema;
    use orbital_core_runtime::config::KernelConfig;
    use orbital_core_runtime::resumable::HostAnswered;
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "jobs",
        "orbitals": [{ "name": "Main",
            "entity": { "name": "Tally", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Job", "linkedEntity": "Tally",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "working" }, { "name": "aborted" }],
                    "transitions": [
                        { "from": "idle", "to": "working", "event": "GO", "effects": [
                            ["call-service", "gate", "wait", {}, { "key": "k1", "emit": { "success": "DONE" } }]
                        ] },
                        { "from": "working", "to": "aborted", "event": "ABORT" }
                    ]
                } }] }]
    }))
    .unwrap();
    let config = KernelConfig {
        service_handler: Some(std::sync::Arc::new(HostAnswered) as std::sync::Arc<dyn orbital_core_runtime::effects::ServiceHandler>),
        ..Default::default()
    };
    TestKernel::server().config(config).register(schema)
}

fn state_of(kernel: &orbital_core_runtime::kernel::RuntimeKernel) -> String {
    kernel.get_state("Job", None).expect("Job has a held state").current_state
}

type Parked = (
    orbital_core_runtime::resumable::DispatchContinuation,
    Vec<orbital_core_runtime::resumable::PendingRequest>,
);

/// Start GO and answer every ask until the keyed call is outstanding; the lane is free while it is parked (EXEC-CONC-0).
fn parked_on_the_keyed_call(kernel: &orbital_core_runtime::kernel::RuntimeKernel) -> Parked {
    use orbital_core_runtime::resumable::{BlockingHost, IoReply, IoRequest, Progress};
    let store = orbital_core_runtime::entity_source::InMemoryEntityCache::new();
    let mut progress = kernel.start_dispatch("Job", None, "GO", Value::Null, None, None).unwrap();
    loop {
        match progress {
            Progress::Done(_) => panic!("GO finished before its keyed ask"),
            Progress::Await { continuation, requests } => {
                if requests.iter().any(|p| matches!(p.request, IoRequest::CallService { .. })) {
                    return (continuation, requests);
                }
                let replies = requests
                    .iter()
                    .map(|p| IoReply {
                        id: p.id,
                        answer: BlockingHost { source: &store, service: None, substrate: None }.answer(&p.request),
                    })
                    .collect();
                progress = kernel.resume(continuation, replies).unwrap();
            }
        }
    }
}

fn answer_deferred(kernel: &orbital_core_runtime::kernel::RuntimeKernel, parked: Parked) {
    use orbital_core_runtime::resumable::{BlockingHost, IoAnswer, IoReply, IoRequest, Progress};
    let store = orbital_core_runtime::entity_source::InMemoryEntityCache::new();
    let (mut continuation, mut requests) = parked;
    loop {
        let replies = requests
            .iter()
            .map(|p| IoReply {
                id: p.id,
                answer: match &p.request {
                    IoRequest::CallService { .. } => IoAnswer::ServiceDeferred,
                    other => BlockingHost { source: &store, service: None, substrate: None }.answer(other),
                },
            })
            .collect();
        match kernel.resume(continuation, replies).unwrap() {
            Progress::Done(_) => return,
            Progress::Await { continuation: c, requests: r } => (continuation, requests) = (c, r),
        }
    }
}

/// EXEC-QUEUE-11: a step commits its transition before its effects' I/O answers; a state another dispatch moved stands after the replay.
#[test]
fn exec_queue_11_a_parked_step_holds_its_entered_state_and_a_state_moved_meanwhile_stands_after_the_replay() {
    let kernel = job_kernel();
    let parked = parked_on_the_keyed_call(&kernel);
    assert_eq!(state_of(&kernel), "working", "the transition committed before the keyed call answered");
    let overlapping = kernel.dispatch("Job", None, "ABORT", Value::Null).unwrap();
    assert!(
        overlapping.iter().any(|(t, r)| t == "Job" && r.executed),
        "an overlapping dispatch saw the entered state, so ABORT (from working) fired"
    );
    answer_deferred(&kernel, parked);
    assert_eq!(state_of(&kernel), "aborted", "the moved state stands after the replay, not reset to the waiting step's state");

    let control = job_kernel();
    let parked = parked_on_the_keyed_call(&control);
    answer_deferred(&control, parked);
    assert_eq!(state_of(&control), "working", "control: with no overlapping dispatch the step's own entered state stands");
}

/// EXEC-EVENT-PAYLOAD-1: a server kernel refuses an external event whose payload breaks the declared event before any transition.
#[test]
fn exec_event_payload_1_a_server_kernel_refuses_a_payload_that_breaks_the_declared_event() {
    use orbital_core_runtime::kernel::KernelError;
    let program = json!({
        "name": "Chat",
        "orbitals": [{
            "name": "ChatOrbital",
            "entity": { "name": "Message", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "Composer", "linkedEntity": "Message",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "sent" }],
                    "events": [{ "key": "SEND", "name": "Send", "payloadSchema": [
                        { "name": "value", "type": "string", "required": true },
                        { "name": "particles", "type": "[object]" }
                    ] }],
                    "transitions": [{ "from": "idle", "to": "sent", "event": "SEND" }]
                } }]
        }]
    });
    let send = |kernel: &orbital_core_runtime::kernel::RuntimeKernel, payload: Json| {
        kernel.dispatch("Composer", None, "SEND", serde_json::from_value::<Value>(payload).unwrap())
    };
    let server = TestKernel::server().build(program.clone());
    let refused = |r: &Result<_, KernelError>, needle: &str| {
        matches!(r, Err(KernelError::PayloadInvalid(m)) if m.starts_with("Payload validation failed for event 'SEND': ") && m.contains(needle))
    };
    assert!(refused(&send(&server, json!({})), "value (missing, expected string)"), "server: a missing required field is refused");
    assert!(refused(&send(&server, json!({ "value": null })), "value (null, expected string)"), "server: a null required field is refused");
    assert!(
        refused(&send(&server, json!({ "value": "hi", "particles": { "id": "p" } })), "particles (wrong-type, expected [object])"),
        "server: a value that is not the declared container is refused"
    );
    assert!(server.get_state("Composer", None).is_none_or(|s| s.current_state == "idle"), "no transition ran for a refused payload");
    assert!(send(&server, json!({ "value": "hi", "particles": [{ "id": "p" }] })).is_ok(), "control: a conforming payload dispatches");
    let client = TestKernel::client().build(program);
    assert!(send(&client, json!({})).is_ok(), "control: a client kernel's events are not checked");
}
