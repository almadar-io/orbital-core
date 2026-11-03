//! Renderer-neutral units and event wires for the AVL trait-flow lens.

use orbital_core_base::event_wires::event_wires;
use orbital_core_base::schema::{OrbitalSchema, TraitReference};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlFlowUnit {
    pub id: String,
    pub name: String,
    pub column: String,
    pub traits: Vec<String>,
    pub render_pieces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvlFlowEdge {
    pub source: String,
    pub target: String,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvlTraitFlow {
    pub units: Vec<AvlFlowUnit>,
    pub edges: Vec<AvlFlowEdge>,
}

fn behavior_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn trait_flow_graph(
    schema: &OrbitalSchema,
    orbital_name: &str,
    layers: &HashMap<String, String>,
) -> AvlTraitFlow {
    let Some(orbital) = schema
        .orbitals
        .iter()
        .find(|orbital| orbital.name == orbital_name)
    else {
        return AvlTraitFlow {
            units: Vec::new(),
            edges: Vec::new(),
        };
    };
    let traits: Vec<_> = orbital
        .traits
        .iter()
        .filter_map(|reference| match reference {
            TraitReference::Inline(def) => Some(def.as_ref()),
            TraitReference::Reference { .. } => None,
        })
        .collect();
    let embedded: HashSet<_> = traits
        .iter()
        .flat_map(|trait_def| {
            trait_def
                .trait_embed_ids
                .iter()
                .flat_map(|ids| ids.values())
        })
        .collect();
    let mut units = Vec::<AvlFlowUnit>::new();
    let mut group_indexes = HashMap::<String, usize>::new();
    let mut unit_by_trait = HashMap::<String, String>::new();
    for trait_def in traits {
        let is_piece = trait_def
            .id
            .as_ref()
            .is_some_and(|id| embedded.contains(id));
        let (id, name, column) = if let Some(source) = &trait_def.source_behavior {
            let name = behavior_name(&source.behavior).to_string();
            let column = layers
                .get(&name)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            (format!("behavior:{name}"), name, column)
        } else {
            (
                format!("trait:{}", trait_def.name),
                trait_def.name.clone(),
                "own".to_string(),
            )
        };
        unit_by_trait.insert(trait_def.name.clone(), id.clone());
        let index = if let Some(index) = group_indexes.get(&id) {
            *index
        } else {
            let index = units.len();
            units.push(AvlFlowUnit {
                id: id.clone(),
                name,
                column,
                traits: Vec::new(),
                render_pieces: Vec::new(),
            });
            group_indexes.insert(id, index);
            index
        };
        if is_piece {
            units[index].render_pieces.push(trait_def.name.clone());
        } else {
            units[index].traits.push(trait_def.name.clone());
        }
    }
    units.sort_by_key(|unit| !unit.id.starts_with("trait:"));
    let mut edges = Vec::<AvlFlowEdge>::new();
    let mut edge_indexes = HashMap::<(String, String), usize>::new();
    for wire in event_wires(schema).into_iter().filter(|wire| {
        wire.emitter_orbital == orbital_name && wire.listener_orbital == orbital_name
    }) {
        let (Some(source), Some(target)) = (
            unit_by_trait.get(&wire.emitter_trait),
            unit_by_trait.get(&wire.listener_trait),
        ) else {
            continue;
        };
        if source == target {
            continue;
        }
        let key = (source.clone(), target.clone());
        let index = if let Some(index) = edge_indexes.get(&key) {
            *index
        } else {
            let index = edges.len();
            edges.push(AvlFlowEdge {
                source: source.clone(),
                target: target.clone(),
                events: Vec::new(),
            });
            edge_indexes.insert(key, index);
            index
        };
        if !edges[index].events.contains(&wire.event) {
            edges[index].events.push(wire.event);
        }
    }
    AvlTraitFlow { units, edges }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_flow_groups_composed_traits_and_rolls_up_delivered_events() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "app", "typeContractVersion": 1,
            "orbitals": [{ "name": "Main", "entity": { "name": "Item", "fields": [] },
              "traits": [
                { "name": "One", "id": "one", "sourceBehavior": { "behavior": "std/atoms/flow", "alias": "Flow", "originalName": "One" },
                  "emits": [{ "event": "NEXT" }], "stateMachine": { "states": [{ "name": "idle" }], "transitions": [] } },
                { "name": "Two", "id": "two", "sourceBehavior": { "behavior": "std/atoms/flow", "alias": "Flow", "originalName": "Two" },
                  "emits": [{ "event": "DONE" }], "stateMachine": { "states": [{ "name": "idle" }], "transitions": [] } },
                { "name": "Viewer", "id": "viewer", "traitEmbedIds": { "Piece": "piece" },
                  "listens": [{ "event": "NEXT", "triggers": "GO" }, { "event": "DONE", "triggers": "GO" }],
                  "stateMachine": { "states": [{ "name": "idle" }], "transitions": [] } },
                { "name": "Piece", "id": "piece", "stateMachine": { "states": [{ "name": "idle" }], "transitions": [] } }
              ]
            }]
        })).unwrap();
        let layers = HashMap::from([("flow".to_string(), "std".to_string())]);
        let flow = trait_flow_graph(&schema, "Main", &layers);
        let grouped = flow
            .units
            .iter()
            .find(|unit| unit.id == "behavior:flow")
            .unwrap();
        assert_eq!(grouped.traits, ["One", "Two"]);
        assert_eq!(grouped.column, "std");
        assert_eq!(flow.units[0].id, "trait:Viewer");
        assert_eq!(flow.units[1].render_pieces, ["Piece"]);
        assert_eq!(flow.edges.len(), 1);
        assert_eq!(flow.edges[0].events, ["NEXT", "DONE"]);
    }
}
