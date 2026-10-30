//! The program's emitter-to-listener wire table: which trait hears which emit, decided by the one source-identity rule
//! the kernel delivers by (`listen_match`), so a drawn wire is a delivery the running program makes.

use crate::bus_event::BusEventSource;
use crate::listen_match::matches_listen_source;
use crate::schema::types::{split_event_address, OrbitalSchema, TraitDefinition, TraitReference};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// One delivery between two different traits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventWire {
    pub emitter_orbital: String,
    pub emitter_trait: String,
    pub listener_orbital: String,
    pub listener_trait: String,
    pub event: String,
}

struct Instance<'a> {
    orbital: &'a str,
    orbital_id: Option<&'a str>,
    def: Arc<TraitDefinition>,
}

/// Every emitter-to-listener delivery between two different traits, in schema order. A trait reference resolves to the
/// inline trait of that name in the schema, as the kernel registers it; one that names none declares nothing.
pub fn event_wires(schema: &OrbitalSchema) -> Vec<EventWire> {
    let mut instances: Vec<Instance<'_>> = Vec::new();
    for orbital in &schema.orbitals {
        for reference in &orbital.traits {
            let def = match reference {
                TraitReference::Inline(def) => Some(Arc::clone(def)),
                TraitReference::Reference {
                    reference, name, ..
                } => reference.as_ref().or(name.as_ref()).and_then(|wanted| {
                    schema
                        .orbitals
                        .iter()
                        .flat_map(|o| &o.traits)
                        .find_map(|r| match r {
                            TraitReference::Inline(def) if &def.name == wanted => {
                                Some(Arc::clone(def))
                            }
                            _ => None,
                        })
                }),
            };
            if let Some(def) = def {
                instances.push(Instance {
                    orbital: &orbital.name,
                    orbital_id: orbital.id.as_deref(),
                    def,
                });
            }
        }
    }
    let mut by_event: HashMap<String, Vec<(usize, Option<crate::schema::types::ListenSource>)>> =
        HashMap::new();
    for (index, listener) in instances.iter().enumerate() {
        for listen in listener.def.listens.iter().flat_map(|l| l.iter()) {
            let (event, source) = match &listen.source {
                Some(source) => (listen.event.clone(), Some(source.clone())),
                None => split_event_address(&listen.event),
            };
            by_event.entry(event).or_default().push((index, source));
        }
    }
    let mut wires = Vec::new();
    for (emitter_index, emitter) in instances.iter().enumerate() {
        let source = BusEventSource {
            orbital: Some(emitter.orbital.to_string()),
            orbital_id: emitter.orbital_id.map(str::to_string),
            r#trait: Some(emitter.def.name.clone()),
            trait_id: emitter.def.id.clone(),
            ..Default::default()
        };
        for emit in emitter.def.emits.iter().flat_map(|e| e.iter()) {
            for (listener_index, declared) in by_event.get(&emit.event).into_iter().flatten() {
                if *listener_index == emitter_index {
                    continue;
                }
                let listener = &instances[*listener_index];
                if !matches_listen_source(declared.as_ref(), listener.orbital, &source) {
                    continue;
                }
                wires.push(EventWire {
                    emitter_orbital: emitter.orbital.to_string(),
                    emitter_trait: emitter.def.name.clone(),
                    listener_orbital: listener.orbital.to_string(),
                    listener_trait: listener.def.name.clone(),
                    event: emit.event.clone(),
                });
            }
        }
    }
    wires
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(listens: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "app", "typeContractVersion": 1,
            "orbitals": [
                { "name": "Cart", "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
                  "traits": [
                    { "name": "Basket", "emits": [{ "event": "ADDED" }],
                      "stateMachine": { "states": [{ "name": "s", "isInitial": true }], "transitions": [] } },
                    { "name": "Badge", "listens": listens,
                      "stateMachine": { "states": [{ "name": "s", "isInitial": true }], "transitions": [] } }] },
                { "name": "Shop", "entity": { "name": "Order", "fields": [{ "name": "id", "type": "string" }] },
                  "traits": [{ "name": "Banner", "listens": [{ "event": "ADDED", "triggers": "SHOW", "source": { "kind": "any" } }],
                      "stateMachine": { "states": [{ "name": "s", "isInitial": true }], "transitions": [] } }] }
            ]
        }))
        .unwrap()
    }

    fn pairs(wires: &[EventWire]) -> Vec<(&str, &str, &str)> {
        wires
            .iter()
            .map(|w| {
                (
                    w.emitter_trait.as_str(),
                    w.listener_trait.as_str(),
                    w.event.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn a_listen_with_a_matching_source_is_a_wire_and_an_any_source_hears_every_emitter() {
        let wires = event_wires(&schema(
            json!([{ "event": "ADDED", "triggers": "BUMP", "source": { "kind": "trait", "trait": "Basket" } }]),
        ));
        assert_eq!(
            pairs(&wires),
            [("Basket", "Badge", "ADDED"), ("Basket", "Banner", "ADDED")]
        );
    }

    #[test]
    fn control_a_listen_naming_another_source_is_no_wire() {
        let wires = event_wires(&schema(
            json!([{ "event": "ADDED", "triggers": "BUMP", "source": { "kind": "trait", "trait": "Other" } }]),
        ));
        assert_eq!(pairs(&wires), [("Basket", "Banner", "ADDED")]);
    }

    #[test]
    fn a_listen_with_no_source_field_reads_its_address_form() {
        let wires = event_wires(&schema(
            json!([{ "event": "Cart.Basket.ADDED", "triggers": "BUMP" }]),
        ));
        assert_eq!(
            pairs(&wires),
            [("Basket", "Badge", "ADDED"), ("Basket", "Banner", "ADDED")]
        );
    }

    #[test]
    fn edge_a_trait_never_hears_itself() {
        let schema: OrbitalSchema = serde_json::from_value(json!({
            "name": "app", "typeContractVersion": 1,
            "orbitals": [{ "name": "O", "entity": { "name": "E", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "T", "emits": [{ "event": "X" }], "listens": [{ "event": "X", "triggers": "Y", "source": { "kind": "any" } }],
                    "stateMachine": { "states": [{ "name": "s", "isInitial": true }], "transitions": [] } }] }]
        }))
        .unwrap();
        assert!(event_wires(&schema).is_empty());
    }
}
