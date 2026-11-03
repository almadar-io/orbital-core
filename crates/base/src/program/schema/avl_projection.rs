//! Serializable AVL trait description projected once for every renderer.

use super::{
    program_machines, EntityRef, OrbitalSchema, SExpression, SourceBehaviorMetadata, StateMachine,
    StringOrArray,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct AvlTraitDescription {
    pub value: AvlTraitValue,
    pub machine: Option<AvlDescriptionMachine>,
    pub events: AvlDescriptionEvents,
    #[serde(default)]
    pub source: Option<AvlTraitSource>,
}

#[derive(Debug, Deserialize)]
pub struct AvlTraitSource {
    pub text: String,
    pub transitions: Vec<AvlSourceTransitionLines>,
}

#[derive(Debug, Deserialize)]
pub struct AvlSourceTransitionLines {
    pub index: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AvlLineSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Deserialize)]
pub struct AvlTraitValue {
    #[serde(rename = "trait")]
    pub trait_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlDescriptionMachine {
    pub linked_entity: Option<String>,
    pub state_machine: StateMachine,
}

#[derive(Debug, Deserialize)]
pub struct AvlDescriptionEvents {
    pub emits: Vec<String>,
    pub listens: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlTraitLevel {
    pub name: String,
    pub linked_entity: String,
    pub states: Vec<AvlStateLevel>,
    pub transitions: Vec<AvlTransitionLevel>,
    pub emitted_events: Vec<String>,
    pub listened_events: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlStateLevel {
    pub name: String,
    pub is_initial: Option<bool>,
    pub is_terminal: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct AvlTransitionLevel {
    pub from: StringOrArray,
    pub to: String,
    pub event: String,
    pub guard: Option<SExpression>,
    pub effects: Vec<AvlEffectLevel>,
    pub index: usize,
}

#[derive(Debug, Serialize)]
pub struct AvlEffectLevel {
    #[serde(rename = "type")]
    pub kind: String,
    pub args: Vec<SExpression>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlProgramTraitRef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_behavior: Option<SourceBehaviorMetadata>,
}

#[derive(Debug, Serialize)]
pub struct AvlProgramTrait {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_ref: AvlProgramTraitRef,
    pub level: AvlTraitLevel,
}

/// Every resolved machine in declaration order, with one shared AVL level projection.
pub fn program_trait_levels(schema: &OrbitalSchema) -> Result<Vec<AvlProgramTrait>, String> {
    let mut entries = Vec::new();
    for (orbital_name, trait_def) in program_machines(schema) {
        let orbital = schema
            .orbitals
            .iter()
            .find(|orbital| orbital.name == orbital_name)
            .ok_or_else(|| format!("missing orbital {orbital_name}"))?;
        let entity_name = match &orbital.entity {
            EntityRef::Inline(entity) => entity.name.clone(),
            EntityRef::Extends(entity) => {
                entity.name.clone().unwrap_or_else(|| orbital.name.clone())
            }
            EntityRef::Reference(name) => name.clone(),
        };
        let description = AvlTraitDescription {
            value: AvlTraitValue {
                trait_name: trait_def.name.clone(),
            },
            machine: Some(AvlDescriptionMachine {
                linked_entity: Some(trait_def.linked_entity.clone().unwrap_or(entity_name)),
                state_machine: (*trait_def.state_machine).clone(),
            }),
            events: AvlDescriptionEvents {
                emits: trait_def
                    .emits
                    .as_ref()
                    .map(|items| items.iter().map(|item| item.event.clone()).collect())
                    .unwrap_or_default(),
                listens: trait_def
                    .listens
                    .as_ref()
                    .map(|items| items.iter().map(|item| item.event.clone()).collect())
                    .unwrap_or_default(),
            },
            source: None,
        };
        let level = trait_level_from_description(description)?
            .ok_or_else(|| format!("missing machine for {}", trait_def.name))?;
        entries.push(AvlProgramTrait {
            orbital: orbital_name.to_string(),
            trait_ref: AvlProgramTraitRef {
                name: trait_def.name.clone(),
                source_behavior: trait_def.source_behavior.clone(),
            },
            level,
        });
    }
    Ok(entries)
}

/// The printed `.lolo` lines for the running arm after composition and renaming.
pub fn described_transition_lines(
    description: &AvlTraitDescription,
    running: &StateMachine,
    transition_index: usize,
) -> Option<AvlLineSpan> {
    let source = description.source.as_ref()?;
    let printed = &description.machine.as_ref()?.state_machine;
    let transition = running.transitions.get(transition_index)?;
    let sources = transition.from.states();
    let mut matched_index = None;
    for from in sources {
        let position = running.transitions[..transition_index]
            .iter()
            .filter(|arm| arm.event == transition.event && arm.from.matches(from))
            .count();
        let printed_index = printed
            .transitions
            .iter()
            .enumerate()
            .filter(|(_, arm)| arm.event == transition.event && arm.from.matches(from))
            .nth(position)
            .map(|(index, _)| index)?;
        if matched_index.is_some_and(|index| index != printed_index) {
            return None;
        }
        matched_index = Some(printed_index);
    }
    let index = matched_index?;
    source
        .transitions
        .iter()
        .find(|lines| lines.index == index)
        .map(|lines| AvlLineSpan {
            start: lines.start,
            end: lines.end,
        })
}

/// The state-machine level carried by a serialized `behavior/describe` result.
pub fn trait_level_from_description(
    description: AvlTraitDescription,
) -> Result<Option<AvlTraitLevel>, String> {
    let Some(machine) = description.machine else {
        return Ok(None);
    };
    let states = machine
        .state_machine
        .states
        .into_iter()
        .map(|state| AvlStateLevel {
            name: state.name,
            is_initial: state.is_initial,
            is_terminal: state.is_terminal,
        })
        .collect();
    let mut transitions = Vec::new();
    for (index, transition) in machine.state_machine.transitions.into_iter().enumerate() {
        let mut effects = Vec::new();
        for effect in transition.effects.unwrap_or_default() {
            let kind = effect
                .operator()
                .ok_or_else(|| format!("transition {index} effect is not an operator call"))?
                .to_string();
            let args = effect
                .arguments()
                .ok_or_else(|| format!("transition {index} effect has no arguments"))?
                .to_vec();
            effects.push(AvlEffectLevel { kind, args });
        }
        transitions.push(AvlTransitionLevel {
            from: transition.from,
            to: transition.to,
            event: transition.event,
            guard: transition.guard,
            effects,
            index,
        });
    }
    Ok(Some(AvlTraitLevel {
        name: description.value.trait_name,
        linked_entity: machine.linked_entity.unwrap_or_default(),
        states,
        transitions,
        emitted_events: description.events.emits,
        listened_events: description.events.listens,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_trait_levels_keep_order_provenance_and_machine_contract() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "app", "typeContractVersion": 1,
            "orbitals": [{
                "name": "Main", "entity": { "name": "Door", "fields": [] },
                "traits": [{
                    "name": "DoorLife", "linkedEntity": "Door",
                    "sourceBehavior": { "behavior": "std/atoms/door", "alias": "Door", "originalName": "Life" },
                    "emits": [{ "event": "OPENED" }],
                    "listens": [{ "event": "OPEN", "triggers": "OPEN" }],
                    "stateMachine": {
                        "states": [{ "name": "closed", "isInitial": true }, { "name": "open" }],
                        "transitions": [{ "from": ["closed", "open"], "to": "open", "event": "OPEN",
                            "effects": [["set", "@entity.open", true]] }]
                    }
                }, {
                    "name": "Observer", "stateMachine": {
                        "states": [{ "name": "idle" }], "transitions": []
                    }
                }]
            }]
        })).unwrap();
        let levels = program_trait_levels(&schema).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].orbital, "Main");
        assert_eq!(levels[0].trait_ref.name, "DoorLife");
        assert_eq!(levels[0].level.linked_entity, "Door");
        assert_eq!(levels[0].level.emitted_events, ["OPENED"]);
        assert_eq!(levels[0].level.listened_events, ["OPEN"]);
        assert_eq!(levels[0].level.transitions[0].effects[0].kind, "set");
        assert_eq!(levels[1].trait_ref.name, "Observer");
        let wire = serde_json::to_value(levels).unwrap();
        assert_eq!(wire[0]["trait"]["sourceBehavior"]["originalName"], "Life");
        assert_eq!(
            wire[0]["level"]["transitions"][0]["from"],
            serde_json::json!(["closed", "open"])
        );
    }

    #[test]
    fn described_lines_follow_the_arm_across_resolved_and_printed_machines() {
        let running: StateMachine = serde_json::from_value(serde_json::json!({
            "states": [{"name":"idle"}, {"name":"ready"}],
            "transitions": [
                {"from":["idle","ready"],"to":"ready","event":"GO"},
                {"from":["idle","ready"],"to":"ready","event":"GO"}
            ]
        }))
        .unwrap();
        let printed: AvlTraitDescription = serde_json::from_value(serde_json::json!({
            "value":{"trait":"Task"},
            "events":{"emits":[],"listens":[]},
            "machine":{"stateMachine":{
                "states":[{"name":"idle"},{"name":"ready"}],
                "transitions":[
                    {"from":["idle","ready"],"to":"ready","event":"GO"},
                    {"from":["idle","ready"],"to":"ready","event":"GO"}
                ]
            }},
            "source":{"text":"trait Task","transitions":[
                {"index":0,"start":4,"end":6},
                {"index":1,"start":7,"end":9}
            ]}
        }))
        .unwrap();
        assert_eq!(
            described_transition_lines(&printed, &running, 1),
            Some(AvlLineSpan { start: 7, end: 9 })
        );
        assert_eq!(described_transition_lines(&printed, &running, 2), None);
    }
}
