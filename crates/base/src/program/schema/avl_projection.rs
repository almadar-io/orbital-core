//! Serializable AVL trait description projected once for every renderer.

use super::{SExpression, StateMachine, StringOrArray};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct AvlTraitDescription {
    pub value: AvlTraitValue,
    pub machine: Option<AvlDescriptionMachine>,
    pub events: AvlDescriptionEvents,
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
