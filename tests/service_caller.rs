// A `call-service` carries its caller: the dispatch's `@user` as `{ principal: id, role }`, as the JS
// runtime passes `{ principal: user.id, role: user.role }` (`effect-stage.ts`). Providers that act
// for a person (workspace, connections, credentials) read it instead of a host-supplied guess.

use orbital_core::effects::{ServiceCaller, ServiceHandler};
use orbital_core::runtime::{EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;

#[derive(Default)]
struct Recording {
    callers: Mutex<Vec<Option<ServiceCaller>>>,
}

impl ServiceHandler for Recording {
    fn call(
        &self,
        _service: &str,
        _method: &str,
        _args: &Value,
        _key: Option<&str>,
        caller: Option<&ServiceCaller>,
    ) -> Result<Value, String> {
        self.callers.lock().push(caller.cloned());
        Ok(Value::Null)
    }
    fn cancel(&self, _key: &str) -> Result<(), String> {
        Ok(())
    }
}

fn kernel(handler: Arc<Recording>) -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Run", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Runner",
                "linkedEntity": "Run",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [
                        { "from": "idle", "to": "idle", "event": "GO", "effects": [["call-service", "workspace", "tree", {}]] }
                    ]
                }
            }]
        }]
    }))
    .unwrap();
    let config = KernelConfig {
        service_handler: Some(handler as Arc<dyn ServiceHandler>),
        ..KernelConfig::default()
    };
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        config,
    );
    kernel.register_schema(schema).unwrap();
    kernel
}

fn caller_of(user: Option<serde_json::Value>) -> Option<ServiceCaller> {
    let handler = Arc::new(Recording::default());
    let k = kernel(Arc::clone(&handler));
    if let Some(user) = user {
        k.set_current_user(serde_json::from_value(user).unwrap());
    }
    k.dispatch("Runner", None, "GO", Value::Null).unwrap();
    let callers = handler.callers.lock();
    assert_eq!(callers.len(), 1);
    callers[0].clone()
}

#[test]
fn the_signed_in_viewer_is_the_caller() {
    assert_eq!(
        caller_of(Some(json!({ "id": "u1", "role": "admin", "email": "u1@x" }))),
        Some(ServiceCaller { principal: "u1".into(), role: Some("admin".into()) })
    );
}

#[test]
fn edge_a_viewer_without_a_role_has_none() {
    assert_eq!(
        caller_of(Some(json!({ "id": "u2" }))),
        Some(ServiceCaller { principal: "u2".into(), role: None })
    );
}

#[test]
fn control_nobody_signed_in_is_no_caller() {
    assert_eq!(caller_of(Some(json!(null))), None);
}

#[test]
fn edge_a_viewer_without_an_id_is_no_caller() {
    assert_eq!(caller_of(Some(json!({ "role": "admin" }))), None);
}
