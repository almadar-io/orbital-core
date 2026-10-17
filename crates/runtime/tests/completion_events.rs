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
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
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
    let k = kernel(json!(["call-service", "webhook", "send", { "texts": ["a"] }, options()]), with_service(false));
    let (state, got) = after_go(&k);
    assert_eq!(state, "done");
    assert_eq!(got, json!({ "embeddings": [1.0] }));
}

#[test]
fn a_call_service_failure_fires_its_failure_event_with_the_error() {
    let k = kernel(json!(["call-service", "webhook", "send", {}, options()]), with_service(true));
    let (state, got) = after_go(&k);
    assert_eq!(state, "failed");
    assert_eq!(got, json!({ "error": "rate limited" }));
}

#[test]
fn control_a_call_service_without_emit_fires_nothing() {
    let k = kernel(json!(["call-service", "webhook", "send", {}]), with_service(false));
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
// (`packages/almadar-parity/fixtures/agent-ports/vectors.json`) through the kernel, the same fixture and the
// same emitted trace as the JS twin (`almadar-runtime/test/agent-ports-effects.test.ts`).
mod agent_ports {
    use super::*;
    use orbital_core_runtime::agent_ports::dispatch_outcome;
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

    /// Plays the host of a `program/dispatch`: a scripted response per event, answered as the host answers.
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
            Ok(dispatch_outcome(&response))
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
