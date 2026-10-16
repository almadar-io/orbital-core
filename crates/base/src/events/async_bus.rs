//! Async Event Bus
//!
//! Async-compatible event bus using Tokio broadcast channels.
//! This module is only available with the "async" feature.

#[cfg(feature = "async")]
use tokio::sync::broadcast;

#[cfg(feature = "async")]
use super::types::{Event, EventFilter, SubscriptionId};
#[cfg(feature = "async")]
use parking_lot::RwLock;
#[cfg(feature = "async")]
use std::collections::HashMap;
#[cfg(feature = "async")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "async")]
use std::sync::Arc;

/// Default channel capacity
#[cfg(feature = "async")]
const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Async event bus using Tokio broadcast channels
///
/// This bus supports both synchronous callbacks and async receivers.
#[cfg(feature = "async")]
pub struct AsyncEventBus {
    /// Subscription counter
    next_id: AtomicU64,

    /// Broadcast sender for all events
    sender: broadcast::Sender<Event>,

    /// Filtered channels for specific patterns
    filtered_senders: RwLock<HashMap<SubscriptionId, (EventFilter, broadcast::Sender<Event>)>>,
}

#[cfg(feature = "async")]
impl Default for AsyncEventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "async")]
impl AsyncEventBus {
    /// Create a new async event bus
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    /// Create an async event bus with custom channel capacity
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            next_id: AtomicU64::new(1),
            sender,
            filtered_senders: RwLock::new(HashMap::new()),
        }
    }

    /// Generate a new subscription ID
    fn generate_id(&self) -> SubscriptionId {
        SubscriptionId::new(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Subscribe to all events, returns a receiver
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    /// Subscribe to events matching a pattern
    ///
    /// Returns a receiver that only receives events matching the filter.
    pub fn subscribe_filtered(
        &self,
        pattern: &str,
    ) -> (SubscriptionId, broadcast::Receiver<Event>) {
        let id = self.generate_id();
        let filter = EventFilter::from(pattern);
        let (sender, receiver) = broadcast::channel(DEFAULT_CHANNEL_CAPACITY);

        self.filtered_senders.write().insert(id, (filter, sender));

        (id, receiver)
    }

    /// Subscribe with a custom filter
    pub fn subscribe_with_filter(
        &self,
        filter: EventFilter,
    ) -> (SubscriptionId, broadcast::Receiver<Event>) {
        let id = self.generate_id();
        let (sender, receiver) = broadcast::channel(DEFAULT_CHANNEL_CAPACITY);

        self.filtered_senders.write().insert(id, (filter, sender));

        (id, receiver)
    }

    /// Unsubscribe a filtered subscription
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        self.filtered_senders.write().remove(&id).is_some()
    }

    /// Emit an event
    pub fn emit(&self, event: Event) {
        // Send to main channel (ignore if no receivers)
        let _ = self.sender.send(event.clone());

        // Send to filtered channels
        let senders = self.filtered_senders.read();
        for (filter, sender) in senders.values() {
            if filter.matches(&event) {
                let _ = sender.send(event.clone());
            }
        }
    }

    /// Emit a simple event
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

    /// Get the number of receivers for the main channel
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Get the number of filtered subscriptions
    pub fn filtered_subscription_count(&self) -> usize {
        self.filtered_senders.read().len()
    }
}

#[cfg(feature = "async")]
impl std::fmt::Debug for AsyncEventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncEventBus")
            .field("receiver_count", &self.receiver_count())
            .field(
                "filtered_subscriptions",
                &self.filtered_subscription_count(),
            )
            .finish()
    }
}

/// Shareable async event bus
#[cfg(feature = "async")]
pub type SharedAsyncEventBus = Arc<AsyncEventBus>;

/// Create a new shared async event bus
#[cfg(feature = "async")]
pub fn shared_async_bus() -> SharedAsyncEventBus {
    Arc::new(AsyncEventBus::new())
}

#[cfg(all(test, feature = "async"))]
mod tests {
    use super::*;
    use crate::value::Value;

    #[tokio::test]
    async fn test_async_subscribe_emit() {
        let bus = AsyncEventBus::new();
        let mut receiver = bus.subscribe();

        bus.emit(Event::new("TEST_EVENT", Value::String("hello".to_string())));

        let event = receiver.recv().await.unwrap();
        assert_eq!(event.name, "TEST_EVENT");
        assert_eq!(event.payload, Value::String("hello".to_string()));
    }

    #[tokio::test]
    async fn test_filtered_subscription() {
        let bus = AsyncEventBus::new();
        let (_, mut receiver) = bus.subscribe_filtered("UserManagement.*");

        bus.emit(Event::namespaced(
            "UserManagement",
            "USER_REGISTERED",
            Value::Null,
        ));
        bus.emit(Event::namespaced(
            "ConnectionManagement",
            "CONNECTION_CREATED",
            Value::Null,
        ));
        bus.emit(Event::namespaced(
            "UserManagement",
            "USER_DELETED",
            Value::Null,
        ));

        // Should only receive UserManagement events
        let event1 = receiver.recv().await.unwrap();
        assert_eq!(event1.name, "UserManagement.USER_REGISTERED");

        let event2 = receiver.recv().await.unwrap();
        assert_eq!(event2.name, "UserManagement.USER_DELETED");
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let bus = AsyncEventBus::new();
        let (id, _receiver) = bus.subscribe_filtered("TEST.*");

        assert_eq!(bus.filtered_subscription_count(), 1);

        bus.unsubscribe(id);

        assert_eq!(bus.filtered_subscription_count(), 0);
    }

    #[tokio::test]
    async fn test_multiple_receivers() {
        let bus = Arc::new(AsyncEventBus::new());
        let mut receiver1 = bus.subscribe();
        let mut receiver2 = bus.subscribe();

        bus.emit(Event::new("TEST_EVENT", Value::Null));

        let event1 = receiver1.recv().await.unwrap();
        let event2 = receiver2.recv().await.unwrap();

        assert_eq!(event1.name, "TEST_EVENT");
        assert_eq!(event2.name, "TEST_EVENT");
    }
}

// Stub implementation when async feature is not enabled
#[cfg(not(feature = "async"))]
pub struct AsyncEventBus {
    _private: (),
}

#[cfg(not(feature = "async"))]
impl Default for AsyncEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncEventBus {
    pub fn new() -> Self {
        panic!("AsyncEventBus requires the 'async' feature to be enabled")
    }
}
