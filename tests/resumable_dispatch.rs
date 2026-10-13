// K4: a dispatch that stops at its I/O points (`Progress::Await`) and is resumed with the host's
// answers, in any order and later, produces the same trace as the blocking run. The fixtures are
// the correctness scenarios of persist_success_emit, client_fetch_success_emit, same_trait_cascade,
// dispatch_roots, cross_trait_dispatch and completion_events.

use orbital_core::effects::{LifecycleEvent, MountSeed, ServiceHandler, SubstrateFailure, SubstrateHandler};
use orbital_core::runtime::resumable::{
    drive_blocking, BlockingHost, DispatchContinuation, DispatchError, DispatchOutcomes,
    HostAnswered, IoAnswer, IoReply, IoRequest, PendingRequest, Progress, RequestId, ResumeError,
};
use orbital_core::runtime::{EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
enum Step {
    Dispatch {
        trait_name: &'static str,
        event: &'static str,
    },
    Mount(Vec<(&'static str, &'static str)>),
}

#[derive(Clone)]
struct Setup {
    schema: Json,
    entities: Vec<&'static str>,
    seed_rows: Vec<(&'static str, Json)>,
    mounted: Vec<&'static str>,
    service: Option<Arc<dyn ServiceHandler>>,
    substrate: Option<Arc<dyn SubstrateHandler>>,
    fail_creates: bool,
    steps: Vec<Step>,
}

impl Setup {
    fn new(schema: Json, entities: &[&'static str], steps: Vec<Step>) -> Self {
        Self {
            schema,
            entities: entities.to_vec(),
            seed_rows: Vec::new(),
            mounted: Vec::new(),
            service: None,
            substrate: None,
            fail_creates: false,
            steps,
        }
    }
}

fn dispatch(trait_name: &'static str, event: &'static str) -> Step {
    Step::Dispatch { trait_name, event }
}

fn persist_success() -> Setup {
    let arm = |event: &str, effects: Json| json!({ "from": "idle", "to": "idle", "event": event, "effects": effects });
    let emit = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    let schema = json!({
        "name": "persist-success",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "Notes", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        arm("CREATE", json!([["persist", "create", "Note", { "id": "n1", "title": "first" }, emit]])),
                        arm("UPDATE", json!([["persist", "update", "Note", { "id": "n1", "title": "second" }, emit]])),
                        arm("DELETE", json!([["persist", "delete", "Note", "n1", emit]])),
                        arm("ANON", json!([["persist", "create", "Note", { "title": "no id" }, emit]]))
                    ]
                }
            }]
        }]
    });
    Setup::new(
        schema,
        &["Note"],
        vec![
            dispatch("Notes", "CREATE"),
            dispatch("Notes", "UPDATE"),
            dispatch("Notes", "ANON"),
            dispatch("Notes", "DELETE"),
        ],
    )
}

fn moves_then_writes() -> Setup {
    let emit = json!({ "emit": { "success": "SAVED", "failure": "SAVE_FAILED" } });
    let schema = json!({
        "name": "moves",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "Draft", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "editing", "isInitial": true }, { "name": "saving" }, { "name": "saved" }, { "name": "failed" }],
                    "transitions": [
                        { "from": "editing", "to": "saving", "event": "SAVE", "effects": [["persist", "create", "Note", { "id": "d1", "title": "draft" }, emit]] },
                        { "from": "saving", "to": "saved", "event": "SAVED" },
                        { "from": "saving", "to": "failed", "event": "SAVE_FAILED" }
                    ]
                }
            }]
        }]
    });
    Setup::new(schema, &["Note"], vec![dispatch("Draft", "SAVE")])
}

fn fetch_success() -> Setup {
    let schema = json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                {
                    "name": "Browse",
                    "linkedEntity": "Item",
                    "stateMachine": {
                        "states": [{ "name": "loading", "isInitial": true }, { "name": "browsing" }],
                        "transitions": [
                            { "from": "loading", "to": "loading", "event": "INIT", "effects": [["fetch", "Item", { "emit": { "success": "Loaded", "failure": "LoadFailed" } }]] },
                            { "from": "loading", "to": "browsing", "event": "Loaded" },
                            { "from": "browsing", "to": "browsing", "event": "Loaded" }
                        ]
                    }
                },
                {
                    "name": "Relay",
                    "linkedEntity": "Item",
                    "listens": [{
                        "event": "Loaded", "triggers": "RelayLoaded",
                        "source": { "kind": "trait", "trait": "Browse" },
                        "payloadMapping": { "rows": "@payload.data" }
                    }],
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "shown" }],
                        "transitions": [
                            { "from": "idle", "to": "shown", "event": "RelayLoaded" },
                            { "from": "shown", "to": "shown", "event": "RelayLoaded" }
                        ]
                    }
                }
            ]
        }]
    });
    let mut setup = Setup::new(schema, &["Item"], vec![dispatch("Browse", "INIT")]);
    setup.seed_rows = vec![
        ("Item", json!({ "id": "i-1" })),
        ("Item", json!({ "id": "i-2" })),
    ];
    setup
}

fn same_trait_cascade() -> Setup {
    let schema = json!({
        "name": "loop",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Counter", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "running" }],
                    "transitions": [
                        { "from": "idle", "to": "running", "event": "GO", "effects": [["emit", "STEP", { "n": 3 }], ["emit", "UNHANDLED", {}]] },
                        { "from": "running", "to": "running", "event": "STEP", "guard": [">", "@payload.n", 0], "effects": [["emit", "STEP", { "n": ["-", "@payload.n", 1] }]] },
                        { "from": "running", "to": "idle", "event": "STEP", "guard": ["=", "@payload.n", 0], "effects": [["emit", "DONE", {}]] }
                    ]
                } },
                { "name": "Watcher",
                  "listens": [{ "event": "STEP", "triggers": "SAW", "source": { "kind": "trait", "trait": "Counter" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "SAW", "effects": [["emit", "WATCHED", { "n": "@payload.n" }]] }] } }
            ]
        }]
    });
    Setup::new(schema, &["Item"], vec![dispatch("Counter", "GO")])
}

fn dispatch_roots() -> Setup {
    let once = json!([
        "not",
        [
            "array/includes",
            [
                "array/map",
                "@prevEvents",
                ["fn", "e", ["object/get", "@e", "event"]]
            ],
            "X"
        ]
    ]);
    let listener = |name: &str, ack: &str, guard: Option<Json>| {
        let mut listen = json!({ "event": "X", "triggers": "GOT", "source": { "kind": "trait", "trait": "Src" } });
        if let Some(g) = guard {
            listen["guard"] = g;
        }
        json!({
            "name": name,
            "listens": [listen],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "GOT", "effects": [["emit", ack, { "n": "@payload.n" }]] }]
            }
        })
    };
    let prov_ack = json!([["emit", "ACK_PROV", {
        "srcTrait": "@event.source.trait", "srcEvent": "@event.event", "srcN": "@event.payload.n",
        "from": "@fromState", "to": "@toState", "state": "@state", "seen": ["array/len", "@prevEvents"]
    }]]);
    let schema = json!({
        "name": "dispatch-roots-app",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Src", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO",
                        "effects": [["emit", "X", { "n": 1 }], ["emit", "X", { "n": 2 }], ["emit", "X", { "n": 3 }]] }]
                } },
                listener("Once", "ACK_ONCE", Some(once.clone())),
                listener("Twin", "ACK_TWIN", Some(once)),
                listener("All", "ACK_ALL", None),
                { "name": "Prov",
                  "listens": [{ "event": "X", "triggers": "GOT", "source": { "kind": "trait", "trait": "Src" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "seen" }],
                    "transitions": [
                        { "from": "idle", "to": "seen", "event": "GOT",
                          "guard": ["and", ["=", "@fromState", "idle"], ["=", "@toState", "seen"], ["=", "@state", "idle"]], "effects": prov_ack },
                        { "from": "seen", "to": "seen", "event": "GOT", "effects": prov_ack }
                    ]
                } },
                { "name": "Reenter",
                  "listens": [{ "event": "X", "triggers": "STEP", "source": { "kind": "trait", "trait": "Src" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "processing" }],
                    "transitions": [
                        { "from": "idle", "to": "processing", "event": "STEP",
                          "guard": ["not", ["array/includes", "@prevStates", "processing"]], "effects": [["emit", "ACK_R", { "to": "@toState" }]] },
                        { "from": "processing", "to": "idle", "event": "STEP", "effects": [["emit", "ACK_R", { "to": "@toState" }]] }
                    ]
                } }
            ]
        }]
    });
    Setup::new(schema, &["Item"], vec![dispatch("Src", "GO")])
}

fn cross_trait() -> Setup {
    let schema = json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Catalog", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT" }]
                } },
                { "name": "CreateModal", "linkedEntity": "Item",
                  "listens": [{ "event": "CREATE", "triggers": "CREATE", "source": { "kind": "trait", "trait": "Catalog" } }],
                  "stateMachine": {
                    "states": [{ "name": "closed", "isInitial": true }, { "name": "open" }],
                    "transitions": [
                        { "from": "closed", "to": "closed", "event": "INIT" },
                        { "from": "closed", "to": "open", "event": "CREATE", "effects": [["render-ui", "modal", { "type": "form" }]] },
                        { "from": "open", "to": "closed", "event": "CLOSE", "effects": [["render-ui", "modal", null]] }
                    ]
                } }
            ]
        }]
    });
    Setup::new(
        schema,
        &["Item"],
        vec![
            dispatch("Catalog", "CREATE"),
            dispatch("CreateModal", "CLOSE"),
        ],
    )
}

fn mount_batch() -> Setup {
    let mut setup = cross_trait();
    setup.mounted = vec!["Catalog", "CreateModal"];
    setup.steps = vec![Step::Mount(vec![
        ("Catalog", "INIT"),
        ("Nowhere", "INIT"),
        ("CreateModal", "INIT"),
    ])];
    setup
}

fn nothing_to_mount() -> Setup {
    let mut setup = cross_trait();
    setup.steps = vec![Step::Mount(vec![
        ("Nowhere", "INIT"),
        ("Elsewhere", "INIT"),
    ])];
    setup
}

struct Llm {
    fail: bool,
}

impl ServiceHandler for Llm {
    fn call(&self, _s: &str, _m: &str, _a: &Value, _k: Option<&str>, _c: Option<&orbital_core::effects::ServiceCaller>) -> Result<Value, String> {
        if self.fail {
            return Err("rate limited".to_string());
        }
        let mut out = HashMap::new();
        out.insert(
            "embeddings".to_string(),
            Value::Array(vec![Value::Number(1.0)]),
        );
        Ok(Value::Object(out))
    }
    fn cancel(&self, _key: &str) -> Result<(), String> {
        Ok(())
    }
}

struct Host {
    fail: bool,
}

impl SubstrateHandler for Host {
    fn run(&self, op: &str, _args: &[Value]) -> Result<Value, SubstrateFailure> {
        if self.fail {
            return Err(SubstrateFailure {
                message: format!("{op} refused"),
                errors: vec![Value::String("ORB_X".into())],
            });
        }
        Ok(Value::String("ok".into()))
    }
}

fn completion(effect: Json) -> Setup {
    let options = json!({ "emit": { "success": "DONE", "failure": "FAILED" } });
    let mut effect = effect;
    effect.as_array_mut().unwrap().push(options);
    let schema = json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Run", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Runner",
                "linkedEntity": "Run",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "GO", "effects": [effect] },
                        { "from": "idle", "to": "done", "event": "DONE", "effects": [["set", "@entity.got", "@payload"]] },
                        { "from": "idle", "to": "failed", "event": "FAILED", "effects": [["set", "@entity.got", "@payload"]] }
                    ]
                }
            }]
        }]
    });
    Setup::new(schema, &["Run"], vec![dispatch("Runner", "GO")])
}

fn service_call(fail: bool) -> Setup {
    let mut setup = completion(json!(["call-service", "llm", "embed", { "texts": ["a"] }]));
    setup.service = Some(Arc::new(Llm { fail }));
    setup
}

fn substrate_call(fail: bool) -> Setup {
    let mut setup =
        completion(json!(["program/eval", { "name": "P", "orbitals": [] }, { "final": true }]));
    setup.substrate = Some(Arc::new(Host { fail }));
    setup
}

fn stamped() -> Setup {
    let schema = json!({
        "name": "stamp",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [
                { "name": "First", "linkedEntity": "Note",
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [
                        ["persist", "create", "Note", { "id": "s1", "title": "one" }],
                        ["emit", "STAMP", { "at": "@now" }]
                    ] }]
                } },
                { "name": "Second",
                  "listens": [{ "event": "STAMP", "triggers": "SEEN", "source": { "kind": "trait", "trait": "First" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "SEEN", "effects": [
                        ["fetch", "Note"],
                        ["emit", "LATER", { "at": "@now" }]
                    ] }]
                } }
            ]
        }]
    });
    Setup::new(schema, &["Note"], vec![dispatch("First", "GO")])
}

fn every_fixture() -> Vec<(&'static str, Setup)> {
    vec![
        ("persist_success", persist_success()),
        ("fetch_success", fetch_success()),
        ("moves_then_writes", moves_then_writes()),
        ("same_trait_cascade", same_trait_cascade()),
        ("dispatch_roots", dispatch_roots()),
        ("cross_trait", cross_trait()),
        ("mount_batch", mount_batch()),
        ("nothing_to_mount", nothing_to_mount()),
        ("service_ok", service_call(false)),
        ("service_failed", service_call(true)),
        ("substrate_ok", substrate_call(false)),
        ("substrate_failed", substrate_call(true)),
    ]
}

// ---------------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------------

struct FailingCreates(InMemoryEntityCache);

impl EntitySource for FailingCreates {
    fn read(&self, t: &str, id: &str) -> Result<Value, String> {
        self.0.read(t, id)
    }
    fn write(&self, t: &str, id: &str, d: &Value) -> Result<Value, String> {
        self.0.write(t, id, d)
    }
    fn create(&self, _t: &str, _d: &Value) -> Result<Value, String> {
        Err("boom".to_string())
    }
    fn delete(&self, t: &str, id: &str) -> Result<(), String> {
        self.0.delete(t, id)
    }
    fn clear(&self) {
        self.0.clear()
    }
    fn count(&self) -> usize {
        self.0.count()
    }
    fn list(&self, t: &str) -> Result<Vec<Value>, String> {
        self.0.list(t)
    }
    fn bind_collection(&self, t: &str, c: &str) {
        self.0.bind_collection(t, c)
    }
}

/// A kernel and the store its rows live in. `resumable` kernels hand service and substrate calls to the host.
fn build(setup: &Setup, resumable: bool) -> (RuntimeKernel, Arc<dyn EntitySource>) {
    let store: Arc<dyn EntitySource> = if setup.fail_creates {
        Arc::new(FailingCreates(InMemoryEntityCache::new()))
    } else {
        Arc::new(InMemoryEntityCache::new())
    };
    for (entity, row) in &setup.seed_rows {
        let row: Value = serde_json::from_value(row.clone()).unwrap();
        let id = row.as_object().unwrap()["id"].as_str().unwrap().to_string();
        store.write(entity, &id, &row).unwrap();
    }
    let mut config = KernelConfig::default();
    if setup.service.is_some() {
        config.service_handler = Some(if resumable {
            Arc::new(HostAnswered) as Arc<dyn ServiceHandler>
        } else {
            setup.service.clone().unwrap()
        });
    }
    if setup.substrate.is_some() {
        config.substrate_handler = Some(if resumable {
            Arc::new(HostAnswered) as Arc<dyn SubstrateHandler>
        } else {
            setup.substrate.clone().unwrap()
        });
    }
    let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), config);
    let schema: OrbitalSchema = serde_json::from_value(setup.schema.clone()).unwrap();
    kernel.register_schema(schema).unwrap();
    kernel.mount_traits(setup.mounted.iter().copied());
    (kernel, store)
}

fn seeds(list: &[(&str, &str)]) -> Vec<MountSeed> {
    list.iter()
        .map(|(t, e)| MountSeed {
            r#trait: t.to_string(),
            event: LifecycleEvent::parse(e).expect("a lifecycle event"),
        })
        .collect()
}

fn blocking_step(kernel: &RuntimeKernel, step: &Step) -> DispatchOutcomes {
    match step {
        Step::Dispatch { trait_name, event } => kernel
            .dispatch_continuing(trait_name, None, event, Value::Null, None, None)
            .unwrap(),
        Step::Mount(list) => kernel
            .dispatch_mount(&seeds(list), None, Value::Null)
            .unwrap(),
    }
}

fn start_step(kernel: &RuntimeKernel, step: &Step) -> Progress<DispatchOutcomes> {
    match step {
        Step::Dispatch { trait_name, event } => kernel
            .start_dispatch(trait_name, None, event, Value::Null, None, None)
            .unwrap(),
        Step::Mount(list) => kernel
            .start_dispatch_mount(&seeds(list), None, Value::Null)
            .unwrap(),
    }
}

fn j(v: &Value) -> Json {
    serde_json::to_value(v).unwrap()
}

/// Everything the dispatch decided, in order, as comparable JSON.
fn trace_of(outcomes: &DispatchOutcomes) -> Json {
    Json::Array(
        outcomes
            .iter()
            .map(|(name, r)| {
                json!({
                    "trait": name,
                    "executed": r.executed,
                    "previous": r.previous_state,
                    "new": r.new_state,
                    "effectsExecuted": r.effects_executed,
                    "effects": r.effect_results.iter().map(|er| json!({
                        "effect": serde_json::to_value(&er.effect).unwrap(),
                        "outcome": format!("{:?}", er.outcome),
                        "return": er.return_value.as_ref().map(j),
                        "error": er.error,
                        "entityType": er.entity_type,
                        "total": er.total_count,
                    })).collect::<Vec<_>>(),
                    "entity": j(&r.entity_data),
                    "linked": r.linked_entity,
                    "event": r.event,
                    "payload": j(&r.payload),
                    "firedArm": r.fired_arm,
                    "runs": r.effect_runs.iter().map(|run| json!({
                        "status": format!("{:?}", run.status),
                        "error": run.error,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// The world after the steps: every trait's state and frame, every stored row.
fn world_of(setup: &Setup, kernel: &RuntimeKernel, store: &Arc<dyn EntitySource>) -> Json {
    let mut states = serde_json::Map::new();
    let schema: OrbitalSchema = serde_json::from_value(setup.schema.clone()).unwrap();
    for orbital in &schema.orbitals {
        for t in &orbital.traits {
            if let Some(name) = t.trait_name() {
                let snap = kernel.snapshot_trait(name, None);
                states.insert(
                    name.to_string(),
                    json!({ "state": snap.state, "frame": j(&snap.entity) }),
                );
            }
        }
    }
    let mut rows = serde_json::Map::new();
    for entity in &setup.entities {
        let mut list: Vec<Json> = store.list(entity).unwrap().iter().map(j).collect();
        list.sort_by_key(|r| r["id"].as_str().unwrap_or_default().to_string());
        rows.insert(entity.to_string(), Json::Array(list));
    }
    json!({ "states": states, "rows": rows })
}

fn blocking_run(setup: &Setup) -> (Vec<Json>, Json) {
    let (kernel, store) = build(setup, false);
    let traces = setup
        .steps
        .iter()
        .map(|step| trace_of(&blocking_step(&kernel, step)))
        .collect();
    (traces, world_of(setup, &kernel, &store))
}

fn host_answers(setup: &Setup, store: &Arc<dyn EntitySource>) -> impl Fn(&IoRequest) -> IoAnswer {
    let store = store.clone();
    let service = setup.service.clone();
    let substrate = setup.substrate.clone();
    move |request: &IoRequest| {
        BlockingHost {
            source: store.as_ref(),
            service: service.as_deref(),
            substrate: substrate.as_deref(),
        }
        .answer(request)
    }
}

struct Flight {
    setup: Setup,
    kernel: RuntimeKernel,
    store: Arc<dyn EntitySource>,
    next: usize,
    traces: Vec<Json>,
    awaiting: Option<(DispatchContinuation, Vec<PendingRequest>)>,
    awaits: usize,
    log: Vec<IoRequest>,
}

impl Flight {
    fn new(setup: &Setup) -> Self {
        let (kernel, store) = build(setup, true);
        Self {
            setup: setup.clone(),
            kernel,
            store,
            next: 0,
            traces: Vec::new(),
            awaiting: None,
            awaits: 0,
            log: Vec::new(),
        }
    }

    fn settle(&mut self, progress: Progress<DispatchOutcomes>) {
        match progress {
            Progress::Done(outcomes) => {
                self.traces.push(trace_of(&outcomes));
                self.next += 1;
            }
            Progress::Await {
                continuation,
                requests,
            } => {
                self.awaits += 1;
                self.log.extend(requests.iter().map(|p| p.request.clone()));
                self.awaiting = Some((continuation, requests));
            }
        }
    }

    fn begin_next(&mut self) -> bool {
        while self.awaiting.is_none() && self.next < self.setup.steps.len() {
            let progress = start_step(&self.kernel, &self.setup.steps[self.next].clone());
            self.settle(progress);
        }
        self.awaiting.is_some()
    }

    fn answer_now(&mut self, answer: &dyn Fn(&IoRequest) -> IoAnswer) {
        let (continuation, requests) = self.awaiting.take().unwrap();
        let mut replies: Vec<IoReply> = requests
            .iter()
            .map(|p| IoReply {
                id: p.id,
                answer: answer(&p.request),
            })
            .collect();
        replies.reverse();
        let progress = self.kernel.resume(continuation, replies).unwrap();
        self.settle(progress);
    }

    fn finished(&self) -> bool {
        self.awaiting.is_none() && self.next >= self.setup.steps.len()
    }
}

struct Shuffle(u64);

impl Shuffle {
    fn next(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

/// Run every flight at once; each round answers a pseudo-randomly chosen pending await, so an answer
/// arrives after other dispatches have gone on and in no fixed order.
fn fly(flights: &mut [Flight], seed: u64, wrong: &dyn Fn(usize, &IoRequest) -> Option<IoAnswer>) {
    let answers: Vec<Box<dyn Fn(&IoRequest) -> IoAnswer>> = flights
        .iter()
        .map(|f| Box::new(host_answers(&f.setup, &f.store)) as Box<dyn Fn(&IoRequest) -> IoAnswer>)
        .collect();
    let mut shuffle = Shuffle(seed);
    loop {
        let waiting: Vec<usize> = (0..flights.len())
            .filter(|&i| flights[i].begin_next())
            .collect();
        if waiting.is_empty() {
            break;
        }
        let pick = waiting[shuffle.next(waiting.len())];
        let answer = |r: &IoRequest| wrong(pick, r).unwrap_or_else(|| answers[pick](r));
        flights[pick].answer_now(&answer);
    }
    assert!(flights.iter().all(Flight::finished));
}

fn no_wrong_answers(_: usize, _: &IoRequest) -> Option<IoAnswer> {
    None
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn answers_given_later_and_shuffled_produce_the_blocking_trace_for_every_fixture() {
    let fixtures = every_fixture();
    let expected: Vec<(Vec<Json>, Json)> = fixtures.iter().map(|(_, s)| blocking_run(s)).collect();
    for seed in [
        0x9E37_79B9_7F4A_7C15u64,
        0xD1B5_4A32_D192_ED03,
        7,
        0xFFFF_FFFF_FFFF_FFF1,
    ] {
        let mut flights: Vec<Flight> = fixtures.iter().map(|(_, s)| Flight::new(s)).collect();
        fly(&mut flights, seed, &no_wrong_answers);
        for ((name, setup), (flight, (traces, world))) in
            fixtures.iter().zip(flights.iter().zip(expected.iter()))
        {
            assert_eq!(
                &flight.traces, traces,
                "{name}: trace differs (seed {seed:#x})"
            );
            assert_eq!(
                &world_of(setup, &flight.kernel, &flight.store),
                world,
                "{name}: world differs (seed {seed:#x})"
            );
        }
    }
}

#[test]
fn drive_blocking_answers_every_await_at_once_and_matches_the_blocking_run() {
    for (name, setup) in every_fixture() {
        let (expected, expected_world) = blocking_run(&setup);
        let (kernel, store) = build(&setup, true);
        let answer = host_answers(&setup, &store);
        let mut traces = Vec::new();
        for step in &setup.steps {
            let progress = start_step(&kernel, step);
            let outcomes = drive_blocking(&kernel, progress, &answer).unwrap();
            traces.push(trace_of(&outcomes));
        }
        assert_eq!(traces, expected, "{name}");
        assert_eq!(world_of(&setup, &kernel, &store), expected_world, "{name}");
    }
}

#[test]
fn a_cascade_awaits_once_per_io_step_and_every_await_is_a_typed_request() {
    let setup = fetch_success();
    let mut flight = Flight::new(&setup);
    fly(std::slice::from_mut(&mut flight), 3, &no_wrong_answers);
    assert!(
        flight.awaits >= 4,
        "the seed step and the cascaded Loaded steps each await: {}",
        flight.awaits
    );
    assert!(flight
        .log
        .iter()
        .any(|r| matches!(r, IoRequest::ListRows { entity_type } if entity_type == "Item")));
    assert!(flight.log.iter().any(
        |r| matches!(r, IoRequest::ReadRow { entity_type, id } if entity_type == "Item" && id == "singleton")
    ));
}

#[test]
fn a_write_is_asked_for_exactly_once_however_often_the_step_is_replayed() {
    let setup = persist_success();
    let mut flight = Flight::new(&setup);
    fly(std::slice::from_mut(&mut flight), 11, &no_wrong_answers);
    let creates = flight
        .log
        .iter()
        .filter(|r| matches!(r, IoRequest::CreateRow { entity_type, data } if entity_type == "Note" && j(data)["id"] == json!("n1")))
        .count();
    assert_eq!(creates, 1);
    let deletes = flight
        .log
        .iter()
        .filter(|r| matches!(r, IoRequest::DeleteRow { .. }))
        .count();
    assert_eq!(deletes, 1);
}

#[test]
fn control_a_wrong_answer_changes_the_trace() {
    let setup = fetch_success();
    let (expected, _) = blocking_run(&setup);
    let mut flight = Flight::new(&setup);
    let empty_lists = |_: usize, r: &IoRequest| match r {
        IoRequest::ListRows { .. } => Some(IoAnswer::Rows(Ok(Vec::new()))),
        _ => None,
    };
    fly(std::slice::from_mut(&mut flight), 5, &empty_lists);
    assert_ne!(flight.traces, expected);
}

#[test]
fn a_dispatch_with_nothing_to_run_finishes_without_awaiting() {
    let setup = nothing_to_mount();
    let (kernel, _) = build(&setup, true);
    let Progress::Done(outcomes) = start_step(&kernel, &setup.steps[0]) else {
        panic!("a mount batch of unknown traits has no I/O to ask for");
    };
    assert!(outcomes.is_empty());
}

#[test]
fn an_error_answer_takes_the_declared_failure_path() {
    let mut setup = persist_success();
    setup.fail_creates = true;
    setup.steps = vec![dispatch("Notes", "CREATE")];
    let (expected, expected_world) = blocking_run(&setup);
    assert!(
        expected[0].to_string().contains("SAVE_FAILED"),
        "the blocking run takes the failure path: {}",
        expected[0]
    );
    let mut flight = Flight::new(&setup);
    let refuse = |_: usize, r: &IoRequest| match r {
        IoRequest::CreateRow { .. } => Some(IoAnswer::Row(Err("boom".to_string()))),
        _ => None,
    };
    fly(std::slice::from_mut(&mut flight), 9, &refuse);
    assert_eq!(flight.traces, expected);
    assert_eq!(
        world_of(&setup, &flight.kernel, &flight.store),
        expected_world
    );
}

#[test]
fn one_stamp_serves_the_whole_dispatch_across_awaits() {
    let setup = stamped();
    let (kernel, store) = build(&setup, true);
    let answer = host_answers(&setup, &store);
    let mut progress = start_step(&kernel, &setup.steps[0]);
    let outcomes = loop {
        match progress {
            Progress::Done(outcomes) => break outcomes,
            Progress::Await {
                continuation,
                requests,
            } => {
                std::thread::sleep(std::time::Duration::from_millis(8));
                let replies = requests
                    .iter()
                    .map(|p| IoReply {
                        id: p.id,
                        answer: answer(&p.request),
                    })
                    .collect();
                progress = kernel.resume(continuation, replies).unwrap();
            }
        }
    };
    let ats: Vec<Json> = trace_of(&outcomes)
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|o| o["effects"].as_array().unwrap().clone())
        .filter(|e| e["effect"]["type"] == json!("emit"))
        .map(|e| e["effect"]["payload"]["at"].clone())
        .collect();
    assert_eq!(ats.len(), 2, "{ats:?}");
    assert!(ats[0].is_number());
    assert_eq!(ats[0], ats[1]);
}

#[test]
fn resume_refuses_answers_that_do_not_fit_the_awaited_requests() {
    let setup = persist_success();
    let ask = || {
        let (kernel, store) = build(&setup, true);
        let progress = start_step(&kernel, &setup.steps[0]);
        let Progress::Await {
            continuation,
            requests,
        } = progress
        else {
            panic!("the first step reads its row");
        };
        (kernel, store, continuation, requests)
    };

    let (kernel, _, continuation, requests) = ask();
    let err = kernel.resume(continuation, Vec::new()).unwrap_err();
    assert!(
        matches!(err, DispatchError::Resume(ResumeError::Unanswered(id)) if id == requests[0].id),
        "{err:?}"
    );

    let (kernel, store, continuation, requests) = ask();
    let good = host_answers(&setup, &store)(&requests[0].request);
    let err = kernel
        .resume(
            continuation,
            vec![
                IoReply {
                    id: requests[0].id,
                    answer: good.clone(),
                },
                IoReply {
                    id: RequestId::from_raw(9_999),
                    answer: good,
                },
            ],
        )
        .unwrap_err();
    assert!(
        matches!(err, DispatchError::Resume(ResumeError::Unknown(id)) if id == RequestId::from_raw(9_999)),
        "{err:?}"
    );

    let (kernel, _, continuation, requests) = ask();
    let err = kernel
        .resume(
            continuation,
            vec![IoReply {
                id: requests[0].id,
                answer: IoAnswer::Deleted(Ok(())),
            }],
        )
        .unwrap_err();
    assert!(
        matches!(err, DispatchError::Resume(ResumeError::WrongKind(id)) if id == requests[0].id),
        "{err:?}"
    );

    let (kernel, store, continuation, requests) = ask();
    let good = host_answers(&setup, &store)(&requests[0].request);
    let err = kernel
        .resume(
            continuation,
            vec![
                IoReply {
                    id: requests[0].id,
                    answer: good.clone(),
                },
                IoReply {
                    id: requests[0].id,
                    answer: good,
                },
            ],
        )
        .unwrap_err();
    assert!(
        matches!(err, DispatchError::Resume(ResumeError::Duplicate(id)) if id == requests[0].id),
        "{err:?}"
    );
}

#[test]
fn control_the_blocking_entry_points_never_await() {
    for (name, setup) in every_fixture() {
        let (kernel, _) = build(&setup, false);
        for step in &setup.steps {
            blocking_step(&kernel, step);
        }
        let _ = name;
    }
}

// ---------------------------------------------------------------------------------------------
// K5: the server-role embedding (`EmbeddedServer`) over JSON
// ---------------------------------------------------------------------------------------------

use orbital_core::embed::wire::{IoAnswerWire, IoReplyWire, IoRequestWire, ServerProgressWire};
use orbital_core::embed::EmbeddedServer;
use orbital_core::effects::ServiceCaller;
use orbital_core::runtime::resumable::{CallRoutes, CallSite};

fn orbital_of(setup: &Setup) -> String {
    setup.schema["orbitals"][0]["name"].as_str().unwrap().to_string()
}

fn request_of(step: &Step) -> Json {
    match step {
        Step::Dispatch { trait_name, event } => json!({ "event": event, "targetTrait": trait_name }),
        Step::Mount(list) => json!({
            "event": list[0].1,
            "mount": list.iter().map(|(t, e)| json!({ "trait": t, "event": e })).collect::<Vec<_>>(),
        }),
    }
}

/// The response the blocking kernel gives each step: the reference an embedded server must equal.
fn blocking_responses(setup: &Setup) -> Vec<Json> {
    let (kernel, _) = build(setup, false);
    setup
        .steps
        .iter()
        .map(|step| {
            let outcomes = blocking_step(&kernel, step);
            let (t, e) = match step {
                Step::Dispatch { trait_name, event } => (*trait_name, *event),
                Step::Mount(list) => list[0],
            };
            serde_json::to_value(orbital_core::EventResponse::from_process_results(&outcomes, t, e)).unwrap()
        })
        .collect()
}

fn embedded_server(setup: &Setup) -> (EmbeddedServer, Arc<dyn EntitySource>) {
    let store: Arc<dyn EntitySource> = if setup.fail_creates {
        Arc::new(FailingCreates(InMemoryEntityCache::new()))
    } else {
        Arc::new(InMemoryEntityCache::new())
    };
    for (entity, row) in &setup.seed_rows {
        let row: Value = serde_json::from_value(row.clone()).unwrap();
        let id = row.as_object().unwrap()["id"].as_str().unwrap().to_string();
        store.write(entity, &id, &row).unwrap();
    }
    let schema: OrbitalSchema = serde_json::from_value(setup.schema.clone()).unwrap();
    let server = EmbeddedServer::new(schema).unwrap();
    server.mount(setup.mounted.iter().map(|t| t.to_string()).collect());
    (server, store)
}

/// A host that only ever sees and speaks JSON text.
fn json_answers(setup: &Setup, store: &Arc<dyn EntitySource>, requests: &[Json]) -> String {
    let store = store.as_ref();
    let replies: Vec<Json> = requests
        .iter()
        .rev()
        .map(|pending| {
            let id = pending["id"].clone();
            let wire: IoRequestWire = serde_json::from_value(pending["request"].clone()).unwrap();
            let request = IoRequest::from(wire);
            let answer = BlockingHost {
                source: store,
                service: setup.service.as_deref(),
                substrate: setup.substrate.as_deref(),
            }
            .answer(&request);
            json!({ "id": id, "answer": serde_json::to_value(IoAnswerWire::from(&answer)).unwrap() })
        })
        .collect();
    serde_json::to_string(&replies).unwrap()
}

enum Wire {
    Done(Json),
    Await(u64, Vec<Json>),
}

fn wire_of(text: &str) -> Wire {
    let value: Json = serde_json::from_str(text).unwrap();
    match value["status"].as_str().unwrap() {
        "done" => {
            // The blocking responses carry no hop trace and no emitter identity: both are the server role's
            // additions (`tests/server_transitions.rs`, `tests/server_emit_stamp.rs`).
            let mut response = value["response"].clone();
            response.as_object_mut().unwrap().remove("transitions");
            for emitted in response["emittedEvents"].as_array_mut().unwrap() {
                if let Some(source) = emitted["source"].as_object_mut() {
                    for key in ["orbital", "orbitalId", "traitId", "eventId"] {
                        source.remove(key);
                    }
                }
            }
            Wire::Done(response)
        }
        "await" => Wire::Await(
            value["flight"].as_u64().unwrap(),
            value["requests"].as_array().unwrap().clone(),
        ),
        other => panic!("unknown status {other}"),
    }
}

fn run_json_loop(setup: &Setup) -> Vec<Json> {
    let (server, store) = embedded_server(setup);
    let orbital = orbital_of(setup);
    let mut responses = Vec::new();
    for step in &setup.steps {
        let mut wire = wire_of(&server.start_json(&orbital, &request_of(step).to_string()).unwrap());
        loop {
            match wire {
                Wire::Done(response) => {
                    responses.push(response);
                    break;
                }
                Wire::Await(flight, requests) => {
                    let replies = json_answers(setup, &store, &requests);
                    wire = wire_of(&server.resume_json(flight, &replies).unwrap());
                }
            }
        }
    }
    assert_eq!(server.in_flight(), 0);
    responses
}

/// `got` without the states the server echoes for traits the dispatch did not run (`tests/server_held_states.rs`).
fn without_held_echo(mut got: Vec<Json>, expected: &[Json]) -> Vec<Json> {
    for (response, expect) in got.iter_mut().zip(expected) {
        if let (Some(states), Some(kept)) = (response["states"].as_object_mut(), expect["states"].as_object()) {
            states.retain(|trait_name, _| kept.contains_key(trait_name));
        }
    }
    got
}

#[test]
fn the_embedded_server_json_loop_matches_the_blocking_response_for_every_fixture() {
    for (name, setup) in every_fixture() {
        let expected = blocking_responses(&setup);
        assert_eq!(without_held_echo(run_json_loop(&setup), &expected), expected, "{name}");
    }
}

#[test]
fn control_a_wrong_row_answer_changes_the_embedded_response() {
    let setup = fetch_success();
    let (server, _) = embedded_server(&setup);
    let orbital = orbital_of(&setup);
    let mut wire = wire_of(&server.start_json(&orbital, &request_of(&setup.steps[0]).to_string()).unwrap());
    let response = loop {
        match wire {
            Wire::Done(response) => break response,
            Wire::Await(flight, requests) => {
                let replies: Vec<Json> = requests
                    .iter()
                    .map(|p| {
                        let result = match p["request"]["kind"].as_str().unwrap() {
                            "listRows" => json!({ "kind": "rows", "result": { "ok": [] } }),
                            _ => json!({ "kind": "row", "result": { "error": "no such row" } }),
                        };
                        json!({ "id": p["id"], "answer": result })
                    })
                    .collect();
                wire = wire_of(&server.resume_json(flight, &Json::Array(replies).to_string()).unwrap());
            }
        }
    };
    assert_ne!(vec![response], blocking_responses(&setup));
}

#[test]
fn embedded_servers_answered_in_a_shuffled_order_match_the_blocking_responses() {
    let fixtures = every_fixture();
    let runs: Vec<(EmbeddedServer, Arc<dyn EntitySource>)> =
        fixtures.iter().map(|(_, s)| embedded_server(s)).collect();
    let mut at = vec![0usize; fixtures.len()];
    let mut waiting: Vec<Option<(u64, Vec<Json>)>> = vec![None; fixtures.len()];
    let mut got: Vec<Vec<Json>> = vec![Vec::new(); fixtures.len()];
    let mut shuffle = Shuffle(0x9E37_79B9_7F4A_7C15);
    loop {
        for i in 0..fixtures.len() {
            let setup = &fixtures[i].1;
            while waiting[i].is_none() && at[i] < setup.steps.len() {
                let request = request_of(&setup.steps[at[i]]).to_string();
                match wire_of(&runs[i].0.start_json(&orbital_of(setup), &request).unwrap()) {
                    Wire::Done(response) => {
                        got[i].push(response);
                        at[i] += 1;
                    }
                    Wire::Await(flight, requests) => waiting[i] = Some((flight, requests)),
                }
            }
        }
        let live: Vec<usize> = (0..fixtures.len()).filter(|&i| waiting[i].is_some()).collect();
        if live.is_empty() {
            break;
        }
        let pick = live[shuffle.next(live.len())];
        let (flight, requests) = waiting[pick].take().unwrap();
        let replies = json_answers(&fixtures[pick].1, &runs[pick].1, &requests);
        match wire_of(&runs[pick].0.resume_json(flight, &replies).unwrap()) {
            Wire::Done(response) => {
                got[pick].push(response);
                at[pick] += 1;
            }
            Wire::Await(flight, requests) => waiting[pick] = Some((flight, requests)),
        }
    }
    for ((name, setup), got) in fixtures.iter().zip(got) {
        let expected = blocking_responses(setup);
        assert_eq!(without_held_echo(got, &expected), expected, "{name}");
    }
}

/// Answer a started dispatch until it asks for a request of `kind`, and return that request.
fn first_request_of_kind(
    server: &EmbeddedServer,
    setup: &Setup,
    store: &Arc<dyn EntitySource>,
    orbital: &str,
    request: &Json,
    kind: &str,
) -> Json {
    let mut wire = wire_of(&server.start_json(orbital, &request.to_string()).unwrap());
    loop {
        match wire {
            Wire::Done(_) => panic!("the dispatch finished without a {kind} request"),
            Wire::Await(flight, requests) => {
                if let Some(found) = requests.iter().find(|p| p["request"]["kind"] == json!(kind)) {
                    return found["request"].clone();
                }
                let replies = json_answers(setup, store, &requests);
                wire = wire_of(&server.resume_json(flight, &replies).unwrap());
            }
        }
    }
}

#[test]
fn the_dispatchs_user_travels_to_the_service_as_its_caller() {
    let setup = service_call(false);
    let (server, store) = embedded_server(&setup);
    let mut request = request_of(&setup.steps[0]);
    request["user"] = json!({ "id": "u1", "role": "admin" });
    let call = first_request_of_kind(&server, &setup, &store, "Main", &request, "callService");
    assert_eq!(call["caller"], json!({ "principal": "u1", "role": "admin" }));
    let wire: IoRequestWire = serde_json::from_value(call).unwrap();
    assert!(matches!(IoRequest::from(wire), IoRequest::CallService { caller: Some(ServiceCaller { principal, .. }), .. } if principal == "u1"));
}

#[test]
fn control_a_request_without_a_user_has_no_service_caller() {
    let setup = service_call(false);
    let (server, store) = embedded_server(&setup);
    let call = first_request_of_kind(&server, &setup, &store, "Main", &request_of(&setup.steps[0]), "callService");
    assert!(call.get("caller").is_none(), "{call}");
}

#[test]
fn a_target_less_request_runs_the_orbitals_first_trait() {
    let setup = persist_success();
    let (server, store) = embedded_server(&setup);
    let create = first_request_of_kind(&server, &setup, &store, "Main", &json!({ "event": "CREATE" }), "createRow");
    assert_eq!(create["entityType"], json!("Note"));
}

#[test]
fn the_embedded_server_refuses_what_it_cannot_run() {
    let setup = persist_success();
    let (server, _) = embedded_server(&setup);
    assert!(server
        .start_json("Nowhere", &json!({ "event": "CREATE" }).to_string())
        .unwrap_err()
        .contains("Orbital not found"));
    assert!(server.resume_json(99, "[]").unwrap_err().contains("flight 99"));
    assert!(server.start_json("Main", "{").unwrap_err().contains("parse request_json"));
}

#[test]
fn a_flight_resumed_with_a_wrong_kind_of_answer_is_refused_and_forgotten() {
    let setup = persist_success();
    let (server, _) = embedded_server(&setup);
    let Wire::Await(flight, requests) = wire_of(
        &server.start_json("Main", &request_of(&setup.steps[0]).to_string()).unwrap(),
    ) else {
        panic!("awaits")
    };
    let wrong = json!([{ "id": requests[0]["id"], "answer": { "kind": "deleted", "result": { "ok": null } } }]);
    let err = server.resume_json(flight, &wrong.to_string()).unwrap_err();
    assert!(err.contains("not the kind"), "{err}");
    assert_eq!(server.in_flight(), 0);

    let Wire::Await(flight, _) = wire_of(
        &server.start_json("Main", &request_of(&setup.steps[0]).to_string()).unwrap(),
    ) else {
        panic!("awaits")
    };
    assert_eq!(server.in_flight(), 1);
    assert!(server.abandon(flight));
    assert!(!server.abandon(flight));
    assert_eq!(server.in_flight(), 0);
}

#[test]
fn the_wire_round_trips_every_request_and_answer() {
    let requests = vec![
        IoRequest::ReadRow { entity_type: "N".into(), id: "1".into() },
        IoRequest::ListRows { entity_type: "N".into() },
        IoRequest::CreateRow { entity_type: "N".into(), data: Value::Null },
        IoRequest::WriteRow { entity_type: "N".into(), id: "1".into(), data: Value::Bool(true) },
        IoRequest::DeleteRow { entity_type: "N".into(), id: "1".into() },
        IoRequest::CallService {
            service: "s".into(),
            method: "m".into(),
            args: Value::Null,
            key: Some("k".into()),
            caller: Some(ServiceCaller { principal: "p".into(), role: None }),
            site: None,
        },
        IoRequest::CallService {
            service: "s".into(),
            method: "m".into(),
            args: Value::Null,
            key: None,
            caller: None,
            site: Some(CallSite {
                orbital: Some("O".into()),
                trait_name: "T".into(),
                follow_on: true,
                emit: CallRoutes { success: Some("S".into()), failure: None, cancelled: Some("C".into()), on_message: Some("M".into()) },
            }),
        },
        IoRequest::CancelCall { key: "k".into() },
        IoRequest::Substrate { op: "program/eval".into(), args: vec![Value::Null] },
    ];
    for request in requests {
        let text = serde_json::to_string(&IoRequestWire::from(&request)).unwrap();
        let back: IoRequestWire = serde_json::from_str(&text).unwrap();
        assert_eq!(IoRequest::from(back), request, "{text}");
    }
    let answers = vec![
        IoAnswer::Row(Ok(Value::Null)),
        IoAnswer::Row(Err("e".into())),
        IoAnswer::Rows(Ok(vec![Value::Bool(false)])),
        IoAnswer::Deleted(Ok(())),
        IoAnswer::Cancelled(Err("e".into())),
        IoAnswer::Service(Ok(Value::Null)),
        IoAnswer::ServiceCancelled,
        IoAnswer::ServiceDeferred,
        IoAnswer::Substrate(Err(SubstrateFailure { message: "m".into(), errors: vec![Value::Null] })),
    ];
    for answer in answers {
        let text = serde_json::to_string(&IoAnswerWire::from(&answer)).unwrap();
        let back: IoAnswerWire = serde_json::from_str(&text).unwrap();
        assert_eq!(IoAnswer::from(back), answer, "{text}");
    }
    let _ = (IoReplyWire { id: 0, answer: IoAnswerWire::from(&IoAnswer::Deleted(Ok(()))) }, ServerProgressWire::Done { response: Default::default() });
}
