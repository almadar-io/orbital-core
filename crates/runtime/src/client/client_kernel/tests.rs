use orbital_core_base::value::ValueMap;
use super::*;
use crate::entity_source::EntitySource;
use {orbital_core_base::bus_event::{BusEventSource, EmittedEvent}, crate::effects::response::EventResponse};
use orbital_core_base::sync::Mutex;
use std::collections::HashMap;

/// Riya (2026-09-24): a mount's seeder INIT must not reach a sibling before
/// that sibling's own INIT. `local` traits keep the dispatch client-only.
#[test]
fn remount_holds_a_seed_until_the_receivers_init() {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "Boot",
        "orbitals": [{
            "name": "Level",
            "entity": { "name": "World", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Seeder", "dispatch": "client", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "SEED", {}]] }] } },
                { "name": "Body", "dispatch": "client",
                  "listens": [{ "event": "SEED", "triggers": "SET", "source": { "kind": "trait", "trait": "Seeder" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "ready" }, { "name": "seeded" }],
                    "transitions": [
                        { "from": "idle", "to": "ready", "event": "INIT" },
                        { "from": "ready", "to": "seeded", "event": "SET" }
                    ] } }
            ]
        }]
    }))
    .unwrap();
    let kernel = ClientKernel::new(schema, None).unwrap();
    kernel.remount(["Seeder", "Body"]);
    kernel
        .dispatch("Seeder", None, "INIT", Value::Null)
        .unwrap();
    let response = kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
    assert_eq!(response.states.get("Body"), Some(&"seeded".to_string()));
}

fn persisted_awaited_with_watcher_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [
                { "name": "id", "type": "string" },
                { "name": "title", "type": "string" }
            ] },
            "traits": [
                {
                    "name": "Persistor",
                    "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "created" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "created", "event": "CREATE",
                                "effects": [
                                    ["set", "@entity.title", "Hello"],
                                    ["persist", "create", "Note", {}],
                                    ["emit", "CREATED", {}]
                                ]
                            }
                        ]
                    }
                },
                {
                    "name": "Watcher",
                    "listens": [{
                        "event": "CREATED", "triggers": "WATCH",
                        "source": { "kind": "trait", "trait": "Persistor" }
                    }],
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "watched" },
                            { "name": "watched-twice" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "watched", "event": "WATCH" },
                            { "from": "watched", "to": "watched-twice", "event": "WATCH" }
                        ]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

/// `LocalToggle` declares `dispatch: "client"` but still
/// carries a `persist` effect — same deliberately-malformed fixture
/// `orbital-core`'s own `client_env_hybrid_local_trait_never_produces_server_leg`
/// uses, reproduced here so the ClientKernel-level test proves the bridge is
/// never even consulted, not just that the leg comes back empty.
fn hybrid_with_stray_persist_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "LocalToggle",
                "linkedEntity": "Note",
                "dispatch": "client",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true },
                        { "name": "created" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        {
                            "from": "idle", "to": "created", "event": "CREATE",
                            "effects": [["persist", "create", "Note", {}]]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// `Move` declares `dispatch: "optimistic"` (RuntimeOptimistic).
/// Its `MOVE` transition carries a `call-service` alongside the `set`, so it DOES
/// still produce a server leg despite the entity being runtime-only (e.g. a
/// multiplayer sync call) — the scenario `RuntimeOptimistic` rollback exists for.
fn runtime_optimistic_with_call_service_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Cursor", "persistence": "runtime",
                "fields": [{ "name": "id", "type": "string" }, { "name": "x", "type": "number" }]
            },
            "traits": [{
                "name": "Move",
                "linkedEntity": "Cursor",
                "dispatch": "optimistic",
                "stateMachine": {
                    "states": [
                        { "name": "idle", "isInitial": true },
                        { "name": "moved" }
                    ],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        {
                            "from": "idle", "to": "moved", "event": "MOVE",
                            "effects": [
                                ["set", "@entity.x", 5],
                                ["call-service", "SyncService", "move", {}]
                            ]
                        }
                    ]
                }
            }]
        }]
    }))
    .unwrap()
}

/// Records every call and panics — proves a code path that must never touch the
/// bridge really doesn't.
struct PanicBridge;
impl ServerBridge for PanicBridge {
    fn send_event_with_options(
        &self,
        _orbital: &str,
        _event: &str,
        _entity_id: Option<&str>,
        _trait_name: Option<&str>,
        _payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        panic!("PanicBridge::send_event_with_options must never be called");
    }
    fn send_request(
        &self,
        _orbital: &str,
        _request: &crate::effects::response::EventRequest,
    ) -> Result<EventResponse, BridgeError> {
        panic!("PanicBridge::send_request must never be called");
    }
}

/// Scripted responses, one per call, returned in order.
struct ScriptedBridge {
    responses: Mutex<Vec<Result<EventResponse, String>>>,
    calls: Mutex<Vec<crate::effects::response::EventRequest>>,
}
impl ScriptedBridge {
    fn new(responses: Vec<Result<EventResponse, String>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }
}
impl ServerBridge for ScriptedBridge {
    fn send_event_with_options(
        &self,
        _orbital: &str,
        _event: &str,
        _entity_id: Option<&str>,
        _trait_name: Option<&str>,
        _payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        panic!("ScriptedBridge only answers send_request in these tests");
    }
    fn send_request(
        &self,
        _orbital: &str,
        request: &crate::effects::response::EventRequest,
    ) -> Result<EventResponse, BridgeError> {
        self.calls.lock().push(request.clone());
        match self.responses.lock().remove(0) {
            Ok(r) => Ok(r),
            Err(e) => Err(BridgeError::Network(e)),
        }
    }
}

#[test]
fn client_kernel_hybrid_local_never_posts() {
    let bridge: Arc<dyn ServerBridge> = Arc::new(PanicBridge);
    let kernel = ClientKernel::new(hybrid_with_stray_persist_schema(), Some(bridge)).unwrap();
    let response = kernel
        .dispatch("LocalToggle", None, "CREATE", Value::Null)
        .unwrap();
    assert!(response.success);
    assert_eq!(
        response.states.get("LocalToggle"),
        Some(&"created".to_string())
    );
}

/// A `local` seed whose cascade reaches a listener that needs the server posts that
/// listener's leg (orbital-core keeps it: `client_env_local_seed_posts_its_cascade_server_work`).
#[test]
fn a_local_seed_posts_the_server_work_its_cascade_reaches() {
    let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [
                { "name": "Composer", "dispatch": "client", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "SUBMIT", "effects": [["emit", "SUBMITTED", {}]] }
                    ] } },
                { "name": "Persistor", "linkedEntity": "Note",
                  "listens": [{ "event": "SUBMITTED", "triggers": "SAVE", "source": { "kind": "trait", "trait": "Composer" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }, { "name": "saved" }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "saved", "event": "SAVE", "effects": [["persist", "create", "Note", { "title": "x" }]] }
                    ] } }
            ]
        }]
    }))
    .unwrap();
    let server = Arc::new(ScriptedBridge::new(vec![Ok(EventResponse::success(""))]));
    let kernel = ClientKernel::new(schema, Some(Arc::clone(&server) as Arc<dyn ServerBridge>)).unwrap();
    kernel.dispatch("Composer", None, "INIT", Value::Null).unwrap();
    kernel.dispatch("Persistor", None, "INIT", Value::Null).unwrap();
    kernel.dispatch("Composer", None, "SUBMIT", Value::Null).unwrap();
    let calls = server.calls.lock();
    assert_eq!(calls.len(), 1, "the listener's server work is posted");
    assert_eq!(calls[0].target_trait.as_deref(), Some("Persistor"));
}

#[test]
fn client_kernel_persisted_awaited_posts_leg_and_folds() {
    let mut row = ValueMap::default();
    row.insert("id".to_string(), Value::string("n-1"));
    row.insert(
        "title".to_string(),
        Value::string("From server"),
    );
    let mut data = HashMap::new();
    data.insert("Note".to_string(), vec![Value::object(row)]);

    let server_response = EventResponse {
        success: true,
        data: Some(data),
        // Echo of the SAME event this client already delivered locally
        // (`CREATED` from `Persistor`) — must be deduped, not re-dispatched.
        emitted_events: vec![EmittedEvent {
            event: "CREATED".to_string(),
            payload: None,
            source: Some(BusEventSource::from_trait("Persistor")),
        }],
        ..Default::default()
    };
    let bridge: Arc<dyn ServerBridge> =
        Arc::new(ScriptedBridge::new(vec![Ok(server_response)]));
    let kernel =
        ClientKernel::new(persisted_awaited_with_watcher_schema(), Some(bridge)).unwrap();

    let response = kernel
        .dispatch("Persistor", None, "CREATE", Value::Null)
        .unwrap();

    // The PRESENTED response is the local one (fold lands data for the next
    // render, it doesn't replace what THIS call returns).
    assert!(response.success);
    assert!(
        response.data.is_none(),
        "presented response stays the local one, not the server's"
    );
    assert_eq!(
        response.states.get("Persistor"),
        Some(&"created".to_string())
    );

    // The fold landed the server's data into the entity source.
    let folded = kernel.embedded.entities().read("Note", "n-1").unwrap();
    assert_eq!(
        folded.as_object().and_then(|o| o.get("title")),
        Some(&Value::string("From server"))
    );

    // The echoed CREATED was already locally delivered (it cascaded to Watcher
    // as part of the SAME local dispatch that built the leg) — the fold must
    // not re-fire Watcher's listener a second time.
    let watcher_state = kernel
        .embedded
        .kernel()
        .get_state("Watcher", None)
        .unwrap();
    assert_eq!(
        watcher_state.current_state, "watched",
        "an echoed already-delivered event must not re-dispatch and advance Watcher again"
    );
}

#[test]
fn begin_paints_locally_and_posts_nothing_until_settle() {
    let bridge = Arc::new(ScriptedBridge::new(vec![Ok(EventResponse {
        success: true,
        ..Default::default()
    })]));
    let kernel = ClientKernel::new(
        persisted_awaited_with_watcher_schema(),
        Some(bridge.clone() as Arc<dyn ServerBridge>),
    )
    .unwrap();
    let begun = kernel
        .begin("Persistor", None, "CREATE", Value::Null)
        .unwrap();
    assert_eq!(
        begun.local.states.get("Persistor"),
        Some(&"created".to_string())
    );
    assert!(
        bridge.calls.lock().is_empty(),
        "nothing posted before settle"
    );
    assert!(
        begun.awaiting().iter().any(|a| a.trait_name == "Persistor"),
        "the persisting trait awaits its leg: {:?}",
        begun.awaiting()
    );
    let delta = kernel.settle(&begun).unwrap();
    assert_eq!(bridge.calls.lock().len(), 1);
    assert!(
        delta.states.is_empty(),
        "control: settle adds only what the server answered"
    );
    assert!(delta.client_effects().is_empty());
}

fn settled_effect_results(
    server: EventResponse,
) -> Option<Vec<crate::effects::response::ServerEffectResult>> {
    let bridge: Arc<dyn ServerBridge> = Arc::new(ScriptedBridge::new(vec![Ok(server)]));
    let kernel =
        ClientKernel::new(persisted_awaited_with_watcher_schema(), Some(bridge)).unwrap();
    kernel
        .dispatch("Persistor", None, "CREATE", Value::Null)
        .unwrap()
        .effect_results
}

#[test]
fn a_server_denied_persist_reaches_the_settled_response() {
    let denied = crate::effects::response::ServerEffectResult {
        effect: crate::effects::response::ServerEffectKind::Persist,
        action: Some("create".into()),
        entity_type: Some("Note".into()),
        name: None,
        data: None,
        success: false,
        denied: Some(true),
        error: Some("access denied".into()),
        code: None,
        target: None,
        routed: None,
    };
    let results = settled_effect_results(EventResponse {
        success: true,
        effect_results: Some(vec![denied.clone()]),
        ..Default::default()
    })
    .expect("the server's verdict is carried");
    assert!(results.contains(&denied));
}

#[test]
fn control_a_server_answer_with_no_outcomes_adds_no_server_results() {
    let results = settled_effect_results(EventResponse {
        success: true,
        ..Default::default()
    });
    assert!(results.iter().flatten().all(|r| r.denied.is_none()));
}

#[test]
fn client_kernel_runtime_optimistic_restores_on_error() {
    let server_response = EventResponse::error("sync rejected");
    let bridge: Arc<dyn ServerBridge> =
        Arc::new(ScriptedBridge::new(vec![Ok(server_response)]));
    let kernel =
        ClientKernel::new(runtime_optimistic_with_call_service_schema(), Some(bridge)).unwrap();

    // INIT first so the pre-MOVE snapshot captures a real prior state
    // ("idle") rather than the empty default for a trait that has never run.
    kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();

    // Local optimistic commit.
    let response = kernel.dispatch("Move", None, "MOVE", Value::Null).unwrap();
    assert!(
        !response.success,
        "the presented response is the server's success:false"
    );
    assert_eq!(
        kernel
            .embedded
            .kernel()
            .get_state("Move", None)
            .unwrap()
            .current_state,
        "idle",
        "RuntimeOptimistic must restore the pre-dispatch snapshot on a disagreeing response"
    );
    assert!(
        kernel.embedded.entities().read("Cursor", "singleton").is_err(),
        "rollback must leave the entity absent, matching the pre-dispatch snapshot"
    );
}

fn optimistic_with_bound_sibling_schema() -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": {
                "name": "Cursor", "persistence": "runtime", "shared": true,
                "fields": [{ "name": "id", "type": "string" }, { "name": "x", "type": "number", "default": 0 }]
            },
            "traits": [
                {
                    "name": "Move", "linkedEntity": "Cursor", "dispatch": "optimistic",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "moved" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "moved", "event": "MOVE", "effects": [
                                ["set", "@entity.x", 5],
                                ["call-service", "SyncService", "move", {}]
                            ] }
                        ]
                    }
                },
                {
                    "name": "Hud", "linkedEntity": "Cursor", "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [
                            ["render-ui", "hud", { "type": "typography", "content": "@entity.x" }]
                        ] }]
                    }
                }
            ]
        }]
    }))
    .unwrap()
}

fn hud_paints(response: &EventResponse) -> Vec<f64> {
    response
        .client_effects_by_trait
        .iter()
        .flatten()
        .filter(|e| e.trait_name == "Hud")
        .filter_map(|e| e.effect.render_ui_value())
        .filter_map(|v| serde_json::to_value(v).ok()?["content"].as_f64())
        .collect()
}

fn mounted_optimistic(server: Result<EventResponse, String>) -> ClientKernel {
    let bridge: Arc<dyn ServerBridge> = Arc::new(ScriptedBridge::new(vec![server]));
    let kernel =
        ClientKernel::new(optimistic_with_bound_sibling_schema(), Some(bridge)).unwrap();
    kernel.dispatch("Hud", None, "INIT", Value::Null).unwrap();
    kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();
    kernel
}

#[test]
fn a_failed_optimistic_leg_repaints_the_bound_sibling_with_the_restored_value() {
    let kernel = mounted_optimistic(Ok(EventResponse::error("sync rejected")));
    let response = kernel.dispatch("Move", None, "MOVE", Value::Null).unwrap();
    assert!(!response.success);
    assert_eq!(
        hud_paints(&response),
        vec![0.0],
        "the rolled-back value, not the optimistic 5"
    );
}

#[test]
fn a_transport_failure_leaves_the_restored_repaint_for_the_next_response() {
    let kernel = mounted_optimistic(Err("down".into()));
    assert!(kernel.dispatch("Move", None, "MOVE", Value::Null).is_err());
    let mut next = EventResponse::default();
    next.fold_rerenders(&kernel.embedded.kernel().rerender_stale());
    assert_eq!(
        hud_paints(&next),
        vec![0.0],
        "nothing was consumed with no response to carry it"
    );
}

#[test]
fn control_a_successful_optimistic_leg_adds_no_repaint_beyond_the_commit() {
    let kernel = mounted_optimistic(Ok(EventResponse {
        success: true,
        ..Default::default()
    }));
    let response = kernel.dispatch("Move", None, "MOVE", Value::Null).unwrap();
    assert!(response.success);
    assert_eq!(
        hud_paints(&response),
        vec![5.0],
        "one repaint, the committed value"
    );
    assert!(kernel.embedded.kernel().rerender_stale().is_empty());
}

#[test]
fn client_kernel_offline_returns_local_response() {
    let kernel = ClientKernel::new(persisted_awaited_with_watcher_schema(), None).unwrap();
    let response = kernel
        .dispatch("Persistor", None, "CREATE", Value::Null)
        .unwrap();
    assert!(response.success);
    assert_eq!(
        response.states.get("Persistor"),
        Some(&"created".to_string())
    );
    // Offline: nothing was ever posted, so the server-assigned row never lands.
    assert!(kernel.embedded.entities().read("Note", "n-1").is_err());
}

struct RecordingBridge {
    posts: Mutex<Vec<(String, String)>>,
}
impl RecordingBridge {
    fn new() -> Self {
        Self {
            posts: Mutex::new(Vec::new()),
        }
    }
}
impl ServerBridge for RecordingBridge {
    fn send_event_with_options(
        &self,
        _orbital: &str,
        _event: &str,
        _entity_id: Option<&str>,
        _trait_name: Option<&str>,
        _payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        panic!("only send_request is used");
    }
    fn send_request(
        &self,
        orbital: &str,
        request: &crate::effects::response::EventRequest,
    ) -> Result<EventResponse, BridgeError> {
        self.posts
            .lock()
            .push((orbital.to_string(), request.event.clone()));
        Ok(EventResponse {
            success: true,
            ..Default::default()
        })
    }
}

fn ticker_trait(ticks: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name": "Ticker", "dispatch": "client",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "active" }],
            "transitions": [
                { "from": "idle", "to": "idle", "event": "INIT" },
                { "from": "idle", "to": "active", "event": "GO" }
            ]
        },
        "ticks": ticks
    })
}

fn listener_trait() -> serde_json::Value {
    serde_json::json!({
        "name": "Listener", "dispatch": "client",
        "listens": [{ "event": "BEAT", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Ticker" } }],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
            "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD" }]
        }
    })
}

fn persistor_trait() -> serde_json::Value {
    serde_json::json!({
        "name": "Persistor", "linkedEntity": "Task",
        "listens": [{ "event": "BEAT", "triggers": "DO_CREATE", "source": { "kind": "orbital", "orbital": "Alpha", "trait": "Ticker" } }],
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }],
            "transitions": [{ "from": "idle", "to": "idle", "event": "DO_CREATE",
                "effects": [["persist", "create", "Task", {}]] }]
        }
    })
}

fn orbital(name: &str, entity: &str, traits: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "entity": { "name": entity, "fields": [{ "name": "id", "type": "string" }] },
        "traits": traits
    })
}

fn schema_of(orbitals: Vec<serde_json::Value>) -> OrbitalSchema {
    serde_json::from_value(serde_json::json!({ "name": "App", "orbitals": orbitals })).unwrap()
}

const BEAT_TICK: &str =
    r#"[{ "name": "beat", "interval": 1000, "effects": [["emit", "BEAT", {}]] }]"#;
const QUIET_TICK: &str = r#"[{ "name": "beat", "interval": 1000, "effects": [] }]"#;

fn ticks(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap()
}

fn fan_out_kernel(tick: &str) -> ClientKernel {
    let schema = schema_of(vec![orbital(
        "Main",
        "Note",
        vec![ticker_trait(ticks(tick)), listener_trait()],
    )]);
    let kernel = ClientKernel::new(schema, None).unwrap();
    kernel
        .dispatch("Ticker", None, "INIT", Value::Null)
        .unwrap();
    kernel
}

#[test]
fn a_tick_emit_fans_out_to_its_listener() {
    let kernel = fan_out_kernel(BEAT_TICK);
    let response = kernel.tick("Ticker", "beat", None).unwrap();
    assert_eq!(
        response.states.get("Listener").map(String::as_str),
        Some("heard")
    );
    assert_eq!(
        kernel
            .embedded
            .kernel()
            .get_state("Listener", None)
            .unwrap()
            .current_state,
        "heard"
    );
    assert!(
        response.emitted_events.is_empty(),
        "the cascade already delivered the emit; the caller's queue must not replay it"
    );
}

#[test]
fn control_a_tick_without_an_emit_leaves_the_listener_idle() {
    let kernel = fan_out_kernel(QUIET_TICK);
    let response = kernel.tick("Ticker", "beat", None).unwrap();
    assert!(response.transitioned);
    assert!(!response.states.contains_key("Listener"));
    assert!(kernel
        .embedded
        .kernel()
        .get_state("Listener", None)
        .is_none_or(|s| s.current_state == "idle"));
}

fn two_orbital_kernel(
    listener_orbital_is_ticker_orbital: bool,
    bridge: Arc<RecordingBridge>,
) -> ClientKernel {
    let schema = if listener_orbital_is_ticker_orbital {
        schema_of(vec![
            orbital(
                "Alpha",
                "Task",
                vec![ticker_trait(ticks(BEAT_TICK)), persistor_trait()],
            ),
            orbital("Beta", "Other", vec![listener_trait_named("Bystander")]),
        ])
    } else {
        schema_of(vec![
            orbital("Alpha", "Note", vec![ticker_trait(ticks(BEAT_TICK))]),
            orbital("Beta", "Task", vec![persistor_trait()]),
        ])
    };
    let kernel = ClientKernel::new(schema, Some(bridge as Arc<dyn ServerBridge>)).unwrap();
    kernel
        .dispatch("Ticker", None, "INIT", Value::Null)
        .unwrap();
    kernel
}

fn listener_trait_named(name: &str) -> serde_json::Value {
    let mut t = listener_trait();
    t["name"] = serde_json::Value::String(name.to_string());
    t
}

#[test]
fn a_cross_orbital_tick_leg_posts_to_the_owning_orbital() {
    let bridge = Arc::new(RecordingBridge::new());
    let kernel = two_orbital_kernel(false, Arc::clone(&bridge));
    kernel.tick("Ticker", "beat", None).unwrap();
    assert_eq!(
        *bridge.posts.lock(),
        vec![("Beta".to_string(), "DO_CREATE".to_string())]
    );
}

#[test]
fn control_a_same_orbital_tick_leg_posts_to_that_orbital() {
    let bridge = Arc::new(RecordingBridge::new());
    let kernel = two_orbital_kernel(true, Arc::clone(&bridge));
    kernel.tick("Ticker", "beat", None).unwrap();
    assert_eq!(
        *bridge.posts.lock(),
        vec![("Alpha".to_string(), "DO_CREATE".to_string())]
    );
}

#[test]
fn a_cross_orbital_dispatch_leg_posts_to_the_owning_orbital() {
    let bridge = Arc::new(RecordingBridge::new());
    let schema = schema_of(vec![
        orbital(
            "Alpha",
            "Note",
            vec![serde_json::json!({
                "name": "Ticker", "dispatch": "client",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT" },
                        { "from": "idle", "to": "idle", "event": "FIRE", "effects": [["emit", "BEAT", {}]] }
                    ]
                }
            })],
        ),
        orbital("Beta", "Task", vec![persistor_trait()]),
    ]);
    let kernel =
        ClientKernel::new(schema, Some(Arc::clone(&bridge) as Arc<dyn ServerBridge>)).unwrap();
    kernel
        .dispatch("Ticker", None, "INIT", Value::Null)
        .unwrap();
    kernel
        .dispatch("Ticker", None, "FIRE", Value::Null)
        .unwrap();
    assert_eq!(
        *bridge.posts.lock(),
        vec![("Beta".to_string(), "DO_CREATE".to_string())]
    );
}

fn applies_to_kernel(applies_to: serde_json::Value) -> ClientKernel {
    let mut tick = serde_json::json!({ "name": "beat", "interval": 1000, "effects": [["emit", "BEAT", {}]] });
    if !applies_to.is_null() {
        tick["appliesTo"] = applies_to;
    }
    let schema = schema_of(vec![orbital(
        "Main",
        "Note",
        vec![ticker_trait(serde_json::json!([tick])), listener_trait()],
    )]);
    let kernel = ClientKernel::new(schema, None).unwrap();
    kernel
        .dispatch("Ticker", None, "INIT", Value::Null)
        .unwrap();
    kernel
}

#[test]
fn a_tick_applies_to_another_state_does_not_fire() {
    let kernel = applies_to_kernel(serde_json::json!(["active"]));
    let response = kernel.tick("Ticker", "beat", None).unwrap();
    assert!(!response.transitioned);
    assert!(
        !response.states.contains_key("Listener"),
        "no emit, no fan-out"
    );
}

#[test]
fn control_a_tick_applies_to_the_current_state_fires() {
    let kernel = applies_to_kernel(serde_json::json!(["active"]));
    kernel.dispatch("Ticker", None, "GO", Value::Null).unwrap();
    let response = kernel.tick("Ticker", "beat", None).unwrap();
    assert!(response.transitioned);
    assert_eq!(
        response.states.get("Listener").map(String::as_str),
        Some("heard")
    );
}

#[test]
fn control_an_absent_or_empty_applies_to_means_every_state() {
    for applies_to in [serde_json::Value::Null, serde_json::json!([])] {
        let kernel = applies_to_kernel(applies_to);
        let response = kernel.tick("Ticker", "beat", None).unwrap();
        assert!(response.transitioned);
    }
}

fn mount_schema() -> OrbitalSchema {
    let loading = |label: &str| serde_json::json!(["render-ui", "main", { "type": "skeleton", "variant": "text", "content": label }]);
    serde_json::from_value(serde_json::json!({
        "name": "MountApp",
        "orbitals": [{
            "name": "PageOrbital",
            "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "List", "linkedEntity": "Item", "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }, { "name": "ready" }],
                    "transitions": [
                        { "from": "loading", "to": "loading", "event": "INIT", "effects": [["fetch", "Item", { "emit": { "success": "LOADED" } }], loading("list")] },
                        { "from": "loading", "to": "ready", "event": "LOADED" }
                    ] } },
                { "name": "Stats", "linkedEntity": "Item", "stateMachine": {
                    "states": [{ "name": "loading", "isInitial": true }],
                    "transitions": [
                        { "from": "loading", "to": "loading", "event": "LOAD", "effects": [["fetch", "Item", {}], loading("stats")] }
                    ] } },
                { "name": "Title", "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "INIT", "effects": [loading("title")] }
                    ] } }
            ]
        }]
    }))
    .unwrap()
}

/// The in-process server ([`crate::client::bridge::KernelBridge`]) behind a request recorder.
struct RecordingServer {
    server: crate::client::bridge::KernelBridge,
    sent: Mutex<Vec<crate::effects::response::EventRequest>>,
    fail: bool,
}
impl ServerBridge for RecordingServer {
    fn send_event_with_options(
        &self,
        _orbital: &str,
        _event: &str,
        _entity_id: Option<&str>,
        _trait_name: Option<&str>,
        _payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        panic!("RecordingServer only answers send_request");
    }
    fn send_request(
        &self,
        orbital: &str,
        request: &crate::effects::response::EventRequest,
    ) -> Result<EventResponse, BridgeError> {
        self.sent.lock().push(request.clone());
        if self.fail {
            return Err(BridgeError::Network("network down".into()));
        }
        self.server.send_request(orbital, request)
    }
}

fn mount_kernel(fail: bool) -> (ClientKernel, Arc<RecordingServer>) {
    let server = Arc::new(RecordingServer {
        server: crate::client::bridge::KernelBridge::new(mount_schema()).unwrap(),
        sent: Mutex::new(Vec::new()),
        fail,
    });
    let kernel = ClientKernel::new(
        mount_schema(),
        Some(Arc::clone(&server) as Arc<dyn ServerBridge>),
    )
    .unwrap();
    (kernel, server)
}

fn seed(t: &str, e: &str) -> crate::effects::response::MountSeed {
    crate::effects::response::MountSeed {
        r#trait: t.to_string(),
        event: crate::effects::response::LifecycleEvent::parse(e).expect("a lifecycle event"),
    }
}

#[test]
fn a_mount_paints_every_seed_then_posts_one_mount_leg_for_the_server_backed_seeds() {
    let (kernel, server) = mount_kernel(false);
    let begun = kernel
        .begin_mount(
            &[
                seed("List", "INIT"),
                seed("Stats", "LOAD"),
                seed("Title", "INIT"),
            ],
            Value::Null,
        )
        .unwrap();
    assert_eq!(
        begun
            .locals()
            .map(|(t, _)| t.to_string())
            .collect::<Vec<_>>(),
        ["List", "Stats", "Title"]
    );
    assert!(
        server.sent.lock().is_empty(),
        "nothing is posted before settle"
    );
    let mut awaiting: Vec<&str> = begun.awaiting().map(|a| a.trait_name.as_str()).collect();
    awaiting.sort();
    assert_eq!(
        awaiting,
        ["List", "Stats"],
        "control: the local-only Title awaits nothing"
    );

    kernel.settle_mount(&begun).unwrap();
    let sent = server.sent.lock();
    assert_eq!(sent.len(), 1, "one mount leg, not one request per seed");
    assert_eq!(
        sent[0].mount,
        Some(vec![seed("List", "INIT"), seed("Stats", "LOAD")])
    );
    drop(sent);
    assert_eq!(
        kernel.snapshot_trait("List", None).state,
        "ready",
        "the folded leg delivered LOADED"
    );
}

#[test]
fn a_mount_with_no_server_backed_seed_posts_nothing() {
    let (kernel, server) = mount_kernel(false);
    let begun = kernel
        .begin_mount(&[seed("Title", "INIT")], Value::Null)
        .unwrap();
    assert!(!begun.has_legs());
    kernel.settle_mount(&begun).unwrap();
    assert!(server.sent.lock().is_empty());
}

#[test]
fn a_failed_mount_leg_is_reported() {
    let (kernel, server) = mount_kernel(true);
    let begun = kernel
        .begin_mount(&[seed("List", "INIT"), seed("Title", "INIT")], Value::Null)
        .unwrap();
    assert!(kernel.settle_mount(&begun).is_err());
    assert_eq!(server.sent.lock().len(), 1);
}
