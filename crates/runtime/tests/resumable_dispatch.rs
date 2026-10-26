// K4: a dispatch that stops at its I/O points (`Progress::Await`) and is resumed with the host's
// answers, in any order and later, produces the same trace as the blocking run. The fixtures are
// the correctness scenarios of persist_success_emit, client_fetch_success_emit, same_trait_cascade,
// dispatch_roots, cross_trait_dispatch and completion_events.

use orbital_core_runtime::effects::{
    LifecycleEvent, MountSeed, ServiceHandler, SubstrateFailure, SubstrateHandler,
};
use orbital_core_base::value::ValueMap;
use orbital_core_runtime::resumable::{drive_blocking, BlockingHost, DispatchContinuation, DispatchError, DispatchOutcomes, HostAnswered, IoAnswer, IoReply, IoRequest, PendingRequest, Progress, RequestId, ResumeError};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
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
    fn call(
        &self,
        _s: &str,
        _m: &str,
        _a: &Value,
        _k: Option<&str>,
        _c: Option<&orbital_core_runtime::effects::ServiceCaller>,
    ) -> Result<Value, String> {
        if self.fail {
            return Err("rate limited".to_string());
        }
        let mut out = ValueMap::new();
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
    let mut setup = completion(json!(["call-service", "test-svc", "run", { "texts": ["a"] }]));
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
    fn read(&self, t: &str, id: &str) -> Result<Arc<Value>, String> {
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
    fn list(&self, t: &str) -> Result<Vec<Arc<Value>>, String> {
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
                    "entity": j(&r.entity_data.to_value()),
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
        let mut list: Vec<Json> = store.list(entity).unwrap().iter().map(|r| j(r)).collect();
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
    type HostAnswer = Box<dyn Fn(&IoRequest) -> IoAnswer>;
    let answers: Vec<HostAnswer> = flights
        .iter()
        .map(|f| Box::new(host_answers(&f.setup, &f.store)) as HostAnswer)
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
fn a_cascade_awaits_once_per_row_and_list_and_every_await_is_a_typed_request() {
    let setup = fetch_success();
    let mut flight = Flight::new(&setup);
    fly(std::slice::from_mut(&mut flight), 3, &no_wrong_answers);
    assert!(
        flight.awaits >= 2,
        "the frame row is asked once for the dispatch and the list once: {}",
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

use orbital_core_runtime::effects::ServiceCaller;
use orbital_core_runtime::embed::wire::{IoAnswerWire, IoReplyWire, IoRequestWire, ServerProgressWire};
use orbital_core_runtime::embed::EmbeddedServer;
use orbital_core_runtime::resumable::{CallRoutes, CallSite};

fn orbital_of(setup: &Setup) -> String {
    setup.schema["orbitals"][0]["name"]
        .as_str()
        .unwrap()
        .to_string()
}

fn request_of(step: &Step) -> Json {
    match step {
        Step::Dispatch { trait_name, event } => {
            json!({ "event": event, "targetTrait": trait_name })
        }
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
            // A mount carries whole rows (EXEC-DIST-7); every other dispatch carries only what it wrote.
            let response = match step {
                Step::Mount(_) => orbital_core_runtime::effects::response::EventResponse::from_process_results_mount(
                    &outcomes, t, e,
                ),
                Step::Dispatch { .. } => orbital_core_runtime::effects::response::EventResponse::from_process_results(
                    &outcomes, t, e,
                ),
            };
            serde_json::to_value(response).unwrap()
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
        let mut wire = wire_of(
            &server
                .start_json(&orbital, &request_of(step).to_string())
                .unwrap(),
        );
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
        if let (Some(states), Some(kept)) = (
            response["states"].as_object_mut(),
            expect["states"].as_object(),
        ) {
            states.retain(|trait_name, _| kept.contains_key(trait_name));
        }
    }
    got
}

#[test]
fn the_embedded_server_json_loop_matches_the_blocking_response_for_every_fixture() {
    for (name, setup) in every_fixture() {
        let expected = blocking_responses(&setup);
        assert_eq!(
            without_held_echo(run_json_loop(&setup), &expected),
            expected,
            "{name}"
        );
    }
}

#[test]
fn control_a_wrong_row_answer_changes_the_embedded_response() {
    let setup = fetch_success();
    let (server, _) = embedded_server(&setup);
    let orbital = orbital_of(&setup);
    let mut wire = wire_of(
        &server
            .start_json(&orbital, &request_of(&setup.steps[0]).to_string())
            .unwrap(),
    );
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
                wire = wire_of(
                    &server
                        .resume_json(flight, &Json::Array(replies).to_string())
                        .unwrap(),
                );
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
        let live: Vec<usize> = (0..fixtures.len())
            .filter(|&i| waiting[i].is_some())
            .collect();
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
                if let Some(found) = requests
                    .iter()
                    .find(|p| p["request"]["kind"] == json!(kind))
                {
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
    assert_eq!(
        call["caller"],
        json!({ "principal": "u1", "role": "admin" })
    );
    let wire: IoRequestWire = serde_json::from_value(call).unwrap();
    assert!(
        matches!(IoRequest::from(wire), IoRequest::CallService { caller: Some(ServiceCaller { principal, .. }), .. } if principal == "u1")
    );
}

#[test]
fn control_a_request_without_a_user_has_no_service_caller() {
    let setup = service_call(false);
    let (server, store) = embedded_server(&setup);
    let call = first_request_of_kind(
        &server,
        &setup,
        &store,
        "Main",
        &request_of(&setup.steps[0]),
        "callService",
    );
    assert!(call.get("caller").is_none(), "{call}");
}

#[test]
fn a_target_less_request_runs_the_orbitals_first_trait() {
    let setup = persist_success();
    let (server, store) = embedded_server(&setup);
    let create = first_request_of_kind(
        &server,
        &setup,
        &store,
        "Main",
        &json!({ "event": "CREATE" }),
        "createRow",
    );
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
    assert!(server
        .resume_json(99, "[]")
        .unwrap_err()
        .contains("flight 99"));
    assert!(server
        .start_json("Main", "{")
        .unwrap_err()
        .contains("parse request_json"));
}

#[test]
fn a_flight_resumed_with_a_wrong_kind_of_answer_is_refused_and_forgotten() {
    let setup = persist_success();
    let (server, _) = embedded_server(&setup);
    let Wire::Await(flight, requests) = wire_of(
        &server
            .start_json("Main", &request_of(&setup.steps[0]).to_string())
            .unwrap(),
    ) else {
        panic!("awaits")
    };
    let wrong = json!([{ "id": requests[0]["id"], "answer": { "kind": "deleted", "result": { "ok": null } } }]);
    let err = server.resume_json(flight, &wrong.to_string()).unwrap_err();
    assert!(err.contains("not the kind"), "{err}");
    assert_eq!(server.in_flight(), 0);

    let Wire::Await(flight, _) = wire_of(
        &server
            .start_json("Main", &request_of(&setup.steps[0]).to_string())
            .unwrap(),
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
        IoRequest::ReadRow {
            entity_type: "N".into(),
            id: "1".into(),
        },
        IoRequest::ListRows {
            entity_type: "N".into(),
        },
        IoRequest::CreateRow {
            entity_type: "N".into(),
            data: Value::Null,
        },
        IoRequest::WriteRow {
            entity_type: "N".into(),
            id: "1".into(),
            data: Value::Bool(true),
        },
        IoRequest::DeleteRow {
            entity_type: "N".into(),
            id: "1".into(),
        },
        IoRequest::CallService {
            service: "s".into(),
            method: "m".into(),
            args: Value::Null,
            key: Some("k".into()),
            caller: Some(ServiceCaller {
                principal: "p".into(),
                role: None,
            }),
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
                entity_id: "singleton".to_string(),
                emit: CallRoutes {
                    success: Some("S".into()),
                    failure: None,
                    cancelled: Some("C".into()),
                    on_message: Some("M".into()),
                },
            }),
        },
        IoRequest::CancelCall { key: "k".into() },
        IoRequest::Substrate {
            op: "program/eval".into(),
            args: vec![Value::Null],
            key: None,
            site: None,
        },
        IoRequest::Substrate {
            op: "program/dispatch".into(),
            args: vec![Value::Null],
            key: Some("k".into()),
            site: Some(CallSite {
                orbital: Some("Desk".into()),
                trait_name: "Agent".into(),
                follow_on: false,
                entity_id: "singleton".to_string(),
                emit: CallRoutes {
                    success: Some("DONE".into()),
                    failure: Some("FAILED".into()),
                    cancelled: None,
                    on_message: None,
                },
            }),
        },
        IoRequest::Deliver {
            envelope: sample_envelope(),
            key: Some("k".into()),
            site: CallSite {
                orbital: Some("Src".into()),
                trait_name: "Sender".into(),
                follow_on: false,
                entity_id: "singleton".to_string(),
                emit: CallRoutes::default(),
            },
        },
    ];
    for request in requests {
        let text = serde_json::to_string(&IoRequestWire::from(&request)).unwrap();
        let back: IoRequestWire = serde_json::from_str(&text).unwrap();
        assert_eq!(IoRequest::from(back), request, "{text}");
    }
    let answers = vec![
        IoAnswer::Row(Ok(Value::Null)),
        IoAnswer::Row(Err("e".into())),
        IoAnswer::Rows(Ok(vec![Arc::new(Value::Bool(false))])),
        IoAnswer::Deleted(Ok(())),
        IoAnswer::Cancelled(Err("e".into())),
        IoAnswer::Service(Ok(Value::Null)),
        IoAnswer::ServiceCancelled,
        IoAnswer::ServiceDeferred,
        IoAnswer::Substrate(Err(SubstrateFailure {
            message: "m".into(),
            errors: vec![Value::Null],
        })),
        IoAnswer::Deliver(Ok(Accepted)),
        IoAnswer::Deliver(Err(DeliverRefusal {
            kind: DeliverRefusalKind::MailboxFull,
            id: "i".into(),
        })),
    ];
    for answer in answers {
        let text = serde_json::to_string(&IoAnswerWire::from(&answer)).unwrap();
        let back: IoAnswerWire = serde_json::from_str(&text).unwrap();
        assert_eq!(IoAnswer::from(back), answer, "{text}");
    }
    let _ = (
        IoReplyWire {
            id: 0,
            answer: IoAnswerWire::from(&IoAnswer::Deleted(Ok(()))),
        },
        ServerProgressWire::Done {
            response: Default::default(),
        },
    );
}

// ---------------------------------------------------------------------------------------------
// Kernel-mode overlap: keyed `call-service` asks of one step are outstanding together (EXEC-QUEUE-8)
// ---------------------------------------------------------------------------------------------

struct EchoArgs;

impl ServiceHandler for EchoArgs {
    fn call(
        &self,
        _s: &str,
        _m: &str,
        args: &Value,
        _k: Option<&str>,
        _c: Option<&orbital_core_runtime::effects::ServiceCaller>,
    ) -> Result<Value, String> {
        Ok(args.clone())
    }
    fn cancel(&self, _key: &str) -> Result<(), String> {
        Ok(())
    }
}

fn call_effect(key: &str, n: u32, event: &str) -> Json {
    json!(["call-service", "svc", "work", { "n": n }, {
        "key": key,
        "emit": { "success": format!("{event}_OK"), "failure": format!("{event}_BAD") }
    }])
}

fn fan_out(effects: Vec<Json>, events: &[&str]) -> Setup {
    let mut transitions =
        vec![json!({ "from": "idle", "to": "idle", "event": "GO", "effects": effects })];
    for event in events {
        for outcome in ["OK", "BAD"] {
            transitions.push(json!({
                "from": "idle", "to": "idle", "event": format!("{event}_{outcome}"),
                "effects": [["set", format!("@entity.{}", event.to_lowercase()), "@payload"]]
            }));
        }
    }
    let schema = json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Run", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Fan",
                "linkedEntity": "Run",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": transitions }
            }]
        }]
    });
    let mut setup = Setup::new(schema, &["Run"], vec![dispatch("Fan", "GO")]);
    setup.service = Some(Arc::new(EchoArgs));
    setup
}

fn three_overlapped() -> Setup {
    fan_out(
        vec![
            call_effect("ka", 1, "KA"),
            call_effect("kb", 2, "KB"),
            call_effect("kc", 3, "KC"),
        ],
        &["KA", "KB", "KC"],
    )
}

fn is_call(request: &IoRequest) -> bool {
    matches!(request, IoRequest::CallService { .. })
}

fn key_of(request: &IoRequest) -> Option<String> {
    match request {
        IoRequest::CallService { key, .. } => key.clone(),
        IoRequest::CancelCall { key } => Some(format!("cancel:{key}")),
        _ => None,
    }
}

struct Flown {
    trace: Json,
    world: Json,
    batches: Vec<Vec<IoRequest>>,
}

impl Flown {
    fn call_batch(&self) -> &Vec<IoRequest> {
        self.batches
            .iter()
            .find(|batch| batch.iter().any(is_call))
            .expect("the dispatch awaited its calls")
    }
}

/// Run the one step to its end, answering every await at once; a batch of calls is answered in `order`.
fn fly_ordered(setup: &Setup, order: &[usize]) -> Flown {
    let (kernel, store) = build(setup, true);
    let answer = host_answers(setup, &store);
    let mut batches = Vec::new();
    let mut progress = start_step(&kernel, &setup.steps[0]);
    loop {
        match progress {
            Progress::Done(outcomes) => {
                return Flown {
                    trace: trace_of(&outcomes),
                    world: world_of(setup, &kernel, &store),
                    batches,
                }
            }
            Progress::Await {
                continuation,
                requests,
            } => {
                batches.push(requests.iter().map(|p| p.request.clone()).collect());
                let replies: Vec<IoReply> = requests
                    .iter()
                    .map(|p| IoReply {
                        id: p.id,
                        answer: answer(&p.request),
                    })
                    .collect();
                let replies = if order.len() == replies.len()
                    && requests.iter().all(|p| is_call(&p.request))
                {
                    order.iter().map(|&i| replies[i].clone()).collect()
                } else {
                    replies
                };
                progress = kernel.resume(continuation, replies).unwrap();
            }
        }
    }
}

type Calls = (
    RuntimeKernel,
    Arc<dyn EntitySource>,
    DispatchContinuation,
    Vec<PendingRequest>,
);

/// Answer the awaits that carry no call, and stop at the first await that does.
fn until_calls(setup: &Setup) -> Calls {
    let (kernel, store) = build(setup, true);
    let answer = host_answers(setup, &store);
    let mut progress = start_step(&kernel, &setup.steps[0]);
    loop {
        match progress {
            Progress::Done(_) => panic!("the dispatch never asked for its calls"),
            Progress::Await {
                continuation,
                requests,
            } => {
                if requests.iter().any(|p| is_call(&p.request)) {
                    return (kernel, store, continuation, requests);
                }
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
    }
}

#[test]
fn keyed_calls_of_one_step_await_together_in_issue_order_with_consecutive_ids() {
    let setup = three_overlapped();
    let (_, _, _, requests) = until_calls(&setup);
    let keys: Vec<Option<String>> = requests.iter().map(|p| key_of(&p.request)).collect();
    assert_eq!(
        keys,
        vec![
            Some("ka".to_string()),
            Some("kb".to_string()),
            Some("kc".to_string())
        ]
    );
    let ids: Vec<u64> = requests.iter().map(|p| p.id.raw()).collect();
    assert_eq!(ids, vec![ids[0], ids[0] + 1, ids[0] + 2]);
}

#[test]
fn overlapped_answers_in_every_order_give_the_blocking_trace_and_one_await_for_all_calls() {
    let setup = three_overlapped();
    let (expected, expected_world) = blocking_run(&setup);
    let text = expected[0].to_string();
    assert!(
        text.contains("KA_OK") && text.contains("KB_OK") && text.contains("KC_OK"),
        "{text}"
    );
    let in_order = fly_ordered(&setup, &[0, 1, 2]);
    assert_eq!(in_order.trace, expected[0]);
    assert_eq!(in_order.world, expected_world);
    let call_awaits = in_order
        .batches
        .iter()
        .filter(|b| b.iter().any(is_call))
        .count();
    assert_eq!(call_awaits, 1, "{:?}", in_order.batches);
    assert_eq!(in_order.call_batch().len(), 3);
    for order in [[0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        let shuffled = fly_ordered(&setup, &order);
        assert_eq!(shuffled.trace, in_order.trace, "{order:?}");
        assert_eq!(shuffled.world, in_order.world, "{order:?}");
    }
}

#[test]
fn control_an_answer_given_to_the_wrong_request_changes_the_overlapped_trace() {
    let setup = three_overlapped();
    let (expected, _) = blocking_run(&setup);
    let (kernel, store, continuation, requests) = until_calls(&setup);
    let answer = host_answers(&setup, &store);
    let replies = vec![
        IoReply {
            id: requests[0].id,
            answer: answer(&requests[1].request),
        },
        IoReply {
            id: requests[1].id,
            answer: answer(&requests[0].request),
        },
        IoReply {
            id: requests[2].id,
            answer: answer(&requests[2].request),
        },
    ];
    let mut progress = kernel.resume(continuation, replies).unwrap();
    let outcomes = loop {
        match progress {
            Progress::Done(outcomes) => break outcomes,
            Progress::Await {
                continuation,
                requests,
            } => {
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
    assert_ne!(trace_of(&outcomes), expected[0]);
}

#[test]
fn overlapped_requests_refuse_an_unanswered_unknown_or_duplicate_reply() {
    let setup = three_overlapped();
    let reply = |p: &PendingRequest| IoReply {
        id: p.id,
        answer: IoAnswer::Service(Ok(Value::Null)),
    };

    let (kernel, _, continuation, requests) = until_calls(&setup);
    let err = kernel
        .resume(continuation, vec![reply(&requests[0]), reply(&requests[2])])
        .unwrap_err();
    assert!(
        matches!(err, DispatchError::Resume(ResumeError::Unanswered(id)) if id == requests[1].id),
        "{err:?}"
    );

    let (kernel, _, continuation, requests) = until_calls(&setup);
    let mut replies: Vec<IoReply> = requests.iter().map(reply).collect();
    replies.push(IoReply {
        id: RequestId::from_raw(9_999),
        answer: IoAnswer::Service(Ok(Value::Null)),
    });
    let err = kernel.resume(continuation, replies).unwrap_err();
    assert!(
        matches!(err, DispatchError::Resume(ResumeError::Unknown(id)) if id == RequestId::from_raw(9_999)),
        "{err:?}"
    );

    let (kernel, _, continuation, requests) = until_calls(&setup);
    let mut replies: Vec<IoReply> = requests.iter().map(reply).collect();
    replies.push(reply(&requests[1]));
    let err = kernel.resume(continuation, replies).unwrap_err();
    assert!(
        matches!(err, DispatchError::Resume(ResumeError::Duplicate(id)) if id == requests[1].id),
        "{err:?}"
    );

    let (kernel, _, continuation, requests) = until_calls(&setup);
    let replies: Vec<IoReply> = requests.iter().rev().map(reply).collect();
    assert!(
        kernel.resume(continuation, replies).is_ok(),
        "control: all answered, in reverse"
    );
}

#[test]
fn a_key_used_twice_in_one_step_is_a_visible_failure_of_the_second_call() {
    let setup = fan_out(
        vec![call_effect("same", 1, "P1"), call_effect("same", 2, "P2")],
        &["P1", "P2"],
    );
    let flown = fly_ordered(&setup, &[]);
    assert_eq!(flown.call_batch().len(), 1, "{:?}", flown.batches);
    let text = flown.trace.to_string();
    assert!(
        text.contains("already used by another call in this step"),
        "{text}"
    );
    assert!(text.contains("P1_OK") && text.contains("P2_BAD"), "{text}");
    let control = fly_ordered(&three_overlapped(), &[]);
    assert!(!control
        .trace
        .to_string()
        .contains("already used by another call"));
}

#[test]
fn a_key_used_twice_in_one_step_fails_the_second_call_on_the_blocking_path_too() {
    let setup = fan_out(
        vec![call_effect("same", 1, "P1"), call_effect("same", 2, "P2")],
        &["P1", "P2"],
    );
    let text = blocking_run(&setup).0[0].to_string();
    assert!(
        text.contains("already used by another call in this step"),
        "{text}"
    );
    assert!(text.contains("P1_OK") && text.contains("P2_BAD"), "{text}");
    let control = blocking_run(&three_overlapped()).0[0].to_string();
    assert!(
        !control.contains("already used by another call"),
        "control: distinct keys"
    );
}

#[test]
fn cancelling_a_pending_call_of_the_step_removes_only_that_request_and_asks_nothing() {
    let setup = fan_out(
        vec![
            call_effect("ka", 1, "KA"),
            call_effect("kb", 2, "KB"),
            json!(["cancel-call", "ka"]),
        ],
        &["KA", "KB"],
    );
    let flown = fly_ordered(&setup, &[]);
    let keys: Vec<Option<String>> = flown.call_batch().iter().map(key_of).collect();
    assert_eq!(keys, vec![Some("kb".to_string())]);
    assert!(
        flown
            .batches
            .iter()
            .flatten()
            .all(|r| !matches!(r, IoRequest::CancelCall { .. })),
        "{:?}",
        flown.batches
    );
    let text = flown.trace.to_string();
    assert!(
        text.contains("\"event\":\"KB_OK\"") && !text.contains("\"event\":\"KA_OK\""),
        "{text}"
    );
}

#[test]
fn control_cancelling_a_call_the_step_did_not_issue_is_an_ask_of_its_own_after_the_calls() {
    let setup = fan_out(
        vec![
            call_effect("ka", 1, "KA"),
            call_effect("kb", 2, "KB"),
            json!(["cancel-call", "elsewhere"]),
        ],
        &["KA", "KB"],
    );
    let flown = fly_ordered(&setup, &[]);
    let keys: Vec<Option<String>> = flown.call_batch().iter().map(key_of).collect();
    assert_eq!(keys, vec![Some("ka".to_string()), Some("kb".to_string())]);
    let later: Vec<Option<String>> = flown
        .batches
        .iter()
        .skip_while(|batch| !batch.iter().any(is_call))
        .skip(1)
        .flatten()
        .filter_map(|r| matches!(r, IoRequest::CancelCall { .. }).then(|| key_of(r)))
        .collect();
    assert_eq!(later, vec![Some("cancel:elsewhere".to_string())]);
    let text = flown.trace.to_string();
    assert!(text.contains("KA_OK") && text.contains("KB_OK"), "{text}");
}

#[test]
fn cancelling_a_list_withdraws_the_pending_calls_it_names_and_asks_for_the_rest() {
    let setup = fan_out(
        vec![
            call_effect("ka", 1, "KA"),
            call_effect("kb", 2, "KB"),
            json!(["cancel-call", ["list", "ka", "elsewhere"]]),
        ],
        &["KA", "KB"],
    );
    let flown = fly_ordered(&setup, &[]);
    let keys: Vec<Option<String>> = flown.call_batch().iter().map(key_of).collect();
    assert_eq!(keys, vec![Some("kb".to_string())], "ka was withdrawn before it was asked");
    let later: Vec<Option<String>> = flown
        .batches
        .iter()
        .skip_while(|batch| !batch.iter().any(is_call))
        .skip(1)
        .flatten()
        .filter_map(|r| matches!(r, IoRequest::CancelCall { .. }).then(|| key_of(r)))
        .collect();
    assert_eq!(later, vec![Some("cancel:elsewhere".to_string())]);
    let text = flown.trace.to_string();
    assert!(text.contains("\"event\":\"KB_OK\"") && !text.contains("\"event\":\"KA_OK\""), "{text}");
}

#[test]
fn control_a_list_naming_no_pending_call_asks_each_key_in_order() {
    let setup = fan_out(
        vec![
            call_effect("ka", 1, "KA"),
            json!(["cancel-call", ["list", "x", "y"]]),
        ],
        &["KA"],
    );
    let flown = fly_ordered(&setup, &[]);
    let asked: Vec<Option<String>> = flown
        .batches
        .iter()
        .flatten()
        .filter_map(|r| matches!(r, IoRequest::CancelCall { .. }).then(|| key_of(r)))
        .collect();
    assert_eq!(asked, vec![Some("cancel:x".to_string()), Some("cancel:y".to_string())]);
}

#[test]
fn control_a_read_after_a_keyed_call_is_withheld_until_the_call_is_answered() {
    let setup = fan_out(
        vec![call_effect("ka", 1, "KA"), json!(["fetch", "Run"])],
        &["KA"],
    );
    let flown = fly_ordered(&setup, &[]);
    let call_at = flown
        .batches
        .iter()
        .position(|b| b.iter().any(is_call))
        .expect("the call was awaited");
    assert_eq!(flown.batches[call_at].len(), 1, "{:?}", flown.batches);
    assert!(
        flown.batches[call_at + 1..]
            .iter()
            .flatten()
            .any(|r| matches!(r, IoRequest::ListRows { entity_type } if entity_type == "Run")),
        "the read is asked after the call: {:?}",
        flown.batches
    );
}

// ---------------------------------------------------------------------------------------------
// Cross-unit delivery: an emit whose declared listener is placed in another unit is an ask (EXEC-QUEUE-9)
// ---------------------------------------------------------------------------------------------

use orbital_core_base::envelope::{envelope_id, Accepted, DeliverRefusal, DeliverRefusalKind, Envelope, EnvelopeVerdict, InstanceRef};
use orbital_core_runtime::kernel::dispatch_run::InboundStart;

fn sample_envelope() -> Envelope {
    Envelope {
        id: "i".into(),
        from: InstanceRef { instance: "api".into(), orbital: "Src".into(), trait_name: Some("Sender".into()) },
        to: InstanceRef { instance: "web".into(), orbital: "Dst".into(), trait_name: None },
        event: "GOT".into(),
        payload: Value::Bool(true),
        causal_id: "c".into(),
        hop: 3,
    }
}

fn placed_program(deployment: Json, with_watch: bool) -> Json {
    let mut src_traits = vec![json!({
        "name": "Sender",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "PING", { "n": 1 }]] }]
        }
    })];
    if with_watch {
        src_traits.push(json!({
            "name": "Watch",
            "listens": [{ "event": "DELIVERY_REJECTED", "triggers": "REFUSED", "source": { "kind": "trait", "trait": "Sender" } }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": "refused" }],
                "transitions": [{ "from": "idle", "to": "refused", "event": "REFUSED" }]
            }
        }));
    }
    json!({
        "name": "placed",
        "deployment": deployment,
        "orbitals": [
            { "name": "Src", "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] }, "traits": src_traits },
            { "name": "Dst", "entity": { "name": "Thing", "fields": [{ "name": "id", "type": "string" }] }, "traits": [{
                "name": "Receiver",
                "listens": [{ "event": "PING", "triggers": "GOT", "source": { "kind": "orbital", "orbital": "Src", "trait": "Sender" } }],
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "got" }, { "name": "twice" }],
                    "transitions": [
                        { "from": "idle", "to": "got", "event": "GOT" },
                        { "from": "got", "to": "twice", "event": "GOT" }
                    ]
                }
            }] }
        ]
    })
}

fn api_web() -> Json {
    json!({ "api": ["Src"], "web": ["Dst"] })
}

fn placed_kernel(program: &Json, unit: Option<&str>) -> (RuntimeKernel, Arc<dyn EntitySource>) {
    let store: Arc<dyn EntitySource> = Arc::new(InMemoryEntityCache::new());
    let config = KernelConfig { unit: unit.map(str::to_string), ..KernelConfig::default() };
    let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), config);
    kernel.register_schema(serde_json::from_value::<OrbitalSchema>(program.clone()).unwrap()).unwrap();
    (kernel, store)
}

/// Answer every await of a dispatch: a `Deliver` by `decide`, anything else from the store. Returns the outcomes and
/// the envelopes the kernel asked the host to deliver, in ask order.
fn fly_placed(
    kernel: &RuntimeKernel,
    store: &Arc<dyn EntitySource>,
    mut progress: Progress<DispatchOutcomes>,
    decide: &dyn Fn(&Envelope) -> Result<Accepted, DeliverRefusal>,
) -> (DispatchOutcomes, Vec<Envelope>) {
    let mut asked = Vec::new();
    loop {
        match progress {
            Progress::Done(outcomes) => return (outcomes, asked),
            Progress::Await { continuation, requests } => {
                let replies = requests
                    .iter()
                    .map(|p| {
                        let answer = match &p.request {
                            IoRequest::Deliver { envelope, .. } => {
                                asked.push(envelope.clone());
                                IoAnswer::Deliver(decide(envelope))
                            }
                            other => BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(other),
                        };
                        IoReply { id: p.id, answer }
                    })
                    .collect();
                progress = kernel.resume(continuation, replies).unwrap();
            }
        }
    }
}

fn accept_all(_: &Envelope) -> Result<Accepted, DeliverRefusal> {
    Ok(Accepted)
}

fn go(kernel: &RuntimeKernel, store: &Arc<dyn EntitySource>, decide: &dyn Fn(&Envelope) -> Result<Accepted, DeliverRefusal>) -> (DispatchOutcomes, Vec<Envelope>) {
    let progress = kernel.start_dispatch("Sender", None, "GO", Value::Null, None, None).unwrap();
    fly_placed(kernel, store, progress, decide)
}

fn ran(outcomes: &DispatchOutcomes, trait_name: &str) -> bool {
    outcomes.iter().any(|(name, result)| name == trait_name && result.executed)
}

#[test]
fn a_cross_unit_emit_is_one_deliver_ask_with_its_hop_causal_id_and_a_replay_stable_id() {
    let program = placed_program(api_web(), false);
    let (kernel, store) = placed_kernel(&program, Some("api"));
    let (outcomes, asked) = go(&kernel, &store, &accept_all);
    assert_eq!(asked.len(), 1, "{asked:?}");
    let envelope = &asked[0];
    assert_eq!(envelope.hop, 1);
    assert_eq!(envelope.causal_id, "api#1");
    assert_eq!(envelope.id, envelope_id("api", "api#1", 1, 0));
    assert_eq!(envelope.from, InstanceRef { instance: "api".into(), orbital: "Src".into(), trait_name: Some("Sender".into()) });
    assert_eq!(envelope.to, InstanceRef { instance: "web".into(), orbital: "Dst".into(), trait_name: Some("Receiver".into()) });
    assert_eq!(envelope.event, "GOT");
    assert_eq!(j(&envelope.payload), json!({ "n": 1.0 }));
    assert!(ran(&outcomes, "Sender") && !ran(&outcomes, "Receiver"), "the remote listener is not on the local worklist");

    let (replayed_kernel, replayed_store) = placed_kernel(&program, Some("api"));
    let (_, replayed) = go(&replayed_kernel, &replayed_store, &accept_all);
    assert_eq!(replayed, asked, "the same dispatch on a fresh kernel derives the same envelope");

    let (_, second) = go(&kernel, &store, &accept_all);
    assert_eq!(second[0].causal_id, "api#2");
    assert_ne!(second[0].id, envelope.id, "the next dispatch has its own id");
}

#[test]
fn control_same_unit_no_deployment_and_no_unit_keep_the_emit_on_the_local_worklist() {
    let same_unit = json!({ "api": ["Src", "Dst"] });
    for (what, deployment, unit) in [
        ("same unit", same_unit, Some("api")),
        ("no deployment", Json::Null, Some("api")),
        ("no unit", api_web(), None),
    ] {
        let (kernel, store) = placed_kernel(&placed_program(deployment, false), unit);
        let (outcomes, asked) = go(&kernel, &store, &accept_all);
        assert!(asked.is_empty(), "{what}: {asked:?}");
        assert!(ran(&outcomes, "Receiver"), "{what}: the listener ran locally");
    }
}

#[test]
fn the_hop_cap_allows_sixteen_and_refuses_the_seventeenth() {
    let program = placed_program(api_web(), true);
    let inbound = |hop: u32| Envelope {
        id: format!("in-{hop}"),
        from: InstanceRef { instance: "web".into(), orbital: "Dst".into(), trait_name: None },
        to: InstanceRef { instance: "api".into(), orbital: "Src".into(), trait_name: Some("Sender".into()) },
        event: "GO".into(),
        payload: Value::Null,
        causal_id: "root".into(),
        hop,
    };
    let run = |hop: u32| {
        let (kernel, store) = placed_kernel(&program, Some("api"));
        match kernel.start_dispatch_envelope(inbound(hop)) {
            InboundStart::Admitted(progress) => fly_placed(&kernel, &store, (*progress).unwrap(), &accept_all),
            InboundStart::Declined(verdict) => panic!("declined: {verdict:?}"),
        }
    };

    let (_, asked) = run(15);
    assert_eq!(asked.len(), 1);
    assert_eq!((asked[0].hop, asked[0].causal_id.as_str()), (16, "root"), "hop 16 is allowed and keeps the causal id");

    let (outcomes, asked) = run(16);
    assert!(asked.is_empty(), "hop 17 is refused before any ask: {asked:?}");
    let (_, sender) = outcomes.iter().find(|(name, _)| name == "Sender").unwrap();
    let refused: Vec<_> = sender.effect_results.iter().flat_map(|r| &r.refused).collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].reason, DeliverRefusalKind::HopCap);
    assert_eq!(refused[0].causal_id, "root");
    assert!(ran(&outcomes, "Watch"), "the refusal reaches the emitter's listener");

    let (kernel, _) = placed_kernel(&program, Some("api"));
    assert_eq!(
        kernel.dispatch_envelope(inbound(17)),
        EnvelopeVerdict::Rejected(DeliverRefusal { kind: DeliverRefusalKind::HopCap, id: "in-17".into() })
    );
    assert_eq!(kernel.dispatch_envelope(inbound(16)), EnvelopeVerdict::Accepted, "control: hop 16 is admitted inbound");
}

fn arrival(id: &str, instance: &str, orbital: &str, trait_name: Option<&str>) -> Envelope {
    Envelope {
        id: id.into(),
        from: InstanceRef { instance: "api".into(), orbital: "Src".into(), trait_name: Some("Sender".into()) },
        to: InstanceRef { instance: instance.into(), orbital: orbital.into(), trait_name: trait_name.map(str::to_string) },
        event: "GOT".into(),
        payload: Value::Null,
        causal_id: "c".into(),
        hop: 1,
    }
}

#[test]
fn an_inbound_envelope_is_accepted_once_then_duplicate_and_unknown_targets_are_rejected() {
    let (kernel, _) = placed_kernel(&placed_program(api_web(), false), Some("web"));
    let state = || kernel.snapshot_trait("Receiver", None).state;

    assert_eq!(kernel.dispatch_envelope(arrival("a", "web", "Dst", Some("Receiver"))), EnvelopeVerdict::Accepted);
    assert_eq!(state(), "got");
    assert_eq!(kernel.dispatch_envelope(arrival("a", "web", "Dst", Some("Receiver"))), EnvelopeVerdict::Duplicate);
    assert_eq!(state(), "got", "a duplicate is not dispatched");
    assert_eq!(kernel.dispatch_envelope(arrival("b", "web", "Dst", Some("Receiver"))), EnvelopeVerdict::Accepted, "control: a new id from the same sender runs");
    assert_eq!(state(), "twice");

    let mut other_sender = arrival("a", "web", "Dst", Some("Receiver"));
    other_sender.from.instance = "other".into();
    assert_eq!(kernel.dispatch_envelope(other_sender), EnvelopeVerdict::Accepted, "control: dedupe is per sender");

    let unknown = |id: &str, instance: &str, orbital: &str, trait_name: Option<&str>| {
        EnvelopeVerdict::Rejected(DeliverRefusal { kind: DeliverRefusalKind::UnknownInstance, id: id.into() })
            == kernel.dispatch_envelope(arrival(id, instance, orbital, trait_name))
    };
    assert!(unknown("u1", "web", "Nope", None), "an undeclared orbital");
    assert!(unknown("u2", "web", "Dst", Some("Nope")), "an undeclared trait");
    assert!(unknown("u3", "web", "Src", Some("Sender")), "an orbital placed in another unit");
    assert!(unknown("u4", "api", "Dst", Some("Receiver")), "an envelope addressed to another instance");
    assert_eq!(
        kernel.dispatch_envelope(arrival("u1", "web", "Dst", Some("Receiver"))),
        EnvelopeVerdict::Accepted,
        "control: a rejected id was not journaled"
    );
}

#[test]
fn a_refusal_fires_delivery_rejected_at_a_listening_trait() {
    let (kernel, store) = placed_kernel(&placed_program(api_web(), true), Some("api"));
    let refuse = |envelope: &Envelope| Err(DeliverRefusal { kind: DeliverRefusalKind::MailboxFull, id: envelope.id.clone() });
    let (outcomes, asked) = go(&kernel, &store, &refuse);
    assert_eq!(asked.len(), 1);
    assert!(ran(&outcomes, "Watch"));
    assert_eq!(kernel.snapshot_trait("Watch", None).state, "refused");
    let (_, watch) = outcomes.iter().find(|(name, _)| name == "Watch").unwrap();
    assert_eq!(
        j(&watch.payload),
        json!({
            "id": asked[0].id,
            "to": { "instance": "web", "orbital": "Dst", "trait": "Receiver" },
            "event": "GOT",
            "reason": "mailbox-full",
            "causalId": "api#1",
        })
    );
    let (_, sender) = outcomes.iter().find(|(name, _)| name == "Sender").unwrap();
    assert!(sender.failure.is_none(), "a heard refusal is not also a step failure");

    let (kernel, store) = placed_kernel(&placed_program(api_web(), true), Some("api"));
    let (outcomes, _) = go(&kernel, &store, &accept_all);
    assert!(!ran(&outcomes, "Watch") && kernel.snapshot_trait("Watch", None).state != "refused", "control: an accepted delivery rejects nothing");
}

#[test]
fn a_refusal_nobody_listens_for_is_a_visible_failure_of_the_emitting_step() {
    let (kernel, store) = placed_kernel(&placed_program(api_web(), false), Some("api"));
    let refuse = |envelope: &Envelope| Err(DeliverRefusal { kind: DeliverRefusalKind::Denied, id: envelope.id.clone() });
    let (outcomes, asked) = go(&kernel, &store, &refuse);
    let (_, sender) = outcomes.iter().find(|(name, _)| name == "Sender").unwrap();
    let failure = sender.failure.as_deref().expect("the refusal is recorded, not dropped");
    assert!(failure.contains("denied") && failure.contains("DELIVERY_REJECTED") && failure.contains(&asked[0].id), "{failure}");
    assert_eq!(sender.new_state, "idle", "the new state stands");

    let (kernel, store) = placed_kernel(&placed_program(api_web(), false), Some("api"));
    let (outcomes, _) = go(&kernel, &store, &accept_all);
    let (_, sender) = outcomes.iter().find(|(name, _)| name == "Sender").unwrap();
    assert!(sender.failure.is_none(), "control: an accepted delivery leaves no failure");
}

#[test]
fn a_blocking_dispatch_has_no_router_so_the_delivery_is_refused_down() {
    let (kernel, _) = placed_kernel(&placed_program(api_web(), true), Some("api"));
    let outcomes = kernel.dispatch("Sender", None, "GO", Value::Null).unwrap();
    let (_, watch) = outcomes.iter().find(|(name, _)| name == "Watch").expect("the refusal is delivered");
    assert_eq!(j(&watch.payload)["reason"], json!("down"));
}

/// The `@entity` each fired step read at its start and its layered read after its effects (row after its persists,
/// frame plus its sets), per hop; `None` after when the kernel was not asked to record it.
fn entity_views(outcomes: &DispatchOutcomes, field: &str) -> Vec<(String, Option<f64>, Option<Option<f64>>)> {
    outcomes
        .iter()
        .filter(|(_, r)| r.executed)
        .map(|(t, r)| {
            let entry = r.entry.as_ref().expect("an executed step carries its entry");
            let num = |v: &Value| j(v)[field].as_f64();
            (t.clone(), num(&entry.entity_before.to_value()), entry.entity_after.as_ref().map(|after| num(&after.to_value())))
        })
        .collect()
}

fn hand_off_board() -> Setup {
    let arm = |event: &str, effects: Json| json!({ "from": "idle", "to": "idle", "event": event, "effects": effects });
    let schema = json!({
        "name": "views",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Board", "shared": true, "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "score", "type": "number", "default": 0 }] },
            "traits": [
                { "name": "Driver", "linkedEntity": "Board", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [arm("BUMP", json!([["set", "@entity.score", ["+", "@entity.score", 1]], ["emit", "HAND_OFF", {}]]))] } },
                { "name": "Second", "linkedEntity": "Board",
                  "listens": [{ "event": "HAND_OFF", "triggers": "ADD", "source": { "kind": "trait", "trait": "Driver" } }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [arm("ADD", json!([["set", "@entity.score", ["+", "@entity.score", 100]]]))] } }
            ]
        }]
    });
    Setup::new(schema, &["Board"], vec![dispatch("Driver", "BUMP")])
}

#[test]
fn each_hop_of_a_cascade_records_the_entity_before_and_after_it() {
    let setup = hand_off_board();
    let (kernel, store) = build(&setup, true);
    kernel.set_record_entity_after(true);
    let outcomes = drive_blocking(&kernel, start_step(&kernel, &setup.steps[0]), &host_answers(&setup, &store)).unwrap();
    assert_eq!(
        entity_views(&outcomes, "score"),
        vec![
            ("Driver".to_string(), Some(0.0), Some(Some(1.0))),
            ("Second".to_string(), Some(1.0), Some(Some(101.0))),
        ],
        "hop 2 starts where hop 1 ended"
    );
    let hops = orbital_core_runtime::effects::response::TransitionHop::of_outcomes(&outcomes);
    let seen: Vec<Json> = hops.iter().map(|h| serde_json::to_value(&h.observed).unwrap()).collect();
    assert_eq!(seen[0]["entityBefore"]["score"], json!(0.0));
    assert_eq!(seen[0]["entityAfter"]["score"], json!(1.0));
    assert_eq!(serde_json::to_value(&hops[0]).unwrap()["firedArm"], json!(0));
    assert_eq!(seen[1]["entityAfter"]["score"], json!(101.0), "the listener hop carries its own views");
}

#[test]
fn control_without_the_record_a_step_carries_no_after_view() {
    let setup = hand_off_board();
    let (kernel, store) = build(&setup, true);
    let outcomes = drive_blocking(&kernel, start_step(&kernel, &setup.steps[0]), &host_answers(&setup, &store)).unwrap();
    assert!(entity_views(&outcomes, "score").iter().all(|(_, _, after)| after.is_none()));
    let hops = orbital_core_runtime::effects::response::TransitionHop::of_outcomes(&outcomes);
    assert!(hops.iter().all(|h| h.observed.is_none()), "an ordinary dispatch's wire is unchanged");
}

#[test]
fn the_after_view_reads_the_row_a_step_persisted() {
    let schema = json!({
        "name": "views-row",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes",
                "fields": [{ "name": "id", "type": "string" }, { "name": "hits", "type": "number", "default": 0 }] },
            "traits": [{ "name": "Notes", "linkedEntity": "Note", "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "HIT",
                    "effects": [["persist", "update", "Note", "singleton", { "hits": ["+", "@entity.hits", 1] }]] }] } }]
        }]
    });
    let setup = Setup::new(schema, &["Note"], vec![dispatch("Notes", "HIT")]);
    let (kernel, store) = build(&setup, true);
    store.write("Note", "singleton", &serde_json::from_value(json!({ "id": "singleton", "hits": 4 })).unwrap()).unwrap();
    kernel.set_record_entity_after(true);
    let outcomes = drive_blocking(&kernel, start_step(&kernel, &setup.steps[0]), &host_answers(&setup, &store)).unwrap();
    assert_eq!(entity_views(&outcomes, "hits"), vec![("Notes".to_string(), Some(4.0), Some(Some(5.0)))]);
}
