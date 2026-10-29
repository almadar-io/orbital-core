//! State Machine Core
//!
//! The core state machine engine that processes events through transitions.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::types::{ArmGuard, EventPayload, GuardResult, TransitionEffects, TransitionResult};
use crate::evaluator::context::BindingContext;
use crate::evaluator::Evaluator;
use orbital_core_base::error::OrbitalError;
use orbital_core_base::schema::types::{StateMachine, TraitDefinition, TransitionDefinition};
use std::sync::Arc;

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
        let (mut result, fired) = self.select(state_machine, current_state, payload, ctx)?;
        if let Some(index) = fired {
            result.effect_list = TransitionEffects::Owned(
                state_machine.transitions[index]
                    .effects
                    .clone()
                    .unwrap_or_default(),
            );
        }
        Ok(result)
    }

    /// [`process_event`](Self::process_event) over a registered trait: the fired transition's effects
    /// stay in the trait definition and the result points at them.
    pub fn process_event_shared(
        &self,
        definition: &Arc<TraitDefinition>,
        current_state: &str,
        payload: &EventPayload,
        ctx: &BindingContext,
    ) -> Result<TransitionResult, OrbitalError> {
        let (mut result, fired) =
            self.select(&definition.state_machine, current_state, payload, ctx)?;
        if let Some(transition) = fired {
            result.effect_list = TransitionEffects::Shared {
                definition: Arc::clone(definition),
                transition,
            };
        }
        Ok(result)
    }

    /// The arm selection; returns the index (into `state_machine.transitions`) of the transition that fired.
    fn select(
        &self,
        state_machine: &StateMachine,
        current_state: &str,
        payload: &EventPayload,
        ctx: &BindingContext,
    ) -> Result<(TransitionResult, Option<usize>), OrbitalError> {
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
        let matching_transitions: Vec<(usize, &TransitionDefinition)> = state_machine
            .transitions
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.from.matches(current_state)
                    && Self::event_matches(t, &payload.event, fired_event_id)
            })
            .collect();

        if matching_transitions.is_empty() {
            return Ok((
                TransitionResult::no_transition(current_state, &payload.event),
                None,
            ));
        }

        // Build context with payload
        let mut eval_ctx = self.build_context(ctx, payload);
        eval_ctx.from_state = Some(std::sync::Arc::from(current_state));

        // First arm whose guard passes (or that has none) fires; every guard tried is recorded.
        let mut guards: Vec<ArmGuard> = Vec::new();
        for (arm, (index, transition)) in matching_transitions.iter().enumerate() {
            eval_ctx.to_state = Some(std::sync::Arc::from(transition.to.as_str()));
            let Some(guard) = &transition.guard else {
                let mut result = TransitionResult::success(
                    current_state,
                    &transition.to,
                    TransitionEffects::None,
                );
                result.fired_arm = Some(arm);
                result.transition_index = Some(*index);
                result.guards = guards;
                return Ok((result, Some(*index)));
            };
            // A guard that cannot be evaluated blocks its arm exactly like a false one; the
            // next arm is tried (same rule on every path).
            let value = match self.evaluator.evaluate(guard, &eval_ctx) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                        event = %payload.event,
                        from = %current_state,
                        to = %transition.to,
                        %error,
                        "guard:error-blocks-arm",
                    );
                    guards.push(ArmGuard {
                        arm,
                        guard: GuardResult::error(Arc::new(guard.clone()), error.to_string()),
                    });
                    continue;
                }
            };
            let holds = match value.boolean() {
                Ok(holds) => holds,
                Err(error) => {
                    tracing::warn!(
                        target: orbital_core_base::logging::ns::RUNTIME_KERNEL,
                        event = %payload.event,
                        from = %current_state,
                        to = %transition.to,
                        %error,
                        "guard:error-blocks-arm",
                    );
                    guards.push(ArmGuard {
                        arm,
                        guard: GuardResult::error(Arc::new(guard.clone()), error.to_string()),
                    });
                    continue;
                }
            };
            if !holds {
                guards.push(ArmGuard {
                    arm,
                    guard: GuardResult::failed(Arc::new(guard.clone()), value),
                });
                continue;
            }
            let passed = GuardResult::passed(Arc::new(guard.clone()), value);
            guards.push(ArmGuard {
                arm,
                guard: passed.clone(),
            });
            let mut result = TransitionResult::success_with_guard(
                current_state,
                &transition.to,
                TransitionEffects::None,
                passed,
            );
            result.fired_arm = Some(arm);
                result.transition_index = Some(*index);
            result.guards = guards;
            return Ok((result, Some(*index)));
        }

        // No arm fired: report the last guarded arm's failure.
        let Some(last) = guards.last().cloned() else {
            return Ok((
                TransitionResult::no_transition(current_state, &payload.event),
                None,
            ));
        };
        let target = &matching_transitions[last.arm].1.to;
        let mut result =
            TransitionResult::guard_blocked(current_state, target, &payload.event, last.guard);
        result.guards = guards;
        Ok((result, None))
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
    use orbital_core_base::schema::types::{StateDefinition, StringOrArray};
    use orbital_core_base::value::Value;
    use orbital_core_base::value::ValueMap;

    fn create_test_state_machine() -> StateMachine {
        StateMachine {
            states: vec![
                StateDefinition {
                    name: "browsing".to_string(),
                    is_initial: Some(true),
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                    holds: None,
                },
                StateDefinition {
                    name: "registering".to_string(),
                    is_initial: None,
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                    holds: None,
                },
                StateDefinition {
                    name: "viewing".to_string(),
                    is_initial: None,
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                    holds: None,
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
                    ensures: None,
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
                    ensures: None,
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
                    ensures: None,
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
                    ensures: None,
                },
            ],
        }
    }

    fn modal_machine() -> StateMachine {
        let arm = |to: &str, guard: Option<&str>| TransitionDefinition {
            from: StringOrArray::Single("NORMAL".to_string()),
            to: to.to_string(),
            event: "KEY".to_string(),
            event_id: None,
            guard: guard.map(|g| serde_json::from_str(g).unwrap()),
            effects: None,
            ensures: None,
        };
        StateMachine {
            states: vec![
                StateDefinition {
                    name: "NORMAL".to_string(),
                    is_initial: Some(true),
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                    holds: None,
                },
                StateDefinition {
                    name: "INSERT".to_string(),
                    is_initial: None,
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                    holds: None,
                },
                StateDefinition {
                    name: "VISUAL".to_string(),
                    is_initial: None,
                    is_terminal: None,
                    description: None,
                    time_bound_ms: None,
                    holds: None,
                },
            ],
            events: None,
            transitions: vec![
                arm("INSERT", Some(r#"["=", "@payload.key", "i"]"#)),
                arm("VISUAL", Some(r#"["=", "@payload.key", "v"]"#)),
            ],
        }
    }

    fn key(k: &str) -> EventPayload {
        let mut data = ValueMap::default();
        data.insert("key".into(), Value::string(k.to_string()));
        EventPayload::new("KEY", Value::object(data))
    }

    /// `modal_machine` with its first guard replaced by one the evaluator cannot run.
    fn machine_with_a_throwing_first_guard(second_guard: &str) -> StateMachine {
        let mut machine = modal_machine();
        machine.transitions[0].guard = Some(serde_json::from_str(r#"["=", 1]"#).unwrap());
        machine.transitions[1].guard = Some(serde_json::from_str(second_guard).unwrap());
        machine
    }

    #[test]
    fn a_guard_that_throws_blocks_its_arm_like_a_false_guard_and_the_next_arm_is_tried() {
        let result = StateMachineCore::new()
            .process_event(
                &machine_with_a_throwing_first_guard(r#"["=", "@payload.key", "v"]"#),
                "NORMAL",
                &key("v"),
                &BindingContext::new(),
            )
            .expect("a throwing guard must not abort the dispatch");
        assert!(result.executed);
        assert_eq!(result.fired_arm, Some(1));
        assert!(!result.guards[0].guard.passed);
        assert!(
            result.guards[0].guard.error.is_some(),
            "the guard's error is recorded"
        );
    }

    #[test]
    fn control_when_every_guard_throws_or_fails_nothing_fires_and_the_result_is_a_rejection() {
        let result = StateMachineCore::new()
            .process_event(
                &machine_with_a_throwing_first_guard(r#"["=", "@payload.key", "v"]"#),
                "NORMAL",
                &key("x"),
                &BindingContext::new(),
            )
            .expect("a throwing guard must not abort the dispatch");
        assert!(!result.executed);
        assert_eq!(result.fired_arm, None);
        assert_eq!(result.guards.len(), 2);
    }

    #[test]
    fn fired_arm_is_the_winning_arm_and_every_guard_tried_is_recorded() {
        let result = StateMachineCore::new()
            .process_event(
                &modal_machine(),
                "NORMAL",
                &key("v"),
                &BindingContext::new(),
            )
            .unwrap();
        assert!(result.executed);
        assert_eq!(result.fired_arm, Some(1));
        assert_eq!(
            result
                .guards
                .iter()
                .map(|g| (g.arm, g.guard.passed))
                .collect::<Vec<_>>(),
            vec![(0, false), (1, true)]
        );
    }

    #[test]
    fn control_no_guard_passes_no_arm_fires_but_the_guards_are_recorded() {
        let result = StateMachineCore::new()
            .process_event(
                &modal_machine(),
                "NORMAL",
                &key("x"),
                &BindingContext::new(),
            )
            .unwrap();
        assert!(!result.executed);
        assert_eq!(result.fired_arm, None);
        assert_eq!(
            result
                .guards
                .iter()
                .map(|g| (g.arm, g.guard.passed))
                .collect::<Vec<_>>(),
            vec![(0, false), (1, false)]
        );
        assert!(!result.guard_result.unwrap().passed);
    }

    #[test]
    fn edge_an_unguarded_arm_fires_with_no_guards_recorded() {
        let result = StateMachineCore::new()
            .process_event(
                &create_test_state_machine(),
                "browsing",
                &EventPayload::new("REGISTER", Value::object(ValueMap::default())),
                &BindingContext::new(),
            )
            .unwrap();
        assert_eq!(result.fired_arm, Some(0));
        assert!(result.guards.is_empty());
    }

    #[test]
    fn edge_no_matching_arm_has_no_fired_arm_and_no_guards() {
        let result = StateMachineCore::new()
            .process_event(
                &modal_machine(),
                "INSERT",
                &key("i"),
                &BindingContext::new(),
            )
            .unwrap();
        assert_eq!(result.fired_arm, None);
        assert!(result.guards.is_empty());
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

        let payload = EventPayload::new("REGISTER", Value::object(ValueMap::default()));
        let result = core.process_event(&sm, "browsing", &payload, &ctx).unwrap();

        assert!(result.executed);
        assert_eq!(result.previous_state, Some("browsing".to_string()));
        assert_eq!(result.new_state, Some("registering".to_string()));
        assert_eq!(result.effects().len(), 1);
    }

    #[test]
    fn test_transition_with_passing_guard() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine();
        let ctx = BindingContext::new();

        // Guard checks @payload.data.email and @payload.data.name
        let mut inner_data = ValueMap::default();
        inner_data.insert(
            "email".into(),
            Value::string("test@example.com"),
        );
        inner_data.insert("name".into(), Value::string("Test User"));
        inner_data.insert("id".into(), Value::string("user-123"));

        let mut data = ValueMap::default();
        data.insert("data".into(), Value::object(inner_data));

        let payload = EventPayload::new("SAVE", Value::object(data));
        let result = core
            .process_event(&sm, "registering", &payload, &ctx)
            .unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("browsing".to_string()));
        assert_eq!(result.effects().len(), 2); // persist + emit

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
        let mut inner_data = ValueMap::default();
        inner_data.insert("email".into(), Value::string(""));
        inner_data.insert("name".into(), Value::string("Test User"));

        let mut data = ValueMap::default();
        data.insert("data".into(), Value::object(inner_data));

        let payload = EventPayload::new("SAVE", Value::object(data));
        let result = core
            .process_event(&sm, "registering", &payload, &ctx)
            .unwrap();

        assert!(!result.executed);
        assert!(result.new_state.is_none());
        assert!(result.effects().is_empty());

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

        let payload = EventPayload::new("UNKNOWN_EVENT", Value::object(ValueMap::default()));
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

        let payload = EventPayload::new("CANCEL", Value::object(ValueMap::default()));
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
        sm.events = Some(vec![orbital_core_base::schema::types::EventDefinition {
            payload_entity: None,
            input: None,
            listened: None,
            inbound: None,
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
        let payload = EventPayload::new("REGISTER", Value::object(ValueMap::default()));
        let result = core.process_event(&sm, "browsing", &payload, &ctx).unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("registering".to_string()));
    }

    #[test]
    fn test_no_id_event_still_dispatches_via_name() {
        let core = StateMachineCore::new();
        let sm = create_test_state_machine(); // no event ids anywhere
        let ctx = BindingContext::new();

        let payload = EventPayload::new("VIEW", Value::object(ValueMap::default()));
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
