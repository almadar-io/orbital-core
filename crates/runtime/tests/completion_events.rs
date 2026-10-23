// A `call-service` or substrate op's declared `emit` completes it (drift D20): `success` carries the
// service result (call-service) or `{ result }` (substrate), `failure` carries `{ error }`, as JS
// `EffectExecutor.ts` does. Twin of the JS runtime's call-service / program-effect emit tests.

use orbital_core_base::value::ValueMap;
use orbital_core_runtime::effects::{ServiceCaller, ServiceHandler, SubstrateFailure, SubstrateHandler};
use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::config::KernelConfig, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

struct Llm {
    fail: bool,
}

impl ServiceHandler for Llm {
    fn call(&self, _service: &str, _method: &str, _args: &Value, _key: Option<&str>, _caller: Option<&ServiceCaller>) -> Result<Value, String> {
        if self.fail {
            return Err("rate limited".to_string());
        }
        let mut out = ValueMap::new();
        out.insert("embeddings".to_string(), Value::Array(vec![Value::Number(1.0)]));
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
            return Err(SubstrateFailure { message: format!("{op} refused"), errors: vec![Value::String("ORB_X".into())] });
        }
        Ok(Value::String("ok".into()))
    }
}

fn kernel(effect: serde_json::Value, config: KernelConfig) -> RuntimeKernel {
    kernel_over(effect, config, Arc::new(InMemoryEntityCache::new()))
}

fn kernel_over(effect: serde_json::Value, config: KernelConfig, source: Arc<dyn EntitySource>) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
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
    }))
    .unwrap();
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        source,
        config,
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn after_go(kernel: &RuntimeKernel) -> (String, serde_json::Value) {
    kernel.dispatch("Runner", None, "GO", Value::Null).unwrap();
    let snapshot = kernel.snapshot_trait("Runner", None);
    let entity = serde_json::to_value(&snapshot.entity).unwrap();
    (snapshot.state, entity["got"].clone())
}

const EMIT: &str = r#"{"emit": {"success": "DONE", "failure": "FAILED"}}"#;

fn options() -> serde_json::Value {
    serde_json::from_str(EMIT).unwrap()
}

fn with_service(fail: bool) -> KernelConfig {
    KernelConfig { service_handler: Some(Arc::new(Llm { fail })), ..KernelConfig::default() }
}

fn with_host(fail: bool) -> KernelConfig {
    KernelConfig { substrate_handler: Some(Arc::new(Host { fail })), ..KernelConfig::default() }
}

#[test]
fn a_call_service_success_fires_its_success_event_with_the_result() {
    let k = kernel(json!(["call-service", "test-svc", "run", { "texts": ["a"] }, options()]), with_service(false));
    let (state, got) = after_go(&k);
    assert_eq!(state, "done");
    assert_eq!(got, json!({ "embeddings": [1.0] }));
}

#[test]
fn a_call_service_failure_fires_its_failure_event_with_the_error() {
    let k = kernel(json!(["call-service", "test-svc", "run", {}, options()]), with_service(true));
    let (state, got) = after_go(&k);
    assert_eq!(state, "failed");
    assert_eq!(got, json!({ "error": "rate limited" }));
}

#[test]
fn control_a_call_service_without_emit_fires_nothing() {
    let k = kernel(json!(["call-service", "test-svc", "run", {}]), with_service(false));
    assert_eq!(after_go(&k).0, "idle");
}

#[test]
fn a_substrate_op_success_fires_its_success_event_with_the_result() {
    let k = kernel(json!(["program/eval", { "name": "P", "orbitals": [] }, { "final": true }, options()]), with_host(false));
    let (state, got) = after_go(&k);
    assert_eq!(state, "done");
    assert_eq!(got, json!({ "result": "ok" }));
}

#[test]
fn a_substrate_op_failure_carries_the_error_and_its_issues() {
    let k = kernel(json!(["program/eval", { "name": "P", "orbitals": [] }, options()]), with_host(true));
    let (state, got) = after_go(&k);
    assert_eq!(state, "failed");
    assert_eq!(got, json!({ "error": "program/eval refused", "errors": ["ORB_X"] }));
}

#[test]
fn edge_a_substrate_op_on_a_server_with_no_host_fails_loudly() {
    let k = kernel(json!(["program/eval", { "name": "P", "orbitals": [] }, options()]), KernelConfig::default());
    let (state, got) = after_go(&k);
    assert_eq!(state, "failed");
    assert_eq!(got, json!({ "error": "no substrate host configured for program/eval" }));
}

// program/inputs, program/dispatch and persist/read-named (EXEC-AGENT-1): the shared vectors
// (`crates/runtime/tests/fixtures/agent-ports/vectors.json`) through the kernel, the same fixture and the
// same emitted trace as the JS twin (`almadar-runtime/test/agent-ports-effects.test.ts`).
mod agent_ports {
    use super::*;
    use orbital_core_runtime::effects::response::EventResponse;
    use parking_lot::Mutex;
    use serde_json::Value as Json;

    const VECTORS: &str = include_str!("fixtures/agent-ports/vectors.json");

    fn norm(json: Json) -> Json {
        match json {
            Json::Number(n) => match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Json::from(f as i64),
                _ => Json::Number(n),
            },
            Json::Array(items) => Json::Array(items.into_iter().map(norm).collect()),
            Json::Object(map) => Json::Object(map.into_iter().map(|(k, v)| (k, norm(v))).collect()),
            other => other,
        }
    }

    fn as_value(json: &Json) -> Value {
        serde_json::from_value::<Value>(json.clone()).expect("a value")
    }

    /// Plays the host of a `program/dispatch`: a scripted response per event, answered raw as the host answers (the kernel shapes it).
    struct Dispatching {
        asked: Mutex<Vec<Vec<Value>>>,
        scripted: Json,
    }

    impl SubstrateHandler for Dispatching {
        fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
            assert_eq!(op, "program/dispatch", "the kernel asks its host for a dispatch and nothing else");
            self.asked.lock().push(args.to_vec());
            let address = args[0].as_str().expect("a qualified address");
            let event = address.rsplit('.').next().expect("an event");
            let response: EventResponse =
                serde_json::from_value(self.scripted[event].clone()).expect("a scripted response");
            Ok(as_value(&serde_json::to_value(&response).expect("serializes")))
        }
    }

    fn schema_with(fixture: &Json, effect: Json) -> OrbitalSchema {
        let mut fixture = fixture.clone();
        let chat = &mut fixture["schema"]["orbitals"][0]["traits"][0]["stateMachine"];
        chat["states"].as_array_mut().unwrap().extend([json!({ "name": "done" }), json!({ "name": "failed" })]);
        chat["events"].as_array_mut().unwrap().extend([
            json!({ "key": "GO", "name": "Go" }),
            json!({ "key": "DONE", "name": "Done" }),
            json!({ "key": "FAILED", "name": "Failed" }),
        ]);
        chat["transitions"].as_array_mut().unwrap().extend([
            json!({ "from": "idle", "to": "idle", "event": "GO", "effects": [effect] }),
            json!({ "from": "idle", "to": "done", "event": "DONE", "effects": [["set", "@entity.got", "@payload"]] }),
            json!({ "from": "idle", "to": "failed", "event": "FAILED", "effects": [["set", "@entity.got", "@payload"]] }),
        ]);
        serde_json::from_value(fixture["schema"].clone()).expect("the vectors' schema")
    }

    fn play(fixture: &Json, case: &Json, host: Option<Arc<Dispatching>>) -> (String, Json) {
        let mut effect = vec![json!(case["op"])];
        effect.extend(case["args"].as_array().unwrap().iter().cloned());
        effect.push(options());
        let cache = Arc::new(InMemoryEntityCache::new());
        for row in fixture["rows"]["Note"].as_array().unwrap() {
            cache.create("Note", &as_value(row)).unwrap();
        }
        let config = KernelConfig {
            substrate_handler: host.map(|h| h as Arc<dyn SubstrateHandler>),
            ..KernelConfig::default()
        };
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, cache as Arc<dyn EntitySource>, config);
        kernel.register_schema(schema_with(fixture, Json::Array(effect))).unwrap();
        kernel.dispatch("Chat", None, "GO", Value::Null).unwrap();
        let snapshot = kernel.snapshot_trait("Chat", None);
        let entity = serde_json::to_value(&snapshot.entity).unwrap();
        (snapshot.state, norm(entity["got"].clone()))
    }

    #[test]
    fn every_shared_vector_emits_the_same_trace_through_the_kernel() {
        let fixture: Json = serde_json::from_str(VECTORS).expect("the shared vectors parse");
        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() > 30, "the corpus carries the three operators with their controls");
        for case in cases {
            let name = case["name"].as_str().unwrap_or_default();
            let host = Arc::new(Dispatching { asked: Mutex::new(Vec::new()), scripted: fixture["scripted"].clone() });
            let (state, got) = play(&fixture, case, Some(host.clone()));
            let expect = &case["expect"];
            if expect["ok"] == true {
                assert_eq!(state, "done", "{name}");
                assert_eq!(got["result"], norm(expect["result"].clone()), "{name}");
                let text = got["text"].as_str().unwrap_or_default();
                let reparsed: Json = serde_json::from_str(text).unwrap_or(Json::Null);
                assert_eq!(norm(reparsed), got["result"], "{name}: text is the result as JSON text");
                if let Some(exact) = case["text"].as_str() {
                    assert_eq!(text, exact, "{name}: the exact text, as the JS twin writes it");
                }
                assert_eq!(got.as_object().map(|o| o.len()), Some(2), "{name}: result and text only");
            } else {
                assert_eq!(state, "failed", "{name}");
                let mut failure = expect.clone();
                failure.as_object_mut().unwrap().remove("ok");
                assert_eq!(got, norm(failure), "{name}");
            }
            let asked: Vec<Json> = host
                .asked
                .lock()
                .iter()
                .map(|args| norm(serde_json::to_value(args).unwrap()))
                .collect();
            let wanted: Vec<Json> = match &case["dispatched"] {
                Json::Null => Vec::new(),
                dispatched => vec![norm(json!([dispatched["address"], dispatched["payload"]]))],
            };
            assert_eq!(asked, wanted, "{name}: what the host was asked to dispatch");
        }
    }

    #[test]
    fn edge_a_dispatch_with_no_host_fails_loudly_and_asks_nothing() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let case = json!({ "op": "program/dispatch", "args": ["Desk.Chat.ADD_NOTE", { "title": "x" }] });
        let (state, got) = play(&fixture, &case, None);
        assert_eq!(state, "failed");
        assert_eq!(got, json!({ "error": "no substrate host configured for program/dispatch" }));
    }

    #[test]
    fn control_a_refused_dispatch_never_reaches_a_missing_host() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let case = json!({ "op": "program/dispatch", "args": ["Desk.Chat.SECRET", {}] });
        let (state, got) = play(&fixture, &case, None);
        assert_eq!(state, "done");
        assert_eq!(got["result"], json!({ "ok": false, "error": "'Desk.Chat.SECRET' is not a declared external input" }));
    }

    fn run_effect(fixture: &Json, effect: Json, payload: Json) -> (String, Json) {
        let cache = Arc::new(InMemoryEntityCache::new());
        for row in fixture["rows"]["Note"].as_array().unwrap() {
            cache.create("Note", &as_value(row)).unwrap();
        }
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, cache as Arc<dyn EntitySource>, KernelConfig::default());
        kernel.register_schema(schema_with(fixture, effect)).unwrap();
        kernel.dispatch("Chat", None, "GO", as_value(&payload)).unwrap();
        let snapshot = kernel.snapshot_trait("Chat", None);
        let entity = serde_json::to_value(&snapshot.entity).unwrap();
        (snapshot.state, norm(entity["got"].clone()))
    }

    #[test]
    fn an_unresolved_binding_fails_the_call_as_it_does_on_the_js_path() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        for (op, args) in [
            ("persist/read-named", json!(["@entity.unset"])),
            ("program/dispatch", json!(["@payload.address", {}])),
            ("program/inputs", json!([{ "select": [{ "event": "@payload.event" }] }])),
        ] {
            let mut effect = vec![json!(op)];
            effect.extend(args.as_array().unwrap().iter().cloned());
            effect.push(options());
            let (state, got) = run_effect(&fixture, Json::Array(effect), json!({}));
            assert_eq!(state, "failed", "{op}");
            assert_eq!(got, json!({ "error": format!("{op}: arguments must be JSON data") }), "{op}");
        }
    }

    #[test]
    fn control_a_binding_the_payload_resolves_is_used() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let effect = json!(["persist/read-named", "@payload.entity", options()]);
        let (state, got) = run_effect(&fixture, effect, json!({ "entity": "Tag" }));
        assert_eq!(state, "done");
        assert_eq!(got["result"], json!({ "entity": "Tag", "count": 0, "rows": [] }));
    }

    fn keyed(key: Json) -> Json {
        let mut options = options();
        options["key"] = key;
        options
    }

    #[test]
    fn a_keyed_ops_completion_echoes_its_key_on_success() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let effect = json!(["persist/read-named", "Tag", keyed(json!("k1"))]);
        let (state, got) = run_effect(&fixture, effect, json!({}));
        assert_eq!(state, "done");
        assert_eq!(got["key"], json!("k1"));
        assert_eq!(got["result"], json!({ "entity": "Tag", "count": 0, "rows": [] }));
    }

    #[test]
    fn a_keyed_ops_completion_echoes_its_key_on_failure() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let effect = json!(["program/dispatch", "Desk.Chat.ADD_NOTE", { "title": "x" }, keyed(json!("k1"))]);
        let (state, got) = run_effect(&fixture, effect, json!({}));
        assert_eq!(state, "failed");
        assert_eq!(got, json!({ "error": "no substrate host configured for program/dispatch", "key": "k1" }));
    }

    #[test]
    fn control_an_unkeyed_ops_completion_carries_no_key() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let effect = json!(["persist/read-named", "Tag", options()]);
        let (state, got) = run_effect(&fixture, effect, json!({}));
        assert_eq!(state, "done");
        assert_eq!(got.as_object().map(|o| o.contains_key("key")), Some(false));
    }

    #[test]
    fn edge_a_bound_key_is_echoed_resolved() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let effect = json!(["persist/read-named", "Tag", keyed(json!("@payload.k"))]);
        let (state, got) = run_effect(&fixture, effect, json!({ "k": "from-payload" }));
        assert_eq!(state, "done");
        assert_eq!(got["key"], json!("from-payload"));
    }

    /// `GO` runs a keyed `program/dispatch` against a host answering through `Progress::Await`; `answer` settles its ask.
    fn keyed_dispatch_answered(answer: orbital_core_runtime::resumable::IoAnswer) -> (Option<String>, bool, String, Json) {
        keyed_op_answered(json!(["program/dispatch", "Desk.Chat.ADD_NOTE", { "title": "x" }, keyed(json!("k1"))]), answer)
    }

    /// `GO` runs `effect` against a host answering through `Progress::Await`: the key and route its substrate ask
    /// carried, and the trait's state and `got` once `answer` settled the ask.
    fn keyed_op_answered(effect: Json, answer: orbital_core_runtime::resumable::IoAnswer) -> (Option<String>, bool, String, Json) {
        use orbital_core_runtime::resumable::{BlockingHost, IoReply, IoRequest, Progress};
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let config = KernelConfig {
            substrate_handler: Some(Arc::new(orbital_core_runtime::resumable::HostAnswered) as Arc<dyn SubstrateHandler>),
            ..KernelConfig::default()
        };
        let store = Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>;
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, store.clone(), config);
        kernel.register_schema(schema_with(&fixture, effect)).unwrap();
        let mut progress = kernel.start_dispatch("Chat", None, "GO", Value::Null, None, None).unwrap();
        let mut asked = None;
        while let Progress::Await { continuation, requests } = progress {
            let replies = requests
                .iter()
                .map(|p| IoReply {
                    id: p.id,
                    answer: match &p.request {
                        IoRequest::Substrate { key, site, .. } => {
                            asked = Some((key.clone(), site.as_ref().is_some_and(|s| s.emit.success.as_deref() == Some("DONE"))));
                            answer.clone()
                        }
                        other => BlockingHost { source: store.as_ref(), service: None, substrate: None }.answer(other),
                    },
                })
                .collect();
            progress = kernel.resume(continuation, replies).unwrap();
        }
        let (key, routed) = asked.expect("GO asked its host for the op");
        let snapshot = kernel.snapshot_trait("Chat", None);
        let entity = serde_json::to_value(&snapshot.entity).unwrap();
        (key, routed, snapshot.state, norm(entity["got"].clone()))
    }

    #[test]
    fn a_keyed_ask_the_host_defers_commits_without_its_completion() {
        let (key, routed, state, got) = keyed_dispatch_answered(orbital_core_runtime::resumable::IoAnswer::ServiceDeferred);
        assert_eq!(key.as_deref(), Some("k1"), "the ask carries its key");
        assert!(routed, "the ask carries the call site its completion dispatches to");
        assert_eq!(state, "idle", "the completion arrives as a dispatch of its own, not in this one");
        assert_eq!(got, Json::Null);
    }

    #[test]
    fn control_a_keyed_ask_answered_in_place_completes_inside_the_dispatch() {
        let mut response = EventResponse::from_process_results(&[], "Notes", "ADD_NOTE");
        response.transitioned = true;
        let answer = orbital_core_runtime::resumable::IoAnswer::Substrate(Ok(as_value(&serde_json::to_value(&response).unwrap())));
        let (_, _, state, got) = keyed_dispatch_answered(answer);
        assert_eq!(state, "done");
        assert_eq!(got["key"], json!("k1"));
        assert_eq!(got["result"], json!({ "ok": true, "emitted": [] }));
    }

    #[test]
    fn a_keyed_substrate_op_the_host_defers_commits_without_its_completion() {
        let effect = json!(["program/eval", { "name": "App", "orbitals": [] }, keyed(json!("k1"))]);
        let (key, routed, state, got) = keyed_op_answered(effect, orbital_core_runtime::resumable::IoAnswer::ServiceDeferred);
        assert_eq!(key.as_deref(), Some("k1"), "the substrate ask carries its key");
        assert!(routed, "and the call site its completion dispatches to");
        assert_eq!(state, "idle", "the answer arrives later as a dispatch of its own");
        assert_eq!(got, Json::Null);
    }

    #[test]
    fn control_an_unkeyed_substrate_op_is_answered_inline() {
        let effect = json!(["program/eval", { "name": "App", "orbitals": [] }, options()]);
        let answer = orbital_core_runtime::resumable::IoAnswer::Substrate(Ok(as_value(&json!({ "valid": true }))));
        let (key, routed, state, got) = keyed_op_answered(effect, answer);
        assert_eq!((key, routed), (None, false), "an unkeyed ask carries neither key nor site");
        assert_eq!(state, "done", "its completion fires inside the asking dispatch");
        assert_eq!(got, json!({ "result": { "valid": true } }), "with no key beside the result");
    }

    fn read_as(user: Json) -> Json {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "Policy",
            "orbitals": [{
                "name": "Main",
                "entity": {
                    "name": "Ticket", "persistence": "persistent",
                    "fields": [{ "name": "id", "type": "string" }, { "name": "owner", "type": "string" }],
                    "read_policy": ["=", ["object/get", "@entity", "owner"], "@user.id"]
                },
                "traits": [{
                    "name": "Reader", "scope": "instance", "linkedEntity": "Ticket",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "done" }, { "name": "failed" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "GO", "effects": [["persist/read-named", "Ticket", options()]] },
                            { "from": "idle", "to": "done", "event": "DONE", "effects": [["set", "@entity.got", "@payload"]] },
                            { "from": "idle", "to": "failed", "event": "FAILED", "effects": [["set", "@entity.got", "@payload"]] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        let cache = Arc::new(InMemoryEntityCache::new());
        for row in [
            json!({ "id": "t1", "owner": "u1" }),
            json!({ "id": "t2", "owner": "u2" }),
            json!({ "id": "t3", "owner": "u1" }),
        ] {
            cache.write("Ticket", row["id"].as_str().unwrap(), &as_value(&row)).unwrap();
        }
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, cache as Arc<dyn EntitySource>, KernelConfig::default());
        kernel.register_schema(schema).unwrap();
        kernel.set_current_user(serde_json::from_value(user).unwrap());
        kernel.dispatch("Reader", None, "GO", Value::Null).unwrap();
        let entity = serde_json::to_value(&kernel.snapshot_trait("Reader", None).entity).unwrap();
        norm(entity["got"].clone())
    }

    #[test]
    fn a_named_read_returns_only_the_rows_the_viewer_may_read() {
        assert_eq!(
            read_as(json!({ "id": "u1" }))["result"],
            json!({ "entity": "Ticket", "count": 2, "rows": [{ "id": "t1", "owner": "u1" }, { "id": "t3", "owner": "u1" }] })
        );
        assert_eq!(
            read_as(json!({ "id": "u2" }))["result"],
            json!({ "entity": "Ticket", "count": 1, "rows": [{ "id": "t2", "owner": "u2" }] })
        );
    }

    #[test]
    fn control_a_viewer_who_owns_nothing_reads_no_rows() {
        let read = read_as(json!({ "id": "u3" }));
        assert_eq!(read["result"], json!({ "entity": "Ticket", "count": 0, "rows": [] }));
        assert_eq!(read["text"], json!("{\"entity\":\"Ticket\",\"count\":0,\"rows\":[]}"));
    }

    /// A host that records every ask and answers each with what `answer` gives.
    struct Recording {
        asked: Mutex<Vec<(String, Json)>>,
        answer: fn(&str, &Json) -> Json,
    }

    impl SubstrateHandler for Recording {
        fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
            let args = norm(serde_json::to_value(args).unwrap());
            self.asked.lock().push((op.to_string(), args.clone()));
            Ok(as_value(&(self.answer)(op, &args)))
        }
    }

    fn run_hosted(fixture: &Json, effect: Json, host: Option<Arc<Recording>>) -> (String, Json) {
        let config = KernelConfig { substrate_handler: host.map(|h| h as Arc<dyn SubstrateHandler>), ..KernelConfig::default() };
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>, config);
        kernel.register_schema(schema_with(fixture, effect)).unwrap();
        kernel.dispatch("Chat", None, "GO", Value::Null).unwrap();
        let snapshot = kernel.snapshot_trait("Chat", None);
        (snapshot.state, norm(serde_json::to_value(&snapshot.entity).unwrap()["got"].clone()))
    }

    #[test]
    fn an_inbound_dispatch_asks_the_host_to_deliver_and_answers_the_handlers_status_and_body() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let host = Arc::new(Recording { asked: Mutex::new(Vec::new()), answer: |_, _| json!({ "status": 202, "body": "queued" }) });
        let effect = json!(["program/dispatch", { "inbound": "github", "body": "{}", "headers": { "x-sig": "s" } }, options()]);
        let (state, got) = run_hosted(&fixture, effect, Some(host.clone()));
        assert_eq!(state, "done", "{got}");
        assert_eq!(got["result"], json!({ "status": 202, "body": "queued" }));
        assert_eq!(
            host.asked.lock().clone(),
            [("program/dispatch".to_string(), json!([{ "inbound": "github", "body": "{}", "headers": { "x-sig": "s" } }]))],
            "the host is asked for the delivery, never an address"
        );
        let refused = json!(["program/dispatch", { "inbound": "github", "verb": "POST" }, options()]);
        let host = Arc::new(Recording { asked: Mutex::new(Vec::new()), answer: |_, _| json!(null) });
        let (state, _) = run_hosted(&fixture, refused, Some(host.clone()));
        assert_eq!(state, "failed", "control: a malformed delivery fails before the host is asked");
        assert!(host.asked.lock().is_empty());
    }

    #[test]
    fn a_dispatch_event_request_asks_the_host_with_its_options_and_answers_what_it_answers() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let host = Arc::new(Recording { asked: Mutex::new(Vec::new()), answer: |_, _| json!({ "success": true, "transitioned": false }) });
        let request = json!({ "wire": true, "orbital": "Desk", "targetTrait": "Chat", "event": "SECRET", "viewer": { "id": "u1" } });
        let effect = json!(["program/dispatch", "Desk.Chat.SECRET", { "title": "x" }, request.clone(), options()]);
        let (state, got) = run_hosted(&fixture, effect, Some(host.clone()));
        assert_eq!(state, "done", "{got}");
        assert_eq!(got["result"], json!({ "success": true, "transitioned": false }));
        assert_eq!(
            host.asked.lock().clone(),
            [("program/dispatch".to_string(), json!(["Desk.Chat.SECRET", { "title": "x" }, request]))],
            "an event request is not restricted to declared inputs: the host routes its options"
        );
        let host = Arc::new(Recording { asked: Mutex::new(Vec::new()), answer: |_, _| json!(null) });
        let (state, refused) = run_hosted(&fixture, json!(["program/dispatch", "Desk.Chat.SECRET", {}, { "verb": "POST", "orbital": "Desk", "event": "SECRET" }, options()]), Some(host.clone()));
        assert_eq!((state.as_str(), refused["error"].clone()), ("failed", json!("program/dispatch: unsupported option \"verb\"")));
        assert!(host.asked.lock().is_empty(), "control: a refused option asks nothing");
    }

    #[test]
    fn program_inputs_answers_the_hosts_last_trace_steps() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let host = Arc::new(Recording { asked: Mutex::new(Vec::new()), answer: |_, _| json!([{ "seq": 1 }]) });
        let (state, got) = run_hosted(&fixture, json!(["program/inputs", { "trace": 2 }, options()]), Some(host.clone()));
        assert_eq!(state, "done", "{got}");
        assert_eq!(got["result"]["trace"], json!([{ "seq": 1 }]));
        assert_eq!(host.asked.lock().clone(), [("program/inputs".to_string(), json!([{ "trace": 2 }]))], "only the trace is asked of the host");
        let (_, plain) = run_hosted(&fixture, json!(["program/inputs", options()]), None);
        for key in ["state", "stats", "trace"] {
            assert!(plain["result"].get(key).is_none(), "control: no option, no {key}: {plain}");
        }
        let (state, hostless) = run_hosted(&fixture, json!(["program/inputs", { "trace": 1 }, options()]), None);
        assert_eq!((state.as_str(), hostless["error"].clone()), ("failed", json!("no substrate host configured for program/inputs")));
    }

    /// `Desk.Chat` moves idle -> busy on `GO`, whose effect reads the running states; `STAY` fires nothing.
    fn state_reading_kernel() -> RuntimeKernel {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "Desk",
            "orbitals": [{
                "name": "Desk",
                "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "got", "type": "string" }] },
                "traits": [{
                    "name": "Chat", "linkedEntity": "Note",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "busy" }],
                        "transitions": [
                            { "from": "idle", "to": "busy", "event": "GO", "effects": [["program/inputs", { "state": "Desk" }, { "emit": { "success": "READ" } }]] },
                            { "from": "busy", "to": "busy", "event": "STAY", "effects": [["program/inputs", { "state": "Desk" }, { "emit": { "success": "READ" } }]] },
                            { "from": "busy", "to": "busy", "event": "READ", "effects": [["set", "@entity.got", ["json/stringify", "@payload.result"]]] }
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Server, Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>, KernelConfig::default());
        kernel.register_schema(schema).unwrap();
        kernel
    }

    #[test]
    fn an_effect_reading_the_states_sees_the_transition_it_belongs_to_committed() {
        let kernel = state_reading_kernel();
        kernel.dispatch("Chat", None, "GO", Value::Null).unwrap();
        let snapshot = kernel.snapshot_trait("Chat", None);
        let got: Json = serde_json::from_str(serde_json::to_value(&snapshot.entity).unwrap()["got"].as_str().unwrap_or("null")).unwrap();
        let chat = got["state"]["traits"].as_array().and_then(|t| t.iter().find(|t| t["trait"] == "Chat").cloned()).unwrap_or(Json::Null);
        assert_eq!(chat["state"], json!("busy"), "the effect ran after its transition committed: {got}");
    }

    #[test]
    fn control_an_event_that_fires_nothing_reads_nothing() {
        let kernel = state_reading_kernel();
        kernel.dispatch("Chat", None, "STAY", Value::Null).unwrap();
        let snapshot = kernel.snapshot_trait("Chat", None);
        assert_eq!(snapshot.state, "idle");
        assert!(serde_json::to_value(&snapshot.entity).unwrap()["got"].is_null(), "no effect ran");
    }

    #[test]
    fn program_inputs_answers_the_orbitals_running_states_and_the_counters() {
        let fixture: Json = serde_json::from_str(VECTORS).unwrap();
        let (state, got) = run_hosted(&fixture, json!(["program/inputs", { "state": "Desk", "stats": true }, options()]), None);
        assert_eq!(state, "done", "{got}");
        let result = &got["result"];
        assert_eq!(result["state"]["orbital"], "Desk");
        let chat = result["state"]["traits"].as_array().unwrap().iter().find(|t| t["trait"] == "Chat").cloned().unwrap_or(Json::Null);
        assert_eq!((chat["entityId"].clone(), chat["state"].clone()), (json!("singleton"), json!("idle")), "{result}");
        assert_eq!(result["stats"]["registeredSchemas"], 1);
        let (state, unknown) = run_hosted(&fixture, json!(["program/inputs", { "state": "Nowhere" }, options()]), None);
        assert_eq!((state.as_str(), unknown["error"].clone()), ("failed", json!("program/inputs: 'Nowhere' is not a registered orbital")));
    }
}

// A substrate operator's arguments reach its host with their bindings resolved, as the JS executor resolves them
// (`resolveEffectArgs`). Pins the `Effect::Custom` path of `exec_leaf_effect`, which passes `do_custom` the raw args.
mod substrate_arguments {
    use super::*;
    use parking_lot::Mutex;

    struct Recording {
        seen: Mutex<Vec<Vec<Value>>>,
    }

    impl SubstrateHandler for Recording {
        fn run(&self, _op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
            self.seen.lock().push(args.to_vec());
            Ok(Value::String("ok".into()))
        }
    }

    #[test]
    fn a_binding_in_a_substrate_operator_argument_is_resolved_before_the_host_sees_it() {
        let host = Arc::new(Recording { seen: Mutex::new(Vec::new()) });
        let config = KernelConfig { substrate_handler: Some(host.clone() as Arc<dyn SubstrateHandler>), ..KernelConfig::default() };
        let k = kernel(json!(["program/eval", "@payload.program", { "nested": "@payload.program" }, options()]), config);
        k.dispatch("Runner", None, "GO", as_value_of(json!({ "program": "p" }))).unwrap();
        assert_eq!(
            *host.seen.lock(),
            vec![vec![Value::String("p".into()), serde_json::from_value::<Value>(json!({ "nested": "p" })).unwrap()]]
        );
    }

    fn as_value_of(json: serde_json::Value) -> Value {
        serde_json::from_value::<Value>(json).unwrap()
    }
}

struct DownStore(&'static str);

impl EntitySource for DownStore {
    fn read(&self, _: &str, _: &str) -> Result<std::sync::Arc<Value>, String> {
        Err(self.0.to_string())
    }
    fn write(&self, _: &str, _: &str, _: &Value) -> Result<Value, String> {
        Err(self.0.to_string())
    }
    fn create(&self, _: &str, _: &Value) -> Result<Value, String> {
        Err(self.0.to_string())
    }
    fn delete(&self, _: &str, _: &str) -> Result<(), String> {
        Err(self.0.to_string())
    }
    fn clear(&self) {}
    fn count(&self) -> usize {
        0
    }
    fn list(&self, _: &str) -> Result<Vec<std::sync::Arc<Value>>, String> {
        Err(self.0.to_string())
    }
}

fn fetch_failure(effect: serde_json::Value, store: Arc<dyn EntitySource>) -> serde_json::Value {
    let k = kernel_over(effect, KernelConfig::default(), store);
    let (state, got) = after_go(&k);
    assert_eq!(state, "failed", "{got}");
    got
}

#[test]
fn a_fetch_store_error_carries_the_message_and_a_null_code() {
    let got = fetch_failure(json!(["fetch", "Run", options()]), Arc::new(DownStore("store is down")));
    assert_eq!(got, json!({ "error": "store is down", "code": null }));
}

#[test]
fn a_fetch_by_id_miss_carries_the_not_found_code() {
    let effect = json!(["fetch", "Run", { "id": "missing", "emit": { "success": "DONE", "failure": "FAILED" } }]);
    let got = fetch_failure(effect, Arc::new(InMemoryEntityCache::new()));
    assert_eq!(got, json!({ "error": "Run missing not found", "code": "not-found" }));
}

#[test]
fn control_a_fetch_error_that_merely_reads_like_a_denial_carries_a_null_code() {
    let words = "@Read denied: the declared access policy for `Run` rejected this row";
    let got = fetch_failure(json!(["fetch", "Run", options()]), Arc::new(DownStore(words)));
    assert_eq!(got, json!({ "error": words, "code": null }));
}
