//! Cross-trait listener fan-out (RS-G20): a user event dispatched to one trait must
//! reach every trait that *listens* for it. Reproduces the "New Item does nothing" bug:
//! a `CREATE` from a source trait (the button's owner) has to open a *different* trait's
//! modal via its `listens` entry — not just process the source trait in isolation.

use orbital_core_base::client_effect::ClientEffect;
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use std::sync::Arc;

fn two_trait_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Catalog",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                    }
                },
                {
                    "name": "CreateModal",
                    "linkedEntity": "Item",
                    "listens": [{
                        "event": "CREATE",
                        "triggers": "CREATE",
                        "source": { "kind": "trait", "trait": "Catalog" }
                    }],
                    "stateMachine": {
                        "states": [
                            { "name": "closed", "isInitial": true },
                            { "name": "open" }
                        ],
                        "transitions": [
                            { "from": "closed", "to": "closed", "event": "INIT" },
                            { "from": "closed", "to": "open", "event": "CREATE",
                              "effects": [["render-ui", "modal", { "type": "form" }]] },
                            { "from": "open", "to": "closed", "event": "CLOSE",
                              "effects": [["render-ui", "modal", null]] }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn kernel_with(schema: OrbitalSchema) -> RuntimeKernel {
    crate::support::TestKernel::server().register(schema)
}

#[test]
fn create_from_source_opens_listening_modal() {
    let kernel = kernel_with(two_trait_schema());

    // The button is owned by `Catalog`, so its CREATE dispatches with source=Catalog.
    let outcomes = kernel
        .dispatch("Catalog", None, "CREATE", Value::Object(Default::default()))
        .unwrap();

    // The modal trait fired via its listener even though CREATE was sent to Catalog.
    let modal = outcomes
        .iter()
        .find(|(t, _)| t == "CreateModal")
        .expect("CreateModal listener should fire for CREATE from Catalog");
    assert!(modal.1.executed, "modal CREATE transition should execute");
    assert_eq!(modal.1.new_state, "open");

    // The merged response carries the modal's render-ui.
    let resp = EventResponse::from_process_results(&outcomes, "Catalog", "CREATE");
    assert!(
        resp.client_effects.iter().flatten().any(|e| matches!(
            e,
            ClientEffect::RenderUi { slot, .. } if slot == "modal"
        )),
        "merged response must contain a render-ui into the modal slot, got {:?}",
        resp.client_effects
    );
}

#[test]
fn create_from_wrong_source_does_not_open_modal() {
    // A source the listener does NOT subscribe to must not trigger the modal — proves
    // the fan-out is source-matched, not event-name broadcast (Inventory vs StockAdjustment
    // both emit CREATE; they must not cross-fire).
    let kernel = kernel_with(two_trait_schema());

    let outcomes = kernel
        .dispatch("CreateModal", None, "INIT", Value::Null) // unrelated dispatch
        .unwrap();
    assert!(
        !outcomes
            .iter()
            .any(|(t, r)| t == "CreateModal" && r.new_state == "open"),
        "INIT must not open the modal"
    );

    // Dispatching CREATE from a trait the listener doesn't name as source → no modal.
    let kernel2 = kernel_with(two_trait_schema());
    let outcomes2 = kernel2
        .dispatch(
            "CreateModal",
            None,
            "CREATE",
            Value::Object(Default::default()),
        )
        .unwrap();
    // CreateModal itself DOES have a CREATE transition, so dispatching CREATE *to it*
    // opens it directly — but no OTHER trait should be dragged in.
    assert_eq!(
        outcomes2.len(),
        1,
        "only the directly-targeted trait should fire"
    );
}

/// Wave-8 (`C-TRAITREF-WASM-UNRESOLVED`): the wasm mount inlines its schema so `@trait.<Peer>`
/// composition resolves. A primary trait composes `@trait.Peer` in its `main` render-ui; the
/// peer's own INIT emits a `main` render-ui. Dispatching INIT to the peer must produce that
/// `main` render-ui — the effect the client indexes under `trait_content[Peer]`, which is what
/// makes the `@trait.Peer` leaf in the primary's children resolve.
fn composed_peer_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Layout",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                            "effects": [["render-ui", "main",
                                { "type": "stack", "children": ["@trait.Peer"] }]] }]
                    }
                },
                {
                    "name": "Peer",
                    "stateMachine": {
                        "states": [{ "name": "ready", "isInitial": true }],
                        "transitions": [{ "from": "ready", "to": "ready", "event": "INIT",
                            "effects": [["render-ui", "main", { "type": "button" }]] }]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn peer_init_emits_main_render_ui_for_trait_content() {
    let kernel = kernel_with(composed_peer_schema());

    let outcomes = kernel.dispatch("Peer", None, "INIT", Value::Null).unwrap();
    let resp = EventResponse::from_process_results(&outcomes, "Peer", "INIT");

    assert!(
        resp.client_effects.iter().flatten().any(|e| matches!(
            e,
            ClientEffect::RenderUi { slot, config: Some(content), .. }
                if slot == "main"
                && content.as_object().and_then(|o| o.get("type")).and_then(|v| v.as_str())
                    == Some("button")
        )),
        "peer INIT must emit its `main` render-ui (indexed under trait_content[Peer]), got {:?}",
        resp.client_effects
    );
}

/// A `listens { Source.EVENT -> LocalEvent }` bare route (no `with {}` mapping) must
/// forward the source event's payload to the listener UNCHANGED — parity with
/// `applyListenPayloadMapping` (`@almadar/core`): "No mapping ... -> the payload passes
/// through unchanged." A `with {}` route remaps instead (§104 ledger).
fn listens_cascade_schema(with_mapping: bool) -> OrbitalSchema {
    let target_trait = serde_json::json!({
        "name": "Target",
        "linkedEntity": "Item",
        "listens": [{
            "event": "SAVE",
            "triggers": "DO_SAVE",
            "source": { "kind": "trait", "trait": "Source" },
            "payloadMapping": if with_mapping {
                serde_json::json!({ "id": "@payload.id" })
            } else {
                serde_json::Value::Null
            }
        }],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "DO_SAVE",
                "effects": [["set", "@entity.id", "@payload.id"]] }]
        }
    });
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Source",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "SAVE" }]
                    }
                },
                target_trait
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn bare_listen_route_forwards_source_payload_unchanged() {
    let kernel = kernel_with(listens_cascade_schema(false));
    let outcomes = kernel
        .dispatch(
            "Source",
            Some("row-1"),
            "SAVE",
            serde_json::from_value(serde_json::json!({"id": "abc123", "row": {"id": "abc123"}}))
                .unwrap(),
        )
        .unwrap();
    let target = outcomes.iter().find(|(t, _)| t == "Target").expect("Target should fire");
    assert_eq!(
        target.1.entity_data.get("id").and_then(|v| v.as_str()),
        Some("abc123"),
        "bare route must forward the source payload's `id` field unchanged"
    );
}

#[test]
fn with_listen_route_remaps_payload() {
    let kernel = kernel_with(listens_cascade_schema(true));
    let outcomes = kernel
        .dispatch(
            "Source",
            Some("row-1"),
            "SAVE",
            serde_json::from_value(serde_json::json!({"id": "abc123", "row": {"id": "abc123"}}))
                .unwrap(),
        )
        .unwrap();
    let target = outcomes.iter().find(|(t, _)| t == "Target").expect("Target should fire");
    assert_eq!(
        target.1.entity_data.get("id").and_then(|v| v.as_str()),
        Some("abc123"),
        "a with-mapping route must still resolve the mapped field"
    );
}

// One dispatch lane per kernel (G-CROSS-291, EXEC-CONC-1): a dispatch holds the kernel from start to commit, so a
// second dispatch or a completion arriving while it runs waits for it. `ADD` commits its write and then parks on its
// `Echo` listener's host ask, so an overlap lands between a transition and its listener cascade (between steps, where
// the per-step I/O gate is open and only the lane keeps a second dispatch out).

/// The `gate` service the deferred completion's own dispatch calls: it answers at once.
struct Gate;

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl orbital_core_runtime::effects::ServiceHandler for Gate {
    fn call(
        &self,
        _s: &str,
        _m: &str,
        _args: &Value,
        _k: Option<&str>,
        _c: Option<&orbital_core_runtime::effects::ServiceCaller>,
    ) -> Result<Value, String> {
        Ok(Value::Null)
    }

    fn cancel(&self, _key: &str) -> Result<(), String> {
        Ok(())
    }
}

fn lane_kernel() -> (Arc<RuntimeKernel>, Arc<dyn EntitySource>) {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "lane",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Tally", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "n", "type": "number", "default": 0 }] },
            "traits": [{
                "name": "Counter", "linkedEntity": "Tally",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "ADD", "effects": [
                            ["set", "@entity.n", ["+", "@entity.n", 1]],
                            ["emit", "ADDED", { "n": "@entity.n" }],
                            ["emit", "STEP", { "who": "@payload.who" }]
                        ] },
                        { "from": "idle", "to": "idle", "event": "DONE", "effects": [
                            ["call-service", "gate", "wait", { "who": "completion" }],
                            ["emit", "ADDED", { "n": "@entity.n" }]
                        ] },
                        { "from": "idle", "to": "idle", "event": "READ", "effects": [["emit", "ADDED", { "n": "@entity.n" }]] },
                        { "from": "idle", "to": "idle", "event": "ASK", "effects": [
                            ["call-service", "gate", "wait", {}],
                            ["set", "@entity.n", ["+", "@entity.n", 1]],
                            ["emit", "ADDED", { "n": "@entity.n" }]
                        ] },
                        { "from": "idle", "to": "idle", "event": "ASK_KEYED", "effects": [
                            ["call-service", "gate", "wait", {}, { "key": "k1", "emit": { "success": "DONE" } }],
                            ["set", "@entity.n", ["+", "@entity.n", 1]],
                            ["emit", "ADDED", { "n": "@entity.n" }]
                        ] },
                        { "from": "idle", "to": "idle", "event": "BUMP", "effects": [
                            ["set", "@entity.n", ["+", "@entity.n", 1]],
                            ["emit", "ADDED", { "n": "@entity.n" }]
                        ] }
                    ]
                }
            }, {
                "name": "Echo",
                "listens": [{ "event": "STEP", "triggers": "STEP", "source": { "kind": "trait", "trait": "Counter" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "STEP", "effects": [
                        ["call-service", "gate", "wait", { "who": "@payload.who" }]
                    ] }]
                }
            }]
        }]
    }))
    .unwrap();
    let config = orbital_core_runtime::config::KernelConfig {
        service_handler: Some(Arc::new(orbital_core_runtime::resumable::HostAnswered) as Arc<dyn orbital_core_runtime::effects::ServiceHandler>),
        ..Default::default()
    };
    let store = Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>;
    let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), config);
    kernel.register_schema(schema).unwrap();
    (Arc::new(kernel), store)
}

fn added(outcomes: &orbital_core_runtime::resumable::DispatchOutcomes) -> Vec<serde_json::Value> {
    outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            orbital_core_base::effect_types::Effect::Emit { event, payload } if event == "ADDED" => {
                Some(serde_json::to_value(payload).unwrap()["n"].clone())
            }
            _ => None,
        })
        .collect()
}


/// `ADD` as `who`, parked where its `Echo` listener's step asks the host: `Counter`'s transition has committed, its
/// listener cascade has not.
fn parked_between_transition_and_listener(kernel: &RuntimeKernel, who: &str) -> (orbital_core_runtime::resumable::DispatchContinuation, Vec<orbital_core_runtime::resumable::PendingRequest>) {
    let payload: Value = serde_json::from_value(serde_json::json!({ "who": who })).unwrap();
    match kernel.start_dispatch("Counter", None, "ADD", payload, None, None).unwrap() {
        orbital_core_runtime::resumable::Progress::Await { continuation, requests } => (continuation, requests),
        orbital_core_runtime::resumable::Progress::Done(_) => panic!("ADD did not park in its listener"),
    }
}

#[test]
fn two_overlapping_dispatches_writing_one_field_both_land_in_arrival_order() {
    use orbital_core_runtime::resumable::IoAnswer;
    let (kernel, store) = lane_kernel();
    let first = parked_between_transition_and_listener(&kernel, "a");
    let (tx, rx) = std::sync::mpsc::channel();
    let second = std::thread::spawn({
        let (kernel, store) = (kernel.clone(), store.clone());
        move || {
            let parked = parked_between_transition_and_listener(&kernel, "b");
            tx.send(()).unwrap();
            finish(&kernel, &store, parked, IoAnswer::Service(Ok(Value::Null)))
        }
    });
    assert!(rx.recv_timeout(NOT_YET).is_err(), "the second dispatch ran between the first's transition and its listener");
    assert_eq!(finish(&kernel, &store, first, IoAnswer::Service(Ok(Value::Null))), vec![serde_json::json!(1.0)]);
    assert_eq!(second.join().unwrap(), vec![serde_json::json!(2.0)]);
    assert_eq!(added(&kernel.dispatch("Counter", None, "READ", Value::Null).unwrap()), vec![serde_json::json!(2.0)]);
}

#[test]
fn a_completion_arriving_mid_dispatch_is_applied_after_that_dispatch_commits() {
    use orbital_core_runtime::os_host::completion::OsCompletionSink;
    use orbital_core_runtime::resumable::IoAnswer;
    let gate = Gate::new();
    let (kernel, store) = lane_kernel();
    let (sink, seen) = completion_sink(&kernel, &store, &gate);
    let first = parked_between_transition_and_listener(&kernel, "a");
    let completion = std::thread::spawn({
        let sink = sink.clone();
        move || sink.complete(&counter_site(), "k1", Ok(Value::Null))
    });
    std::thread::sleep(NOT_YET);
    assert!(seen.lock().unwrap().is_empty(), "the completion ran between the open dispatch's transition and its listener");
    assert_eq!(finish(&kernel, &store, first, IoAnswer::Service(Ok(Value::Null))), vec![serde_json::json!(1.0)]);
    completion.join().unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![serde_json::json!(1.0)]);
}

type Seen = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

fn counter_site() -> orbital_core_runtime::resumable::CallSite {
    orbital_core_runtime::resumable::CallSite {
        orbital: None,
        trait_name: "Counter".to_string(),
        follow_on: false,
        entity_id: orbital_core_runtime::process_result::RUNTIME_SINGLETON.to_string(),
        emit: orbital_core_runtime::resumable::CallRoutes { success: Some("DONE".to_string()), ..Default::default() },
    }
}

fn completion_sink(
    kernel: &Arc<RuntimeKernel>,
    store: &Arc<dyn EntitySource>,
    gate: &Arc<Gate>,
) -> (Arc<orbital_core_runtime::os_host::completion::KernelCompletionSink>, Seen) {
    use orbital_core_runtime::resumable::CallSite;
    let (store, gate_host) = (store.clone(), gate.clone());
    let seen: Seen = Arc::default();
    let sink = orbital_core_runtime::os_host::completion::KernelCompletionSink::new(
        kernel.clone(),
        Arc::new(move |request: &orbital_core_runtime::resumable::IoRequest| {
            orbital_core_runtime::resumable::BlockingHost { source: store.as_ref(), service: Some(gate_host.as_ref()), substrate: None }
                .answer(request)
        }),
        Arc::new({
            let seen = seen.clone();
            move |_: &CallSite, _: &str, _: &orbital_core_base::value::Value, _: Option<&str>, result: Result<orbital_core_runtime::resumable::DispatchOutcomes, orbital_core_runtime::resumable::DispatchError>| {
                seen.lock().unwrap().extend(added(&result.unwrap()));
            }
        }),
    );
    (Arc::new(sink), seen)
}

/// Resume a parked dispatch to its end: its service ask answered `service`, its row asks by `store`.
fn finish(
    kernel: &RuntimeKernel,
    store: &Arc<dyn EntitySource>,
    parked: (orbital_core_runtime::resumable::DispatchContinuation, Vec<orbital_core_runtime::resumable::PendingRequest>),
    service: orbital_core_runtime::resumable::IoAnswer,
) -> Vec<serde_json::Value> {
    use orbital_core_runtime::resumable::{BlockingHost, IoReply, IoRequest, Progress};
    let (mut continuation, mut requests) = parked;
    loop {
        let replies = requests
            .iter()
            .map(|p| IoReply {
                id: p.id,
                answer: match &p.request {
                    IoRequest::CallService { .. } => service.clone(),
                    other => BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(other),
                },
            })
            .collect();
        match kernel.resume(continuation, replies).unwrap() {
            Progress::Done(outcomes) => return added(&outcomes),
            Progress::Await { continuation: c, requests: r } => (continuation, requests) = (c, r),
        }
    }
}

fn parked(kernel: &RuntimeKernel, event: &str) -> (orbital_core_runtime::resumable::DispatchContinuation, Vec<orbital_core_runtime::resumable::PendingRequest>) {
    match kernel.start_dispatch("Counter", None, event, Value::Null, None, None).unwrap() {
        orbital_core_runtime::resumable::Progress::Await { continuation, requests } => (continuation, requests),
        orbital_core_runtime::resumable::Progress::Done(_) => panic!("{event} did not park on its ask"),
    }
}

/// `ASK_KEYED`, its row asks answered by `store`, parked on its keyed call-service with the lane free (EXEC-CONC-0).
fn parked_on_keyed_call(
    kernel: &RuntimeKernel,
    store: &Arc<dyn EntitySource>,
) -> (orbital_core_runtime::resumable::DispatchContinuation, Vec<orbital_core_runtime::resumable::PendingRequest>) {
    use orbital_core_runtime::resumable::{BlockingHost, IoReply, IoRequest, Progress};
    let (mut continuation, mut requests) = parked(kernel, "ASK_KEYED");
    while !requests.iter().any(|p| matches!(p.request, IoRequest::CallService { .. })) {
        let replies = requests
            .iter()
            .map(|p| IoReply {
                id: p.id,
                answer: BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(&p.request),
            })
            .collect();
        match kernel.resume(continuation, replies).unwrap() {
            Progress::Await { continuation: c, requests: r } => (continuation, requests) = (c, r),
            Progress::Done(_) => panic!("ASK_KEYED finished before its keyed ask"),
        }
    }
    (continuation, requests)
}

const NOT_YET: std::time::Duration = std::time::Duration::from_millis(250);

#[test]
fn a_dispatch_parked_on_an_unkeyed_ask_keeps_the_lane_until_it_commits() {
    use orbital_core_runtime::resumable::IoAnswer;
    let (kernel, store) = lane_kernel();
    let first = parked(&kernel, "ASK");
    let (tx, rx) = std::sync::mpsc::channel();
    let second = std::thread::spawn({
        let kernel = kernel.clone();
        move || tx.send(added(&kernel.dispatch("Counter", None, "BUMP", Value::Null).unwrap())).unwrap()
    });
    assert!(rx.recv_timeout(NOT_YET).is_err(), "the second dispatch ran while the first was parked");
    let first = finish(&kernel, &store, first, IoAnswer::Service(Ok(Value::Null)));
    assert_eq!(first, vec![serde_json::json!(1.0)]);
    assert_eq!(rx.recv().unwrap(), vec![serde_json::json!(2.0)]);
    second.join().unwrap();
}

#[test]
fn a_call_carrying_a_parked_dispatchs_turn_is_refused_not_hung() {
    use orbital_core_runtime::kernel::KernelError;
    use orbital_core_runtime::resumable::IoAnswer;
    let (kernel, store) = lane_kernel();
    let first = parked(&kernel, "ASK");
    let turn = first.0.turn().expect("a parked dispatch holds a lane turn");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn({
        let kernel = kernel.clone();
        move || tx.send(kernel.dispatch_in_turn(turn, "Counter", None, "BUMP", Value::Null).map(|o| added(&o))).unwrap()
    });
    let refused = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("a call carrying the parked dispatch's turn hung on the lane");
    assert!(
        matches!(&refused, Err(KernelError::LaneHeldByParkedDispatch { event, .. }) if event == "ASK"),
        "{refused:?}"
    );
    assert_eq!(finish(&kernel, &store, first, IoAnswer::Service(Ok(Value::Null))), vec![serde_json::json!(1.0)]);
    assert_eq!(added(&kernel.dispatch_in_turn(turn, "Counter", None, "BUMP", Value::Null).unwrap()), vec![serde_json::json!(2.0)]);
}

#[test]
fn control_an_untokened_call_on_the_thread_that_parked_a_dispatch_waits_its_turn() {
    use orbital_core_runtime::resumable::IoAnswer;
    let (kernel, store) = lane_kernel();
    let (handoff_tx, handoff_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let owner = std::thread::spawn({
        let kernel = kernel.clone();
        move || {
            handoff_tx.send(parked(&kernel, "ASK")).unwrap();
            let second = kernel.dispatch("Counter", None, "BUMP", Value::Null).map(|o| added(&o));
            done_tx.send(()).unwrap();
            second
        }
    });
    let first = handoff_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(NOT_YET).is_err(), "the untokened call ran while the first was parked");
    assert_eq!(finish(&kernel, &store, first, IoAnswer::Service(Ok(Value::Null))), vec![serde_json::json!(1.0)]);
    assert_eq!(owner.join().unwrap().expect("the untokened call was refused"), vec![serde_json::json!(2.0)]);
}

#[test]
fn control_an_abandoned_parked_dispatch_releases_the_lane() {
    let (kernel, _) = lane_kernel();
    drop(parked(&kernel, "ASK"));
    assert_eq!(added(&kernel.dispatch("Counter", None, "BUMP", Value::Null).unwrap()), vec![serde_json::json!(1.0)]);
}

#[test]
fn a_keyed_asks_completion_runs_as_its_own_dispatch_after_the_asking_dispatch_commits() {
    use orbital_core_runtime::os_host::completion::OsCompletionSink;
    use orbital_core_runtime::resumable::IoAnswer;
    let gate = Gate::new();
    let (kernel, store) = lane_kernel();
    let (sink, seen) = completion_sink(&kernel, &store, &gate);
    // A completion exists only once its key is deferred: the host registers the key before it answers (DispatchScope),
    // so it arrives with the keyed ask outstanding, never while the asker is still on an earlier row ask.
    let asking = parked_on_keyed_call(&kernel, &store);
    let completion = std::thread::spawn({
        let sink = sink.clone();
        move || sink.complete(&counter_site(), "k1", Ok(Value::Null))
    });
    std::thread::sleep(NOT_YET);
    assert!(seen.lock().unwrap().is_empty(), "the completion ran while its asking dispatch was parked");
    let first = finish(&kernel, &store, asking, IoAnswer::ServiceDeferred);
    assert_eq!(first, vec![serde_json::json!(1.0)]);
    completion.join().unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![serde_json::json!(1.0)]);
}

// A keyed `program/dispatch` (EXEC-CONC-0, R-a): the asking dispatch commits, the input runs as a dispatch of its own
// behind it, then the completion runs as a third dispatch to the asker. An unkeyed one made on behalf of the parked
// asker carries its turn and is refused.

struct Offline;

impl orbital_core_runtime::http_service::Transport for Offline {
    fn send(
        &self,
        _: &orbital_core_runtime::http_service::HttpRequest,
    ) -> Result<orbital_core_runtime::http_service::HttpResponse, orbital_core_runtime::http_service::TransportError> {
        Err(orbital_core_runtime::http_service::TransportError::Failed("offline".to_string()))
    }
}

/// The host half of `program/dispatch`: dispatches the address on the same kernel. Answered in place it acts on
/// behalf of the asking dispatch and carries its turn; deferred, it is a dispatch of its own and carries none.
struct Nested {
    kernel: Arc<RuntimeKernel>,
    turn: std::sync::Mutex<Option<orbital_core_runtime::kernel::LaneTurn>>,
}

impl Nested {
    fn dispatch(
        kernel: &RuntimeKernel,
        turn: Option<orbital_core_runtime::kernel::LaneTurn>,
        args: &[Value],
    ) -> Result<Value, orbital_core_runtime::effects::SubstrateFailure> {
        let address = args[0].as_str().expect("a qualified address").to_string();
        let event = address.rsplit('.').next().expect("an event").to_string();
        let dispatched = match turn {
            Some(turn) => kernel.dispatch_in_turn(turn, "Target", None, &event, args[1].clone()),
            None => kernel.dispatch("Target", None, &event, args[1].clone()),
        };
        dispatched
            .map(|outcomes| {
                let response = orbital_core_runtime::effects::response::EventResponse::from_process_results(&outcomes, "Target", &event);
                serde_json::from_value(serde_json::to_value(&response).expect("a response serializes")).expect("a response is a value")
            })
            .map_err(|e| orbital_core_runtime::effects::SubstrateFailure { message: e.to_string(), errors: Vec::new() })
    }
}

impl orbital_core_runtime::effects::SubstrateHandler for Nested {
    fn run(&self, op: &str, args: &[Value]) -> Result<Value, orbital_core_runtime::effects::SubstrateFailure> {
        assert_eq!(op, "program/dispatch");
        let turn = self.turn.lock().unwrap().expect("the host knows the turn it answers for");
        Self::dispatch(&self.kernel, Some(turn), args)
    }
}

type Done = std::sync::Mutex<std::sync::mpsc::Sender<(String, Vec<String>)>>;

struct Asking {
    kernel: Arc<RuntimeKernel>,
    nested: Arc<Nested>,
    sink: Arc<orbital_core_runtime::os_host::completion::KernelCompletionSink>,
    completions: std::sync::mpsc::Receiver<(String, Vec<String>)>,
}

fn asking_kernel(keyed: bool) -> Asking {
    use orbital_core_runtime::os_host::completion::KernelCompletionSink;
    let mut options = serde_json::json!({ "emit": { "success": "X_DONE", "failure": "X_FAILED" } });
    if keyed {
        options["key"] = serde_json::json!("k1");
    }
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "asking",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Tally", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" }, { "name": "n", "type": "number", "default": 0 },
                { "name": "got", "type": "string" }, { "name": "answered", "type": "boolean", "default": false }] },
            "traits": [{
                "name": "Asker", "linkedEntity": "Tally",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "GO", "effects": [["program/dispatch", "Main.Target.HIT", {}, options]] },
                        { "from": "idle", "to": "done", "event": "X_DONE", "effects": [["set", "@entity.got", "@payload.key"], ["set", "@entity.answered", "@payload.result.ok"]] },
                        { "from": "idle", "to": "failed", "event": "X_FAILED", "effects": [["set", "@entity.got", "@payload.error"]] }
                    ]
                }
            }, {
                "name": "Target", "linkedEntity": "Tally",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "events": [{ "key": "HIT", "name": "Hit", "input": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "HIT", "effects": [["set", "@entity.n", ["+", "@entity.n", 1]]] }]
                }
            }]
        }]
    }))
    .unwrap();
    let config = orbital_core_runtime::config::KernelConfig {
        substrate_handler: Some(Arc::new(orbital_core_runtime::resumable::HostAnswered) as Arc<dyn orbital_core_runtime::effects::SubstrateHandler>),
        ..Default::default()
    };
    let store = Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>;
    let kernel = Arc::new(RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), config));
    kernel.register_schema(schema).unwrap();
    let nested = Arc::new(Nested { kernel: kernel.clone(), turn: std::sync::Mutex::new(None) });
    let handles = Arc::new(orbital_core_runtime::os_host::OsHandles::default());
    let native = Arc::new(
        orbital_core_runtime::native_host::NativeHost::new(
            store,
            Arc::new(Offline),
            Arc::new(orbital_core_runtime::credentials::Credentials::from_env(Default::default())),
            Vec::new(),
            orbital_core_runtime::os_host::OsGrants::default(),
            handles.clone(),
            Arc::new(|_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {}),
        )
        .with_substrate(Some(nested.clone() as Arc<dyn orbital_core_runtime::effects::SubstrateHandler>))
        .with_substrate_later(Some({
            let kernel = kernel.clone();
            Arc::new(move |_: &str, args: &[Value]| {
                let (kernel, args) = (kernel.clone(), args.to_vec());
                Box::new(move || Nested::dispatch(&kernel, None, &args)) as orbital_core_runtime::native_host::SubstrateWork
            }) as Arc<orbital_core_runtime::native_host::SubstrateLater>
        })),
    );
    let (tx, completions) = std::sync::mpsc::channel();
    let done: Done = std::sync::Mutex::new(tx);
    let sink = Arc::new(KernelCompletionSink::new(
        kernel.clone(),
        Arc::new(move |request: &orbital_core_runtime::resumable::IoRequest| native.answer(request)),
        Arc::new(move |_: &orbital_core_runtime::resumable::CallSite, event: &str, _: &orbital_core_base::value::Value, _: Option<&str>, result: Result<orbital_core_runtime::resumable::DispatchOutcomes, orbital_core_runtime::resumable::DispatchError>| {
            let ran = result.unwrap().iter().filter(|(_, r)| r.executed).map(|(t, _)| t.clone()).collect();
            done.lock().unwrap().send((event.to_string(), ran)).unwrap();
        }),
    ));
    handles.completions.set_sink(sink.clone());
    Asking { kernel, nested, sink, completions }
}

/// `GO` run to its end by the host, the host answering for `GO`'s turn: the traits and events it ran itself.
fn go(asking: &Asking) -> Vec<(String, String)> {
    use orbital_core_runtime::resumable::Progress;
    let Progress::Await { continuation, requests } = asking.kernel.start_dispatch("Asker", None, "GO", Value::Null, None, None).unwrap() else {
        panic!("GO did not ask its host");
    };
    *asking.nested.turn.lock().unwrap() = continuation.turn();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn({
        let sink = asking.sink.clone();
        move || {
            let outcomes = sink.drive(Progress::Await { continuation, requests });
            let ran: Vec<(String, String)> = outcomes
                .unwrap()
                .iter()
                .filter(|(_, r)| r.executed)
                .map(|(t, r)| (t.clone(), r.event.clone().unwrap_or_default()))
                .collect();
            tx.send(ran).unwrap();
        }
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("GO's dispatch hung")
}

fn tally(kernel: &RuntimeKernel) -> (String, serde_json::Value, serde_json::Value) {
    let snapshot = kernel.snapshot_trait("Asker", None);
    let target = serde_json::to_value(&kernel.snapshot_trait("Target", None).entity).unwrap();
    (snapshot.state, serde_json::to_value(&snapshot.entity).unwrap(), target["n"].clone())
}

#[test]
fn a_keyed_program_dispatch_runs_after_the_asking_dispatch_commits_and_its_completion_after_it() {
    let asking = asking_kernel(true);
    let ran = go(&asking);
    assert_eq!(ran, vec![("Asker".to_string(), "GO".to_string())], "GO committed alone: no nested input, no completion");
    let (event, completion_ran) = asking.completions.recv_timeout(std::time::Duration::from_secs(5)).expect("the completion ran");
    assert_eq!(event, "X_DONE");
    assert_eq!(completion_ran, vec!["Asker".to_string()]);
    let (state, entity, hits) = tally(&asking.kernel);
    assert_eq!(state, "done");
    assert_eq!(entity["got"], "k1", "the completion echoes the ask's key");
    assert_eq!(entity["answered"], true, "the completion carries the input's own answer, so the input ran first");
    assert_eq!(hits, 1.0, "the input ran once");
}

#[test]
fn control_an_unkeyed_program_dispatch_on_behalf_of_a_parked_dispatch_is_refused_not_hung() {
    let asking = asking_kernel(false);
    let ran = go(&asking);
    assert!(ran.contains(&("Asker".to_string(), "X_FAILED".to_string())), "the refusal is the ask's own failure, inside GO: {ran:?}");
    let (state, entity, hits) = tally(&asking.kernel);
    assert_eq!(state, "failed");
    assert!(entity["got"].as_str().unwrap_or_default().contains("parked holding the dispatch lane"), "{entity}");
    assert!(hits.as_f64().unwrap_or(0.0) == 0.0, "no input ran: {hits}");
    assert!(asking.completions.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "nothing completes later");
}

// A keyed substrate stream (EXEC-QUEUE-13): each item the host's source yields arrives as the call's `onMessage` event, in
// order, until `cancel-call` drops the key; an unkeyed subscribe is answered in place, and this host refuses it.

struct Streams;

impl orbital_core_runtime::effects::SubstrateHandler for Streams {
    fn run(&self, op: &str, _: &[Value]) -> Result<Value, orbital_core_runtime::effects::SubstrateFailure> {
        Err(orbital_core_runtime::effects::SubstrateFailure {
            message: format!("{op} streams to its onMessage route: give it a key"),
            errors: Vec::new(),
        })
    }
}

struct Live {
    kernel: Arc<RuntimeKernel>,
    sink: Arc<orbital_core_runtime::os_host::completion::KernelCompletionSink>,
    feed: std::sync::mpsc::Sender<Value>,
    delivered: std::sync::mpsc::Receiver<(String, serde_json::Value)>,
}

fn live_kernel() -> Live {
    use orbital_core_runtime::os_host::completion::{KernelCompletionSink, StreamPoll};
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "live",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Feed", "persistence": "runtime", "fields": [
                { "name": "id", "type": "string" }, { "name": "count", "type": "number", "default": 0 }, { "name": "error", "type": "string" }] },
            "traits": [{
                "name": "Watch", "linkedEntity": "Feed",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "OPEN", "effects": [["program/live", {}, { "key": "live", "emit": { "onMessage": "ITEM", "failure": "LIVE_FAILED" } }]] },
                        { "from": "idle", "to": "idle", "event": "OPEN_UNKEYED", "effects": [["program/live", {}, { "emit": { "onMessage": "ITEM", "failure": "LIVE_FAILED" } }]] },
                        { "from": "idle", "to": "idle", "event": "STOP", "effects": [["cancel-call", "live"]] },
                        { "from": "idle", "to": "idle", "event": "ITEM", "effects": [["set", "@entity.count", ["+", "@entity.count", 1]]] },
                        { "from": "idle", "to": "idle", "event": "LIVE_FAILED", "effects": [["set", "@entity.error", "@payload.error"]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    let config = orbital_core_runtime::config::KernelConfig {
        substrate_handler: Some(Arc::new(orbital_core_runtime::resumable::HostAnswered) as Arc<dyn orbital_core_runtime::effects::SubstrateHandler>),
        ..Default::default()
    };
    let store = Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>;
    let kernel = Arc::new(RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), config));
    kernel.register_schema(schema).unwrap();
    let (feed, source) = std::sync::mpsc::channel::<Value>();
    let source = std::sync::Mutex::new(Some(source));
    let handles = Arc::new(orbital_core_runtime::os_host::OsHandles::default());
    let native = Arc::new(
        orbital_core_runtime::native_host::NativeHost::new(
            store,
            Arc::new(Offline),
            Arc::new(orbital_core_runtime::credentials::Credentials::from_env(Default::default())),
            Vec::new(),
            orbital_core_runtime::os_host::OsGrants::default(),
            handles.clone(),
            Arc::new(|_: Option<&orbital_core_runtime::resumable::CallSite>, _: Value| {}),
        )
        .with_substrate(Some(Arc::new(Streams) as Arc<dyn orbital_core_runtime::effects::SubstrateHandler>))
        .with_substrate_streams(Some(Arc::new(move |op: &str, _: &[Value]| {
            let receiver = source.lock().unwrap().take().filter(|_| op == "program/live")?;
            Some(Box::new(move || match receiver.try_recv() {
                Ok(item) => StreamPoll::Item(item),
                Err(std::sync::mpsc::TryRecvError::Empty) => StreamPoll::Idle,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => StreamPoll::Ended,
            }) as orbital_core_runtime::native_host::SubstrateSource)
        }) as Arc<orbital_core_runtime::native_host::SubstrateStreams>)),
    );
    let (tx, delivered) = std::sync::mpsc::channel();
    let done = std::sync::Mutex::new(tx);
    let sink = Arc::new(KernelCompletionSink::new(
        kernel.clone(),
        Arc::new(move |request: &orbital_core_runtime::resumable::IoRequest| native.answer(request)),
        Arc::new(move |_: &orbital_core_runtime::resumable::CallSite, event: &str, _: &orbital_core_base::value::Value, _: Option<&str>, result: Result<orbital_core_runtime::resumable::DispatchOutcomes, orbital_core_runtime::resumable::DispatchError>| {
            let payload = result.unwrap().first().map(|(_, r)| serde_json::to_value(&*r.payload).unwrap()).unwrap_or_default();
            done.lock().unwrap().send((event.to_string(), payload)).unwrap();
        }),
    ));
    handles.completions.set_sink(sink.clone());
    Live { kernel, sink, feed, delivered }
}

fn send_live(live: &Live, event: &str) {
    let started = live.kernel.start_dispatch("Watch", None, event, Value::Null, None, None).unwrap();
    live.sink.drive(started).unwrap();
}

fn live_item(n: f64) -> Value {
    Value::Object([("n".to_string(), Value::Number(n))].into_iter().collect())
}

fn watch(kernel: &RuntimeKernel) -> serde_json::Value {
    serde_json::to_value(&kernel.snapshot_trait("Watch", None).entity).unwrap()
}

#[test]
fn a_keyed_stream_delivers_each_item_to_its_message_route_in_order_until_cancelled() {
    let live = live_kernel();
    send_live(&live, "OPEN");
    live.feed.send(live_item(1.0)).unwrap();
    live.feed.send(live_item(2.0)).unwrap();
    let wait = std::time::Duration::from_secs(5);
    let first = live.delivered.recv_timeout(wait).expect("the first item arrived");
    let second = live.delivered.recv_timeout(wait).expect("the second item arrived");
    assert_eq!((first.0.as_str(), first.1["n"].clone(), first.1["key"].clone()), ("ITEM", serde_json::json!(1.0), serde_json::json!("live")));
    assert_eq!((second.0.as_str(), second.1["n"].clone()), ("ITEM", serde_json::json!(2.0)), "in the order the source yielded them");
    send_live(&live, "STOP");
    live.feed.send(live_item(3.0)).unwrap();
    assert!(live.delivered.recv_timeout(std::time::Duration::from_millis(300)).is_err(), "cancel-call stopped the stream");
    assert_eq!(watch(&live.kernel)["count"].as_f64(), Some(2.0));
}

#[test]
fn control_an_unkeyed_subscribe_is_answered_in_place_and_refused() {
    let live = live_kernel();
    send_live(&live, "OPEN_UNKEYED");
    live.feed.send(live_item(1.0)).unwrap();
    assert!(live.delivered.recv_timeout(std::time::Duration::from_millis(300)).is_err(), "no stream was opened");
    let entity = watch(&live.kernel);
    assert!(entity["error"].as_str().unwrap_or_default().contains("give it a key"), "{entity}");
    assert_eq!(entity["count"].as_f64().unwrap_or(0.0), 0.0, "no item arrived");
}

fn two_listener_schema() -> OrbitalSchema {
    let target = |name: &str, mapping: serde_json::Value| {
        serde_json::json!({
            "name": name,
            "linkedEntity": "Item",
            "listens": [{ "event": "SAVE", "triggers": "DO_SAVE", "source": { "kind": "trait", "trait": "Source" }, "payloadMapping": mapping }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "DO_SAVE", "effects": [["set", "@entity.id", "@payload.id"]] }]
            }
        })
    };
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Source", "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": [{ "from": "idle", "to": "idle", "event": "SAVE" }] } },
                target("TargetA", serde_json::Value::Null),
                target("TargetB", serde_json::Value::Null),
                target("Mapped", serde_json::json!({ "id": "@payload.id" }))
            ]
        }]
    }))
    .unwrap()
}

#[test]
fn listeners_of_one_delivery_keep_one_shared_payload_in_their_outcomes() {
    let kernel = kernel_with(two_listener_schema());
    let payload: Value = serde_json::from_value(serde_json::json!({ "id": "abc123", "rows": [1, 2, 3] })).unwrap();
    let outcomes = kernel.dispatch("Source", Some("row-1"), "SAVE", payload).unwrap();
    let of = |name: &str| &outcomes.iter().find(|(t, _)| t == name).unwrap_or_else(|| panic!("{name} should fire")).1;
    assert!(Arc::ptr_eq(&of("TargetA").payload, &of("TargetB").payload), "bare listeners share the delivered payload, never a copy each");
    assert!(!Arc::ptr_eq(&of("TargetA").payload, &of("Mapped").payload), "control: a mapped listener holds its own payload");
    assert_eq!(serde_json::to_value(&*of("Mapped").payload).unwrap()["id"], "abc123");
}
