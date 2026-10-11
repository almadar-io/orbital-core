//! State Machine Core
//!
//! The core state machine engine that processes events through transitions.

use super::types::{EventPayload, GuardResult, TransitionResult};
use crate::error::OrbitalError;
use crate::evaluator::{BindingContext, Evaluator};
use crate::schema::{StateMachine, TraitDefinition, TransitionDefinition};

/// Core state machine processor
///
/// Processes events through state machine transitions, evaluating guards
/// and returning effects without executing them.
#[derive(Debug)]
pub struct StateMachineCore {
    /// The evaluator for guard expressions
    evaluator: Evaluator,
}

impl Default for StateMachineCore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachineCore {
    /// Create a new state machine core
    pub fn new() -> Self {
        Self {
            evaluator: Evaluator::new(),
        }
    }

    /// Create a new state machine core with a custom evaluator
    pub fn with_evaluator(evaluator: Evaluator) -> Self {
        Self { evaluator }
    }

    /// Process an event through the state machine
    ///
    /// Returns a TransitionResult indicating:
    /// - Whether a transition was executed
    /// - The new state (if executed)
    /// - Effects to execute (if executed)
    /// - Guard result (if a guard was evaluated)
    pub fn process_event(
        &self,
        state_machine: &StateMachine,
        current_state: &str,
        payload: &EventPayload,
        ctx: &BindingContext,
    ) -> Result<TransitionResult, OrbitalError> {
        // V4 id-primary dispatch: resolve the fired event's id from the machine's
        // own `events[]` (key -> id) so we don't need to change the public
        // EventPayload/process_event signature. Falls back to name matching when
        // either side has no id (old files, or an event not listed in `events`).
        let fired_event_id: Option<&str> = state_machine
            .events
            .as_ref()
            .and_then(|events| events.iter().find(|e| e.key == payload.event))
            .and_then(|e| e.id.as_deref());

        // Find matching transition(s)
        let matching_transitions: Vec<&TransitionDefinition> = state_machine
            .transitions
            .iter()
            .filter(|t| {
                t.from.matches(current_state)
                    && Self::event_matches(t, &payload.event, fired_event_id)
            })
            .collect();

        if matching_transitions.is_empty() {
            return Ok(TransitionResult::no_transition(
                current_state,
                &payload.event,
            ));
        }

        // Build context with payload
        let mut eval_ctx = self.build_context(ctx, payload);
        eval_ctx.from_state = Some(current_state.to_string());

        // Keep track of the last transition for guard failure reporting
        let mut last_guarded_transition: Option<&TransitionDefinition> = None;

        // Try each matching transition (first one with passing guard wins)
        for transition in &matching_transitions {
            if transition.guard.is_some() {
                last_guarded_transition = Some(transition);
            }
            eval_ctx.to_state = Some(transition.to.clone());
            match self.try_transition(transition, current_state, &eval_ctx)? {
                Some(result) => return Ok(result),
                None => continue, // Guard failed, try next transition
            }
        }

        // No transition had a passing guard
        // Return the last transition's guard failure
        if let Some(last_transition) = last_guarded_transition {
            if let Some(guard) = &last_transition.guard {
                let result = self.evaluator.evaluate(guard, &eval_ctx)?;
                return Ok(TransitionResult::guard_blocked(
                    current_state,
                    &last_transition.to,
                    &payload.event,
                    GuardResult::failed(guard.clone(), result),
                ));
            }
        }

        Ok(TransitionResult::no_transition(
            current_state,
            &payload.event,
        ))
    }

    /// Whether a transition matches the fired event, id-primary.
    ///
    /// When the fired event carries a resolved id AND the transition carries
    /// `event_id`, match on id. Otherwise fall back to the name match — this
    /// keeps dispatch behavior-preserving for machines/events without ids.
    fn event_matches(
        transition: &TransitionDefinition,
        fired_name: &str,
        fired_id: Option<&str>,
    ) -> bool {
        if let (Some(fired_id), Some(transition_id)) = (fired_id, transition.event_id.as_deref()) {
            return fired_id == transition_id;
        }
        transition.event == fired_name
    }

    /// Try to execute a single transition
    ///
    /// Returns Some(TransitionResult) if the transition executes (guard passes),
    /// None if the guard fails.
    fn try_transition(
        &self,
        transition: &TransitionDefinition,
        current_state: &str,
        ctx: &BindingContext,
    ) -> Result<Option<TransitionResult>, OrbitalError> {
        // Evaluate guard if present
        if let Some(guard) = &transition.guard {
            let result = self.evaluator.evaluate(guard, ctx)?;
            let passed = result.is_truthy();

            if !passed {
                // Guard failed - return None so we try next transition
                return Ok(None);
            }

            // Guard passed - execute transition
            let effects = transition.effects.clone().unwrap_or_default();
            return Ok(Some(TransitionResult::success_with_guard(
                current_state,
                &transition.to,
                effects,
                GuardResult::passed(guard.clone(), result),
            )));
        }

        // No guard - transition executes unconditionally
        let effects = transition.effects.clone().unwrap_or_default();
        Ok(Some(TransitionResult::success(
            current_state,
            &transition.to,
            effects,
        )))
    }

    /// Build evaluation context with payload
    fn build_context(&self, base_ctx: &BindingContext, payload: &EventPayload) -> BindingContext {
        let mut ctx = base_ctx.child();

        // Set payload directly to the data (not wrapped in object)
        // This matches the schema convention where @payload.amount directly accesses the amount field
        // instead of requiring @payload.data.amount
        ctx.set_payload(payload.data.clone());
        ctx
    }

    /// Get the initial state from a state machine
    pub fn get_initial_state(state_machine: &StateMachine) -> Option<&str> {
        state_machine
            .states
            .iter()
            .find(|s| s.is_initial == Some(true))
            .or_else(|| state_machine.states.first())
            .map(|s| s.name.as_str())
    }

    /// Get the initial state from a trait definition
    pub fn get_trait_initial_state(trait_def: &TraitDefinition) -> Option<&str> {
        Self::get_initial_state(&trait_def.state_machine)
    }

    /// Find all transitions for an event from any state
    pub fn find_transitions_for_event<'a>(
        state_machine: &'a StateMachine,
        event: &str,
    ) -> Vec<&'a TransitionDefinition> {
        state_machine
            .transitions
            .iter()
            .filter(|t| t.event == event)
            .collect()
    }

    /// Find transitions from a specific state
    pub fn find_transitions_from_state<'a>(
        state_machine: &'a StateMachine,
        state: &str,
    ) -> Vec<&'a TransitionDefinition> {
        state_machine
            .transitions
            .iter()
            .filter(|t| t.from.matches(state))
            .collect()
    }

    /// Check if a state is valid in the state machine
    pub fn is_valid_state(state_machine: &StateMachine, state: &str) -> bool {
        state_machine.states.iter().any(|s| s.name == state)
    }

    /// Get all state names
    pub fn get_state_names(state_machine: &StateMachine) -> Vec<&str> {
        state_machine
            .states
            .iter()
            .map(|s| s.name.as_str())
            .collect()
    }

    /// Get all event names used in transitions
    pub fn get_event_names(state_machine: &StateMachine) -> Vec<&str> {
        let mut events: Vec<&str> = state_machine
            .transitions
            .iter()
            .map(|t| t.event.as_str())
            .collect();
        events.sort();
        events.dedup();
        events
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::TransitionFailureReason;
    use super::*;
    use crate::evaluator::Value;
    use crate::schema::{StateDefinition, StringOrArray};
    use std::collections::HashMap;

    fn create_test_state_machine() -> StateMachine {
        StateMachine {
            states: vec![
                StateDefinition {
                    name: "browsing".to_string(),
                    is_initial: Some(true),
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                },
                StateDefinition {
                    name: "registering".to_string(),
                    is_initial: None,
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                },
                StateDefinition {
                    name: "viewing".to_string(),
                    is_initial: None,
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                },
            ],
            events: None,
            transitions: vec![
                TransitionDefinition {
                    from: StringOrArray::Single("browsing".to_string()),
                    to: "registering".to_string(),
                    event: "REGISTER".to_string(),
                    event_id: None,
                    guard: None,
                    effects: Some(vec![
                        serde_json::from_str(r#"["render-ui", "main", {"type": "form-section"}]"#).unwrap(),
                    ]),
                },
                TransitionDefinition {
                    from: StringOrArray::Single("registering".to_string()),
                    to: "browsing".to_string(),
                    event: "SAVE".to_string(),
                    event_id: None,
                    guard: Some(
                        serde_json::from_str(r#"["and", ["!=", "@payload.data.email", ""], ["!=", "@payload.data.name", ""]]"#).unwrap(),
                    ),
                    effects: Some(vec![
                        serde_json::from_str(r#"["persist", "create", "User", "@payload.data"]"#).unwrap(),
                        serde_json::from_str(r#"["emit", "USER_REGISTERED", {"userId": "@payload.data.id"}]"#).unwrap(),
                    ]),
                },
                TransitionDefinition {
                    from: StringOrArray::Single("registering".to_string()),
                    to: "browsing".to_string(),
                    event: "CANCEL".to_string(),
                    event_id: None,
                    guard: None,
                    effects: Some(vec![
                        serde_json::from_str(r#"["navigate", "/users"]"#).unwrap(),
                    ]),
                },
                TransitionDefinition {
                    from: StringOrArray::Single("browsing".to_string()),
                    to: "viewing".to_string(),
                    event: "VIEW".to_string(),
                    event_id: None,
                    guard: None,
                    effects: Some(vec![
                        serde_json::from_str(r#"["render-ui", "main", {"type": "entity-detail"}]"#).unwrap(),
                    ]),
                },
            ],
        }
    }

    #[test]
    fn test_get_initial_state() {
        let sm = create_test_state_machine();
        let initial = StateMachineCore::get_initial_state(&sm);
        assert_eq!(initial, Some("browsing"));
    }

    #[test]
    fn test_simple_transition() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine();
        let ctx = BindingContext::new();

        let payload = EventPayload::new("REGISTER", Value::Object(HashMap::new()));
        let result = core.process_event(&sm, "browsing", &payload, &ctx).unwrap();

        assert!(result.executed);
        assert_eq!(result.previous_state, Some("browsing".to_string()));
        assert_eq!(result.new_state, Some("registering".to_string()));
        assert_eq!(result.effects.len(), 1);
    }

    #[test]
    fn test_transition_with_passing_guard() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine();
        let ctx = BindingContext::new();

        // Guard checks @payload.data.email and @payload.data.name
        let mut inner_data = HashMap::new();
        inner_data.insert(
            "email".to_string(),
            Value::String("test@example.com".to_string()),
        );
        inner_data.insert("name".to_string(), Value::String("Test User".to_string()));
        inner_data.insert("id".to_string(), Value::String("user-123".to_string()));

        let mut data = HashMap::new();
        data.insert("data".to_string(), Value::Object(inner_data));

        let payload = EventPayload::new("SAVE", Value::Object(data));
        let result = core
            .process_event(&sm, "registering", &payload, &ctx)
            .unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("browsing".to_string()));
        assert_eq!(result.effects.len(), 2); // persist + emit

        // Check guard result
        let guard_result = result.guard_result.unwrap();
        assert!(guard_result.passed);
    }

    #[test]
    fn test_transition_with_failing_guard() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine();
        let ctx = BindingContext::new();

        // Empty email should fail the guard (@payload.data.email != "")
        let mut inner_data = HashMap::new();
        inner_data.insert("email".to_string(), Value::String("".to_string()));
        inner_data.insert("name".to_string(), Value::String("Test User".to_string()));

        let mut data = HashMap::new();
        data.insert("data".to_string(), Value::Object(inner_data));

        let payload = EventPayload::new("SAVE", Value::Object(data));
        let result = core
            .process_event(&sm, "registering", &payload, &ctx)
            .unwrap();

        assert!(!result.executed);
        assert!(result.new_state.is_none());
        assert!(result.effects.is_empty());

        // Check guard result
        let guard_result = result.guard_result.unwrap();
        assert!(!guard_result.passed);

        // Check failure reason
        match result.reason {
            Some(TransitionFailureReason::GuardBlocked {
                state,
                target_state,
                event,
            }) => {
                assert_eq!(state, "registering");
                assert_eq!(target_state, "browsing");
                assert_eq!(event, "SAVE");
            }
            _ => panic!("Expected GuardBlocked reason"),
        }
    }

    #[test]
    fn test_no_matching_transition() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine();
        let ctx = BindingContext::new();

        let payload = EventPayload::new("UNKNOWN_EVENT", Value::Object(HashMap::new()));
        let result = core.process_event(&sm, "browsing", &payload, &ctx).unwrap();

        assert!(!result.executed);

        match result.reason {
            Some(TransitionFailureReason::NoMatchingTransition { state, event }) => {
                assert_eq!(state, "browsing");
                assert_eq!(event, "UNKNOWN_EVENT");
            }
            _ => panic!("Expected NoMatchingTransition reason"),
        }
    }

    #[test]
    fn test_cancel_transition_no_guard() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine();
        let ctx = BindingContext::new();

        let payload = EventPayload::new("CANCEL", Value::Object(HashMap::new()));
        let result = core
            .process_event(&sm, "registering", &payload, &ctx)
            .unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("browsing".to_string()));
        assert!(result.guard_result.is_none()); // No guard on CANCEL
    }

    #[test]
    fn test_find_transitions_for_event() {
        let sm = create_test_state_machine();

        let register_transitions = StateMachineCore::find_transitions_for_event(&sm, "REGISTER");
        assert_eq!(register_transitions.len(), 1);

        let save_transitions = StateMachineCore::find_transitions_for_event(&sm, "SAVE");
        assert_eq!(save_transitions.len(), 1);
    }

    #[test]
    fn test_find_transitions_from_state() {
        let sm = create_test_state_machine();

        let from_browsing = StateMachineCore::find_transitions_from_state(&sm, "browsing");
        assert_eq!(from_browsing.len(), 2); // REGISTER and VIEW

        let from_registering = StateMachineCore::find_transitions_from_state(&sm, "registering");
        assert_eq!(from_registering.len(), 2); // SAVE and CANCEL
    }

    #[test]
    fn test_get_state_names() {
        let sm = create_test_state_machine();
        let states = StateMachineCore::get_state_names(&sm);

        assert_eq!(states.len(), 3);
        assert!(states.contains(&"browsing"));
        assert!(states.contains(&"registering"));
        assert!(states.contains(&"viewing"));
    }

    #[test]
    fn test_id_primary_dispatch_stale_name_fires_via_id() {
        let core = StateMachineCore::new();
        let mut sm = create_test_state_machine();

        // Fired event key "REGISTER" carries id "evt-register".
        sm.events = Some(vec![crate::schema::EventDefinition {
            payload_entity: None,
            external: None,
            listened: None,
            key: "REGISTER".to_string(),
            id: Some("evt-register".to_string()),
            name: None,
            description: None,
            synonyms: None,
            tier: None,
            payload_schema: None,
        }]);

        // Transition's `event` name is stale ("OLD_REGISTER_NAME") but its
        // `event_id` matches the fired event's resolved id — should still fire.
        sm.transitions[0].event = "OLD_REGISTER_NAME".to_string();
        sm.transitions[0].event_id = Some("evt-register".to_string());

        let ctx = BindingContext::new();
        let payload = EventPayload::new("REGISTER", Value::Object(HashMap::new()));
        let result = core.process_event(&sm, "browsing", &payload, &ctx).unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("registering".to_string()));
    }

    #[test]
    fn test_no_id_event_still_dispatches_via_name() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine(); // no event ids anywhere
        let ctx = BindingContext::new();

        let payload = EventPayload::new("VIEW", Value::Object(HashMap::new()));
        let result = core.process_event(&sm, "browsing", &payload, &ctx).unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("viewing".to_string()));
    }

    #[test]
    fn test_get_event_names() {
        let sm = create_test_state_machine();
        let events = StateMachineCore::get_event_names(&sm);

        assert!(events.contains(&"REGISTER"));
        assert!(events.contains(&"SAVE"));
        assert!(events.contains(&"CANCEL"));
        assert!(events.contains(&"VIEW"));
    }

    #[test]
    fn test_is_valid_state() {
        let sm = create_test_state_machine();

        assert!(StateMachineCore::is_valid_state(&sm, "browsing"));
        assert!(StateMachineCore::is_valid_state(&sm, "registering"));
        assert!(!StateMachineCore::is_valid_state(&sm, "nonexistent"));
    }
}
