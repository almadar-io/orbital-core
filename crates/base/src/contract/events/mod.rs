//! Event Bus
//!
//! Pub/sub event system for cross-orbital communication.
//!
//! # Overview
//!
//! The event bus provides:
//! - Synchronous callbacks via `EventBus`
//! - Async receivers via `AsyncEventBus` (requires "async" feature)
//! - Event namespacing: `TraitName.EVENT_NAME`
//! - Wildcard patterns: `TraitName.*` or `*`
//!
//! # Example
//!
//! ```
//! use orbital_core::events::{EventBus, Event};
//! use orbital_core::evaluator::Value;
//!
//! let bus = EventBus::new();
//!
//! // Subscribe to specific event
//! bus.on("USER_REGISTERED", |event| {
//!     println!("User registered!");
//! });
//!
//! // Subscribe to namespace
//! bus.on("UserManagement.*", |event| {
//!     println!("UserManagement event: {}", event.name);
//! });
//!
//! // Subscribe to all events
//! bus.on_any(|event| {
//!     println!("Event: {}", event.name);
//! });
//!
//! // Emit events
//! bus.emit(Event::new("USER_REGISTERED", Value::Null));
//! bus.emit(Event::namespaced("UserManagement", "USER_DELETED", Value::Null));
//! ```

pub mod async_bus;
pub mod bus;
pub mod types;

/// Event names the bus deliberately never delivers: the operator table's `emit` `busExcluded` list, or the copy a
/// registered client program carries (`registry::dispatch_facts`).
///
/// A bare `UI:INIT` on the bus would broadcast a re-INIT into every trait on the page, so a control targeting one of
/// these can never fire and no affordance bound to a lifecycle name counts as an emitter.
pub fn bus_excluded_events() -> &'static [String] {
    crate::language::registry::dispatch_facts()
        .map(|facts| facts.bus_excluded.as_slice())
        .expect("a kernel dispatches only after its operator table or a registered program supplied the dispatch facts")
}

/// True when the bus will never deliver `event`, so a control wired to it is
/// dead on both execution paths.
pub fn is_bus_excluded_event(event: &str) -> bool {
    bus_excluded_events().iter().any(|excluded| excluded == event)
}

pub use bus::{shared_bus, EventBus, EventCallback, SharedEventBus};
pub use types::{Event, EventFilter, SubscriptionId};

#[cfg(feature = "async")]
pub use async_bus::{shared_async_bus, AsyncEventBus, SharedAsyncEventBus};

#[cfg(not(feature = "async"))]
pub use async_bus::AsyncEventBus;
