//! Orbital Client Runtime
//!
//! Thin client-side wrapper around `crate::RuntimeKernel`.

use crate::{
    runtime::{InMemoryEntityCache, KernelConfig, ProcessEventResult, RuntimeKernel, RuntimeStats},
    RuntimeEnvironment, TraitState, Value,
};
use std::sync::Arc;
use tracing::debug;

/// Orbital event
#[derive(Debug, Clone)]
pub struct OrbitalEvent {
    /// Trait name
    pub trait_name: String,
    /// Entity ID (optional for singleton traits)
    pub entity_id: Option<String>,
    /// Event name
    pub event: String,
    /// Event payload
    pub payload: Value,
}

impl OrbitalEvent {
    /// Create a new orbital event
    pub fn new(trait_name: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            trait_name: trait_name.into(),
            entity_id: None,
            event: event.into(),
            payload: Value::Null,
        }
    }

    /// Set the entity ID
    pub fn with_entity(mut self, entity_id: impl Into<String>) -> Self {
        self.entity_id = Some(entity_id.into());
        self
    }

    /// Set the payload
    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
}

/// Request to process an event (for server delegation)
#[derive(Debug, Clone)]
pub struct ProcessEventRequest {
    pub trait_name: String,
    pub entity_id: String,
    pub event: String,
    pub payload: Value,
}

/// Client runtime configuration
#[derive(Debug, Clone, Default)]
pub struct RuntimeConfig {
    /// Server URL for delegated effects
    pub server_url: Option<String>,
    /// Enable offline mode
    pub offline_mode: bool,
    /// Enable debug logging
    pub debug: bool,
}

/// Orbital Client Runtime
///
/// Manages schemas, state machines, and effect execution on the client by
/// delegating to `crate::RuntimeKernel`.
pub struct OrbitalClientRuntime {
    config: RuntimeConfig,
    kernel: RuntimeKernel,
}

impl OrbitalClientRuntime {
    /// Create a new client runtime
    pub fn new() -> Self {
        Self::with_config(RuntimeConfig::default())
    }

    /// Create a new client runtime with configuration
    pub fn with_config(config: RuntimeConfig) -> Self {
        let entity_source = Arc::new(InMemoryEntityCache::new());
        // Client connection settings (`server_url`/`offline_mode`/`debug`) live on
        // `config` and are read directly by the client; only the execution
        // environment crosses into the kernel.
        let kernel_config = KernelConfig::for_environment(RuntimeEnvironment::Client);
        let kernel = RuntimeKernel::new(RuntimeEnvironment::Client, entity_source, kernel_config);

        Self { config, kernel }
    }

    /// Register a schema with the runtime
    pub fn register_schema(&self, schema: crate::OrbitalSchema) {
        if let Err(err) = self.kernel.register_schema(schema) {
            debug!(error = %err, "Failed to register schema");
        }
    }

    /// Process an orbital event
    pub fn process_event(&self, event: &OrbitalEvent) -> Result<ProcessEventResult, String> {
        self.kernel
            .process_event(
                &event.trait_name,
                event.entity_id.as_deref(),
                &event.event,
                event.payload.clone(),
            )
            .map_err(|e| e.to_string())
    }

    /// Get the current state for an entity
    pub fn get_state(&self, trait_name: &str, entity_id: Option<&str>) -> Option<TraitState> {
        self.kernel.get_state(trait_name, entity_id)
    }

    /// Set the current user
    pub fn set_current_user(&self, user: Value) {
        self.kernel.set_current_user(user);
    }

    /// Get the current user
    pub fn get_current_user(&self) -> Value {
        self.kernel.get_current_user()
    }

    /// Cache entity data
    pub fn cache_entity(&self, entity_type: &str, _entity_id: &str, data: Value) {
        let _ = self.kernel.entity_source().create(entity_type, &data);
    }

    /// Get cached entity data
    pub fn get_cached_entity(&self, entity_type: &str, entity_id: &str) -> Option<Value> {
        self.kernel
            .entity_source()
            .read(entity_type, entity_id)
            .ok()
    }

    /// Clear entity cache
    pub fn clear_cache(&self) {
        self.kernel.entity_source().clear();
    }

    /// Get runtime statistics
    pub fn stats(&self) -> RuntimeStats {
        self.kernel.stats()
    }

    /// Get configuration
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Check if server delegation is enabled
    pub fn has_server(&self) -> bool {
        self.config.server_url.is_some() && !self.config.offline_mode
    }
}

impl Default for OrbitalClientRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::evaluator::ValueMap;
    use super::*;

    fn create_test_schema() -> crate::OrbitalSchema {
        serde_json::from_str(
            r#"{
            "name": "TestApp",
            "version": "1.0.0",
            "orbitals": [
                {
                    "name": "Counter",
                    "entity": {
                        "name": "Counter",
                        "collection": "counters",
                        "fields": [
                            { "name": "id", "type": "string", "primaryKey": true },
                            { "name": "value", "type": "number", "default": 0 }
                        ]
                    },
                    "traits": [
                        {
                            "name": "CounterManagement",
                            "linkedEntity": "Counter",
                            "stateMachine": {
                                "states": [
                                    { "name": "idle", "isInitial": true },
                                    { "name": "counting" }
                                ],
                                "transitions": [
                                    {
                                        "from": "idle",
                                        "to": "counting",
                                        "event": "START",
                                        "effects": []
                                    },
                                    {
                                        "from": "counting",
                                        "to": "counting",
                                        "event": "INCREMENT",
                                        "effects": [
                                            ["log", "info", "Counter incremented"]
                                        ]
                                    },
                                    {
                                        "from": "counting",
                                        "to": "idle",
                                        "event": "STOP",
                                        "effects": []
                                    }
                                ]
                            }
                        }
                    ]
                }
            ]
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn test_runtime_creation() {
        let runtime = OrbitalClientRuntime::new();
        let stats = runtime.stats();
        assert_eq!(stats.registered_schemas, 0);
        assert_eq!(stats.registered_traits, 0);
    }

    #[test]
    fn test_schema_registration() {
        let runtime = OrbitalClientRuntime::new();
        let schema = create_test_schema();

        runtime.register_schema(schema);

        let stats = runtime.stats();
        assert_eq!(stats.registered_schemas, 1);
        assert_eq!(stats.registered_traits, 1);
    }

    #[test]
    fn test_process_event() {
        let runtime = OrbitalClientRuntime::new();
        let schema = create_test_schema();
        runtime.register_schema(schema);

        let event = OrbitalEvent::new("CounterManagement", "START").with_entity("counter-1");

        let result = runtime.process_event(&event).unwrap();

        assert!(result.executed);
        assert_eq!(result.previous_state, "idle");
        assert_eq!(result.new_state, "counting");
    }

    #[test]
    fn test_get_state() {
        let runtime = OrbitalClientRuntime::new();
        let schema = create_test_schema();
        runtime.register_schema(schema);

        let event = OrbitalEvent::new("CounterManagement", "START").with_entity("counter-1");
        runtime.process_event(&event).unwrap();

        let state = runtime.get_state("CounterManagement", Some("counter-1"));
        assert!(state.is_some());
        assert_eq!(state.unwrap().current_state, "counting");
    }

    #[test]
    fn test_entity_cache() {
        let runtime = OrbitalClientRuntime::new();

        let mut data = ValueMap::new();
        data.insert("id".to_string(), Value::String("test-1".to_string()));
        data.insert("value".to_string(), Value::Number(42.0));

        runtime.cache_entity("Counter", "test-1", Value::Object(data));

        let cached = runtime.get_cached_entity("Counter", "test-1");
        assert!(cached.is_some());

        runtime.clear_cache();
        let cached = runtime.get_cached_entity("Counter", "test-1");
        assert!(cached.is_none());
    }

    #[test]
    fn test_current_user() {
        let runtime = OrbitalClientRuntime::new();

        let mut user = ValueMap::new();
        user.insert("id".to_string(), Value::String("user-1".to_string()));
        user.insert("name".to_string(), Value::String("Test User".to_string()));

        runtime.set_current_user(Value::Object(user));

        let current = runtime.get_current_user();
        match current {
            Value::Object(obj) => {
                assert_eq!(obj.get("id"), Some(&Value::String("user-1".to_string())));
            }
            _ => panic!("Expected object"),
        }
    }
}
