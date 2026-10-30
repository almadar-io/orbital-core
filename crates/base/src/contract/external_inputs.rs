//! Declared external inputs — the events a client outside the program (an API
//! caller, an agent acting as the user, an MCP client) may send a trait.
//! Authored as `listens { EVENT -> input { … } }`, lowered onto the trait's
//! own `stateMachine.events[]` entry as `input: true`. Hosts gate their input
//! channel with this list and clients build tool lists from it. Mirrors
//! `@almadar/core` `src/access/externalInputs.ts`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::types::{EventDefinition, PayloadField, TraitReference};
use crate::schema::types::OrbitalSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalInput {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    pub payload_schema: Vec<PayloadField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

/// The input record for one event of one trait, or `None` when the event is
/// not a declared external input. The single constructor every consumer uses.
pub fn external_input_of(
    orbital: &str,
    trait_name: &str,
    ev: &EventDefinition,
) -> Option<ExternalInput> {
    if ev.input != Some(true) {
        return None;
    }
    Some(ExternalInput {
        orbital: orbital.to_string(),
        trait_name: trait_name.to_string(),
        event: ev.key.clone(),
        payload_schema: ev.payload_schema.clone().unwrap_or_default(),
        description: ev.description.clone(),
        synonyms: ev.synonyms.clone(),
        tier: ev.tier.clone(),
    })
}

pub fn external_inputs_of(schema: &OrbitalSchema) -> Vec<ExternalInput> {
    let mut out = Vec::new();
    for orbital in &schema.orbitals {
        for trait_ref in &orbital.traits {
            let TraitReference::Inline(td) = trait_ref else {
                continue;
            };
            out.extend(
                td.state_machine
                    .events
                    .iter()
                    .flatten()
                    .filter_map(|ev| external_input_of(&orbital.name, &td.name, ev)),
            );
        }
    }
    out
}

pub fn find_external_input(
    schema: &OrbitalSchema,
    orbital: &str,
    trait_name: &str,
    event: &str,
) -> Option<ExternalInput> {
    external_inputs_of(schema)
        .into_iter()
        .find(|i| i.orbital == orbital && i.trait_name == trait_name && i.event == event)
}

/// The entities a `call-service` provider acting as the caller may read: every persisted entity, once, in schema
/// order (each orbital's primary entity, then its auxiliaries). Rows stay filtered by each entity's `@read` policy at
/// read time.
pub fn readable_entities_of(schema: &OrbitalSchema) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for def in crate::policy::inline_entity_defs(schema) {
        if def.persistence == Some(crate::schema::types::Persistence::Runtime) || out.contains(&def.name) {
            continue;
        }
        out.push(def.name.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "TaskOrbital",
                "entity": { "name": "Task", "collection": "tasks", "persistence": "persistent",
                            "fields": [{ "name": "id", "type": "string", "required": true }] },
                "traits": [{
                    "name": "TaskPersistor",
                    "linkedEntity": "Task",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [
                            { "key": "INIT", "name": "Init" },
                            { "key": "DO_CREATE", "name": "Do create", "input": true,
                              "description": "Create a task",
                              "payloadSchema": [{ "name": "title", "type": "string", "required": true }] },
                            { "key": "SAVE", "name": "Save" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "DO_CREATE" }
                        ]
                    }
                }],
                "pages": []
            }]
        }))
        .expect("fixture deserializes")
    }

    #[test]
    fn lists_each_declared_external_input_with_payload_and_annotations() {
        let inputs = external_inputs_of(&schema());
        assert_eq!(inputs.len(), 1);
        let json = serde_json::to_value(&inputs[0]).unwrap();
        assert_eq!(
            json,
            json!({
                "orbital": "TaskOrbital", "trait": "TaskPersistor", "event": "DO_CREATE",
                "payloadSchema": [{ "name": "title", "type": "string", "required": true }],
                "description": "Create a task"
            })
        );
    }

    #[test]
    fn events_without_the_marker_are_not_inputs() {
        let events: Vec<String> = external_inputs_of(&schema())
            .into_iter()
            .map(|i| i.event)
            .collect();
        assert!(!events.contains(&"SAVE".to_string()) && !events.contains(&"INIT".to_string()));
    }

    #[test]
    fn find_requires_orbital_trait_and_event_to_match() {
        let s = schema();
        assert!(find_external_input(&s, "TaskOrbital", "TaskPersistor", "DO_CREATE").is_some());
        assert!(find_external_input(&s, "TaskOrbital", "TaskPersistor", "SAVE").is_none());
        assert!(find_external_input(&s, "TaskOrbital", "Other", "DO_CREATE").is_none());
        assert!(find_external_input(&s, "Other", "TaskPersistor", "DO_CREATE").is_none());
    }

    #[test]
    fn readable_entities_are_the_persisted_ones_once_in_schema_order() {
        let mut s = schema();
        assert_eq!(readable_entities_of(&s), vec!["Task".to_string()]);
        let runtime: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [{
                "name": "A",
                "entity": { "name": "Scratch", "persistence": "runtime", "fields": [{ "name": "id", "type": "string", "required": true }] },
                "auxiliaryEntities": [
                    { "name": "Note", "collection": "notes", "fields": [{ "name": "id", "type": "string", "required": true }] },
                    { "name": "Note", "collection": "notes", "fields": [{ "name": "id", "type": "string", "required": true }] }
                ],
                "traits": [], "pages": []
            }]
        }))
        .unwrap();
        assert_eq!(readable_entities_of(&runtime), vec!["Note".to_string()]);
        let two: OrbitalSchema = serde_json::from_value(json!({
            "name": "App",
            "orbitals": [
                { "name": "TaskOrbital",
                  "entity": { "name": "Task", "collection": "tasks", "persistence": "persistent", "fields": [{ "name": "id", "type": "string", "required": true }] },
                  "auxiliaryEntities": [{ "name": "Comment", "collection": "comments", "persistence": "persistent", "fields": [{ "name": "id", "type": "string", "required": true }] }],
                  "traits": [], "pages": [] },
                { "name": "ProjectOrbital",
                  "entity": { "name": "Project", "collection": "projects", "persistence": "persistent", "fields": [{ "name": "id", "type": "string", "required": true }] },
                  "traits": [], "pages": [] }
            ]
        }))
        .unwrap();
        assert_eq!(readable_entities_of(&two), vec!["Task".to_string(), "Comment".to_string(), "Project".to_string()]);
        s.orbitals.clear();
        assert!(readable_entities_of(&s).is_empty());
    }
}
