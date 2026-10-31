//! Event Response Types
//!
//! The Rust mirror of `@almadar/core`'s `OrbitalEventResponse`/`OrbitalEventRequest`
//! (`packages/almadar-core/src/types/bus.ts`) — canonical = the TypeScript runtime's
//! implementation (`groovy-inventing-cloud.md` Program A, §156). Field-for-field, in
//! `camelCase`, every optional/collection defaulted so a hand-built or fixture JSON
//! missing a key still deserializes.
//!
//! Reference: builder/docs/Orbital_Pattern.orb
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use orbital_core_base::client_effect::ClientEffect;
use orbital_core_base::effect_types::{Effect, EffectOutcome, EffectResult};

/// The evaluator value of a delivery record `{ event, payload, source }` (`@event`); twin of
/// `@almadar/core` `deliveryRecordValue`. `source` carries only the fields that are set.
pub fn delivery_record_value(event: &str, payload: &Value, source: &BusEventSource) -> Value {
    let mut src: ValueMap = ValueMap::default();
    let mut put_str = |key: &str, v: &Option<String>| {
        if let Some(v) = v {
            src.insert(key.to_string().into(), Value::string(v.clone()));
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
    put_str("instanceKey", &source.instance_key);
    if let Some(v) = source.from_bridge {
        src.insert(orbital_core_base::static_key!("fromBridge"), Value::Bool(v));
    }
    if let Some(v) = source.dispatched {
        src.insert(orbital_core_base::static_key!("dispatched"), Value::Bool(v));
    }
    let mut record = ValueMap::default();
    record.insert(orbital_core_base::static_key!("event"), Value::string(event.to_string()));
    record.insert(orbital_core_base::static_key!("payload"), payload.clone());
    record.insert(orbital_core_base::static_key!("source"), Value::object(src));
    Value::object(record)
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
    /// Set when the paint is a repaint with no transition behind it that a renderer must act on beyond the
    /// content: the rows a row-set reference in it points at changed (EXEC-RENDER-3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repaint: Option<RepaintReason>,
}

/// Why a paint was repainted without a transition, when the content alone does not say.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepaintReason {
    /// A kernel write to an entity type a row-set reference in the paint points at: re-resolve it, though the
    /// reference's ids are unchanged.
    RowsChanged,
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
    /// The namespaced operator (`llm/generate`) — only on `ServerEffectKind::Substrate`.
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
    /// Coded failure reason for a persist that resolved no target row, or created on a taken id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ServerEffectCode>,
    /// The event an `emit` effect targets; only on `ServerEffectKind::Emit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The declared `emit.failure` event a failed effect was routed to (the kernel fired it, so the failure is handled); absent when it declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed: Option<String>,
}

/// `EventRequest.run_tick`: which trait's tick the data authority runs once.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTick {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub tick: String,
}

/// Where a leg runs (EXEC-DIST-2): the browser store when every row it touches is browser-stored, else the server.
/// The kernel decides it from the effects the leg carries; a host routes by it and decides nothing.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LegResidence {
    Browser,
    Server,
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
    /// Where this leg runs, set by the kernel on every leg it builds (EXEC-DIST-6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residence: Option<LegResidence>,
    /// Not a dispatch: the client holds this version of the session's frames and asks what it is missing. The answer
    /// is a response carrying only `frames`, a snapshot unless the client is current (EXEC-DIST-7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames_since: Option<u64>,
}

/// The option keys of a `program/dispatch` event request that become request fields as they are named.
const DISPATCH_REQUEST_KEYS: [&str; 10] =
    ["event", "entityId", "targetTrait", "clientId", "locale", "delivery", "dispatchLog", "mount", "entityByTrait", "framesSince"];

/// An empty text, record or list in an event request is absent, as the HTTP routes read it.
fn dispatch_option_present(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(text) => !text.is_empty(),
        serde_json::Value::Object(fields) => !fields.is_empty(),
        serde_json::Value::Array(items) => !items.is_empty(),
        _ => true,
    }
}

impl EventRequest {
    /// The typed request a `program/dispatch` call names: `payload` and the option record (`option` reads one key of
    /// it). The one parse every host shares; a record that is no event request is refused with the reason, which a host
    /// answers as `INVALID_REQUEST` (400).
    pub fn from_dispatch(
        payload: serde_json::Value,
        option: impl Fn(&str) -> Option<serde_json::Value>,
    ) -> Result<Self, String> {
        let mut request = serde_json::Map::new();
        request.insert("payload".to_string(), payload);
        for key in DISPATCH_REQUEST_KEYS {
            if let Some(value) = option(key).filter(dispatch_option_present) {
                request.insert(key.to_string(), value);
            }
        }
        if let Some(traits) = option("traits").filter(|v| !v.is_null()) {
            request.insert("traits".to_string(), traits);
        }
        if option("viewer").is_some_and(|v| !v.is_null()) {
            return Err("program/dispatch: `viewer` is retired; a session's `@user` is its bound identity (identity/sign-in)".to_string());
        }
        serde_json::from_value(serde_json::Value::Object(request))
            .map_err(|e| format!("program/dispatch: not an event request: {e}"))
    }
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
    /// Fetched pages delivered by reference (`deliver: ids`), keyed by entity name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_refs: Option<HashMap<String, orbital_core_base::row_set::RowSetRef>>,
    /// What this response changes in the frames the client holds, against the kernel's view record (EXEC-DIST-7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<crate::effects::frame_delta::FrameDelta>,
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
    /// The event is an inbound hook's (`listens { E -> inbound … }`): only `/api/hooks/<name>` delivers it.
    InboundOnlyEvent,
    /// An effect of the trait's transition failed; its later effects did not run.
    EffectFailed,
    /// The request's `locale` is not one of the locales the program declares.
    LocaleNotDeclared,
    /// A server-role request carried circuit state (`traits` / `entityByTrait`) without being a delegated leg.
    ClientCircuitStateRefused,
    /// A slot's repaint for an entity change failed to resolve (a call in its props threw); the slot keeps its last
    /// paint.
    RepaintFailed,
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
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub guard: Option<Value>,
    /// `effect-failed`: the failure's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `repaint-failed`: the slot whose repaint failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
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
    /// The arm that fired, by its position among the state's arms for the event (declaration order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_arm: Option<usize>,
    /// `true` when the fired arm's guard held; absent for an unguarded arm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_result: Option<bool>,
    pub effects: Vec<HopEffect>,
    /// What a verifier judges the hop's `@ensures` on; present only when the kernel records each step's entity
    /// after its effects (`set_record_entity_after`, EXEC-STEP-10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<HopObservation>,
}

/// A hop as a proposition judge reads it: the payload it was delivered with, and the trait's `@entity` at the
/// step's start and after its effects.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HopObservation {
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: Value,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub entity_before: Value,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub entity_after: Value,
}

impl TransitionHop {
    /// The executed hops among `outcomes`, in order.
    pub fn of_outcomes(outcomes: &[(String, crate::process_result::ProcessEventResult)]) -> Vec<Self> {
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
                    fired_arm: result.fired_arm,
                    guard_result,
                    effects: result.effect_runs.iter().map(HopEffect::of_run).collect(),
                    observed: result.entry.as_ref().and_then(|entry| {
                        Some(HopObservation {
                            payload: Value::clone(&result.payload),
                            entity_before: entry.entity_before.to_value(),
                            entity_after: entry.entity_after.as_ref()?.to_value(),
                        })
                    }),
                })
            })
            .collect()
    }
}

impl HopEffect {
    fn of_run(run: &crate::process_result::EffectRun) -> Self {
        use crate::process_result::EffectRunStatus;
        let (head, args) = match &run.effect {
            orbital_core_base::schema::sexpr::SExpression::Array(items) => match items.split_first() {
                Some((orbital_core_base::schema::sexpr::SExpression::String(head), rest)) => (
                    head.to_string(),
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
        outcomes: &[(String, crate::process_result::ProcessEventResult)],
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
                    slot: None,
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

    /// A request refused before any dispatch, for `code` on `trait_name.event`.
    pub fn refused(code: TransitionRejectionCode, trait_name: &str, event: &str, message: String) -> Self {
        Self {
            success: false,
            error: Some(message),
            rejections: Some(vec![TransitionRejection {
                code,
                trait_name: Some(trait_name.to_string()),
                from: None,
                event: Some(event.to_string()),
                states_declaring_event: None,
                transition: None,
                guard: None,
                error: None,
                slot: None,
            }]),
            ..Default::default()
        }
    }

    /// A request refused whole, for `code`, before any trait is chosen: the rejection names the event only.
    pub fn refused_request(code: TransitionRejectionCode, event: &str, message: String) -> Self {
        Self {
            success: false,
            error: Some(message),
            rejections: Some(vec![TransitionRejection {
                code,
                trait_name: None,
                from: None,
                event: Some(event.to_string()),
                states_declaring_event: None,
                transition: None,
                guard: None,
                error: None,
                slot: None,
            }]),
            ..Default::default()
        }
    }

    /// A client-facing request naming an inbound hook's event: only `/api/hooks/<name>` delivers it.
    pub fn inbound_only(trait_name: &str, event: &str) -> Self {
        Self::refused(
            TransitionRejectionCode::InboundOnlyEvent,
            trait_name,
            event,
            orbital_core_base::inbound::inbound_only_message(trait_name, event),
        )
    }

    /// An input-channel request for an event that is not a declared external input of its trait.
    pub fn not_an_external_input(trait_name: &str, event: &str) -> Self {
        Self::refused(
            TransitionRejectionCode::NotAnExternalInput,
            trait_name,
            event,
            format!("{trait_name}.{event} is not a declared external input"),
        )
    }

    /// The client effects in paint order, without their attribution: a view of `client_effects_by_trait`.
    pub fn client_effects(&self) -> Vec<&ClientEffect> {
        self.client_effects_by_trait.iter().flatten().map(|e| &e.effect).collect()
    }

    /// Append the renders of traits repainted for an entity change
    /// ([`crate::runtime::Rerender`]), after everything the dispatch itself rendered.
    pub fn fold_rerenders(&mut self, rerenders: &[crate::kernel::Rerender]) {
        for rerender in rerenders {
            for fault in &rerender.faults {
                self.rejections.get_or_insert_with(Vec::new).push(TransitionRejection {
                    code: TransitionRejectionCode::RepaintFailed,
                    trait_name: Some(rerender.trait_name.clone()),
                    from: None,
                    event: None,
                    states_declaring_event: None,
                    transition: None,
                    guard: None,
                    error: Some(fault.error.clone()),
                    slot: Some(fault.slot.clone()),
                });
            }
            for effect in &rerender.effects {
                let Some(client_effect) = ClientEffect::from_effect(effect) else {
                    continue;
                };
                let repaint = match &client_effect {
                    ClientEffect::RenderUi { slot, .. } if rerender.rows_changed.contains(slot) => {
                        Some(RepaintReason::RowsChanged)
                    }
                    _ => None,
                };
                self.client_effects_by_trait
                    .get_or_insert_with(Vec::new)
                    .push(ClientEffectByTrait {
                        trait_name: rerender.trait_name.clone(),
                        effect: client_effect,
                        event: None,
                        from_state: None,
                        callsite_payload: None,
                        repaint,
                    });
            }
        }
    }

    /// Drop the `render-ui` effects that paint a `(trait, slot)` in `superseded`: a newer paint
    /// of that slot reaches the host through another response.
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
    ///
    /// The frames a dispatch wrote are not in the response until the kernel seals them against its view record
    /// ([`RuntimeKernel::seal_outcomes`](crate::kernel::RuntimeKernel::seal_outcomes), EXEC-DIST-7).
    pub fn from_process_results(
        outcomes: &[(String, crate::process_result::ProcessEventResult)],
        seed_trait: &str,
        event_name: &str,
    ) -> Self {
        Self::fold_outcomes(
            outcomes.iter().map(|(t, r)| (t.as_str(), r)),
            seed_trait,
            event_name,
        )
    }

    fn fold_outcomes<'a>(
        outcomes: impl Iterator<Item = (&'a str, &'a crate::process_result::ProcessEventResult)>,
        seed_trait: &str,
        event_name: &str,
    ) -> Self {
        let mut merged = EventResponse {
            success: true,
            ..Default::default()
        };
        let mut data: HashMap<String, Vec<Value>> = HashMap::new();
        let mut data_refs: HashMap<String, orbital_core_base::row_set::RowSetRef> = HashMap::new();
        let mut client_effects_by_trait: Vec<ClientEffectByTrait> = Vec::new();
        let mut effect_results: Vec<ServerEffectResult> = Vec::new();
        let mut emitted_events: Vec<EmittedEvent> = Vec::new();
        let mut cascade_truncated: Vec<String> = Vec::new();

        let mut rejections: Vec<TransitionRejection> = Vec::new();
        for (trait_name, result) in outcomes {
            let trait_name: &str = trait_name;
            if let (Some(error), Some(event)) = (&result.failure, &result.event) {
                rejections.push(TransitionRejection {
                    code: TransitionRejectionCode::EffectFailed,
                    trait_name: Some(trait_name.to_string()),
                    from: Some(result.previous_state.clone()),
                    event: Some(event.clone()),
                    states_declaring_event: None,
                    transition: None,
                    guard: None,
                    error: Some(error.clone()),
                    slot: None,
                });
            }
            for dropped in &result.cascade_dropped {
                if !cascade_truncated.contains(dropped) {
                    cascade_truncated.push(dropped.clone());
                }
            }
            merged
                .states
                .insert(trait_name.to_string(), result.new_state.clone());
            merged.transitioned =
                merged.transitioned || (result.executed && result.failure.is_none());

            if trait_name == seed_trait && !result.executed {
                merged
                    .guard_failed
                    .get_or_insert_with(|| format!("{seed_trait}.{event_name}"));
            }

            for (index, er) in result.effect_results.iter().enumerate() {
                // A failed client effect or `emit` reached no one: its operands never resolved, so it paints nothing.
                if er.is_failed()
                    && (matches!(er.effect, Effect::Emit { .. }) || ClientEffect::from_result(er).is_some())
                {
                    continue;
                }
                if let Some(ce) = ClientEffect::from_result_in(er, Some((trait_name, crate::process_result::frame_row(&result.entity_id)))) {
                    client_effects_by_trait.push(ClientEffectByTrait {
                        trait_name: trait_name.to_string(),
                        effect: ce,
                        event: result.event.clone(),
                        from_state: result.event.as_ref().map(|_| result.previous_state.clone()),
                        callsite_payload: result.callsite_payload.clone(),
                        repaint: None,
                    });
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
                            ..result.emit_source.clone()
                        }),
                    });
                    effect_results.push(ServerEffectResult {
                        effect: ServerEffectKind::Emit,
                        action: None,
                        entity_type: None,
                        name: None,
                        data: None,
                        success: true,
                        denied: None,
                        error: None,
                        code: None,
                        target: Some(event.clone()),
                        routed: None,
                    });
                    continue;
                }
                if let Effect::Fetch { entity_type, .. } = &er.effect {
                    if matches!(er.outcome, EffectOutcome::Executed) {
                        if let Some(rows) = er.return_value.as_ref().and_then(Value::as_array) {
                            data.entry(entity_type.clone())
                                .or_default()
                                .extend(rows.iter().cloned());
                        } else if let Some(reference) = er
                            .return_value
                            .as_ref()
                            .and_then(crate::row_query::as_row_set_marker)
                        {
                            data_refs.insert(entity_type.clone(), reference);
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
        merged.data_refs = if data_refs.is_empty() {
            None
        } else {
            Some(data_refs)
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
        result: &crate::process_result::ProcessEventResult,
    ) -> Self {
        // No seed/event context on the single-result path — pass a seed that can never
        // match so `guard_failed` stays unset (ticks don't dispatch a named event).
        Self::fold_outcomes(std::iter::once((trait_name, result)), "", "")
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
    // The host runs a delegated effect and reports its own verdict (`with_server_effect_results`).
    if matches!(er.outcome, EffectOutcome::Delegated(_)) {
        return None;
    }
    let (kind, entity_type, action) = match &er.effect {
        Effect::Set { path, .. } => (ServerEffectKind::Set, None, Some(path.to_string())),
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
    let denied = matches!(er.outcome, EffectOutcome::Failed) && er.code == Some(ServerEffectCode::Denied);
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
        target: None,
        routed: er.routed_failure(),
    })
}


#[allow(unused_imports)]
pub use orbital_core_base::bus_event::{BUS_EVENT_SOURCE_FIELDS, BusEventSource, EmittedEvent, ServerEffectCode, tick_event_key};

#[cfg(test)]
mod tests;
