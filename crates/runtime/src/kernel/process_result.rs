//! Shared result and info types produced by the runtime kernel.

use super::dispatch_memory::DispatchView;
use super::shared_entity::SharedEntity;
use orbital_core_base::effect_types::{Effect, EffectResult};
use orbital_core_base::value::Value;
use orbital_core_base::schema::types::{FieldDefinition, PageRef};
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_eval::state_machine::types::ArmGuard;

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
    pub entity_data: SharedEntity,

    /// Linked entity type name (for data keying in responses).
    pub linked_entity: String,

    /// Event key of the transition that fired; `None` when none did (a blocked event, a tick).
    pub event: Option<String>,

    /// The payload this event was delivered to the trait with, after any listen mapping; `Null` when none (a tick, an empty delivery).
    pub payload: Value,

    /// What the delivery looked like going in; `None` for a delivery that did not execute and for a tick.
    pub entry: Option<DeliveryEntry>,

    /// Position of the arm that fired among the state's `from --event-->` arms.
    pub fired_arm: Option<usize>,
    pub transition_index: Option<usize>,
    pub entity_id: String,
    pub state_entity_id: String,

    /// Every guarded arm evaluated while choosing the arm, in order.
    pub guards: Vec<ArmGuard>,

    /// One run per authored effect of the fired arm, in order.
    pub effect_runs: Vec<EffectRun>,

    /// Traits the dispatch's cross-trait worklist dropped when it hit `CROSS_TRAIT_CASCADE_CAP`,
    /// in first-dropped order; carried on the last outcome processed before the cap. Empty when the
    /// whole worklist ran.
    pub cascade_dropped: Vec<String>,

    /// Where `CROSS_TRAIT_CASCADE_CAP` cut the dispatch, carried on the same outcome as `cascade_dropped`; `None` when the whole
    /// worklist ran.
    pub cascade_cut: Option<CascadeCut>,

    /// Positions in `effect_results` of the `emit`s this dispatch consumed itself: the emitter's own
    /// arm for the event, or a listener target. The response marks them `source.dispatched`.
    pub consumed_emits: Vec<usize>,

    /// The message of the first effect that failed the way the JS executor throws (an unevaluable `set`,
    /// a `call-service` or substrate operator with no `emit.failure` route). The new state stands.
    pub failure: Option<String>,

    /// The payload this trait's `@callsitePayload` read during the step (its composer's), when it has one.
    pub callsite_payload: Option<Value>,
}

impl ProcessEventResult {
    /// The result as a dispatch keeps it among its outcomes: each `set` names its path, and the value it wrote is the one
    /// in `entity_data`, so a long cascade does not hold a second copy of every value it set.
    pub fn released(mut self) -> Self {
        for result in &mut self.effect_results {
            if let Effect::Set { value, .. } = &mut result.effect {
                *value = Value::Null;
            }
        }
        self
    }
}

/// How one authored effect ran: failed when any effect it expanded to failed, executed when
/// any ran (here or delegated), skipped when none did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectRunStatus {
    Executed,
    Failed,
    Skipped,
}

/// One authored effect of a fired arm and how it ran.
#[derive(Debug, Clone)]
pub struct EffectRun {
    pub effect: SExpression,
    pub status: EffectRunStatus,
    pub error: Option<String>,
}

/// The state a transition ran against: the trait's `@entity` frame before its effects, and the dispatch's `@event` / `@prevEvents` / `@prevStates` view.
#[derive(Debug, Clone)]
pub struct DeliveryEntry {
    pub entity_before: SharedEntity,
    /// The same layered read after the effects (the row after its persists, the frame with its sets), recorded only
    /// when the kernel is asked to ([`RuntimeKernel::set_record_entity_after`](crate::kernel::RuntimeKernel)): what a
    /// transition's `@ensures` is judged on, with `entity_before` as its `@fromEntity`.
    pub entity_after: Option<SharedEntity>,
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

/// What a route needs to know of an orbital's entity: its name (the reference text for an imported one) and, for an inline
/// entity, its fields. An imported entity has no fields here.
#[derive(Debug, Clone)]
pub struct EntitySummary {
    pub name: String,
    pub fields: Vec<FieldDefinition>,
}

/// Detailed orbital information.
#[derive(Debug, Clone)]
pub struct OrbitalDetail {
    /// Orbital name.
    pub name: String,

    /// The primary entity's name and declared fields. Never its authored `instances`: a detail is read on every
    /// dispatch's route, and cloning a 100 000-row table to read a name cost 30 ms.
    pub entity: EntitySummary,

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

/// The delivery a dispatch did not run because it reached its step cap: the cap and the trait and event it stopped at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeCut {
    pub cap: usize,
    pub trait_name: String,
    pub event: String,
}
