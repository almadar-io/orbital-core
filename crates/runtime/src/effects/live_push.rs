//! SSE wire items — the Rust mirror of the TS runtime's two push channels
//! (`groovy-inventing-cloud.md` Program A, §156):
//!
//! - [`StreamItem`]: `POST /api/orbitals/:orbital/events?stream=true` incremental item
//!   sequence, mirroring `OrbitalServerRuntime.ts`'s `onPush`/`sendSSEEvent` shapes
//!   (`{type:'event'|'effect'|'complete'|'error', data, timestamp}`).
//! - [`LiveBroadcastItem`]: the persistent `GET /api/events` channel's payload
//!   (`OrbitalServerRuntime.ts:344-356`), fanned out to every OTHER connected client.

use serde::{Deserialize, Serialize};

use orbital_core_base::bus_event::{BusEventSource, EmittedEvent};
use orbital_core_base::client_effect::ClientEffect;
use crate::effects::response::EventResponse;
use orbital_core_base::value::Value;

/// `{ message: string }` — the `data` payload of a `type: "error"` stream item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamErrorData {
    pub message: String,
}

/// The `{type, data}` half of a [`StreamItem`] — adjacently tagged so the wire reads
/// `{"type":"event","data":{...}}`, matching `sendSSEEvent`'s `ev` argument exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "lowercase")]
pub enum StreamItemPayload {
    /// One entry of `emittedEvents`, pushed as it fires (`onPush({type:'event', ...})`).
    Event(EmittedEvent),
    /// One `clientEffects` entry, pushed as it fires.
    Effect(ClientEffect),
    /// The full response, sent once processing finishes successfully.
    Complete(Box<EventResponse>),
    /// Sent instead of `complete` when processing throws.
    Error(StreamErrorData),
}

/// One SSE frame on the `?stream=true` branch of `POST /api/orbitals/:orbital/events`.
/// `timestamp` sits alongside `type`/`data` (never nested under `data`) — mirrors
/// `sendSSEEvent(res, { type, data, timestamp: Date.now() })` byte for byte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamItem {
    #[serde(flatten)]
    pub payload: StreamItemPayload,
    /// Milliseconds since the Unix epoch (`Date.now()` on the TS side).
    pub timestamp: i64,
}

impl StreamItem {
    pub fn event(event: EmittedEvent) -> Self {
        Self {
            payload: StreamItemPayload::Event(event),
            timestamp: orbital_core_base::time_util::now_millis(),
        }
    }

    pub fn effect(effect: ClientEffect) -> Self {
        Self {
            payload: StreamItemPayload::Effect(effect),
            timestamp: orbital_core_base::time_util::now_millis(),
        }
    }

    pub fn complete(response: EventResponse) -> Self {
        Self {
            payload: StreamItemPayload::Complete(Box::new(response)),
            timestamp: orbital_core_base::time_util::now_millis(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            payload: StreamItemPayload::Error(StreamErrorData {
                message: message.into(),
            }),
            timestamp: orbital_core_base::time_util::now_millis(),
        }
    }
}

/// One persist-envelope success emit, handed to the live-broadcast sink. Mirror of
/// `LiveBroadcastItem` (`OrbitalServerRuntime.ts:344-356`) — fanned out to every OTHER
/// connected client on `GET /api/events`, excluding `originClientId`'s own connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveBroadcastItem {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    pub source: BusEventSource,
    /// `clientId` of the request that produced this emit; excluded from delivery by the
    /// transport (never re-delivered to its own origin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_client_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_item_event_shape() {
        let item = StreamItem::event(EmittedEvent {
            event: "DRAFT_CREATED".to_string(),
            payload: None,
            source: Some(BusEventSource::from_trait("NotePersistor")),
        });
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json.get("type"), Some(&serde_json::json!("event")));
        assert_eq!(
            json.get("data").and_then(|d| d.get("event")),
            Some(&serde_json::json!("DRAFT_CREATED"))
        );
        assert!(json.get("timestamp").is_some());
    }

    #[test]
    fn stream_item_complete_shape() {
        let item = StreamItem::complete(EventResponse::success("idle"));
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json.get("type"), Some(&serde_json::json!("complete")));
        assert!(json.get("data").and_then(|d| d.get("success")).is_some());
    }

    #[test]
    fn stream_item_error_shape() {
        let item = StreamItem::error("boom");
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json.get("type"), Some(&serde_json::json!("error")));
        assert_eq!(
            json.get("data").and_then(|d| d.get("message")),
            Some(&serde_json::json!("boom"))
        );
    }

    #[test]
    fn live_broadcast_item_round_trip() {
        let item = LiveBroadcastItem {
            event: "NoteCreated".to_string(),
            payload: None,
            source: BusEventSource::from_trait("NotePersistor"),
            origin_client_id: Some("client-1".to_string()),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: LiveBroadcastItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }
}
