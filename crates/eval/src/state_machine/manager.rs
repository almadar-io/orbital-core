//! State Machine Manager
//!
//! Manages multiple trait instances and their states across entities.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::core::StateMachineCore;
use super::types::{EventPayload, TraitInstanceId, TraitState, TransitionResult};
use crate::evaluator::context::{BindingContext, BindingContextBuilder};
use crate::evaluator::Evaluator;
use orbital_core_base::error::OrbitalError;
use orbital_core_base::schema::types::TraitDefinition;
use orbital_core_base::value::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Manages state machine instances for multiple traits and entities
#[derive(Debug)]
pub struct StateMachineManager {
    /// The core state machine processor
    core: StateMachineCore,

    /// Registered trait definitions by name
    trait_definitions: HashMap<String, Arc<TraitDefinition>>,

    /// Current state for each trait instance
    states: HashMap<TraitInstanceId, TraitState>,

    /// Entity data stores for count queries.
    ///
    /// Shared via `Arc` so each event's evaluation context references the same
    /// stores instead of deep-cloning the whole map per event.
    entity_stores: Arc<HashMap<String, Vec<Value>>>,
}

impl Default for StateMachineManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachineManager {
    /// Create a new state machine manager
    pub fn new() -> Self {
        Self {
            core: StateMachineCore::new(),
            trait_definitions: HashMap::new(),
            states: HashMap::new(),
            entity_stores: Arc::new(HashMap::new()),
        }
    }

    /// Create a new manager with a custom evaluator
    pub fn with_evaluator(evaluator: Evaluator) -> Self {
        Self {
            core: StateMachineCore::with_evaluator(evaluator),
            trait_definitions: HashMap::new(),
            states: HashMap::new(),
            entity_stores: Arc::new(HashMap::new()),
        }
    }

    /// Register a trait definition
    pub fn register_trait(&mut self, trait_def: TraitDefinition) {
        self.trait_definitions
            .insert(trait_def.name.clone(), Arc::new(trait_def));
    }

    /// Register a definition another manager already holds, sharing it rather than copying it (a kernel fork).
    pub fn register_shared_trait(&mut self, trait_def: Arc<TraitDefinition>) {
        self.trait_definitions.insert(trait_def.name.clone(), trait_def);
    }

    /// The registered definition itself, to share with another manager.
    pub fn shared_trait(&self, name: &str) -> Option<Arc<TraitDefinition>> {
        self.trait_definitions.get(name).cloned()
    }

    /// Register multiple trait definitions
    pub fn register_traits(&mut self, traits: Vec<TraitDefinition>) {
        for trait_def in traits {
            self.register_trait(trait_def);
        }
    }

    /// Get a registered trait definition
    pub fn get_trait(&self, name: &str) -> Option<&TraitDefinition> {
        self.trait_definitions.get(name).map(|t| &**t)
    }

    /// Register an entity store for count queries
    pub fn register_entity_store(&mut self, entity_type: &str, entities: Vec<Value>) {
        Arc::make_mut(&mut self.entity_stores).insert(entity_type.to_string(), entities);
    }

    /// Get or create a trait state
    ///
    /// If the trait instance doesn't exist, creates it at the initial state.
    pub fn get_or_create_state(
        &mut self,
        trait_name: &str,
        entity_id: Option<&str>,
    ) -> Result<&TraitState, OrbitalError> {
        let instance_id = TraitInstanceId::new(trait_name, entity_id);

        if !self.states.contains_key(&instance_id) {
            let trait_def = self.trait_definitions.get(trait_name).ok_or_else(|| {
                OrbitalError::StateMachine(format!("Trait not found: {}", trait_name))
            })?;

            let initial_state =
                StateMachineCore::get_trait_initial_state(trait_def).ok_or_else(|| {
                    OrbitalError::StateMachine(format!(
                        "No initial state for trait: {}",
                        trait_name
                    ))
                })?;

            let state = if let Some(eid) = entity_id {
                TraitState::with_entity(trait_name, initial_state, eid, Value::Null)
            } else {
                TraitState::new(trait_name, initial_state)
            };

            self.states.insert(instance_id.clone(), state);
        }

        Ok(self.states.get(&instance_id).unwrap())
    }

    /// Get a trait state (read-only)
    pub fn get_state(&self, trait_name: &str, entity_id: Option<&str>) -> Option<&TraitState> {
        let instance_id = TraitInstanceId::new(trait_name, entity_id);
        self.states.get(&instance_id)
    }

    /// Get a mutable trait state
    pub fn get_state_mut(
        &mut self,
        trait_name: &str,
        entity_id: Option<&str>,
    ) -> Option<&mut TraitState> {
        let instance_id = TraitInstanceId::new(trait_name, entity_id);
        self.states.get_mut(&instance_id)
    }

    /// Process an event for a trait instance against a CALLER-BUILT binding
    /// context (§157 `identity`: guard/transition evaluation must see the exact
    /// same `payload`/`state`/`entity`/`user`/`config`/`currentTheme` the kernel
    /// already assembled for this dispatch — never a second, poorer context
    /// built from scratch). `entity_stores` (this manager's multi-entity index
    /// for `count`/aggregate guards) is layered on top, since it is
    /// manager-local state the caller cannot supply. Guards like
    /// `(when ["=", "@user.role", "clinician"])` and `@config.*` reads only see
    /// real values through this path; `process_event`/`process_event_with_entity`
    /// below (which build a state+entity-only context) remain for callers with
    /// no richer context to contribute.
    pub fn process_event_with_ctx(
        &mut self,
        trait_name: &str,
        entity_id: Option<&str>,
        payload: &EventPayload,
        base_ctx: &BindingContext,
    ) -> Result<TransitionResult, OrbitalError> {
        if !self.trait_definitions.contains_key(trait_name) {
            return Err(OrbitalError::StateMachine(format!(
                "Trait not found: {}",
                trait_name
            )));
        }
        self.get_or_create_state(trait_name, entity_id)?;

        let instance_id = TraitInstanceId::new(trait_name, entity_id);

        let Self {
            core,
            trait_definitions,
            states,
            entity_stores,
        } = self;

        let trait_def = trait_definitions.get(trait_name).unwrap();
        let state = states.get(&instance_id).unwrap();
        let current_state = state.current_state.clone();

        let mut ctx = base_ctx.child();
        ctx.set_entity_stores(Arc::clone(entity_stores));

        let result = core.process_event_shared(trait_def, &current_state, payload, &ctx)?;

        if result.executed {
            if let Some(new_state) = &result.new_state {
                let state = states.get_mut(&instance_id).unwrap();
                state.transition_to(new_state);
            }
        }

        Ok(result)
    }

    /// Process an event for a trait instance
    ///
    /// Returns the transition result with effects to execute.
    pub fn process_event(
        &mut self,
        trait_name: &str,
        entity_id: Option<&str>,
        payload: &EventPayload,
    ) -> Result<TransitionResult, OrbitalError> {
        // Ensure trait exists and state is created.
        if !self.trait_definitions.contains_key(trait_name) {
            return Err(OrbitalError::StateMachine(format!(
                "Trait not found: {}",
                trait_name
            )));
        }
        self.get_or_create_state(trait_name, entity_id)?;

        let instance_id = TraitInstanceId::new(trait_name, entity_id);

        // Split-borrow disjoint fields so the trait definition can be referenced
        // (not cloned) while `states` is later mutated.
        let Self {
            core,
            trait_definitions,
            states,
            entity_stores,
        } = self;

        let trait_def = trait_definitions.get(trait_name).unwrap();
        let state = states.get(&instance_id).unwrap();
        let current_state = state.current_state.clone();

        // Build evaluation context (borrows entity_stores by Arc, not by deep clone).
        let ctx = Self::build_context_with(state, entity_stores);

        // Process through core
        let result = core.process_event_shared(trait_def, &current_state, payload, &ctx)?;

        // Update state if transition executed
        if result.executed {
            if let Some(new_state) = &result.new_state {
                let state = states.get_mut(&instance_id).unwrap();
                state.transition_to(new_state);
            }
        }

        Ok(result)
    }

    /// Process an event with custom entity data
    pub fn process_event_with_entity(
        &mut self,
        trait_name: &str,
        entity_id: &str,
        entity_data: Value,
        payload: &EventPayload,
    ) -> Result<TransitionResult, OrbitalError> {
        // Ensure trait exists and state is created.
        if !self.trait_definitions.contains_key(trait_name) {
            return Err(OrbitalError::StateMachine(format!(
                "Trait not found: {}",
                trait_name
            )));
        }
        self.get_or_create_state(trait_name, Some(entity_id))?;

        let instance_id = TraitInstanceId::for_entity(trait_name, entity_id);

        let Self {
            core,
            trait_definitions,
            states,
            entity_stores,
        } = self;

        // Update entity data
        {
            let state = states.get_mut(&instance_id).unwrap();
            state.set_entity_data(entity_data.clone());
        }

        let trait_def = trait_definitions.get(trait_name).unwrap();
        let state = states.get(&instance_id).unwrap();
        let current_state = state.current_state.clone();

        // Build evaluation context with entity data
        let mut ctx = Self::build_context_with(state, entity_stores);
        ctx.set_entity(entity_data);

        // Process through core
        let result = core.process_event_shared(trait_def, &current_state, payload, &ctx)?;

        // Update state if transition executed
        if result.executed {
            if let Some(new_state) = &result.new_state {
                let state = states.get_mut(&instance_id).unwrap();
                state.transition_to(new_state);
            }
        }

        Ok(result)
    }

    /// Build evaluation context, sharing the given entity stores by `Arc`.
    fn build_context_with(
        state: &TraitState,
        entity_stores: &Arc<HashMap<String, Vec<Value>>>,
    ) -> BindingContext {
        let mut builder =
            BindingContextBuilder::new().state(Value::string(state.current_state.clone()));

        if let Some(entity_data) = &state.entity_data {
            builder = builder.entity(entity_data.clone());
        }

        // Share entity stores by Arc instead of deep-cloning every collection.
        let mut ctx = builder.build();
        ctx.set_entity_stores(Arc::clone(entity_stores));

        ctx
    }

    /// Initialize a trait at a specific state (for testing or recovery)
    pub fn initialize_trait(
        &mut self,
        trait_name: &str,
        entity_id: Option<&str>,
        state_name: &str,
    ) -> Result<(), OrbitalError> {
        let trait_def = self.trait_definitions.get(trait_name).ok_or_else(|| {
            OrbitalError::StateMachine(format!("Trait not found: {}", trait_name))
        })?;

        // Validate state exists
        if !StateMachineCore::is_valid_state(&trait_def.state_machine, state_name) {
            return Err(OrbitalError::StateMachine(format!(
                "Invalid state '{}' for trait '{}'",
                state_name, trait_name
            )));
        }

        let instance_id = TraitInstanceId::new(trait_name, entity_id);
        let state = if let Some(eid) = entity_id {
            TraitState::with_entity(trait_name, state_name, eid, Value::Null)
        } else {
            TraitState::new(trait_name, state_name)
        };

        self.states.insert(instance_id, state);
        Ok(())
    }

    /// Reset a trait to its initial state
    pub fn reset_trait(
        &mut self,
        trait_name: &str,
        entity_id: Option<&str>,
    ) -> Result<(), OrbitalError> {
        let trait_def = self.trait_definitions.get(trait_name).ok_or_else(|| {
            OrbitalError::StateMachine(format!("Trait not found: {}", trait_name))
        })?;

        let initial_state =
            StateMachineCore::get_trait_initial_state(trait_def).ok_or_else(|| {
                OrbitalError::StateMachine(format!("No initial state for trait: {}", trait_name))
            })?;

        let instance_id = TraitInstanceId::new(trait_name, entity_id);
        if let Some(state) = self.states.get_mut(&instance_id) {
            state.transition_to(initial_state);
        }

        Ok(())
    }

    /// Remove a trait instance
    pub fn remove_trait_instance(&mut self, trait_name: &str, entity_id: Option<&str>) {
        let instance_id = TraitInstanceId::new(trait_name, entity_id);
        self.states.remove(&instance_id);
    }

    /// Keep only the instances `keep` accepts.
    pub fn retain_instances(&mut self, mut keep: impl FnMut(&TraitState) -> bool) {
        self.states.retain(|_, state| keep(state));
    }

    /// Get all trait instances for a trait name
    pub fn get_instances_for_trait(&self, trait_name: &str) -> Vec<&TraitState> {
        self.states
            .iter()
            .filter(|(id, _)| id.trait_name == trait_name)
            .map(|(_, state)| state)
            .collect()
    }

    /// Get all trait instances for an entity
    pub fn get_instances_for_entity(&self, entity_id: &str) -> Vec<&TraitState> {
        self.states
            .iter()
            .filter(|(id, _)| id.entity_id.as_deref() == Some(entity_id))
            .map(|(_, state)| state)
            .collect()
    }

    /// Get statistics about the manager
    pub fn stats(&self) -> ManagerStats {
        ManagerStats {
            registered_traits: self.trait_definitions.len(),
            active_instances: self.states.len(),
            entity_stores: self.entity_stores.len(),
        }
    }
}

/// Statistics about the state machine manager
#[derive(Debug, Clone)]
pub struct ManagerStats {
    pub registered_traits: usize,
    pub active_instances: usize,
    pub entity_stores: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::schema::types::{
        StateDefinition, StateMachine, StringOrArray, TransitionDefinition,
    };
    use orbital_core_base::value::ValueMap;

    fn create_user_management_trait() -> TraitDefinition {
        TraitDefinition {
            effect_row: None,
            type_params: None,
            type_args: None,
            name: "UserManagement".to_string(),
            entity_rebindable: None,
            entity_contract: None,
            description: Some("Manages user CRUD operations".to_string()),
            linked_entity: Some("User".to_string()),
            entity_ref_ids: None,
            trait_embed_ids: None,
            emits: None,
            listens: None,
            state_machine: StateMachine {
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
                    StateDefinition {
                        name: "editing".to_string(),
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
                        effects: None,
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
                    TransitionDefinition {
                        from: StringOrArray::Single("viewing".to_string()),
                        to: "editing".to_string(),
                        event: "EDIT".to_string(),
                        event_id: None,
                        guard: None,
                        effects: Some(vec![
                            serde_json::from_str(r#"["render-ui", "main", {"type": "form-section"}]"#).unwrap(),
                        ]),
                        ensures: None,
                    },
                    TransitionDefinition {
                        from: StringOrArray::Single("editing".to_string()),
                        to: "viewing".to_string(),
                        event: "SAVE".to_string(),
                        event_id: None,
                        guard: Some(
                            serde_json::from_str(r#"["!=", "@payload.data.name", ""]"#).unwrap(),
                        ),
                        effects: Some(vec![
                            serde_json::from_str(r#"["persist", "update", "User", "@entity.id", "@payload.data"]"#).unwrap(),
                        ]),
                        ensures: None,
                    },
                    TransitionDefinition {
                        from: StringOrArray::Single("editing".to_string()),
                        to: "viewing".to_string(),
                        event: "CANCEL".to_string(),
                        event_id: None,
                        guard: None,
                        effects: None,
                        ensures: None,
                    },
                    TransitionDefinition {
                        from: StringOrArray::Single("viewing".to_string()),
                        to: "browsing".to_string(),
                        event: "BACK".to_string(),
                        event_id: None,
                        guard: None,
                        effects: None,
                        ensures: None,
                    },
                ],
            }.into(),
            ticks: None,
            config: None,
            data_entities: None,
            dispatch: None,
            source_declaration: None,
            source_behavior: None,
            source_entity_definition: None,
                    id: None,
                    linked_entity_id: None,
}
    }

    #[test]
    fn test_register_trait() {
        let mut manager = StateMachineManager::new();
        let trait_def = create_user_management_trait();

        manager.register_trait(trait_def);

        assert!(manager.get_trait("UserManagement").is_some());
        assert!(manager.get_trait("NonExistent").is_none());
    }

    #[test]
    fn test_get_or_create_state() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        // First call creates state
        let state = manager.get_or_create_state("UserManagement", None).unwrap();
        assert_eq!(state.current_state, "browsing");
        assert_eq!(state.trait_name, "UserManagement");

        // Second call returns existing state
        let state2 = manager.get_or_create_state("UserManagement", None).unwrap();
        assert_eq!(state2.current_state, "browsing");
    }

    #[test]
    fn test_process_simple_event() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        let payload = EventPayload::new("REGISTER", Value::object(ValueMap::default()));
        let result = manager
            .process_event("UserManagement", None, &payload)
            .unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("registering".to_string()));

        // State should be updated
        let state = manager.get_state("UserManagement", None).unwrap();
        assert_eq!(state.current_state, "registering");
    }

    #[test]
    fn test_process_event_with_guard() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        // First go to registering state
        let payload = EventPayload::new("REGISTER", Value::object(ValueMap::default()));
        manager
            .process_event("UserManagement", None, &payload)
            .unwrap();

        // Try to save with valid data (guard checks @payload.data.email/name)
        let mut inner_data = ValueMap::default();
        inner_data.insert(
            "email".to_string(),
            Value::string("test@example.com"),
        );
        inner_data.insert("name".to_string(), Value::string("Test User"));
        inner_data.insert("id".to_string(), Value::string("user-123"));

        let mut data = ValueMap::default();
        data.insert("data".to_string(), Value::object(inner_data));

        let save_payload = EventPayload::new("SAVE", Value::object(data));
        let result = manager
            .process_event("UserManagement", None, &save_payload)
            .unwrap();

        assert!(result.executed);
        assert_eq!(result.new_state, Some("browsing".to_string()));
        assert_eq!(result.effects().len(), 2); // persist + emit
    }

    #[test]
    fn test_process_event_guard_blocks() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        // Go to registering state
        let payload = EventPayload::new("REGISTER", Value::object(ValueMap::default()));
        manager
            .process_event("UserManagement", None, &payload)
            .unwrap();

        // Try to save with empty email (should fail guard @payload.data.email != "")
        let mut inner_data = ValueMap::default();
        inner_data.insert("email".to_string(), Value::string(""));
        inner_data.insert("name".to_string(), Value::string("Test User"));

        let mut data = ValueMap::default();
        data.insert("data".to_string(), Value::object(inner_data));

        let save_payload = EventPayload::new("SAVE", Value::object(data));
        let result = manager
            .process_event("UserManagement", None, &save_payload)
            .unwrap();

        assert!(!result.executed);

        // State should remain registering
        let state = manager.get_state("UserManagement", None).unwrap();
        assert_eq!(state.current_state, "registering");
    }

    #[test]
    fn test_multiple_trait_instances() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        // Create instances for different entities
        manager
            .get_or_create_state("UserManagement", Some("user-1"))
            .unwrap();
        manager
            .get_or_create_state("UserManagement", Some("user-2"))
            .unwrap();
        manager
            .get_or_create_state("UserManagement", Some("user-3"))
            .unwrap();

        // Move user-1 to viewing
        let view_payload = EventPayload::new("VIEW", Value::object(ValueMap::default()));
        manager
            .process_event("UserManagement", Some("user-1"), &view_payload)
            .unwrap();

        // Move user-2 to registering
        let register_payload = EventPayload::new("REGISTER", Value::object(ValueMap::default()));
        manager
            .process_event("UserManagement", Some("user-2"), &register_payload)
            .unwrap();

        // Verify states
        let state1 = manager.get_state("UserManagement", Some("user-1")).unwrap();
        let state2 = manager.get_state("UserManagement", Some("user-2")).unwrap();
        let state3 = manager.get_state("UserManagement", Some("user-3")).unwrap();

        assert_eq!(state1.current_state, "viewing");
        assert_eq!(state2.current_state, "registering");
        assert_eq!(state3.current_state, "browsing");
    }

    #[test]
    fn test_initialize_trait_at_state() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        // Initialize at editing state
        manager
            .initialize_trait("UserManagement", Some("user-1"), "editing")
            .unwrap();

        let state = manager.get_state("UserManagement", Some("user-1")).unwrap();
        assert_eq!(state.current_state, "editing");
    }

    #[test]
    fn test_reset_trait() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        // Move to a different state
        let view_payload = EventPayload::new("VIEW", Value::object(ValueMap::default()));
        manager
            .process_event("UserManagement", None, &view_payload)
            .unwrap();

        let state = manager.get_state("UserManagement", None).unwrap();
        assert_eq!(state.current_state, "viewing");

        // Reset to initial
        manager.reset_trait("UserManagement", None).unwrap();

        let state = manager.get_state("UserManagement", None).unwrap();
        assert_eq!(state.current_state, "browsing");
    }

    #[test]
    fn test_get_instances_for_trait() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        manager
            .get_or_create_state("UserManagement", Some("user-1"))
            .unwrap();
        manager
            .get_or_create_state("UserManagement", Some("user-2"))
            .unwrap();
        manager.get_or_create_state("UserManagement", None).unwrap();

        let instances = manager.get_instances_for_trait("UserManagement");
        assert_eq!(instances.len(), 3);
    }

    #[test]
    fn test_full_user_flow() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        // 1. Start at browsing
        manager.get_or_create_state("UserManagement", None).unwrap();

        // 2. Click register -> registering
        let result = manager
            .process_event(
                "UserManagement",
                None,
                &EventPayload::new("REGISTER", Value::object(ValueMap::default())),
            )
            .unwrap();
        assert!(result.executed);
        assert_eq!(
            manager
                .get_state("UserManagement", None)
                .unwrap()
                .current_state,
            "registering"
        );

        // 3. Submit form with valid data -> browsing (guard checks @payload.data.email/name)
        let mut inner_data = ValueMap::default();
        inner_data.insert(
            "email".to_string(),
            Value::string("new@example.com"),
        );
        inner_data.insert("name".to_string(), Value::string("New User"));
        inner_data.insert("id".to_string(), Value::string("user-new"));

        let mut data = ValueMap::default();
        data.insert("data".to_string(), Value::object(inner_data));

        let result = manager
            .process_event(
                "UserManagement",
                None,
                &EventPayload::new("SAVE", Value::object(data)),
            )
            .unwrap();
        assert!(result.executed);
        assert_eq!(result.effects().len(), 2); // persist + emit
        assert_eq!(
            manager
                .get_state("UserManagement", None)
                .unwrap()
                .current_state,
            "browsing"
        );

        // 4. View a user -> viewing
        let result = manager
            .process_event(
                "UserManagement",
                None,
                &EventPayload::new("VIEW", Value::object(ValueMap::default())),
            )
            .unwrap();
        assert!(result.executed);
        assert_eq!(
            manager
                .get_state("UserManagement", None)
                .unwrap()
                .current_state,
            "viewing"
        );

        // 5. Edit user -> editing
        let result = manager
            .process_event(
                "UserManagement",
                None,
                &EventPayload::new("EDIT", Value::object(ValueMap::default())),
            )
            .unwrap();
        assert!(result.executed);
        assert_eq!(
            manager
                .get_state("UserManagement", None)
                .unwrap()
                .current_state,
            "editing"
        );

        // 6. Cancel edit -> viewing
        let result = manager
            .process_event(
                "UserManagement",
                None,
                &EventPayload::new("CANCEL", Value::object(ValueMap::default())),
            )
            .unwrap();
        assert!(result.executed);
        assert_eq!(
            manager
                .get_state("UserManagement", None)
                .unwrap()
                .current_state,
            "viewing"
        );

        // 7. Go back -> browsing
        let result = manager
            .process_event(
                "UserManagement",
                None,
                &EventPayload::new("BACK", Value::object(ValueMap::default())),
            )
            .unwrap();
        assert!(result.executed);
        assert_eq!(
            manager
                .get_state("UserManagement", None)
                .unwrap()
                .current_state,
            "browsing"
        );
    }

    #[test]
    fn test_stats() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_user_management_trait());

        manager
            .get_or_create_state("UserManagement", Some("user-1"))
            .unwrap();
        manager
            .get_or_create_state("UserManagement", Some("user-2"))
            .unwrap();

        manager.register_entity_store("User", vec![Value::Null]);

        let stats = manager.stats();
        assert_eq!(stats.registered_traits, 1);
        assert_eq!(stats.active_instances, 2);
        assert_eq!(stats.entity_stores, 1);
    }

    fn create_gated_trait() -> TraitDefinition {
        TraitDefinition {
            effect_row: None,
            type_params: None,
            type_args: None,
            name: "TicketOwner".to_string(),
            entity_rebindable: None,
            entity_contract: None,
            description: None,
            linked_entity: Some("Ticket".to_string()),
            entity_ref_ids: None,
            trait_embed_ids: None,
            emits: None,
            listens: None,
            state_machine: StateMachine {
                states: vec![
                    StateDefinition {
                        name: "open".to_string(),
                        is_initial: Some(true),
                        is_terminal: None,
                        description: None,
                        time_bound_ms: None,
                        holds: None,
                    },
                    StateDefinition {
                        name: "closed".to_string(),
                        is_initial: None,
                        is_terminal: None,
                        description: None,
                        time_bound_ms: None,
                        holds: None,
                    },
                ],
                events: None,
                transitions: vec![TransitionDefinition {
                    from: StringOrArray::Single("open".to_string()),
                    to: "closed".to_string(),
                    event: "CLOSE".to_string(),
                    event_id: None,
                    guard: Some(
                        serde_json::from_str(r#"["=", "@user.role", "clinician"]"#).unwrap(),
                    ),
                    effects: Some(vec![serde_json::from_str(
                        r#"["set", "@entity.limitSeen", "@config.limit"]"#,
                    )
                    .unwrap()]),
                    ensures: None,
                }],
            }.into(),
            ticks: None,
            config: None,
            data_entities: None,
            dispatch: None,
            source_declaration: None,
            source_behavior: None,
            source_entity_definition: None,
            id: None,
            linked_entity_id: None,
        }
    }

    /// §157 `identity`: guard/transition evaluation must run against the SAME
    /// `BindingContext` the kernel built for this dispatch — `@user.*` and
    /// `@config.*` must be real values inside `(when ...)`, not resolve to
    /// `Null` because the manager rebuilt a state+entity-only context.
    #[test]
    fn guards_see_user_and_config() {
        let mut manager = StateMachineManager::new();
        manager.register_trait(create_gated_trait());

        let mut user = ValueMap::default();
        user.insert("id".to_string(), Value::string("u1"));
        user.insert("role".to_string(), Value::string("clinician"));
        let mut config = ValueMap::default();
        config.insert("limit".to_string(), Value::Number(10.0));

        let ctx = BindingContextBuilder::new()
            .user(Value::object(user))
            .config(Value::object(config))
            .build();

        let payload = EventPayload::new("CLOSE", Value::Null);
        let result = manager
            .process_event_with_ctx("TicketOwner", None, &payload, &ctx)
            .unwrap();

        assert!(
            result.executed,
            "the @user.role guard must pass when the caller-built context carries the real user"
        );
        assert_eq!(result.new_state, Some("closed".to_string()));

        // A different role fails the same guard through the same path.
        let mut other_user = ValueMap::default();
        other_user.insert("id".to_string(), Value::string("u2"));
        other_user.insert("role".to_string(), Value::string("member"));
        let other_ctx = BindingContextBuilder::new()
            .user(Value::object(other_user))
            .build();

        let mut manager2 = StateMachineManager::new();
        manager2.register_trait(create_gated_trait());
        let result2 = manager2
            .process_event_with_ctx("TicketOwner", None, &payload, &other_ctx)
            .unwrap();
        assert!(
            !result2.executed,
            "a non-clinician role must fail the @user.role guard"
        );
    }
}
