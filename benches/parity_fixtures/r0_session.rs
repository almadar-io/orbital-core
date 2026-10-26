//! R0's driver: the server-role `RuntimeKernel`, because B1 (`@almadar/runtime` through
//! `OrbitalServerRuntime`) is a server-role runtime and R0 must emit B1's traces; the client-role
//! `embed::EmbeddedKernel` routes server legs out instead of running them (R1 measures that role).
//! `states` lists the scenario orbital's traits the kernel holds a state for (every row reports the same way).

use crate::scenario_bench::{Driver, Fixture, Scenario, Session, Step};
use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{EventResponse, OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::{json, Map, Value as Json};
use std::sync::Arc;

/// The viewer `@almadar/core`'s `DEFAULT_VIEWER` names, which B1 runs as.
fn default_viewer() -> Value {
    serde_json::from_value(json!({
        "id": "viewer-1", "name": "Dev Viewer", "email": "viewer@example.com", "role": ""
    }))
    .expect("default viewer is a Value")
}

pub struct R0Driver;

pub struct KernelSession {
    kernel: RuntimeKernel,
    orbital_traits: Vec<String>,
}

impl Driver for R0Driver {
    type Open = KernelSession;

    fn open(&self, fixture: &Fixture, scenario: &Scenario) -> Result<KernelSession, String> {
        let schema: OrbitalSchema = serde_json::from_value(fixture.schema.clone())
            .map_err(|e| format!("{}: schema: {e}", fixture.name))?;
        let orbital_traits: Vec<String> = schema
            .orbitals
            .iter()
            .find(|o| o.name == scenario.orbital)
            .ok_or_else(|| format!("{}: no orbital \"{}\"", fixture.name, scenario.orbital))?
            .traits
            .iter()
            .filter_map(|t| t.trait_name().map(str::to_string))
            .collect();
        let kernel = RuntimeKernel::new(
            RuntimeEnvironment::Server,
            Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
            Default::default(),
        );
        kernel
            .register_schema(schema)
            .map_err(|e| format!("{}: register_schema: {e}", fixture.name))?;
        kernel.set_current_user(default_viewer());
        Ok(KernelSession {
            kernel,
            orbital_traits,
        })
    }
}

impl Session for KernelSession {
    fn dispatch(&self, step: &Step) -> Result<Json, String> {
        let payload: Value = match &step.payload {
            Some(p) => serde_json::from_value(p.clone()).map_err(|e| format!("payload: {e}"))?,
            None => Value::Object(Default::default()),
        };
        let outcomes = self
            .kernel
            .dispatch(&step.trait_name, None, &step.event, payload)
            .map_err(|e| format!("{}.{}: {e}", step.trait_name, step.event))?;
        let response =
            EventResponse::from_process_results(&outcomes, &step.trait_name, &step.event);
        let response = serde_json::to_value(&response).map_err(|e| e.to_string())?;
        let mut states = Map::new();
        for name in &self.orbital_traits {
            if let Some(state) = self.kernel.get_state(name, None) {
                states.insert(name.clone(), Json::String(state.current_state));
            }
        }
        if !states.contains_key(&step.trait_name) {
            return Err(format!(
                "trait \"{}\" is not registered under the scenario's orbital",
                step.trait_name
            ));
        }
        let emits: Vec<Json> = response["emittedEvents"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| json!({ "event": e["event"], "payload": e.get("payload").cloned().unwrap_or(Json::Null) }))
                    .collect()
            })
            .unwrap_or_default();
        let effects = response
            .get("clientEffects")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(json!({
            "trait": step.trait_name, "event": step.event,
            "states": Json::Object(states), "effects": effects, "emits": emits,
        }))
    }
}
