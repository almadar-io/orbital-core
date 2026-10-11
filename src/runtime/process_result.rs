//! Shared result and info types produced by the runtime kernel.

use super::dispatch_memory::DispatchView;
use crate::effects::EffectResult;
use crate::evaluator::Value;
use crate::schema::{EntityRef, PageRef};

/// Result of processing an event through the runtime kernel.
#[derive(Debug, Clone)]
pub struct ProcessEventResult {
    /// Whether the transition was executed.
    pub executed: bool,

    /// Previous state name.
    pub previous_state: String,

    /// New state name after the transition (or current state if blocked).
    pub new_state: String,

    /// Number of effects executed.
    pub effects_executed: usize,

    /// Individual effect results.
    pub effect_results: Vec<EffectResult>,

    /// Entity data after effect execution (captures Set effects).
    pub entity_data: Value,

    /// Linked entity type name (for data keying in responses).
    pub linked_entity: String,

    /// Event key of the transition that fired; `None` when none did (a blocked event, a tick).
    pub event: Option<String>,

    /// The payload this event was delivered to the trait with, after any listen mapping; `Null` when none (a tick, an empty delivery).
    pub payload: Value,

    /// What the delivery looked like going in; `None` for a delivery that did not execute and for a tick.
    pub entry: Option<DeliveryEntry>,
}

/// The state a transition ran against: the trait's `@entity` frame before its effects, and the dispatch's `@event` / `@prevEvents` / `@prevStates` view.
#[derive(Debug, Clone)]
pub struct DeliveryEntry {
    pub entity_before: Value,
    pub view: DispatchView,
}

/// Summary info about an orbital.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrbitalInfo {
    /// Orbital name.
    pub name: String,

    /// Primary entity name.
    pub entity_name: String,

    /// Number of traits.
    pub trait_count: usize,

    /// Number of pages.
    pub page_count: usize,
}

/// Detailed orbital information.
#[derive(Debug, Clone)]
pub struct OrbitalDetail {
    /// Orbital name.
    pub name: String,

    /// Primary entity reference.
    pub entity: EntityRef,

    /// Trait names.
    pub traits: Vec<String>,

    /// Page references.
    pub pages: Vec<PageRef>,
}

/// Runtime statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeStats {
    /// Number of registered schemas.
    pub registered_schemas: usize,

    /// Number of registered traits.
    pub registered_traits: usize,

    /// Number of active trait instances.
    pub active_instances: usize,

    /// Number of cached/stored entities.
    pub cached_entities: usize,
}
