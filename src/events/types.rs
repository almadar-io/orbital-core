//! Event Types
//!
//! Core types for the event bus system.

use crate::evaluator::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a subscription
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub(crate) u64);

impl SubscriptionId {
    /// Create a new subscription ID
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sub_{}", self.0)
    }
}

/// Event data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Event name (may be namespaced: "TraitName.EVENT_NAME")
    pub name: String,

    /// Event payload data
    pub payload: Value,

    /// Source trait/orbital (if applicable)
    pub source: Option<String>,

    /// Target trait/orbital (if applicable)
    pub target: Option<String>,

    /// Timestamp when the event was created
    pub timestamp: i64,

    /// Metadata
    pub metadata: HashMap<String, Value>,
}

impl Event {
    /// Create a new event
    pub fn new(name: &str, payload: Value) -> Self {
        Self {
            name: name.to_string(),
            payload,
            source: None,
            target: None,
            timestamp: crate::time_util::now_millis(),
            metadata: HashMap::new(),
        }
    }

    /// Create an event with source
    pub fn with_source(name: &str, payload: Value, source: &str) -> Self {
        Self {
            name: name.to_string(),
            payload,
            source: Some(source.to_string()),
            target: None,
            timestamp: crate::time_util::now_millis(),
            metadata: HashMap::new(),
        }
    }

    /// Create a namespaced event (TraitName.EVENT_NAME)
    pub fn namespaced(trait_name: &str, event_name: &str, payload: Value) -> Self {
        Self {
            name: format!("{}.{}", trait_name, event_name),
            payload,
            source: Some(trait_name.to_string()),
            target: None,
            timestamp: crate::time_util::now_millis(),
            metadata: HashMap::new(),
        }
    }

    /// Set the target
    pub fn to(mut self, target: &str) -> Self {
        self.target = Some(target.to_string());
        self
    }

    /// Add metadata
    pub fn with_metadata(mut self, key: &str, value: Value) -> Self {
        self.metadata.insert(key.to_string(), value);
        self
    }

    /// Get the namespace (trait name) if this is a namespaced event
    pub fn namespace(&self) -> Option<&str> {
        self.name.split('.').next()
    }

    /// Get the local event name (without namespace)
    pub fn local_name(&self) -> &str {
        self.name.split('.').next_back().unwrap_or(&self.name)
    }

    /// Check if this event matches a pattern
    ///
    /// Patterns:
    /// - Exact match: "USER_REGISTERED"
    /// - Namespaced: "UserManagement.USER_REGISTERED"
    /// - Wildcard namespace: "UserManagement.*"
    /// - Wildcard all: "*"
    pub fn matches_pattern(&self, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }

        if let Some(namespace) = pattern.strip_suffix(".*") {
            // Namespace wildcard: "TraitName.*"
            return self.name.starts_with(namespace)
                && self.name.get(namespace.len()..namespace.len() + 1) == Some(".");
        }

        // Exact match
        self.name == pattern
    }
}

/// Event filter for subscriptions
#[derive(Debug, Clone)]
pub enum EventFilter {
    /// Match exact event name
    Exact(String),

    /// Match events from a specific namespace (trait)
    Namespace(String),

    /// Match all events
    All,

    /// Match multiple patterns
    Any(Vec<EventFilter>),
}

impl EventFilter {
    /// Create an exact match filter
    pub fn exact(name: &str) -> Self {
        EventFilter::Exact(name.to_string())
    }

    /// Create a namespace filter (matches "Namespace.*")
    pub fn namespace(ns: &str) -> Self {
        EventFilter::Namespace(ns.to_string())
    }

    /// Create a filter that matches all events
    pub fn all() -> Self {
        EventFilter::All
    }

    /// Create a filter that matches any of the given patterns
    pub fn any(filters: Vec<EventFilter>) -> Self {
        EventFilter::Any(filters)
    }

    /// Check if an event matches this filter
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            EventFilter::Exact(name) => event.name == *name,
            EventFilter::Namespace(ns) => {
                event.name.starts_with(ns) && event.name.get(ns.len()..ns.len() + 1) == Some(".")
            }
            EventFilter::All => true,
            EventFilter::Any(filters) => filters.iter().any(|f| f.matches(event)),
        }
    }

    /// Convert to a pattern string for display
    pub fn to_pattern(&self) -> String {
        match self {
            EventFilter::Exact(name) => name.clone(),
            EventFilter::Namespace(ns) => format!("{}.*", ns),
            EventFilter::All => "*".to_string(),
            EventFilter::Any(filters) => filters
                .iter()
                .map(|f| f.to_pattern())
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }
}

impl From<&str> for EventFilter {
    fn from(pattern: &str) -> Self {
        if pattern == "*" {
            EventFilter::All
        } else if let Some(ns) = pattern.strip_suffix(".*") {
            EventFilter::Namespace(ns.to_string())
        } else {
            EventFilter::Exact(pattern.to_string())
        }
    }
}

impl From<String> for EventFilter {
    fn from(pattern: String) -> Self {
        EventFilter::from(pattern.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_creation() {
        let event = Event::new("USER_REGISTERED", Value::Null);
        assert_eq!(event.name, "USER_REGISTERED");
        assert!(event.source.is_none());
    }

    #[test]
    fn test_namespaced_event() {
        let event = Event::namespaced("UserManagement", "USER_REGISTERED", Value::Null);
        assert_eq!(event.name, "UserManagement.USER_REGISTERED");
        assert_eq!(event.source, Some("UserManagement".to_string()));
        assert_eq!(event.namespace(), Some("UserManagement"));
        assert_eq!(event.local_name(), "USER_REGISTERED");
    }

    #[test]
    fn test_event_pattern_matching() {
        let event = Event::namespaced("UserManagement", "USER_REGISTERED", Value::Null);

        // Exact match
        assert!(event.matches_pattern("UserManagement.USER_REGISTERED"));
        assert!(!event.matches_pattern("UserManagement.USER_DELETED"));

        // Namespace wildcard
        assert!(event.matches_pattern("UserManagement.*"));
        assert!(!event.matches_pattern("ConnectionManagement.*"));

        // Global wildcard
        assert!(event.matches_pattern("*"));
    }

    #[test]
    fn test_event_filter() {
        let event = Event::namespaced("UserManagement", "USER_REGISTERED", Value::Null);

        assert!(EventFilter::exact("UserManagement.USER_REGISTERED").matches(&event));
        assert!(!EventFilter::exact("OTHER_EVENT").matches(&event));

        assert!(EventFilter::namespace("UserManagement").matches(&event));
        assert!(!EventFilter::namespace("Other").matches(&event));

        assert!(EventFilter::all().matches(&event));
    }

    #[test]
    fn test_event_filter_from_pattern() {
        let filter: EventFilter = "UserManagement.*".into();
        assert!(matches!(filter, EventFilter::Namespace(_)));

        let filter: EventFilter = "*".into();
        assert!(matches!(filter, EventFilter::All));

        let filter: EventFilter = "EXACT_EVENT".into();
        assert!(matches!(filter, EventFilter::Exact(_)));
    }
}
