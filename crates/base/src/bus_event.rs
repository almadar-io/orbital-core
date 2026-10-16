use crate::value::Value;
use serde::Deserialize;
use serde::Serialize;


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
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusEventSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbital: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(type = "OrbitalId"))]
    pub orbital_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "trait")]
    pub r#trait: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(type = "TraitId"))]
    pub trait_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(type = "EventId"))]
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

/// One event a transition's effects emitted, carried back to the client to re-dispatch
/// in order. Mirror of `EmittedEvent` (`bus.ts`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedEvent {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(type = "EventPayload"))]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BusEventSource>,
}

/// `ServerEffectResult.code` — mirror of core's `"not-found" | "no-row-key" | "duplicate-id"`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerEffectCode {
    NotFound,
    NoRowKey,
    DuplicateId,
}

/// The event a tick firing carries on the wire (a `runTick` leg's `event`). Twin of
/// `@almadar/runtime` `tickEventKey`.
pub fn tick_event_key(tick: &str) -> String {
    format!("$TICK_{tick}")
}