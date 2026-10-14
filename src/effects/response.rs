//! Event Response Types
//!
//! The Rust mirror of `@almadar/core`'s `OrbitalEventResponse`/`OrbitalEventRequest`
//! (`packages/almadar-core/src/types/bus.ts`) — canonical = the TypeScript runtime's
//! implementation (`groovy-inventing-cloud.md` Program A, §156). Field-for-field, in
//! `camelCase`, every optional/collection defaulted so a hand-built or fixture JSON
//! missing a key still deserializes.
//!
//! Reference: builder/docs/Orbital_Pattern.orb

use crate::evaluator::ValueMap;
use crate::evaluator::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{ClientEffect, Effect, EffectOutcome, EffectResult};

/// The `@event.source` keys of [`BusEventSource`] (core's `deliveryRecordValue`); the wire struct
/// also carries `pendingKey` and `persistSuccess`, which a delivery record's value leaves out.
pub const BUS_EVENT_SOURCE_FIELDS: [&str; 10] = [
    "orbital",
    "orbitalId",
    "trait",
    "traitId",
    "eventId",
    "transition",
    "tick",
    "fromBridge",
    "dispatched",
    "originClientId",
];

/// Mirror of `BusEventSource` (`bus.ts`). Identifies the origin of an emitted event —
/// cross-trait listeners filter on `orbital`/`trait`; the rest are debugging/dedup aids
/// the client-side bridge uses and the Rust kernel does not populate yet.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusEventSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbital: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "OrbitalId"))]
    pub orbital_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "trait")]
    pub r#trait: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "TraitId"))]
    pub trait_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EventId"))]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_bridge: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_client_id: Option<String>,
    /// Per-activation key a UI control stamps on the event it emits; never set by the kernel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_key: Option<String>,
    /// Set on a `persist` effect's `emit.success` envelope: the emit a live-push host fans out to the
    /// other connected clients. On the wire, not an `@event.source` binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_success: Option<bool>,
}

impl BusEventSource {
    /// The common case: a bare `{ trait: ... }` source (the emitting trait).
    pub fn from_trait(trait_name: impl Into<String>) -> Self {
        Self {
            r#trait: Some(trait_name.into()),
            ..Default::default()
        }
    }
}

/// The evaluator value of a delivery record `{ event, payload, source }` (`@event`); twin of
/// `@almadar/core` `deliveryRecordValue`. `source` carries only the fields that are set.
pub fn delivery_record_value(event: &str, payload: &Value, source: &BusEventSource) -> Value {
    let mut src: ValueMap = ValueMap::new();
    let mut put_str = |key: &str, v: &Option<String>| {
        if let Some(v) = v {
            src.insert(key.to_string(), Value::String(v.clone()));
        }
    };
    put_str("orbital", &source.orbital);
    put_str("orbitalId", &source.orbital_id);
    put_str("trait", &source.r#trait);
    put_str("traitId", &source.trait_id);
    put_str("eventId", &source.event_id);
    put_str("transition", &source.transition);
    put_str("tick", &source.tick);
    put_str("originClientId", &source.origin_client_id);
    if let Some(v) = source.from_bridge {
        src.insert("fromBridge".to_string(), Value::Bool(v));
    }
    if let Some(v) = source.dispatched {
        src.insert("dispatched".to_string(), Value::Bool(v));
    }
    let mut record = ValueMap::new();
    record.insert("event".to_string(), Value::String(event.to_string()));
    record.insert("payload".to_string(), payload.clone());
    record.insert("source".to_string(), Value::Object(src));
    Value::Object(record)
}

/// One event a transition's effects emitted, carried back to the client to re-dispatch
/// in order. Mirror of `EmittedEvent` (`bus.ts`).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedEvent {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EventPayload"))]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BusEventSource>,
}

/// `Array<{ trait: string; from: string; event?: string }>` — stateless
/// addressing (`bus.ts` `OrbitalEventRequest.traits`). `event` is the event the
/// trait fired on in a cascade; residence routing judges the trait by it.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraitFrom {
    #[serde(rename = "trait")]
    pub r#trait: String,
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// One `{ traitName, effect }` pairing — the per-trait attribution twin of the flat
/// `clientEffects` array (same length/order, 1:1 by index). Mirror of
/// `OrbitalEventResponse.clientEffectsByTrait`.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientEffectByTrait {
    pub trait_name: String,
    pub effect: ClientEffect,
    /// Event key of the transition that produced the effect (a listens delivery's triggered event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// State that transition fired from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_state: Option<String>,
    /// The payload an embedded `@callsitePayload` child was composed with (sticky).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EventPayload"))]
    pub callsite_payload: Option<Value>,
}

/// The `effect` discriminant of `ServerEffectResult` — mirror of core's
/// `SERVER_REPORTED_EFFECTS` (`effect.ts`, derived from the effect union) plus the
/// `substrate` grouping for namespaced operators (head rides `name`). Kept in sync by
/// `tests/events_wire_fixtures.rs` (the `effect-results-every-kind` case).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerEffectKind {
    Set,
    Persist,
    CallService,
    Fetch,
    FetchStream,
    Ref,
    Deref,
    #[serde(rename = "swap")]
    Swap,
    Atomic,
    Emit,
    Substrate,
}

fn is_not_denied(denied: &Option<bool>) -> bool {
    !matches!(denied, Some(true))
}

/// Mirror of `ServerEffectResult` (`effect-result.ts`) — the wire shape a server-side
/// effect (persist/set/fetch/call-service/...) reports back on
/// `OrbitalEventResponse.effectResults`.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerEffectResult {
    pub effect: ServerEffectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,
    /// The namespaced operator (`memory/store`) — only on `ServerEffectKind::Substrate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "NamespacedEffectName"))]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EntityRow | ServerBatchSummary | EventPayloadValue"))]
    pub data: Option<Value>,
    pub success: bool,
    /// `true` when a persist failed because an access policy rejected it — never
    /// serialized as `false` (the wire type is the TS literal `true`, not `bool`).
    #[serde(default, skip_serializing_if = "is_not_denied")]
    #[cfg_attr(test, ts(type = "true"))]
    pub denied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Coded failure reason for a persist that resolved no target row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ServerEffectCode>,
}

/// `ServerEffectResult.code` — mirror of core's `"not-found" | "no-row-key"`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerEffectCode {
    NotFound,
    NoRowKey,
}

/// `EventRequest.run_tick`: which trait's tick the data authority runs once.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTick {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub tick: String,
}

/// The event a tick firing carries on the wire (a `runTick` leg's `event`). Twin of
/// `@almadar/runtime` `tickEventKey`.
pub fn tick_event_key(tick: &str) -> String {
    format!("$TICK_{tick}")
}

/// Client → server event dispatch request. Mirror of `OrbitalEventRequest` (`bus.ts`).
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRequest {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EventId"))]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EventPayload"))]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_trait: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "RawUserClaims"))]
    pub user: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// The viewer's locale (one of the program's declared `locales`); `None` → the first declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_trait: Option<String>,
    /// Run one firing of this trait's tick at the data authority (`@almadar/core`
    /// `OrbitalEventRequest.runTick`): a browser-run tick's data effects on browser-stored
    /// entities travel here, since a tick has no event to replay under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_tick: Option<RunTick>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<TraitFrom>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "{ [key in string]: EntityRow }"))]
    pub entity_by_trait: Option<ValueMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<String>,
    /// `@event` for the target when this request continues a client-begun dispatch; `None` = direct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryRecord>,
    /// The target trait's log so far in that dispatch; `None` = empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_log: Option<DispatchLog>,
    /// A mount batch: each listed trait runs its own lifecycle event, in order,
    /// with the shared `payload` (`bus.ts` `OrbitalEventRequest.mount`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<Vec<MountSeed>>,
}

/// One trait of a mount batch and the lifecycle event (`INIT`/`LOAD`/`$MOUNT`) it runs.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountSeed {
    #[serde(rename = "trait")]
    pub r#trait: String,
    pub event: LifecycleEvent,
}

/// The lifecycle events a trait runs when it enters a page; twin of `LifecycleEvent` in `bus.ts`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleEvent {
    #[serde(rename = "INIT")]
    Init,
    #[serde(rename = "LOAD")]
    Load,
    #[serde(rename = "$MOUNT")]
    Mount,
}

impl LifecycleEvent {
    pub fn parse(event: &str) -> Option<Self> {
        match event {
            "INIT" => Some(Self::Init),
            "LOAD" => Some(Self::Load),
            "$MOUNT" => Some(Self::Mount),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Init => "INIT",
            Self::Load => "LOAD",
            Self::Mount => "$MOUNT",
        }
    }
}

/// Mirror of `DeliveryRecord` (`bus.ts`): the delivery a transition is processing.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryRecord {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "EventPayload"))]
    pub payload: Option<Value>,
    #[serde(default)]
    pub source: BusEventSource,
}

impl DeliveryRecord {
    /// The inverse of [`Self::value`]; `None` for a value that is not a delivery record.
    pub fn from_value(value: &Value) -> Option<Self> {
        serde_json::to_value(value)
            .and_then(serde_json::from_value)
            .ok()
    }

    pub fn value(&self) -> Value {
        delivery_record_value(
            &self.event,
            self.payload.as_ref().unwrap_or(&Value::Null),
            &self.source,
        )
    }
}

/// Mirror of `DispatchLog` (`bus.ts`): a trait's log so far in a dispatch that began elsewhere.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchLog {
    pub prev_events: Vec<DeliveryRecord>,
    pub prev_states: Vec<String>,
}

/// Server → client event dispatch response. Canonical mirror of `OrbitalEventResponse`
/// (`bus.ts`) — this IS the wire shape, not an internal type flattened at the route
/// (§156: the route used to own a private `TypeScriptEventResponse` that duplicated
/// this and re-encoded effects into arrays; that duplication is gone, `ClientEffect`
/// owns its own array encoding).
///
/// # Example
///
/// When processing INIT event from Orbital_Pattern.orb:
/// ```json
/// {
///   "success": true,
///   "transitioned": true,
///   "states": { "UserManagement": "browsing" },
///   "emittedEvents": [],
///   "data": { "User": [{"id": "1", "name": "John"}] },
///   "clientEffects": [["render-ui", "main", {"type": "entity-cards"}]]
/// }
/// ```
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub transitioned: bool,
    /// Whole-orbital state snapshot, keyed by trait name.
    #[serde(default)]
    pub states: HashMap<String, String>,
    /// Always present (`[]` when none) — never optional-chained by a consumer.
    #[serde(default)]
    pub emitted_events: Vec<EmittedEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "{ [key in string]: Array<EntityRow> }"))]
    pub data: Option<HashMap<String, Vec<Value>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "{ [key in string]: EntityRow }"))]
    pub entity_by_trait: Option<ValueMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_effects: Option<Vec<ClientEffect>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_effects_by_trait: Option<Vec<ClientEffectByTrait>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_results: Option<Vec<ServerEffectResult>>,
    /// `"<Trait>.<event>"` when the SEED trait's guard rejected the dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_failed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejections: Option<Vec<TransitionRejection>>,
    /// Traits dropped from the cascade when the cross-trait worklist hit its cap; their states,
    /// effects and rows in this response are incomplete. Absent when the whole worklist ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cascade_truncated: Option<Vec<String>>,
    /// Every executed hop of the dispatch, in order (the server role's trace); absent when none ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitions: Option<Vec<TransitionHop>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Mirrors `@almadar/core` `ExternalInputRequest`: a request on the outside-client
/// input channel (`POST /orbitals/:orbital/inputs`). Only a declared external input of
/// `target_trait` is dispatched; anything else is refused with `not-an-external-input`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalInputRequest {
    pub target_trait: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
}

/// Mirrors `@almadar/core` `TransitionRejectionCode`.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransitionRejectionCode {
    NoMatchingTransition,
    GuardRejected,
    NoDispatchableTraits,
    NotAnExternalInput,
    /// An effect of the trait's transition failed; its later effects did not run.
    EffectFailed,
}

/// Mirrors `@almadar/core` `TransitionRejection`: why a trait (or the whole request)
/// did not transition.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionRejection {
    pub code: TransitionRejectionCode,
    #[serde(default, rename = "trait", skip_serializing_if = "Option::is_none")]
    pub trait_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states_declaring_event: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "unknown"))]
    pub guard: Option<Value>,
    /// `effect-failed`: the failure's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How one authored effect of a hop ran.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HopEffectStatus {
    Executed,
    Failed,
    Skipped,
}

/// One authored effect of a hop: its operator head, its arguments as authored, and how it ran.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HopEffect {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub args: Vec<Value>,
    pub status: HopEffectStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One executed hop of a dispatch (the seed's, a same-trait cascade step's, a listener delivery's), in
/// dispatch order. Mirrors the JS runtime's `TransitionObserver.onTransition` trace.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionHop {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub from: String,
    pub to: String,
    pub event: String,
    /// `true` when the fired arm's guard held; absent for an unguarded arm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_result: Option<bool>,
    pub effects: Vec<HopEffect>,
}

impl TransitionHop {
    /// The executed hops among `outcomes`, in order.
    pub fn of_outcomes(outcomes: &[(String, crate::runtime::ProcessEventResult)]) -> Vec<Self> {
        outcomes
            .iter()
            .filter(|(_, result)| result.executed)
            .filter_map(|(trait_name, result)| {
                let event = result.event.clone()?;
                let guard_result = result
                    .fired_arm
                    .and_then(|arm| result.guards.iter().find(|g| g.arm == arm))
                    .map(|_| true);
                Some(Self {
                    trait_name: trait_name.clone(),
                    from: result.previous_state.clone(),
                    to: result.new_state.clone(),
                    event,
                    guard_result,
                    effects: result.effect_runs.iter().map(HopEffect::of_run).collect(),
                })
            })
            .collect()
    }
}

impl HopEffect {
    fn of_run(run: &crate::runtime::EffectRun) -> Self {
        use crate::runtime::EffectRunStatus;
        let (head, args) = match &run.effect {
            crate::schema::SExpression::Array(items) => match items.split_first() {
                Some((crate::schema::SExpression::String(head), rest)) => (
                    head.clone(),
                    rest.iter().cloned().map(Value::from).collect(),
                ),
                _ => (
                    String::new(),
                    items.iter().cloned().map(Value::from).collect(),
                ),
            },
            other => (String::new(), vec![Value::from(other.clone())]),
        };
        Self {
            effect_type: head,
            args,
            status: match run.status {
                EffectRunStatus::Executed => HopEffectStatus::Executed,
                EffectRunStatus::Failed => HopEffectStatus::Failed,
                EffectRunStatus::Skipped => HopEffectStatus::Skipped,
            },
            error: run.error.clone(),
        }
    }
}

impl EventResponse {
    /// The answer to a request that named no trait: the merged outcomes of the `handlers` it was
    /// delivered to, with `held` (the held state of every trait of the orbital) echoed under `states`.
    /// No handler is a `no-dispatchable-traits` rejection. JS twin: `evaluateOrbitalEvent`'s discovery.
    pub fn from_discovery(
        outcomes: &[(String, crate::runtime::ProcessEventResult)],
        handlers: &[String],
        event_name: &str,
        held: HashMap<String, String>,
    ) -> Self {
        let mut response = match handlers.first() {
            Some(first) => Self::from_process_results(outcomes, first, event_name),
            None => EventResponse {
                success: true,
                rejections: Some(vec![TransitionRejection {
                    code: TransitionRejectionCode::NoDispatchableTraits,
                    trait_name: None,
                    from: None,
                    event: Some(event_name.to_string()),
                    states_declaring_event: None,
                    transition: None,
                    guard: None,
                    error: None,
                }]),
                ..Default::default()
            },
        };
        for (trait_name, state) in held {
            response.states.entry(trait_name).or_insert(state);
        }
        response
    }

    /// Minimal successful response with no cascade — used as the `ServerBridge::tick`
    /// default (no server-side ticking to report) and by tests that don't need a full
    /// `states{}` snapshot.
    pub fn success(_new_state: impl Into<String>) -> Self {
        Self {
            success: true,
            ..Default::default()
        }
    }

    /// Minimal error response.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(message.into()),
            ..Default::default()
        }
    }

    /// Append the renders of traits repainted for an entity change
    /// ([`crate::runtime::Rerender`]), after everything the dispatch itself rendered.
    pub fn fold_rerenders(&mut self, rerenders: &[crate::runtime::Rerender]) {
        for rerender in rerenders {
            for effect in &rerender.effects {
                let Some(client_effect) = ClientEffect::from_effect(effect) else {
                    continue;
                };
                self.client_effects_by_trait
                    .get_or_insert_with(Vec::new)
                    .push(ClientEffectByTrait {
                        trait_name: rerender.trait_name.clone(),
                        effect: client_effect.clone(),
                        event: None,
                        from_state: None,
                        callsite_payload: None,
                    });
                self.client_effects
                    .get_or_insert_with(Vec::new)
                    .push(client_effect);
            }
        }
    }

    /// Drop the `render-ui` effects that paint a `(trait, slot)` in `superseded`: a newer paint
    /// of that slot reaches the host through another response. `client_effects` and
    /// `client_effects_by_trait` are built in lockstep, so the two lists drop the same entries.
    pub fn drop_renders_of(&mut self, superseded: &std::collections::HashSet<(String, String)>) {
        let Some(by_trait) = self.client_effects_by_trait.as_mut() else {
            return;
        };
        let dropped: Vec<bool> = by_trait
            .iter()
            .map(|e| match &e.effect {
                ClientEffect::RenderUi { slot, .. } => {
                    superseded.contains(&(e.trait_name.clone(), slot.clone()))
                }
                _ => false,
            })
            .collect();
        let mut keep = dropped.iter().map(|d| !d);
        by_trait.retain(|_| keep.next().unwrap_or(true));
        if let Some(effects) = self
            .client_effects
            .as_mut()
            .filter(|e| e.len() == dropped.len())
        {
            let mut keep = dropped.iter().map(|d| !d);
            effects.retain(|_| keep.next().unwrap_or(true));
        }
    }

    /// Merge a cascade of `(trait, ProcessEventResult)` outcomes (from
    /// [`crate::runtime::RuntimeKernel::dispatch`]) into one client-facing response: every
    /// processed trait's new state, client effects, emitted events, and fetched/entity
    /// data, in dispatch order — so a single user event returns the full cross-trait
    /// fan-out (modal open, persist, list refetch) in one response.
    ///
    /// `seed_trait`/`event_name` are the ORIGINAL dispatch target — used only to detect
    /// and address a `guardFailed` on that one outcome (§156: cascaded guard failures
    /// downstream of the seed are tolerated no-ops, exactly like the JS runtime's embed
    /// bubble; only the seed's own rejection is reported to the caller).
    pub fn from_process_results(
        outcomes: &[(String, crate::runtime::ProcessEventResult)],
        seed_trait: &str,
        event_name: &str,
    ) -> Self {
        let mut merged = EventResponse {
            success: true,
            ..Default::default()
        };
        let mut data: HashMap<String, Vec<Value>> = HashMap::new();
        let mut entity_by_trait: ValueMap = ValueMap::new();
        let mut client_effects: Vec<ClientEffect> = Vec::new();
        let mut client_effects_by_trait: Vec<ClientEffectByTrait> = Vec::new();
        let mut effect_results: Vec<ServerEffectResult> = Vec::new();
        let mut emitted_events: Vec<EmittedEvent> = Vec::new();
        let mut cascade_truncated: Vec<String> = Vec::new();

        let mut rejections: Vec<TransitionRejection> = Vec::new();
        for (trait_name, result) in outcomes {
            if let (Some(error), Some(event)) = (&result.failure, &result.event) {
                rejections.push(TransitionRejection {
                    code: TransitionRejectionCode::EffectFailed,
                    trait_name: Some(trait_name.clone()),
                    from: Some(result.previous_state.clone()),
                    event: Some(event.clone()),
                    states_declaring_event: None,
                    transition: None,
                    guard: None,
                    error: Some(error.clone()),
                });
            }
            for dropped in &result.cascade_dropped {
                if !cascade_truncated.contains(dropped) {
                    cascade_truncated.push(dropped.clone());
                }
            }
            merged
                .states
                .insert(trait_name.clone(), result.new_state.clone());
            merged.transitioned =
                merged.transitioned || (result.executed && result.failure.is_none());

            if trait_name == seed_trait && !result.executed {
                merged
                    .guard_failed
                    .get_or_insert_with(|| format!("{seed_trait}.{event_name}"));
            }

            let is_empty_obj = result
                .entity_data
                .as_object()
                .map(|o| o.is_empty())
                .unwrap_or(false);
            if !result.entity_data.is_null() && !is_empty_obj {
                entity_by_trait.insert(trait_name.clone(), result.entity_data.clone());
            }

            for (index, er) in result.effect_results.iter().enumerate() {
                // A `render-ui` or `emit` whose operands failed to evaluate reached no one: the failure is the rejection above.
                if er.is_failed()
                    && matches!(er.effect, Effect::RenderUi { .. } | Effect::Emit { .. })
                {
                    continue;
                }
                if let Some(ce) = ClientEffect::from_effect(&er.effect) {
                    client_effects_by_trait.push(ClientEffectByTrait {
                        trait_name: trait_name.clone(),
                        effect: ce.clone(),
                        event: result.event.clone(),
                        from_state: result.event.as_ref().map(|_| result.previous_state.clone()),
                        callsite_payload: result.callsite_payload.clone(),
                    });
                    client_effects.push(ce);
                    continue;
                }
                if let Effect::Emit { event, payload } = &er.effect {
                    emitted_events.push(EmittedEvent {
                        event: event.clone(),
                        payload: if payload.is_null() {
                            None
                        } else {
                            Some(payload.clone())
                        },
                        source: Some(BusEventSource {
                            dispatched: result.consumed_emits.contains(&index).then_some(true),
                            persist_success: er.persist_success.then_some(true),
                            ..BusEventSource::from_trait(trait_name.clone())
                        }),
                    });
                    continue;
                }
                if let Effect::Fetch { entity_type, .. } = &er.effect {
                    if matches!(er.outcome, EffectOutcome::Executed) {
                        if let Some(rows) = er.return_value.as_ref().and_then(Value::as_array) {
                            data.entry(entity_type.clone())
                                .or_default()
                                .extend(rows.clone());
                        }
                    }
                }
                if let Some(sr) = server_effect_result(er) {
                    effect_results.push(sr);
                }
            }
        }

        dedup_rows_by_id(&mut data);

        merged.data = if data.is_empty() { None } else { Some(data) };
        merged.entity_by_trait = if entity_by_trait.is_empty() {
            None
        } else {
            Some(entity_by_trait)
        };
        merged.client_effects = if client_effects.is_empty() {
            None
        } else {
            Some(client_effects)
        };
        merged.client_effects_by_trait = if client_effects_by_trait.is_empty() {
            None
        } else {
            Some(client_effects_by_trait)
        };
        merged.effect_results = if effect_results.is_empty() {
            None
        } else {
            Some(effect_results)
        };
        merged.emitted_events = emitted_events;
        merged.rejections = (!rejections.is_empty()).then_some(rejections);
        merged.cascade_truncated = (!cascade_truncated.is_empty()).then_some(cascade_truncated);
        merged
    }

    /// This response with the server's `effect_results` appended to the local run's: a persist
    /// executes for real only on the server, so a settled response must carry its verdict.
    pub fn with_server_effect_results(mut self, posted: &EventResponse) -> Self {
        if let Some(server) = posted.effect_results.as_ref().filter(|r| !r.is_empty()) {
            self.effect_results
                .get_or_insert_with(Vec::new)
                .extend(server.iter().cloned());
        }
        self
    }

    /// Build the canonical client-facing response from a single kernel
    /// `ProcessEventResult`. Shares the per-outcome logic with
    /// [`from_process_results`](Self::from_process_results) via [`single_outcome`] — this
    /// is the single-trait primitive `KernelBridge::tick` and the headless verifier use.
    /// No `event_name` is available here (a tick has no dispatch event), so
    /// `guard_failed` is never populated on this path.
    pub fn from_process_result(
        trait_name: &str,
        result: &crate::runtime::ProcessEventResult,
    ) -> Self {
        let outcomes = [(trait_name.to_string(), result.clone())];
        // No seed/event context on the single-result path — pass a seed that can never
        // match so `guard_failed` stays unset (ticks don't dispatch a named event).
        Self::from_process_results(&outcomes, "", "")
    }
}

fn dedup_rows_by_id(data: &mut HashMap<String, Vec<Value>>) {
    for rows in data.values_mut() {
        let mut seen = std::collections::HashSet::new();
        rows.retain(|row| {
            match row
                .as_object()
                .and_then(|o| o.get("id"))
                .and_then(Value::as_str)
            {
                Some(id) => seen.insert(id.to_string()),
                None => true,
            }
        });
    }
}

/// Map an internal server-side [`EffectResult`] to the wire [`ServerEffectResult`].
/// `None` for effects with no server-effect wire form (client-targeted or internal-only
/// kinds — `render-ui`/`navigate`/`navigate-back`/`emit`/`log`/`spawn`/`despawn`/`custom`).
fn server_effect_result(er: &EffectResult) -> Option<ServerEffectResult> {
    let (kind, entity_type, action) = match &er.effect {
        Effect::Set { path, .. } => (ServerEffectKind::Set, None, Some(path.clone())),
        Effect::Persist {
            operation,
            entity_type,
            ..
        } => (
            ServerEffectKind::Persist,
            Some(entity_type.clone()),
            Some(format!("{operation:?}").to_lowercase()),
        ),
        Effect::PersistBatch { .. } => (ServerEffectKind::Persist, None, Some("batch".to_string())),
        Effect::CallService {
            service, method, ..
        } => (
            ServerEffectKind::CallService,
            None,
            Some(format!("{service}.{method}")),
        ),
        Effect::Fetch { entity_type, .. } => {
            (ServerEffectKind::Fetch, Some(entity_type.clone()), None)
        }
        _ => return None,
    };
    let success = matches!(er.outcome, EffectOutcome::Executed);
    let denied = matches!(er.outcome, EffectOutcome::Failed)
        && er
            .error
            .as_deref()
            .map(crate::runtime::entity_access::is_access_denied_error)
            .unwrap_or(false);
    Some(ServerEffectResult {
        effect: kind,
        action,
        entity_type,
        name: None,
        data: er.return_value.clone(),
        success,
        denied: if denied { Some(true) } else { None },
        error: er.error.clone(),
        code: er.code,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_bus_event_source_round_trips_pending_key() {
        let wire = serde_json::json!({ "trait": "Form", "pendingKey": "k-1" });
        let parsed: super::BusEventSource = serde_json::from_value(wire.clone()).expect("parses");
        assert_eq!(parsed.pending_key.as_deref(), Some("k-1"));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
        let bare = serde_json::to_value(super::BusEventSource::from_trait("Form")).unwrap();
        assert_eq!(bare, serde_json::json!({ "trait": "Form" }));
    }

    #[test]
    fn a_mount_seed_accepts_only_lifecycle_events() {
        for e in ["INIT", "LOAD", "$MOUNT"] {
            let wire = serde_json::json!({ "trait": "T", "event": e });
            let seed: super::MountSeed =
                serde_json::from_value(wire.clone()).expect("lifecycle parses");
            assert_eq!(seed.event.as_str(), e);
            assert_eq!(serde_json::to_value(&seed).unwrap(), wire);
        }
        let bad = serde_json::json!({ "trait": "T", "event": "SAVE" });
        assert!(serde_json::from_value::<super::MountSeed>(bad).is_err());
    }

    #[test]
    fn an_effect_failed_rejection_has_the_core_wire_shape() {
        let wire = serde_json::json!({ "code": "effect-failed", "trait": "Preview", "from": "idle", "event": "INIT", "error": "boom" });
        let parsed: super::TransitionRejection =
            serde_json::from_value(wire.clone()).expect("parses");
        assert_eq!(parsed.code, super::TransitionRejectionCode::EffectFailed);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
    }

    #[test]
    fn control_a_rejection_without_an_error_omits_the_field() {
        let wire = serde_json::json!({ "code": "no-matching-transition", "trait": "Preview", "event": "SAVE" });
        let parsed: super::TransitionRejection =
            serde_json::from_value(wire.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
    }

    fn painted(trait_name: &str, slot: &str) -> (String, Effect) {
        (
            trait_name.to_string(),
            Effect::RenderUi {
                slot: slot.to_string(),
                content: Value::Null,
            },
        )
    }

    fn response_of(paints: &[(String, Effect)]) -> EventResponse {
        let mut r = EventResponse::default();
        for (t, e) in paints {
            let ce = ClientEffect::from_effect(e).unwrap();
            r.client_effects_by_trait
                .get_or_insert_with(Vec::new)
                .push(ClientEffectByTrait {
                    trait_name: t.clone(),
                    effect: ce.clone(),
                    event: None,
                    from_state: None,
                    callsite_payload: None,
                });
            r.client_effects.get_or_insert_with(Vec::new).push(ce);
        }
        r
    }

    #[test]
    fn drop_renders_of_removes_only_the_named_trait_slot_from_both_lists() {
        let mut r = response_of(&[
            painted("Hud", "hud"),
            painted("Hud", "side"),
            painted("Other", "hud"),
        ]);
        r.drop_renders_of(
            &[("Hud".to_string(), "hud".to_string())]
                .into_iter()
                .collect(),
        );
        let kept: Vec<_> = r
            .client_effects_by_trait
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.trait_name.clone())
            .collect();
        assert_eq!(kept, vec!["Hud", "Other"]);
        assert_eq!(r.client_effects.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn control_drop_renders_of_an_empty_set_changes_nothing() {
        let mut r = response_of(&[painted("Hud", "hud")]);
        let before = r.clone();
        r.drop_renders_of(&Default::default());
        assert_eq!(r, before);
    }

    #[test]
    fn bus_event_source_fields_are_exactly_the_wire_keys() {
        let full = BusEventSource {
            orbital: Some(String::new()),
            orbital_id: Some(String::new()),
            r#trait: Some(String::new()),
            trait_id: Some(String::new()),
            event_id: Some(String::new()),
            transition: Some(String::new()),
            tick: Some(String::new()),
            from_bridge: Some(true),
            dispatched: Some(true),
            origin_client_id: Some(String::new()),
            pending_key: Some(String::new()),
            persist_success: Some(true),
        };
        let json = serde_json::to_value(full).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        // `pendingKey` is a UI activation key and `persistSuccess` a live-push mark: on the wire, but not
        // `@event.source` bindings.
        let mut declared = super::BUS_EVENT_SOURCE_FIELDS.to_vec();
        declared.push("pendingKey");
        declared.push("persistSuccess");
        declared.sort_unstable();
        assert_eq!(keys, declared);
    }

    use super::*;
    use crate::runtime::ProcessEventResult;

    /// `@almadar/core` `ClientEffectByTrait.callsitePayload` (2026-09-30, G-ORB-025): an
    /// embedded child's frame carries the payload it was composed with; absent stays absent.
    #[test]
    fn client_effect_by_trait_carries_the_optional_callsite_payload() {
        let wire = serde_json::json!({
            "traitName": "Body",
            "effect": ["render-ui", "main", { "type": "timeline" }],
            "callsitePayload": { "data": [{ "id": "e1" }] }
        });
        let parsed: ClientEffectByTrait = serde_json::from_value(wire.clone()).expect("wire shape");
        assert!(parsed.callsite_payload.is_some());
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["callsitePayload"],
            wire["callsitePayload"]
        );

        let bare = serde_json::json!({ "traitName": "Body", "effect": ["render-ui", "main", { "type": "timeline" }] });
        let parsed: ClientEffectByTrait = serde_json::from_value(bare).expect("wire shape");
        assert!(parsed.callsite_payload.is_none());
        assert!(serde_json::to_value(&parsed)
            .unwrap()
            .get("callsitePayload")
            .is_none());
    }

    fn denied_persist() -> ServerEffectResult {
        ServerEffectResult {
            effect: ServerEffectKind::Persist,
            action: Some("create".into()),
            entity_type: Some("Note".into()),
            name: None,
            data: None,
            success: false,
            denied: Some(true),
            error: Some("access denied".into()),
            code: None,
        }
    }

    #[test]
    fn a_server_denied_persist_appears_on_the_settled_response() {
        let posted = EventResponse {
            success: true,
            effect_results: Some(vec![denied_persist()]),
            ..Default::default()
        };
        let settled = EventResponse::success("idle").with_server_effect_results(&posted);
        assert_eq!(settled.effect_results, Some(vec![denied_persist()]));
    }

    #[test]
    fn the_server_results_follow_the_local_ones() {
        let local = ServerEffectResult {
            success: true,
            denied: None,
            error: None,
            ..denied_persist()
        };
        let mut base = EventResponse::success("idle");
        base.effect_results = Some(vec![local.clone()]);
        let posted = EventResponse {
            effect_results: Some(vec![denied_persist()]),
            ..Default::default()
        };
        let settled = base.with_server_effect_results(&posted);
        assert_eq!(settled.effect_results, Some(vec![local, denied_persist()]));
    }

    #[test]
    fn control_a_server_answer_with_no_outcomes_adds_none() {
        let settled =
            EventResponse::success("idle").with_server_effect_results(&EventResponse::default());
        assert_eq!(settled.effect_results, None);
        let empty = EventResponse {
            effect_results: Some(Vec::new()),
            ..Default::default()
        };
        assert_eq!(
            EventResponse::success("idle")
                .with_server_effect_results(&empty)
                .effect_results,
            None
        );
    }

    fn empty_result(new_state: &str, executed: bool) -> ProcessEventResult {
        ProcessEventResult {
            event: None,
            executed,
            previous_state: "idle".to_string(),
            new_state: new_state.to_string(),
            effects_executed: 0,
            effect_results: Vec::new(),
            entity_data: Value::Null,
            linked_entity: "Note".to_string(),
            payload: Value::Null,
            entry: None,
            fired_arm: None,
            guards: Vec::new(),
            effect_runs: Vec::new(),
            cascade_dropped: Vec::new(),
            consumed_emits: Vec::new(),
            failure: None,
            callsite_payload: None,
        }
    }

    #[test]
    fn success_and_error_constructors_compile_and_default() {
        let ok = EventResponse::success("browsing");
        assert!(ok.success);
        assert!(ok.emitted_events.is_empty());

        let err = EventResponse::error("Guard failed");
        assert!(!err.success);
        assert_eq!(err.error, Some("Guard failed".to_string()));
    }

    #[test]
    fn from_process_results_fills_states_and_transitioned() {
        let outcomes = vec![("NoteAppLayout".to_string(), empty_result("browsing", true))];
        let resp = EventResponse::from_process_results(&outcomes, "NoteAppLayout", "INIT");
        assert!(resp.success);
        assert!(resp.transitioned);
        assert_eq!(
            resp.states.get("NoteAppLayout"),
            Some(&"browsing".to_string())
        );
        assert!(resp.guard_failed.is_none());
    }

    #[test]
    fn from_process_results_sets_guard_failed_on_seed_rejection() {
        let outcomes = vec![("NotePersistor".to_string(), empty_result("idle", false))];
        let resp = EventResponse::from_process_results(&outcomes, "NotePersistor", "CREATE_DRAFT");
        assert_eq!(
            resp.guard_failed.as_deref(),
            Some("NotePersistor.CREATE_DRAFT")
        );
    }

    #[test]
    fn from_process_results_does_not_set_guard_failed_for_non_seed_no_op() {
        // A tolerated no-op host (embed bubble) must not poison guard_failed.
        let outcomes = vec![
            ("Child".to_string(), empty_result("open", true)),
            ("Host".to_string(), empty_result("idle", false)),
        ];
        let resp = EventResponse::from_process_results(&outcomes, "Child", "CREATE_DRAFT");
        assert!(resp.guard_failed.is_none());
    }

    #[test]
    fn from_process_result_matches_single_outcome_of_from_process_results() {
        let result = empty_result("browsing", true);
        let single = EventResponse::from_process_result("NoteAppLayout", &result);
        assert_eq!(
            single.states.get("NoteAppLayout"),
            Some(&"browsing".to_string())
        );
        assert!(single.guard_failed.is_none());
    }

    #[test]
    fn emitted_events_always_serialized_as_empty_array() {
        let resp = EventResponse::success("idle");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json.get("emittedEvents"), Some(&serde_json::json!([])));
    }

    #[test]
    fn client_effects_from_render_ui_and_emit_split_correctly() {
        let mut result = empty_result("open", true);
        result.effect_results = vec![
            EffectResult::executed(Effect::RenderUi {
                slot: "modal".to_string(),
                content: Value::Object(ValueMap::from([(
                    "type".to_string(),
                    Value::String("form".to_string()),
                )])),
            }),
            EffectResult::executed(Effect::Emit {
                event: "DRAFT_CREATED".to_string(),
                payload: Value::Null,
            }),
        ];
        let outcomes = vec![("NotePersistor".to_string(), result)];
        let resp = EventResponse::from_process_results(&outcomes, "NotePersistor", "CREATE_DRAFT");

        let effects = resp.client_effects.expect("client effects");
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], ClientEffect::RenderUi { .. }));

        let by_trait = resp.client_effects_by_trait.expect("by-trait");
        assert_eq!(by_trait.len(), 1);
        assert_eq!(by_trait[0].trait_name, "NotePersistor");

        assert_eq!(resp.emitted_events.len(), 1);
        assert_eq!(resp.emitted_events[0].event, "DRAFT_CREATED");
        assert_eq!(
            resp.emitted_events[0]
                .source
                .as_ref()
                .and_then(|s| s.r#trait.clone()),
            Some("NotePersistor".to_string())
        );
    }

    #[test]
    fn bus_event_source_round_trip_uses_trait_key() {
        let source = BusEventSource::from_trait("NotePersistor");
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json, serde_json::json!({ "trait": "NotePersistor" }));
        let back: BusEventSource = serde_json::from_value(json).unwrap();
        assert_eq!(back, source);
    }

    #[test]
    fn a_server_effect_result_code_round_trips_both_core_values() {
        for code in ["not-found", "no-row-key"] {
            let wire = serde_json::json!({ "effect": "persist", "success": false, "code": code });
            let parsed: ServerEffectResult = serde_json::from_value(wire.clone()).expect("parses");
            assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
        }
        let bad = serde_json::json!({ "effect": "persist", "success": false, "code": "denied" });
        assert!(serde_json::from_value::<ServerEffectResult>(bad).is_err());
    }

    #[test]
    fn server_effect_kind_is_kebab_case() {
        let result = ServerEffectResult {
            effect: ServerEffectKind::CallService,
            action: None,
            entity_type: None,
            name: None,
            data: None,
            success: true,
            denied: None,
            error: None,
            code: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.get("code").is_none(), "code omitted when unset");
        assert_eq!(json.get("effect"), Some(&serde_json::json!("call-service")));
        assert!(json.get("denied").is_none(), "denied omitted when not true");
    }

    #[test]
    fn server_effect_kind_swap_is_spelled_like_the_effect_head() {
        assert_eq!(
            serde_json::to_value(ServerEffectKind::Swap).unwrap(),
            serde_json::json!("swap")
        );
        assert_eq!(
            serde_json::to_value(ServerEffectKind::FetchStream).unwrap(),
            serde_json::json!("fetch-stream")
        );
        // The retired spelling (renamed 2026-09-25: `!` is the lexer's required marker).
        assert!(serde_json::from_value::<ServerEffectKind>(serde_json::json!("swap!")).is_err());
    }

    #[test]
    fn event_request_round_trip_traits_and_entity_by_trait() {
        let json = serde_json::json!({
            "event": "CREATE_DRAFT",
            "targetTrait": "InlineButtonRender3",
            "traits": [{ "trait": "InlineButtonRender3", "from": "click" }],
            "entityByTrait": { "InlineButtonRender3": { "id": "1" } },
        });
        let req: EventRequest = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(req.event, "CREATE_DRAFT");
        assert_eq!(req.target_trait.as_deref(), Some("InlineButtonRender3"));
        assert_eq!(req.traits.as_ref().map(|t| t.len()), Some(1));
        let back = serde_json::to_value(&req).unwrap();
        assert_eq!(back.get("event"), json.get("event"));
        assert_eq!(back.get("targetTrait"), json.get("targetTrait"));
    }

    #[test]
    fn event_request_carries_the_viewer_locale() {
        let req: EventRequest =
            serde_json::from_value(serde_json::json!({ "event": "INIT", "locale": "ar" })).unwrap();
        assert_eq!(req.locale.as_deref(), Some("ar"));
        assert_eq!(
            serde_json::to_value(&req).unwrap().get("locale"),
            Some(&serde_json::json!("ar"))
        );
    }

    #[test]
    fn control_an_event_request_without_a_locale_serializes_none() {
        let req: EventRequest =
            serde_json::from_value(serde_json::json!({ "event": "INIT" })).unwrap();
        assert_eq!(req.locale, None);
        assert_eq!(serde_json::to_value(&req).unwrap().get("locale"), None);
    }
}
