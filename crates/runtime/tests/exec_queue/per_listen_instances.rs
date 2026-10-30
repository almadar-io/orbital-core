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
