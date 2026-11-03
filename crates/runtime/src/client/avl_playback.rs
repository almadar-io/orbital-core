//! Renderer-neutral projection of played AVL steps onto declared transition arms.

use orbital_core_base::schema::{transition_arm_position, StateMachine};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct AvlSceneStep {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub result: AvlStepResult,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlStepResult {
    pub event: String,
    pub transition_fired: bool,
    pub fired_arm: Option<usize>,
    #[serde(default)]
    pub state: Option<AvlPlayedState>,
    #[serde(default)]
    pub guards: Vec<AvlGuardRun>,
    #[serde(default)]
    pub effects: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AvlPlayedState {
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AvlGuardRun {
    pub arm: usize,
    pub guard: serde_json::Value,
    pub passed: bool,
    #[serde(default)]
    pub trace: Vec<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlTransitionPlayback {
    pub step_index: usize,
    pub fired: bool,
    pub guard: Option<AvlGuardRun>,
    pub effects: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlTransitionTrace {
    pub trait_name: String,
    pub from: String,
    pub to: String,
    pub event: String,
    pub fired_arm: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvlMachinePlayback {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_transition: Option<usize>,
    pub visited_states: Vec<String>,
}

pub use orbital_core_base::schema::avl_projection::{
    trait_level_from_description, AvlTraitDescription,
};

/// The declared transition fired by one played step, by kernel-reported arm.
pub fn fired_transition_index(machine: &StateMachine, step: &AvlSceneStep) -> Option<usize> {
    if !step.result.transition_fired {
        return None;
    }
    let arm = step.result.fired_arm?;
    arm_indexes(machine, &step.from, &step.result.event)
        .get(arm)
        .copied()
}

/// Declaration indices for the arms offered by one state and event.
pub fn arm_indexes(machine: &StateMachine, from: &str, event: &str) -> Vec<usize> {
    machine
        .transitions
        .iter()
        .enumerate()
        .filter(|(_, transition)| transition.from.matches(from) && transition.event == event)
        .map(|(index, _)| index)
        .collect()
}

/// The exact arm of a recorded hop; an ambiguous target without an arm stays unresolved.
pub fn traced_transition_index(
    machine: &StateMachine,
    trace: &AvlTransitionTrace,
) -> Option<usize> {
    let candidates = arm_indexes(machine, &trace.from, &trace.event);
    if let Some(arm) = trace.fired_arm {
        return candidates.get(arm).copied();
    }
    let entering: Vec<_> = candidates
        .into_iter()
        .filter(|index| machine.transitions[*index].to == trace.to)
        .collect();
    (entering.len() == 1).then(|| entering[0])
}

/// Machine highlights through the scene cursor for one orbital and trait.
pub fn state_machine_playback(
    machine: &StateMachine,
    orbital: &str,
    trait_name: &str,
    scene: &[AvlSceneStep],
    cursor: usize,
) -> AvlMachinePlayback {
    let played: Vec<_> = scene
        .iter()
        .take(cursor.saturating_add(1))
        .filter(|step| step.orbital == orbital && step.trait_name == trait_name)
        .collect();
    let mut visited_states = Vec::new();
    for step in &played {
        if let Some(state) = &step.result.state {
            for state in [state.before.as_ref(), state.after.as_ref()]
                .into_iter()
                .flatten()
            {
                if visited_states.last() != Some(state) {
                    visited_states.push(state.clone());
                }
            }
        }
    }
    let Some(last) = played.last() else {
        return AvlMachinePlayback {
            active_state: None,
            active_transition: None,
            visited_states,
        };
    };
    let active_state = last
        .result
        .state
        .as_ref()
        .and_then(|state| state.after.clone().or_else(|| state.before.clone()));
    AvlMachinePlayback {
        active_state,
        active_transition: fired_transition_index(machine, last),
        visited_states,
    }
}

/// The machine state after its recorded hops, using the kernel's arm when available.
pub fn trace_playback(
    machine: &StateMachine,
    trait_name: &str,
    traces: &[AvlTransitionTrace],
) -> AvlMachinePlayback {
    let hops: Vec<_> = traces
        .iter()
        .filter(|trace| trace.trait_name == trait_name)
        .collect();
    let mut visited_states = Vec::new();
    for hop in &hops {
        for state in [&hop.from, &hop.to] {
            if visited_states.last() != Some(state) {
                visited_states.push(state.clone());
            }
        }
    }
    let Some(last) = hops.last() else {
        return AvlMachinePlayback {
            active_state: None,
            active_transition: None,
            visited_states,
        };
    };
    AvlMachinePlayback {
        active_state: Some(last.to.clone()),
        active_transition: traced_transition_index(machine, last),
        visited_states,
    }
}

/// The last step through `cursor` that evaluated this exact declared arm.
pub fn transition_playback(
    machine: &StateMachine,
    transition_index: usize,
    orbital: &str,
    trait_name: &str,
    scene: &[AvlSceneStep],
    cursor: usize,
) -> Option<AvlTransitionPlayback> {
    let transition = machine.transitions.get(transition_index)?;
    let arm = transition_arm_position(machine, transition_index)?;
    for (step_index, step) in scene
        .iter()
        .enumerate()
        .take(cursor.saturating_add(1))
        .rev()
    {
        if step.orbital != orbital
            || step.trait_name != trait_name
            || !transition.from.matches(&step.from)
            || step.result.event != transition.event
        {
            continue;
        }
        let fired = step.result.transition_fired && step.result.fired_arm == Some(arm);
        let guard = step
            .result
            .guards
            .iter()
            .find(|guard| guard.arm == arm)
            .cloned();
        if fired || guard.is_some() {
            return Some(AvlTransitionPlayback {
                step_index,
                fired,
                guard,
                effects: if fired {
                    step.result.effects.clone()
                } else {
                    Vec::new()
                },
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::schema::StringOrArray;

    #[test]
    fn transition_playback_uses_the_exact_guarded_arm_and_cursor() {
        let machine: StateMachine = serde_json::from_value(serde_json::json!({
            "states": [{"name": "idle"}, {"name": "ready"}],
            "transitions": [
                {"from": "idle", "to": "ready", "event": "GO"},
                {"from": "idle", "to": "idle", "event": "GO"}
            ]
        }))
        .unwrap();
        let scene: Vec<AvlSceneStep> = serde_json::from_value(serde_json::json!([
            {"orbital": "O", "trait": "T", "from": "idle", "result": {
                "event": "GO", "transitionFired": false, "guards": [{"arm": 0, "guard": ["=", 1, 2], "passed": false, "trace": []}]
            }},
            {"orbital": "O", "trait": "T", "from": "idle", "result": {
                "event": "GO", "transitionFired": true, "firedArm": 1,
                "guards": [{"arm": 0, "guard": ["=", 1, 2], "passed": false}, {"arm": 1, "guard": ["=", 1, 1], "passed": true}],
                "effects": [{"type": "set", "status": "executed"}]
            }}
        ])).unwrap();
        let first = transition_playback(&machine, 0, "O", "T", &scene, 0).unwrap();
        assert!(!first.fired);
        assert_eq!(first.step_index, 0);
        assert!(!first.guard.unwrap().passed);
        assert!(first.effects.is_empty());
        let second = transition_playback(&machine, 1, "O", "T", &scene, 1).unwrap();
        assert!(second.fired);
        assert_eq!(second.step_index, 1);
        assert_eq!(second.effects.len(), 1);
        assert!(transition_playback(&machine, 1, "O", "T", &scene, 0).is_none());
    }

    #[test]
    fn trace_playback_rejects_ambiguous_arms_without_kernel_index() {
        let machine: StateMachine = serde_json::from_value(serde_json::json!({
            "states": [{"name": "idle"}, {"name": "ready"}],
            "transitions": [
                {"from": "idle", "to": "ready", "event": "GO"},
                {"from": "idle", "to": "ready", "event": "GO"}
            ]
        }))
        .unwrap();
        let mut traces: Vec<AvlTransitionTrace> = serde_json::from_value(serde_json::json!([
            {"traitName": "Other", "from": "idle", "to": "ready", "event": "GO"},
            {"traitName": "T", "from": "idle", "to": "ready", "event": "GO"}
        ]))
        .unwrap();
        let ambiguous = trace_playback(&machine, "T", &traces);
        assert_eq!(ambiguous.active_state.as_deref(), Some("ready"));
        assert_eq!(ambiguous.active_transition, None);
        assert_eq!(ambiguous.visited_states, vec!["idle", "ready"]);
        traces[1].fired_arm = Some(1);
        assert_eq!(
            trace_playback(&machine, "T", &traces).active_transition,
            Some(1)
        );
    }

    #[test]
    fn scene_playback_uses_state_and_fired_arm_at_cursor() {
        let machine: StateMachine = serde_json::from_value(serde_json::json!({
            "states": [{"name": "idle"}, {"name": "ready"}],
            "transitions": [
                {"from": "idle", "to": "ready", "event": "GO"},
                {"from": "idle", "to": "idle", "event": "GO"}
            ]
        }))
        .unwrap();
        let scene: Vec<AvlSceneStep> = serde_json::from_value(serde_json::json!([
            {"orbital": "O", "trait": "T", "from": "idle", "result": {
                "event": "GO", "transitionFired": true, "firedArm": 1,
                "state": {"before": "idle", "after": "idle"}
            }},
            {"orbital": "O", "trait": "T", "from": "idle", "result": {
                "event": "GO", "transitionFired": true, "firedArm": 0,
                "state": {"before": "idle", "after": "ready"}
            }}
        ]))
        .unwrap();
        let first = state_machine_playback(&machine, "O", "T", &scene, 0);
        assert_eq!(first.active_transition, Some(1));
        assert_eq!(first.visited_states, vec!["idle"]);
        let second = state_machine_playback(&machine, "O", "T", &scene, 1);
        assert_eq!(second.active_state.as_deref(), Some("ready"));
        assert_eq!(second.active_transition, Some(0));
        assert_eq!(second.visited_states, vec!["idle", "ready"]);
    }

    #[test]
    fn described_trait_level_preserves_multi_source_transition() {
        let description: AvlTraitDescription = serde_json::from_value(serde_json::json!({
            "value": {"trait": "DoorLife"},
            "machine": {"linkedEntity": "Door", "stateMachine": {
                "states": [{"name": "closed", "isInitial": true}, {"name": "open"}],
                "transitions": [{"from": ["closed", "open"], "to": "open", "event": "OPEN", "effects": [["set", "@entity.open", true]]}]
            }},
            "events": {"emits": ["OPENED"], "listens": ["OPEN"]}
        })).unwrap();
        let level = trait_level_from_description(description).unwrap().unwrap();
        assert_eq!(level.transitions.len(), 1);
        assert!(matches!(
            level.transitions[0].from,
            StringOrArray::Multiple(_)
        ));
        assert_eq!(level.transitions[0].effects[0].kind, "set");
        assert_eq!(level.emitted_events, vec!["OPENED"]);
    }
}
