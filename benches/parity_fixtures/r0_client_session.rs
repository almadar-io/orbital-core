//! R0-client's driver: `embed::EmbeddedKernel` in-process, the client-role kernel R1, R2 and R3
//! embed. A server leg is returned in the wire rather than run, so its traces are the reference
//! for those three rows; R0 (server role) stays the reference for B1. `states` and the trace
//! shape match `r0_session` and R1.

use crate::scenario_bench::{Driver, Fixture, Scenario, Session, Step};
use orbital_core::embed::EmbeddedKernel;
use orbital_core::{OrbitalSchema, Value};
use serde_json::{json, Map, Value as Json};
use std::collections::HashMap;

fn default_viewer() -> Value {
    serde_json::from_value(json!({
        "id": "viewer-1", "name": "Dev Viewer", "email": "viewer@example.com", "role": ""
    }))
    .expect("default viewer is a Value")
}

pub struct R0ClientDriver;

pub struct EmbeddedSession {
    kernel: EmbeddedKernel,
    orbital_traits: Vec<String>,
}

impl Driver for R0ClientDriver {
    type Open = EmbeddedSession;

    fn open(&self, fixture: &Fixture, scenario: &Scenario) -> Result<EmbeddedSession, String> {
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
        let kernel = EmbeddedKernel::new(schema, "bench")
            .map_err(|e| format!("{}: EmbeddedKernel::new: {e}", fixture.name))?;
        kernel.set_current_user(default_viewer());
        Ok(EmbeddedSession {
            kernel,
            orbital_traits,
        })
    }
}

impl Session for EmbeddedSession {
    fn dispatch(&self, step: &Step) -> Result<Json, String> {
        let payload: Value = match &step.payload {
            Some(p) => serde_json::from_value(p.clone()).map_err(|e| format!("payload: {e}"))?,
            None => Value::Object(Default::default()),
        };
        let wire = self
            .kernel
            .dispatch(&step.trait_name, None, &step.event, payload)
            .map_err(|e| format!("{}.{}: {e}", step.trait_name, step.event))?;
        let wire = serde_json::to_value(&wire).map_err(|e| e.to_string())?;
        let mut states = Map::new();
        for name in &self.orbital_traits {
            let state = self.kernel.get_state(name, Some("singleton"));
            if !state.is_empty() {
                states.insert(name.clone(), Json::String(state));
            }
        }
        if !states.contains_key(&step.trait_name) {
            return Err(format!(
                "trait \"{}\" has no state after the dispatch",
                step.trait_name
            ));
        }
        let response = &wire["response"];
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
