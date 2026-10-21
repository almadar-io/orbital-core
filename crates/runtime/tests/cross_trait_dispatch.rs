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
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel.register_schema(schema).unwrap();
    kernel
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
// second dispatch or a completion arriving while it runs waits for it. `ADD` commits its write and then waits in its
// `Echo` listener's step, so an overlap lands between a transition and its listener cascade.

/// The first caller (`who: "a"`) waits up to 400 ms for a second caller; `overlapped` records that one arrived
/// while the first was still inside its dispatch.
struct Gate {
    entered: std::sync::Mutex<(bool, bool)>,
    signal: std::sync::Condvar,
    overlapped: std::sync::atomic::AtomicBool,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: std::sync::Mutex::new((false, false)),
            signal: std::sync::Condvar::new(),
            overlapped: Default::default(),
        })
    }

    fn wait_first(&self) {
        let guard = self.entered.lock().unwrap();
        let (state, _) = self
            .signal
            .wait_timeout_while(guard, std::time::Duration::from_secs(5), |(first, _)| !*first)
            .unwrap();
        assert!(state.0, "the first dispatch never reached its gate");
    }
}

impl orbital_core_runtime::effects::ServiceHandler for Gate {
    fn call(
        &self,
        _s: &str,
        _m: &str,
        args: &Value,
        _k: Option<&str>,
        _c: Option<&orbital_core_runtime::effects::ServiceCaller>,
    ) -> Result<Value, String> {
        let who = serde_json::to_value(args).unwrap()["who"].clone();
        let mut state = self.entered.lock().unwrap();
        if who == "a" {
            state.0 = true;
            self.signal.notify_all();
            let wait = std::time::Duration::from_millis(400);
            let (state, _) = self.signal.wait_timeout_while(state, wait, |(_, second)| !*second).unwrap();
            self.overlapped.store(state.1, std::sync::atomic::Ordering::SeqCst);
        } else {
            state.1 = true;
            self.signal.notify_all();
        }
        Ok(Value::Null)
    }

    fn cancel(&self, _key: &str) -> Result<(), String> {
        Ok(())
    }
}

fn lane_kernel(gate: Arc<Gate>, host_answered: bool) -> (Arc<RuntimeKernel>, Arc<dyn EntitySource>) {
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
        service_handler: Some(if host_answered {
            Arc::new(orbital_core_runtime::resumable::HostAnswered) as Arc<dyn orbital_core_runtime::effects::ServiceHandler>
        } else {
            gate as Arc<dyn orbital_core_runtime::effects::ServiceHandler>
        }),
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

fn add(kernel: &RuntimeKernel, who: &str) -> Vec<serde_json::Value> {
    let payload: Value = serde_json::from_value(serde_json::json!({ "who": who })).unwrap();
    added(&kernel.dispatch("Counter", None, "ADD", payload).unwrap())
}

#[test]
fn two_overlapping_dispatches_writing_one_field_both_land_in_arrival_order() {
    let gate = Gate::new();
    let (kernel, _) = lane_kernel(gate.clone(), false);
    let first = std::thread::spawn({
        let kernel = kernel.clone();
        move || add(&kernel, "a")
    });
    gate.wait_first();
    let second = add(&kernel, "b");
    assert!(!gate.overlapped.load(std::sync::atomic::Ordering::SeqCst), "the second dispatch ran inside the first");
    assert_eq!(first.join().unwrap(), vec![serde_json::json!(1.0)]);
    assert_eq!(second, vec![serde_json::json!(2.0)]);
    assert_eq!(added(&kernel.dispatch("Counter", None, "READ", Value::Null).unwrap()), vec![serde_json::json!(2.0)]);
}

#[test]
fn a_completion_arriving_mid_dispatch_is_applied_after_that_dispatch_commits() {
    use orbital_core_runtime::os_host::completion::OsCompletionSink;
    let gate = Gate::new();
    let (kernel, store) = lane_kernel(gate.clone(), false);
    let (sink, seen) = completion_sink(&kernel, &store, &gate);
    let first = std::thread::spawn({
        let kernel = kernel.clone();
        move || add(&kernel, "a")
    });
    gate.wait_first();
    sink.complete(&counter_site(), "k1", Ok(Value::Null));
    assert!(!gate.overlapped.load(std::sync::atomic::Ordering::SeqCst), "the completion ran inside the open dispatch");
    assert_eq!(first.join().unwrap(), vec![serde_json::json!(1.0)]);
    assert_eq!(*seen.lock().unwrap(), vec![serde_json::json!(1.0)]);
}

type Seen = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

fn counter_site() -> orbital_core_runtime::resumable::CallSite {
    orbital_core_runtime::resumable::CallSite {
        orbital: None,
        trait_name: "Counter".to_string(),
        follow_on: false,
        entity_id: "singleton".to_string(),
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
            move |_: &CallSite, _: &str, result: Result<orbital_core_runtime::resumable::DispatchOutcomes, orbital_core_runtime::resumable::DispatchError>| {
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

const NOT_YET: std::time::Duration = std::time::Duration::from_millis(250);

#[test]
fn a_dispatch_parked_on_an_unkeyed_ask_keeps_the_lane_until_it_commits() {
    use orbital_core_runtime::resumable::IoAnswer;
    let (kernel, store) = lane_kernel(Gate::new(), true);
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
    let (kernel, store) = lane_kernel(Gate::new(), true);
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
    let (kernel, store) = lane_kernel(Gate::new(), true);
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
    let (kernel, _) = lane_kernel(Gate::new(), true);
    drop(parked(&kernel, "ASK"));
    assert_eq!(added(&kernel.dispatch("Counter", None, "BUMP", Value::Null).unwrap()), vec![serde_json::json!(1.0)]);
}

#[test]
fn a_keyed_asks_completion_runs_as_its_own_dispatch_after_the_asking_dispatch_commits() {
    use orbital_core_runtime::os_host::completion::OsCompletionSink;
    use orbital_core_runtime::resumable::IoAnswer;
    let gate = Gate::new();
    let (kernel, store) = lane_kernel(gate.clone(), true);
    let (sink, seen) = completion_sink(&kernel, &store, &gate);
    let asking = parked(&kernel, "ASK_KEYED");
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
