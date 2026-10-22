//! Event Bus Implementation
//!
//! A thread-safe pub/sub event system supporting:
//! - Synchronous callbacks
//! - Async receivers via Tokio broadcast channels
//! - Event namespacing and wildcard patterns
//! - Subscription management

use super::types::{Event, EventFilter, SubscriptionId};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Callback function type for synchronous event handling
pub type EventCallback = Box<dyn Fn(&Event) + Send + Sync>;

/// A subscription entry
struct Subscription {
    id: SubscriptionId,
    filter: EventFilter,
    callback: EventCallback,
}

/// Thread-safe event bus for pub/sub communication
///
/// # Example
///
/// ```
/// use orbital_core::events::{EventBus, Event, EventFilter};
/// use orbital_core::evaluator::Value;
///
/// let bus = EventBus::new();
///
/// // Subscribe to events
/// let sub_id = bus.on("USER_REGISTERED", |event| {
///     println!("User registered: {:?}", event.payload);
/// });
///
/// // Emit an event
/// bus.emit(Event::new("USER_REGISTERED", Value::String("user-123".to_string())));
///
/// // Unsubscribe
/// bus.off(sub_id);
/// ```
pub struct EventBus {
    /// Subscription counter for generating unique IDs
    next_id: AtomicU64,

    /// Active subscriptions
    subscriptions: RwLock<Vec<Subscription>>,

    /// Event history (optional, for debugging)
    history: RwLock<Vec<Event>>,

    /// Whether to keep event history
    keep_history: bool,

    /// Maximum history size
    max_history: usize,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Create a new event bus
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            subscriptions: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            keep_history: false,
            max_history: 100,
        }
    }

    /// Create an event bus that keeps history
    pub fn with_history(max_size: usize) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            subscriptions: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            keep_history: true,
            max_history: max_size,
        }
    }

    /// Generate a new subscription ID
    fn generate_id(&self) -> SubscriptionId {
        SubscriptionId::new(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Subscribe to events matching a pattern
    ///
    /// Patterns:
    /// - Exact: "USER_REGISTERED"
    /// - Namespaced: "UserManagement.USER_REGISTERED"
    /// - Wildcard: "UserManagement.*" or "*"
    pub fn on<F>(&self, pattern: &str, callback: F) -> SubscriptionId
    where
        F: Fn(&Event) + Send + Sync + 'static,
    {
        let id = self.generate_id();
        let filter = EventFilter::from(pattern);
        let subscription = Subscription {
            id,
            filter,
            callback: Box::new(callback),
        };

        self.subscriptions.write().push(subscription);
        id
    }

    /// Subscribe to events with a custom filter
    pub fn on_filter<F>(&self, filter: EventFilter, callback: F) -> SubscriptionId
    where
        F: Fn(&Event) + Send + Sync + 'static,
    {
        let id = self.generate_id();
        let subscription = Subscription {
            id,
            filter,
            callback: Box::new(callback),
        };

        self.subscriptions.write().push(subscription);
        id
    }

    /// Subscribe to all events
    pub fn on_any<F>(&self, callback: F) -> SubscriptionId
    where
        F: Fn(&Event) + Send + Sync + 'static,
    {
        self.on("*", callback)
    }

    /// Unsubscribe from events
    pub fn off(&self, id: SubscriptionId) -> bool {
        let mut subs = self.subscriptions.write();
        let len_before = subs.len();
        subs.retain(|s| s.id != id);
        subs.len() < len_before
    }

    /// Emit an event to all matching subscribers
    pub fn emit(&self, event: Event) {
        // Store in history if enabled
        if self.keep_history {
            let mut history = self.history.write();
            history.push(event.clone());
            if history.len() > self.max_history {
                history.remove(0);
            }
        }

        // Notify subscribers
        let subs = self.subscriptions.read();
        for sub in subs.iter() {
            if sub.filter.matches(&event) {
                (sub.callback)(&event);
            }
        }
    }

    /// Emit a simple event by name
    pub fn emit_simple(&self, name: &str, payload: crate::value::Value) {
        self.emit(Event::new(name, payload));
    }

    /// Emit a namespaced event
    pub fn emit_namespaced(
        &self,
        namespace: &str,
        event_name: &str,
        payload: crate::value::Value,
    ) {
        self.emit(Event::namespaced(namespace, event_name, payload));
    }

    /// Get the number of active subscriptions
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.read().len()
    }

    /// Get subscriptions for a specific pattern (for debugging)
    pub fn subscriptions_for(&self, pattern: &str) -> Vec<SubscriptionId> {
        let test_event = Event::new(pattern, crate::value::Value::Null);
        self.subscriptions
            .read()
            .iter()
            .filter(|s| s.filter.matches(&test_event))
            .map(|s| s.id)
            .collect()
    }

    /// Clear all subscriptions
    pub fn clear(&self) {
        self.subscriptions.write().clear();
    }

    /// Get event history (if enabled)
    pub fn history(&self) -> Vec<Event> {
        self.history.read().clone()
    }

    /// Clear event history
    pub fn clear_history(&self) {
        self.history.write().clear();
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscription_count", &self.subscription_count())
            .field("keep_history", &self.keep_history)
            .finish()
    }
}

/// A shareable event bus handle
pub type SharedEventBus = Arc<EventBus>;

/// Create a new shared event bus
pub fn shared_bus() -> SharedEventBus {
    Arc::new(EventBus::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn test_simple_subscribe_emit() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        bus.on("TEST_EVENT", move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        bus.emit(Event::new("TEST_EVENT", Value::Null));
        bus.emit(Event::new("TEST_EVENT", Value::Null));

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_unsubscribe() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let sub_id = bus.on("TEST_EVENT", move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        bus.emit(Event::new("TEST_EVENT", Value::Null));
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        bus.off(sub_id);

        bus.emit(Event::new("TEST_EVENT", Value::Null));
        assert_eq!(counter.load(Ordering::SeqCst), 1); // Still 1, not called
    }

    #[test]
    fn test_wildcard_subscription() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        bus.on_any(move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        bus.emit(Event::new("EVENT_1", Value::Null));
        bus.emit(Event::new("EVENT_2", Value::Null));
        bus.emit(Event::namespaced("Trait", "EVENT_3", Value::Null));

        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_namespace_wildcard() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        bus.on("UserManagement.*", move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        bus.emit(Event::namespaced(
            "UserManagement",
            "USER_REGISTERED",
            Value::Null,
        ));
        bus.emit(Event::namespaced(
            "UserManagement",
            "USER_DELETED",
            Value::Null,
        ));
        bus.emit(Event::namespaced(
            "ConnectionManagement",
            "CONNECTION_CREATED",
            Value::Null,
        ));

        assert_eq!(counter.load(Ordering::SeqCst), 2); // Only UserManagement events
    }

    #[test]
    fn test_multiple_subscribers() {
        let bus = EventBus::new();
        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));
        let counter1_clone = counter1.clone();
        let counter2_clone = counter2.clone();

        bus.on("TEST_EVENT", move |_event| {
            counter1_clone.fetch_add(1, Ordering::SeqCst);
        });

        bus.on("TEST_EVENT", move |_event| {
            counter2_clone.fetch_add(1, Ordering::SeqCst);
        });

        bus.emit(Event::new("TEST_EVENT", Value::Null));

        assert_eq!(counter1.load(Ordering::SeqCst), 1);
        assert_eq!(counter2.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_event_payload() {
        let bus = EventBus::new();
        let received = Arc::new(RwLock::new(None));
        let received_clone = received.clone();

        bus.on("TEST_EVENT", move |event| {
            *received_clone.write() = Some(event.payload.clone());
        });

        bus.emit(Event::new("TEST_EVENT", Value::String("hello".to_string())));

        let payload = received.read().clone();
        assert_eq!(payload, Some(Value::String("hello".to_string())));
    }

    #[test]
    fn test_event_history() {
        let bus = EventBus::with_history(10);

        bus.emit(Event::new("EVENT_1", Value::Null));
        bus.emit(Event::new("EVENT_2", Value::Null));
        bus.emit(Event::new("EVENT_3", Value::Null));

        let history = bus.history();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].name, "EVENT_1");
        assert_eq!(history[2].name, "EVENT_3");
    }

    #[test]
    fn test_history_max_size() {
        let bus = EventBus::with_history(3);

        for i in 0..5 {
            bus.emit(Event::new(&format!("EVENT_{}", i), Value::Null));
        }

        let history = bus.history();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].name, "EVENT_2");
        assert_eq!(history[2].name, "EVENT_4");
    }

    #[test]
    fn test_subscription_count() {
        let bus = EventBus::new();

        assert_eq!(bus.subscription_count(), 0);

        let id1 = bus.on("EVENT_1", |_| {});
        let id2 = bus.on("EVENT_2", |_| {});

        assert_eq!(bus.subscription_count(), 2);

        bus.off(id1);
        assert_eq!(bus.subscription_count(), 1);

        bus.off(id2);
        assert_eq!(bus.subscription_count(), 0);
    }

    #[test]
    fn test_clear() {
        let bus = EventBus::new();

        bus.on("EVENT_1", |_| {});
        bus.on("EVENT_2", |_| {});

        assert_eq!(bus.subscription_count(), 2);

        bus.clear();

        assert_eq!(bus.subscription_count(), 0);
    }

    #[test]
    fn test_shared_bus() {
        let bus = shared_bus();
        let bus_clone = bus.clone();

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        bus.on("TEST_EVENT", move |_| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        // Emit from clone
        bus_clone.emit(Event::new("TEST_EVENT", Value::Null));

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_emit_helpers() {
        let bus = EventBus::with_history(10);

        bus.emit_simple("SIMPLE_EVENT", Value::Number(42.0));
        bus.emit_namespaced("Trait", "NAMESPACED", Value::Bool(true));

        let history = bus.history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].name, "SIMPLE_EVENT");
        assert_eq!(history[1].name, "Trait.NAMESPACED");
    }
}
