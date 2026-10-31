// EXEC-STEP-1a/1b: a listen edge with a `key` clause (`per <expr>` in .lolo) steps the row of the listener's entity
// whose id the expression names. The row is created from defaults on first use with INIT once, a key that is not a
// non-empty string is refused as DELIVERY_REJECTED, and a row that is quiet is dropped.

use orbital_core_base::effect_types::Effect;
use orbital_core_base::value::Value;
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_runtime::kernel::RuntimeKernel;
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn listen(event: &str, triggers: &str, from: &str, key: Option<Json>) -> Json {
    let mut edge = json!({ "event": event, "triggers": triggers, "source": { "kind": "orbital", "orbital": "CallerOrbital", "trait": from } });
    if let Some(key) = key {
        edge["key"] = key;
    }
    edge
}

fn schema(key: Option<Json>, with_init: bool) -> Json {
    let init = if with_init {
        json!([{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "INITED", { "key": "@entity.id" }]] }])
    } else {
        json!([])
    };
    let mut slot_arms = vec![
        json!({ "from": "idle", "to": "busy", "event": "SEND", "effects": [
            ["set", "@entity.val", "@payload.n"],
            ["emit", "SENT", { "key": "@entity.id", "val": "@entity.val" }],
            ["emit", "AGAIN", {}]
        ] }),
        json!({ "from": "busy", "to": "busy", "event": "AGAIN", "effects": [["emit", "AGAINED", { "key": "@entity.id" }]] }),
        json!({ "from": "idle", "to": "idle", "event": "REPLY", "effects": [["emit", "STRAY", { "key": "@entity.id" }]] }),
        json!({ "from": "busy", "to": "busy", "event": "SEND", "effects": [
            ["emit", "REFUSED", { "key": "@entity.id", "val": "@entity.val" }]
        ] }),
        json!({ "from": "busy", "to": "idle", "event": "REPLY", "effects": [
            ["emit", "REPLIED", { "key": "@entity.id", "val": "@entity.val" }],
            ["set", "@entity.val", 0]
        ] }),
    ];
    slot_arms.extend(init.as_array().cloned().unwrap_or_default());
    json!({
        "name": "App",
        "typeContractVersion": 1,
        "orbitals": [
            {
                "name": "CallerOrbital",
                "entity": { "name": "Call", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Caller", "linkedEntity": "Call",
                    "listens": [listen("DELIVERY_REJECTED", "REJECTED", "Caller", None)],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [["emit", "SEND", { "key": "@payload.key", "n": "@payload.n" }]] },
                            { "from": "idle", "to": "idle", "event": "BACK", "effects": [["emit", "REPLY", { "key": "@payload.key" }]] },
                            { "from": "idle", "to": "idle", "event": "REJECTED", "effects": [["emit", "WAS_REJECTED", { "reason": "@payload.reason" }]] }
                        ] }
                }]
            },
            {
                "name": "SlotOrbital",
                "entity": { "name": "Slot", "persistence": "runtime",
                    "fields": [{ "name": "id", "type": "string" }, { "name": "val", "type": "number", "default": 0 }] },
                "traits": [{
                    "name": "PerKey", "linkedEntity": "Slot",
                    "listens": [
                        listen("SEND", "SEND", "Caller", key.clone()),
                        listen("REPLY", "REPLY", "Caller", key)
                    ],
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }], "transitions": slot_arms }
                }]
            }
        ]
    })
}

fn kernel(key: Option<Json>, with_init: bool) -> (RuntimeKernel, Arc<InMemoryEntityCache>) {
    let cache = Arc::new(InMemoryEntityCache::new());
    let k = crate::support::TestKernel::server()
        .source(cache.clone() as Arc<dyn EntitySource>)
        .build(schema(key, with_init));
    (k, cache)
}

fn per_payload_key() -> Option<Json> {
    Some(json!("@payload.key"))
}

fn val(j: Json) -> Value {
    serde_json::from_value(j).unwrap()
}

fn whole(v: Json) -> Json {
    match v {
        Json::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 => json!(f as i64),
            _ => Json::Number(n),
        },
        Json::Object(m) => Json::Object(m.into_iter().map(|(k, v)| (k, whole(v))).collect()),
        other => other,
    }
}

fn emits(
    outcomes: &[(
        String,
        orbital_core_runtime::process_result::ProcessEventResult,
    )],
) -> Vec<(String, Json)> {
    outcomes
        .iter()
        .flat_map(|(_, r)| r.effect_results.iter())
        .filter_map(|er| match &er.effect {
            Effect::Emit { event, payload } => {
                Some((event.clone(), whole(serde_json::to_value(payload).unwrap())))
            }
            _ => None,
        })
        .collect()
}

fn go(k: &RuntimeKernel, key: Json, n: i64) -> Vec<(String, Json)> {
    emits(
        &k.dispatch("Caller", None, "GO", val(json!({ "key": key, "n": n })))
            .unwrap(),
    )
}

fn back(k: &RuntimeKernel, key: Json) -> Vec<(String, Json)> {
    emits(
        &k.dispatch("Caller", None, "BACK", val(json!({ "key": key })))
            .unwrap(),
    )
}

fn state(k: &RuntimeKernel, row: Option<&str>) -> Option<String> {
    k.get_state("PerKey", row).map(|s| s.current_state)
}

fn rows(cache: &InMemoryEntityCache) -> Vec<String> {
    let mut ids: Vec<String> = cache
        .list("Slot")
        .unwrap()
        .iter()
        .filter_map(|r| {
            r.get_path("id")
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .collect();
    ids.sort();
    ids
}

fn named(emitted: &[(String, Json)], event: &str) -> Vec<Json> {
    emitted
        .iter()
        .filter(|(e, _)| e == event)
        .map(|(_, p)| p.clone())
        .collect()
}

#[test]
fn two_keys_step_independently_each_on_its_own_row() {
    let (k, _) = kernel(per_payload_key(), false);
    let a = go(&k, json!("A"), 1);
    let b = go(&k, json!("B"), 2);
    assert_eq!(named(&a, "SENT"), [json!({ "key": "A", "val": 1 })]);
    assert_eq!(named(&b, "SENT"), [json!({ "key": "B", "val": 2 })]);
    assert!(
        named(&b, "REFUSED").is_empty(),
        "B must not see A's busy state: {b:?}"
    );
    assert_eq!(state(&k, Some("A")).as_deref(), Some("busy"));
    assert_eq!(state(&k, Some("B")).as_deref(), Some("busy"));
    assert_eq!(
        state(&k, None),
        None,
        "no delivery reached the singleton frame"
    );
    let replied = back(&k, json!("A"));
    assert_eq!(
        named(&replied, "REPLIED"),
        [json!({ "key": "A", "val": 1 })]
    );
    assert_eq!(
        state(&k, Some("B")).as_deref(),
        Some("busy"),
        "A's reply never touched B"
    );
}

#[test]
fn control_without_a_key_clause_both_deliveries_share_the_singleton_frame() {
    let (k, _) = kernel(None, false);
    go(&k, json!("A"), 1);
    let b = go(&k, json!("B"), 2);
    assert_eq!(named(&b, "REFUSED").len(), 1, "{b:?}");
    assert_eq!(state(&k, None).as_deref(), Some("busy"));
    assert_eq!(state(&k, Some("A")), None);
}

#[test]
fn the_same_key_is_fifo_on_one_row() {
    let (k, _) = kernel(per_payload_key(), false);
    go(&k, json!("A"), 1);
    let second = go(&k, json!("A"), 2);
    assert_eq!(named(&second, "REFUSED"), [json!({ "key": "A", "val": 1 })]);
    let reply = back(&k, json!("A"));
    assert_eq!(named(&reply, "REPLIED"), [json!({ "key": "A", "val": 1 })]);
}

#[test]
fn first_use_creates_the_row_and_runs_init_once() {
    let (k, cache) = kernel(per_payload_key(), true);
    assert!(rows(&cache).is_empty());
    let first = go(&k, json!("A"), 1);
    assert_eq!(named(&first, "INITED"), [json!({ "key": "A" })]);
    assert_eq!(rows(&cache), ["A"]);
    let again = go(&k, json!("A"), 2);
    assert!(
        named(&again, "INITED").is_empty(),
        "INIT runs once per row: {again:?}"
    );
    let other = go(&k, json!("B"), 3);
    assert_eq!(named(&other, "INITED"), [json!({ "key": "B" })]);
    assert_eq!(rows(&cache), ["A", "B"]);
}

#[test]
fn a_key_that_is_not_a_non_empty_string_is_refused_never_the_singleton() {
    for bad in [json!(""), json!(7), Json::Null] {
        let (k, cache) = kernel(per_payload_key(), false);
        let emitted = go(&k, bad.clone(), 1);
        assert_eq!(
            named(&emitted, "WAS_REJECTED"),
            [json!({ "reason": "key-invalid" })],
            "key {bad}: {emitted:?}"
        );
        assert_eq!(
            state(&k, None),
            None,
            "key {bad} must not reach the singleton"
        );
        assert!(rows(&cache).is_empty(), "key {bad} must not create a row");
    }
}

// EXEC-STEP-1b: a quiet keyed row is dropped, a later delivery recreates it from defaults.
#[test]
fn a_quiet_row_is_dropped_and_a_later_delivery_recreates_it_from_defaults() {
    let (k, cache) = kernel(per_payload_key(), true);
    go(&k, json!("A"), 1);
    assert_eq!(rows(&cache), ["A"]);
    back(&k, json!("A"));
    assert_eq!(
        rows(&cache),
        Vec::<String>::new(),
        "idle with default fields: the row is dropped"
    );
    assert_eq!(state(&k, Some("A")), None);
    let fresh = go(&k, json!("A"), 5);
    assert_eq!(
        named(&fresh, "INITED"),
        [json!({ "key": "A" })],
        "a fresh row runs INIT again"
    );
    assert_eq!(named(&fresh, "SENT"), [json!({ "key": "A", "val": 5 })]);
}

#[test]
fn control_a_busy_row_is_not_dropped() {
    let (k, cache) = kernel(per_payload_key(), false);
    go(&k, json!("A"), 1);
    go(&k, json!("B"), 2);
    back(&k, json!("A"));
    assert_eq!(rows(&cache), ["B"]);
}

#[test]
fn a_same_trait_cascade_stays_on_the_row_that_made_it() {
    let (k, _) = kernel(per_payload_key(), false);
    let a = go(&k, json!("A"), 1);
    assert_eq!(named(&a, "AGAINED"), [json!({ "key": "A" })], "{a:?}");
}

#[test]
fn control_a_same_trait_cascade_without_a_key_clause_runs_on_the_singleton() {
    let (k, _) = kernel(None, false);
    let a = go(&k, json!("A"), 1);
    assert_eq!(named(&a, "AGAINED").len(), 1, "{a:?}");
}

#[test]
fn a_refused_key_with_no_declared_listener_for_the_rejection_fails_the_dispatch() {
    let mut program = schema(per_payload_key(), false);
    program["orbitals"][0]["traits"][0]["listens"] = json!([]);
    let k = crate::support::TestKernel::server().build(program);
    let refused = k.dispatch("Caller", None, "GO", val(json!({ "key": "", "n": 1 })));
    let message = refused.unwrap_err().to_string();
    assert!(
        message.contains("key-invalid") && message.contains("DELIVERY_REJECTED"),
        "{message}"
    );
}

fn memory_program() -> Json {
    let mut program = schema(per_payload_key(), false);
    let seen = |extra: &str| json!({ "key": "@entity.id", "prior": ["array/len", "@prevEvents"], "left": ["array/len", "@prevStates"], "via": extra });
    program["orbitals"][0]["traits"][0]["stateMachine"]["transitions"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({ "from": "idle", "to": "idle", "event": "TWO_KEYS", "effects": [
                ["emit", "SEND", { "key": "A", "n": 1 }],
                ["emit", "SEND", { "key": "B", "n": 2 }]
            ] }),
            json!({ "from": "idle", "to": "idle", "event": "ONE_KEY_TWICE", "effects": [
                ["emit", "SEND", { "key": "A", "n": 1 }],
                ["emit", "SEND", { "key": "A", "n": 2 }]
            ] }),
        ]);
    let arms = program["orbitals"][1]["traits"][0]["stateMachine"]["transitions"]
        .as_array_mut()
        .unwrap();
    arms[0] = json!({ "from": "idle", "to": "busy", "event": "SEND", "effects": [
        ["set", "@entity.val", "@payload.n"],
        ["emit", "SENT", seen("sent")]
    ] });
    arms[3] = json!({ "from": "busy", "to": "busy", "event": "SEND", "effects": [
        ["emit", "REFUSED", seen("refused")]
    ] });
    program
}

fn memory_kernel() -> RuntimeKernel {
    crate::support::TestKernel::server().build(memory_program())
}

fn dispatch_caller(k: &RuntimeKernel, event: &str) -> Vec<(String, Json)> {
    emits(&k.dispatch("Caller", None, event, val(json!({}))).unwrap())
}

#[test]
fn prev_events_and_prev_states_are_per_row_not_per_trait() {
    let k = memory_kernel();
    let emitted = dispatch_caller(&k, "TWO_KEYS");
    let sent = named(&emitted, "SENT");
    assert_eq!(sent.len(), 2, "{emitted:?}");
    for payload in &sent {
        assert_eq!(
            payload["prior"],
            json!(1),
            "a row's log holds only its own deliveries, the current one included: {payload}"
        );
        assert_eq!(payload["left"], json!(0), "{payload}");
    }
}

#[test]
fn control_the_same_row_keeps_its_own_delivery_and_state_memory() {
    let k = memory_kernel();
    let emitted = dispatch_caller(&k, "ONE_KEY_TWICE");
    assert_eq!(named(&emitted, "SENT")[0]["prior"], json!(1), "{emitted:?}");
    let refused = named(&emitted, "REFUSED");
    assert_eq!(refused.len(), 1, "{emitted:?}");
    assert_eq!(
        refused[0]["prior"],
        json!(2),
        "the row saw its own first delivery: {emitted:?}"
    );
    assert_eq!(
        refused[0]["left"],
        json!(1),
        "it left idle once: {emitted:?}"
    );
}

mod keyed_completion {
    use super::*;
    use orbital_core_runtime::embed::{EmbeddedServer, ServerProgress};
    use orbital_core_runtime::resumable::{IoAnswer, IoReply, IoRequest};

    fn program() -> Json {
        let mut program = schema(per_payload_key(), false);
        let call = json!(["call-service", "svc", "work", {}, { "key": "@payload.key", "emit": { "success": "DONE", "failure": "FAILED" } }]);
        let slot = &mut program["orbitals"][1]["traits"][0]["stateMachine"];
        slot["states"] = json!([{ "name": "idle", "isInitial": true }, { "name": "busy" }, { "name": "waiting" }]);
        slot["transitions"] = json!([
            { "from": "idle", "to": "waiting", "event": "SEND", "effects": [call] },
            { "from": "waiting", "to": "idle", "event": "DONE", "effects": [["emit", "DONE_SEEN", { "key": "@entity.id" }]] },
            { "from": "waiting", "to": "waiting", "event": "SEND", "effects": [["emit", "REFUSED", { "key": "@entity.id" }]] }
        ]);
        program
    }

    fn run(program: Json, event_payload: Json) -> (Vec<String>, Vec<String>) {
        let server = EmbeddedServer::new(support_schema(program)).unwrap();
        let request = orbital_core_runtime::effects::response::EventRequest {
            event: "GO".to_string(),
            target_trait: Some("Caller".to_string()),
            payload: Some(val(event_payload)),
            ..Default::default()
        };
        let mut sites = Vec::new();
        let mut progress = server.start("CallerOrbital", request).unwrap();
        loop {
            match progress {
                ServerProgress::Done(response) => {
                    let emitted = response
                        .emitted_events
                        .iter()
                        .map(|e| {
                            format!("{}:{}", e.event, serde_json::to_string(&e.payload).unwrap())
                        })
                        .collect();
                    return (sites, emitted);
                }
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|pending| IoReply {
                            id: pending.id,
                            answer: match pending.request {
                                IoRequest::CallService { site, .. } => {
                                    sites.push(site.unwrap().entity_id);
                                    IoAnswer::Service(Ok(serde_json::from_value(
                                        json!({ "ok": true }),
                                    )
                                    .unwrap()))
                                }
                                other => panic!("unexpected ask {other:?}"),
                            },
                        })
                        .collect();
                    progress = server.resume(flight, replies).unwrap();
                }
                ServerProgress::Queued { ticket } => panic!("queued {ticket}"),
            }
        }
    }

    fn support_schema(program: Json) -> orbital_core_base::schema::types::OrbitalSchema {
        serde_json::from_value(program).unwrap()
    }

    #[test]
    fn a_keyed_ask_is_stamped_with_its_row_and_its_completion_lands_on_that_row() {
        let (sites, emitted) = run(program(), json!({ "key": "A", "n": 1 }));
        assert_eq!(sites, ["A"]);
        assert!(
            emitted
                .iter()
                .any(|e| e.starts_with("DONE_SEEN") && e.contains("\"A\"")),
            "{emitted:?}"
        );
    }

    #[test]
    fn control_without_a_key_clause_the_ask_is_stamped_with_the_singleton() {
        let mut p = program();
        p["orbitals"][1]["traits"][0]["listens"] = json!([listen("SEND", "SEND", "Caller", None)]);
        let (sites, _) = run(p, json!({ "key": "A", "n": 1 }));
        assert_eq!(
            sites,
            [orbital_core_runtime::process_result::RUNTIME_SINGLETON.to_string()]
        );
    }

    #[test]
    fn a_row_with_a_deferred_ask_is_kept_until_the_completion_lands_then_dropped_when_quiet() {
        let mut p = program();
        let call = json!(["call-service", "svc", "work", {}, { "key": "@payload.key", "emit": { "success": "DONE", "failure": "FAILED" } }]);
        p["orbitals"][1]["traits"][0]["stateMachine"]["transitions"] = json!([
            { "from": "idle", "to": "idle", "event": "SEND", "effects": [call] },
            { "from": "idle", "to": "idle", "event": "DONE", "effects": [["emit", "DONE_SEEN", { "key": "@entity.id" }]] }
        ]);
        let server = EmbeddedServer::new(support_schema(p)).unwrap();
        let go = orbital_core_runtime::effects::response::EventRequest {
            event: "GO".to_string(),
            target_trait: Some("Caller".to_string()),
            payload: Some(val(json!({ "key": "A", "n": 1 }))),
            ..Default::default()
        };
        let mut progress = server.start("CallerOrbital", go).unwrap();
        loop {
            match progress {
                ServerProgress::Done(_) => break,
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|pending| IoReply {
                            id: pending.id,
                            answer: IoAnswer::ServiceDeferred,
                        })
                        .collect();
                    progress = server.resume(flight, replies).unwrap();
                }
                ServerProgress::Queued { ticket } => panic!("queued {ticket}"),
            }
        }
        let held = |server: &EmbeddedServer| {
            server
                .kernel()
                .get_state("PerKey", Some("A"))
                .map(|s| s.current_state)
        };
        assert_eq!(
            held(&server).as_deref(),
            Some("idle"),
            "idle with default fields, yet the deferred ask keeps the row"
        );
        let done = orbital_core_runtime::effects::response::EventRequest {
            event: "DONE".to_string(),
            target_trait: Some("PerKey".to_string()),
            entity_id: Some("A".to_string()),
            payload: Some(val(json!({ "key": "A" }))),
            ..Default::default()
        };
        match server.start("SlotOrbital", done).unwrap() {
            ServerProgress::Done(response) => {
                assert!(response
                    .emitted_events
                    .iter()
                    .any(|e| e.event == "DONE_SEEN"));
            }
            _ => panic!("the completion needs no host ask"),
        }
        assert_eq!(
            held(&server),
            None,
            "the completion landed and the quiet row was dropped"
        );
    }

    fn ticking_program() -> Json {
        let mut p = schema(per_payload_key(), true);
        let slot = &mut p["orbitals"][1]["traits"][0];
        slot["ticks"] = json!([{ "name": "beat", "interval": 10, "runsInBackground": true,
            "effects": [["emit", "TICKED", { "key": "@entity.id" }]] }]);
        p
    }

    fn tick_request() -> orbital_core_runtime::effects::response::EventRequest {
        orbital_core_runtime::effects::response::EventRequest {
            event: orbital_core_base::bus_event::tick_event_key("beat"),
            run_tick: Some(orbital_core_runtime::effects::RunTick {
                trait_name: "PerKey".to_string(),
                tick: "beat".to_string(),
            }),
            target_trait: Some("PerKey".to_string()),
            ..Default::default()
        }
    }

    fn ticked_rows(server: &EmbeddedServer, now_ms: u64) -> Vec<String> {
        server.tick_pass(now_ms);
        let fired = server.tick_pass(now_ms + 10);
        let mut keys = Vec::new();
        for _ in fired.values().flatten() {
            keys.extend(
                done(server.start("SlotOrbital", tick_request()).unwrap())
                    .emitted_events
                    .iter()
                    .filter(|e| e.event == "TICKED")
                    .filter_map(|e| {
                        e.payload
                            .as_ref()
                            .and_then(|p| p.get_path("key"))
                            .and_then(|k| k.as_str().map(str::to_string))
                    }),
            );
        }
        keys.sort();
        keys
    }

    fn tick_emissions(server: &EmbeddedServer, now_ms: u64) -> Vec<Json> {
        server.tick_pass(now_ms);
        let fired = server.tick_pass(now_ms + 10);
        let mut emitted = Vec::new();
        for _ in fired.values().flatten() {
            emitted.extend(
                done(server.start("SlotOrbital", tick_request()).unwrap())
                    .emitted_events
                    .iter()
                    .filter(|e| e.event == "TICKED")
                    .map(|e| serde_json::to_value(&e.payload).unwrap()),
            );
        }
        emitted
    }

    fn gated_ticking_program(applies_to: Json) -> Json {
        let mut p = ticking_program();
        let call = json!(["call-service", "svc", "work", {}, { "key": "@payload.key", "emit": { "success": "DONE", "failure": "FAILED" } }]);
        let trait_def = &mut p["orbitals"][1]["traits"][0];
        trait_def["ticks"] = json!([{ "name": "beat", "interval": 10, "runsInBackground": true,
            "appliesTo": applies_to,
            "effects": [["emit", "TICKED", { "key": "@entity.id", "val": "@entity.val" }]] }]);
        trait_def["stateMachine"]["transitions"] = json!([
            { "from": "idle", "to": "busy", "event": "SEND", "effects": [["set", "@entity.val", "@payload.n"], call] },
            { "from": "busy", "to": "idle", "event": "REPLY", "effects": [] }
        ]);
        p
    }

    fn one_busy_one_idle(applies_to: Json) -> EmbeddedServer {
        let server = EmbeddedServer::new(support_schema(gated_ticking_program(applies_to))).unwrap();
        run_deferring(&server, "GO", json!({ "key": "A", "n": 7 }));
        run_deferring(&server, "BACK", json!({ "key": "A" }));
        run_deferring(&server, "GO", json!({ "key": "B", "n": 9 }));
        assert_eq!(held(&server, "A").as_deref(), Some("idle"));
        assert_eq!(held(&server, "B").as_deref(), Some("busy"));
        server
    }

    #[test]
    fn a_keyed_tick_judges_applies_to_on_each_row_s_own_state_and_reads_that_row() {
        let server = one_busy_one_idle(json!(["busy"]));
        let emitted = tick_emissions(&server, 0);
        assert_eq!(emitted.len(), 1, "only the busy row ticks: {emitted:?}");
        assert_eq!(emitted[0]["key"], json!("B"), "{emitted:?}");
        assert_eq!(emitted[0]["val"].as_f64(), Some(9.0), "reads B's own entity: {emitted:?}");
    }

    #[test]
    fn control_a_keyed_tick_applies_to_idle_fires_for_the_idle_row_only() {
        let server = one_busy_one_idle(json!(["idle"]));
        let emitted = tick_emissions(&server, 0);
        assert_eq!(emitted.len(), 1, "{emitted:?}");
        assert_eq!(emitted[0]["key"], json!("A"), "{emitted:?}");
        assert_eq!(emitted[0]["val"].as_f64(), Some(7.0), "{emitted:?}");
    }

    #[test]
    fn a_keyed_tick_with_no_rows_does_not_fire_on_the_singleton() {
        let server = EmbeddedServer::new(support_schema(ticking_program())).unwrap();
        assert!(slot_rows(&server).is_empty());
        let emitted = tick_emissions(&server, 0);
        assert!(emitted.is_empty(), "no rows, no tick: {emitted:?}");
    }

    #[test]
    fn control_a_non_keyed_runtime_entity_with_no_rows_ticks_once_on_the_singleton() {
        let mut p = schema(None, false);
        p["orbitals"][1]["traits"][0]["ticks"] = json!([{ "name": "beat", "interval": 10, "runsInBackground": true,
            "effects": [["emit", "TICKED", { "key": "@entity.id" }]] }]);
        let server = EmbeddedServer::new(support_schema(p)).unwrap();
        let emitted = tick_emissions(&server, 0);
        assert_eq!(emitted.len(), 1, "EXEC-TICK-5 singleton tick: {emitted:?}");
    }

    fn slot_rows(server: &EmbeddedServer) -> Vec<String> {
        rows_of(server.kernel().entity_source().as_ref())
    }

    fn rows_of(source: &dyn EntitySource) -> Vec<String> {
        let mut ids: Vec<String> = source
            .list("Slot")
            .unwrap()
            .iter()
            .filter_map(|r| {
                r.get_path("id")
                    .and_then(|v| v.as_str().map(str::to_string))
            })
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn a_ticking_keyed_row_whose_run_ended_is_dropped_and_its_tick_never_fires_again() {
        let server = EmbeddedServer::new(support_schema(ticking_program())).unwrap();
        let keys = ["k0", "k1", "k2", "k3", "k4"];
        for key in keys {
            done(
                server
                    .start(
                        "CallerOrbital",
                        caller_request("GO", json!({ "key": key, "n": 1 })),
                    )
                    .unwrap(),
            );
        }
        assert_eq!(slot_rows(&server), keys, "every busy row is held");
        assert_eq!(ticked_rows(&server, 0), keys, "each busy row ticks once");
        for key in keys {
            done(
                server
                    .start(
                        "CallerOrbital",
                        caller_request("BACK", json!({ "key": key })),
                    )
                    .unwrap(),
            );
        }
        assert!(
            slot_rows(&server).is_empty(),
            "every ended run released its row"
        );
        for key in keys {
            assert_eq!(held(&server, key), None, "{key}");
        }
        assert!(
            ticked_rows(&server, 1000).is_empty(),
            "no tick fires for a dropped row"
        );
        assert!(ticked_rows(&server, 2000).is_empty(), "nor on a later pass");
    }

    #[test]
    fn control_a_ticking_row_with_a_pending_ask_keeps_its_row_and_its_tick() {
        let mut p = ticking_program();
        let call = json!(["call-service", "svc", "work", {}, { "key": "@payload.key", "emit": { "success": "DONE", "failure": "FAILED" } }]);
        p["orbitals"][1]["traits"][0]["stateMachine"]["transitions"] = json!([
            { "from": "idle", "to": "idle", "event": "SEND", "effects": [call] },
            { "from": "idle", "to": "idle", "event": "DONE", "effects": [["emit", "DONE_SEEN", { "key": "@entity.id" }]] }
        ]);
        let server = EmbeddedServer::new(support_schema(p)).unwrap();
        run_deferring(&server, "GO", json!({ "key": "A", "n": 1 }));
        assert_eq!(
            held(&server, "A").as_deref(),
            Some("idle"),
            "the pending ask keeps the row"
        );
        assert_eq!(slot_rows(&server), ["A"]);
        assert_eq!(ticked_rows(&server, 0), ["A"], "its tick still fires");
        let done_request = orbital_core_runtime::effects::response::EventRequest {
            event: "DONE".to_string(),
            target_trait: Some("PerKey".to_string()),
            entity_id: Some("A".to_string()),
            payload: Some(val(json!({ "key": "A" }))),
            ..Default::default()
        };
        done(server.start("SlotOrbital", done_request).unwrap());
        assert_eq!(
            held(&server, "A"),
            None,
            "the completion ended the ask and the run: the row drops"
        );
        assert!(slot_rows(&server).is_empty());
        assert!(
            !ticked_rows(&server, 1000).contains(&"A".to_string()),
            "the dropped row's tick is gone"
        );
    }

    #[test]
    fn a_key_reused_after_its_ticking_row_dropped_starts_fresh_with_init_once_and_one_new_tick() {
        let server = EmbeddedServer::new(support_schema(ticking_program())).unwrap();
        let inited = |response: &orbital_core_runtime::effects::response::EventResponse| {
            response
                .emitted_events
                .iter()
                .filter(|e| e.event == "INITED")
                .count()
        };
        let first = done(
            server
                .start(
                    "CallerOrbital",
                    caller_request("GO", json!({ "key": "A", "n": 1 })),
                )
                .unwrap(),
        );
        assert_eq!(inited(&first), 1);
        assert_eq!(ticked_rows(&server, 0), ["A"]);
        done(
            server
                .start(
                    "CallerOrbital",
                    caller_request("BACK", json!({ "key": "A" })),
                )
                .unwrap(),
        );
        assert!(slot_rows(&server).is_empty());
        let again = done(
            server
                .start(
                    "CallerOrbital",
                    caller_request("GO", json!({ "key": "A", "n": 2 })),
                )
                .unwrap(),
        );
        assert_eq!(inited(&again), 1, "INIT runs once for the fresh row");
        assert_eq!(slot_rows(&server), ["A"]);
        assert_eq!(
            ticked_rows(&server, 1000),
            ["A"],
            "exactly one tick for the reborn row"
        );
    }

    fn cancelling_program() -> Json {
        let mut p = program();
        let call = json!(["call-service", "svc", "work", {}, { "key": "@payload.key", "emit": { "success": "DONE", "failure": "FAILED" } }]);
        p["orbitals"][0]["traits"][0]["stateMachine"]["transitions"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "from": "idle", "to": "idle", "event": "STOP", "effects": [["emit", "CANCEL", { "key": "@payload.key" }]] }));
        p["orbitals"][1]["traits"][0]["listens"]
            .as_array_mut()
            .unwrap()
            .push(listen("CANCEL", "CANCEL", "Caller", per_payload_key()));
        p["orbitals"][1]["traits"][0]["stateMachine"]["transitions"] = json!([
            { "from": "idle", "to": "idle", "event": "SEND", "effects": [call] },
            { "from": "idle", "to": "idle", "event": "CANCEL", "effects": [["cancel-call", "@payload.key"]] }
        ]);
        p
    }

    fn run_deferring(server: &EmbeddedServer, event: &str, payload: Json) {
        let mut progress = server
            .start("CallerOrbital", caller_request(event, payload))
            .unwrap();
        loop {
            match progress {
                ServerProgress::Done(_) => return,
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|pending| IoReply {
                            id: pending.id,
                            answer: match pending.request {
                                IoRequest::CancelCall { .. } => IoAnswer::Cancelled(Ok(())),
                                _ => IoAnswer::ServiceDeferred,
                            },
                        })
                        .collect();
                    progress = server.resume(flight, replies).unwrap();
                }
                ServerProgress::Queued { ticket } => panic!("queued {ticket}"),
            }
        }
    }

    fn held(server: &EmbeddedServer, row: &str) -> Option<String> {
        server
            .kernel()
            .get_state("PerKey", Some(row))
            .map(|s| s.current_state)
    }

    #[test]
    fn a_cancel_call_of_the_rows_own_key_ends_its_pending_ask_and_the_quiet_row_drops() {
        let server = EmbeddedServer::new(support_schema(cancelling_program())).unwrap();
        run_deferring(&server, "GO", json!({ "key": "A", "n": 1 }));
        assert_eq!(
            held(&server, "A").as_deref(),
            Some("idle"),
            "the pending ask keeps the row"
        );
        run_deferring(&server, "STOP", json!({ "key": "A" }));
        assert_eq!(
            held(&server, "A"),
            None,
            "the cancelled ask no longer keeps the row"
        );
    }

    #[test]
    fn control_a_cancel_of_another_key_leaves_the_pending_ask_and_its_row() {
        let server = EmbeddedServer::new(support_schema(cancelling_program())).unwrap();
        run_deferring(&server, "GO", json!({ "key": "A", "n": 1 }));
        run_deferring(&server, "STOP", json!({ "key": "B" }));
        assert_eq!(
            held(&server, "A").as_deref(),
            Some("idle"),
            "A's ask is still pending"
        );
        assert_eq!(
            held(&server, "B"),
            None,
            "B had nothing pending and is quiet"
        );
    }

    #[test]
    fn a_cancel_call_issued_by_another_row_drops_the_row_whose_ask_it_ended() {
        let mut p = cancelling_program();
        let arms = p["orbitals"][1]["traits"][0]["stateMachine"]["transitions"]
            .as_array_mut()
            .unwrap();
        arms[1] = json!({ "from": "idle", "to": "idle", "event": "CANCEL", "effects": [["cancel-call", "A"]] });
        let server = EmbeddedServer::new(support_schema(p)).unwrap();
        run_deferring(&server, "GO", json!({ "key": "A", "n": 1 }));
        run_deferring(&server, "STOP", json!({ "key": "B" }));
        assert_eq!(
            held(&server, "A"),
            None,
            "B's step cancelled A's key, so A is quiet and dropped"
        );
    }

    #[test]
    fn the_end_of_a_rows_stream_ends_its_pending_ask_and_the_quiet_row_drops() {
        let server = EmbeddedServer::new(support_schema(cancelling_program())).unwrap();
        run_deferring(&server, "GO", json!({ "key": "A", "n": 1 }));
        assert_eq!(
            held(&server, "A").as_deref(),
            Some("idle"),
            "the open stream keeps the row"
        );
        server.kernel().end_keyed_ask("PerKey", "A", "A").unwrap();
        assert_eq!(
            held(&server, "A"),
            None,
            "the stream ended, so the quiet row is dropped"
        );
    }

    #[test]
    fn control_the_end_of_another_key_leaves_the_rows_pending_ask() {
        let server = EmbeddedServer::new(support_schema(cancelling_program())).unwrap();
        run_deferring(&server, "GO", json!({ "key": "A", "n": 1 }));
        server
            .kernel()
            .end_keyed_ask("PerKey", "A", "other")
            .unwrap();
        server.kernel().end_keyed_ask("PerKey", "B", "A").unwrap();
        assert_eq!(
            held(&server, "A").as_deref(),
            Some("idle"),
            "A's own key has not ended"
        );
    }

    #[test]
    fn a_keyed_row_belongs_to_the_session_that_made_it_and_goes_with_it() {
        let server = EmbeddedServer::new(support_schema(schema(per_payload_key(), false))).unwrap();
        let stream = server.open_session_stream("tab-a");
        let go = orbital_core_runtime::effects::response::EventRequest {
            event: "GO".to_string(),
            target_trait: Some("Caller".to_string()),
            payload: Some(val(json!({ "key": "A", "n": 1 }))),
            client_id: Some("tab-a".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            server.start("CallerOrbital", go).unwrap(),
            ServerProgress::Done(_)
        ));
        assert!(
            server
                .kernel()
                .frame_sessions()
                .contains(&"tab-a".to_string()),
            "the busy row's frame is the session's"
        );
        assert!(server.close_session_stream(stream));
        assert!(
            !server
                .kernel()
                .frame_sessions()
                .contains(&"tab-a".to_string()),
            "ending the session dropped its keyed row"
        );
    }

    fn done(progress: ServerProgress) -> orbital_core_runtime::effects::response::EventResponse {
        match progress {
            ServerProgress::Done(response) => response,
            _ => panic!("the dispatch needs no host ask"),
        }
    }

    fn caller_request(
        event: &str,
        payload: Json,
    ) -> orbital_core_runtime::effects::response::EventRequest {
        orbital_core_runtime::effects::response::EventRequest {
            event: event.to_string(),
            target_trait: Some("Caller".to_string()),
            payload: Some(val(payload)),
            ..Default::default()
        }
    }

    #[test]
    fn a_keyed_rows_emit_carries_its_row_in_the_stamp_and_its_drop_is_reported_as_a_frame_removal()
    {
        let server = EmbeddedServer::new(support_schema(schema(per_payload_key(), false))).unwrap();
        let sent = done(
            server
                .start(
                    "CallerOrbital",
                    caller_request("GO", json!({ "key": "A", "n": 1 })),
                )
                .unwrap(),
        );
        let stamp = sent
            .emitted_events
            .iter()
            .find(|e| e.event == "SENT")
            .and_then(|e| e.source.clone())
            .unwrap();
        assert_eq!(stamp.instance_key.as_deref(), Some("A"));
        let caller = sent
            .emitted_events
            .iter()
            .find(|e| e.event == "SEND")
            .and_then(|e| e.source.clone())
            .unwrap();
        assert_eq!(
            caller.instance_key, None,
            "an unkeyed emitter carries no row"
        );
        let frame = |ops: &[orbital_core_runtime::effects::FrameOp]| {
            ops.iter().find_map(|op| match op {
                orbital_core_runtime::effects::FrameOp::Del { frame } => Some(frame.clone()),
                _ => None,
            })
        };
        assert_eq!(
            frame(
                &sent
                    .frames
                    .as_ref()
                    .map(|f| f.ops.clone())
                    .unwrap_or_default()
            ),
            None
        );
        let replied = done(
            server
                .start(
                    "CallerOrbital",
                    caller_request("BACK", json!({ "key": "A" })),
                )
                .unwrap(),
        );
        let ops = replied.frames.expect("the drop is reported").ops;
        assert_eq!(frame(&ops).as_deref(), Some("PerKey\u{1e}A"), "{ops:?}");
    }
}
