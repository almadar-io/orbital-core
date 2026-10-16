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
    let k = kernel(json!(["call-service", "llm", "embed", { "texts": ["a"] }, options()]), with_service(false));
    let (state, got) = after_go(&k);
    assert_eq!(state, "done");
    assert_eq!(got, json!({ "embeddings": [1.0] }));
}

#[test]
fn a_call_service_failure_fires_its_failure_event_with_the_error() {
    let k = kernel(json!(["call-service", "llm", "embed", {}, options()]), with_service(true));
    let (state, got) = after_go(&k);
    assert_eq!(state, "failed");
    assert_eq!(got, json!({ "error": "rate limited" }));
}

#[test]
fn control_a_call_service_without_emit_fires_nothing() {
    let k = kernel(json!(["call-service", "llm", "embed", {}]), with_service(false));
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
