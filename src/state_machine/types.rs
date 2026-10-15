//! State Machine Types
//!
//! Core types for state machine execution.

use crate::evaluator::Value;
use crate::evaluator::ValueMap;
use crate::schema::{SExpression, TraitDefinition};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Current state of a trait instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraitState {
    /// Trait name
    pub trait_name: String,

    /// Current state name
    pub current_state: String,

    /// Entity ID this trait is bound to (if any)
    pub entity_id: Option<String>,

    /// Entity data
    pub entity_data: Option<Value>,

    /// Additional state data
    pub data: ValueMap,

    /// Timestamp when state was entered
    pub entered_at: i64,
}

impl TraitState {
    /// Create a new trait state at the initial state
    pub fn new(trait_name: &str, initial_state: &str) -> Self {
        Self {
            trait_name: trait_name.to_string(),
            current_state: initial_state.to_string(),
            entity_id: None,
            entity_data: None,
            data: ValueMap::new(),
            entered_at: crate::time_util::now_millis(),
        }
    }

    /// Create a new trait state bound to an entity
    pub fn with_entity(
        trait_name: &str,
        initial_state: &str,
        entity_id: &str,
        entity_data: Value,
    ) -> Self {
        Self {
            trait_name: trait_name.to_string(),
            current_state: initial_state.to_string(),
            entity_id: Some(entity_id.to_string()),
            entity_data: Some(entity_data),
            data: ValueMap::new(),
            entered_at: crate::time_util::now_millis(),
        }
    }

    /// Transition to a new state
    pub fn transition_to(&mut self, new_state: &str) {
        self.current_state = new_state.to_string();
        self.entered_at = crate::time_util::now_millis();
    }

    /// Update entity data
    pub fn set_entity_data(&mut self, data: Value) {
        self.entity_data = Some(data);
    }

    /// Get a data value by key
    pub fn get_data(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    /// Set a data value
    pub fn set_data(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_string(), value);
    }
}

/// Result of processing an event through the state machine
#[derive(Debug, Clone)]
pub struct TransitionResult {
    /// Whether a transition was executed
    pub executed: bool,

    /// Previous state (if transition executed)
    pub previous_state: Option<String>,

    /// New state (if transition executed)
    pub new_state: Option<String>,

    /// Effects to execute (if transition executed); read them through [`TransitionResult::effects`].
    pub effect_list: TransitionEffects,

    /// Guard evaluation result (if guard was present)
    pub guard_result: Option<GuardResult>,

    /// Reason for not executing (if not executed)
    pub reason: Option<TransitionFailureReason>,

    /// Position of the arm that fired among the state's `from --event-->` arms, in declaration order.
    pub fired_arm: Option<usize>,

    /// Every guarded arm evaluated while choosing the arm, in order.
    pub guards: Vec<ArmGuard>,
}

/// The effect list of a fired transition: borrowed from the registered trait (one `Arc` bump, no
/// copy of the expressions) or owned when the caller handed the machine in by reference.
#[derive(Debug, Clone)]
pub enum TransitionEffects {
    None,
    Shared {
        definition: Arc<TraitDefinition>,
        transition: usize,
    },
    Owned(Vec<SExpression>),
}

impl TransitionEffects {
    pub fn as_slice(&self) -> &[SExpression] {
        match self {
            Self::None => &[],
            Self::Shared {
                definition,
                transition,
            } => definition.state_machine.transitions[*transition]
                .effects
                .as_deref()
                .unwrap_or(&[]),
            Self::Owned(effects) => effects,
        }
    }
}

/// One guarded arm evaluated during arm selection.
#[derive(Debug, Clone)]
pub struct ArmGuard {
    /// Position of the arm among the state's `from --event-->` arms.
    pub arm: usize,
    pub guard: GuardResult,
}

impl TransitionResult {
    /// The effects to execute (empty when no transition fired).
    pub fn effects(&self) -> &[SExpression] {
        self.effect_list.as_slice()
    }

    /// Create a successful transition result
    pub fn success(previous_state: &str, new_state: &str, effects: TransitionEffects) -> Self {
        Self {
            executed: true,
            previous_state: Some(previous_state.to_string()),
            new_state: Some(new_state.to_string()),
            effect_list: effects,
            guard_result: None,
            reason: None,
            fired_arm: None,
            guards: Vec::new(),
        }
    }

    /// Create a successful transition with guard result
    pub fn success_with_guard(
        previous_state: &str,
        new_state: &str,
        effects: TransitionEffects,
        guard_result: GuardResult,
    ) -> Self {
        Self {
            executed: true,
            previous_state: Some(previous_state.to_string()),
            new_state: Some(new_state.to_string()),
            effect_list: effects,
            guard_result: Some(guard_result),
            reason: None,
            fired_arm: None,
            guards: Vec::new(),
        }
    }

    /// Create a failed transition (no matching transition)
    pub fn no_transition(current_state: &str, event: &str) -> Self {
        Self {
            executed: false,
            previous_state: Some(current_state.to_string()),
            new_state: None,
            effect_list: TransitionEffects::None,
            guard_result: None,
            reason: Some(TransitionFailureReason::NoMatchingTransition {
                state: current_state.to_string(),
                event: event.to_string(),
            }),
            fired_arm: None,
            guards: Vec::new(),
        }
    }

    /// Create a failed transition (guard blocked)
    pub fn guard_blocked(
        current_state: &str,
        target_state: &str,
        event: &str,
        guard_result: GuardResult,
    ) -> Self {
        Self {
            executed: false,
            previous_state: Some(current_state.to_string()),
            new_state: None,
            effect_list: TransitionEffects::None,
            guard_result: Some(guard_result),
            reason: Some(TransitionFailureReason::GuardBlocked {
                state: current_state.to_string(),
                target_state: target_state.to_string(),
                event: event.to_string(),
            }),
            fired_arm: None,
            guards: Vec::new(),
        }
    }
}

/// Result of evaluating a guard
#[derive(Debug, Clone)]
pub struct GuardResult {
    /// Whether the guard passed
    pub passed: bool,

    /// The guard expression that was evaluated
    pub guard: Arc<SExpression>,

    /// The evaluated value
    pub value: Value,

    /// Error message if evaluation failed
    pub error: Option<String>,
}

impl GuardResult {
    /// Create a passed guard result
    pub fn passed(guard: impl Into<Arc<SExpression>>, value: Value) -> Self {
        Self {
            passed: true,
            guard: guard.into(),
            value,
            error: None,
        }
    }

    /// Create a failed guard result
    pub fn failed(guard: impl Into<Arc<SExpression>>, value: Value) -> Self {
        Self {
            passed: false,
            guard: guard.into(),
            value,
            error: None,
        }
    }

    /// Create an error guard result
    pub fn error(guard: impl Into<Arc<SExpression>>, error: String) -> Self {
        Self {
            passed: false,
            guard: guard.into(),
            value: Value::Null,
            error: Some(error),
        }
    }
}

/// Reason why a transition failed
#[derive(Debug, Clone)]
pub enum TransitionFailureReason {
    /// No transition matches the current state and event
    NoMatchingTransition { state: String, event: String },

    /// Guard condition blocked the transition
    GuardBlocked {
        state: String,
        target_state: String,
        event: String,
    },

    /// Guard evaluation error
    GuardError {
        state: String,
        event: String,
        error: String,
    },

    /// State machine not found
    StateMachineNotFound { trait_name: String },

    /// Invalid state
    InvalidState { state: String },
}

/// Event payload for processing
#[derive(Debug, Clone)]
pub struct EventPayload {
    /// Event name
    pub event: String,

    /// Event data
    pub data: Value,

    /// Source trait (for cross-orbital events)
    pub source: Option<String>,

    /// Timestamp
    pub timestamp: i64,
}

impl EventPayload {
    /// Create a new event payload
    pub fn new(event: &str, data: Value) -> Self {
        Self {
            event: event.to_string(),
            data,
            source: None,
            timestamp: crate::time_util::now_millis(),
        }
    }

    /// Create an event with source
    pub fn with_source(event: &str, data: Value, source: &str) -> Self {
        Self {
            event: event.to_string(),
            data,
            source: Some(source.to_string()),
            timestamp: crate::time_util::now_millis(),
        }
    }
}

/// Unique identifier for a trait instance
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct TraitInstanceId {
    /// Trait name
    pub trait_name: String,

    /// Entity ID (empty for singleton traits)
    pub entity_id: Option<String>,
}

impl TraitInstanceId {
    /// Create a new trait instance ID
    pub fn new(trait_name: &str, entity_id: Option<&str>) -> Self {
        Self {
            trait_name: trait_name.to_string(),
            entity_id: entity_id.map(|s| s.to_string()),
        }
    }

    /// Create an ID for a singleton trait
    pub fn singleton(trait_name: &str) -> Self {
        Self {
            trait_name: trait_name.to_string(),
            entity_id: None,
        }
    }

    /// Create an ID for an entity-bound trait
    pub fn for_entity(trait_name: &str, entity_id: &str) -> Self {
        Self {
            trait_name: trait_name.to_string(),
            entity_id: Some(entity_id.to_string()),
        }
    }
}

impl std::fmt::Display for TraitInstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.entity_id {
            Some(id) => write!(f, "{}:{}", self.trait_name, id),
            None => write!(f, "{}", self.trait_name),
        }
    }
}
