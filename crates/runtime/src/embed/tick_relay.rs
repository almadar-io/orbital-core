//! The coalesced relay of tick-stamped dispatches to the other clients (a live-item policy, T6): between flushes the
//! newest item per (origin client, orbital, event) wins, so peers receive positions at snapshot rate, never one per
//! emission. The kernel decides what is kept and when a flush is due; the host owns only the timer.

use crate::effects::response::EventRequest;
use crate::effects::{LiveBroadcastItem, LiveTarget};
use orbital_core_base::bus_event::BusEventSource;

/// How long a window stays open before its items are due (ms): ≈20 Hz.
pub const DEFAULT_TICK_RELAY_INTERVAL_MS: u64 = 50;

type Key = (Option<String>, String, String);

pub(super) struct TickRelay {
    interval_ms: u64,
    pending: Vec<(Key, LiveBroadcastItem)>,
}

impl Default for TickRelay {
    fn default() -> Self {
        Self {
            interval_ms: DEFAULT_TICK_RELAY_INTERVAL_MS,
            pending: Vec::new(),
        }
    }
}

impl TickRelay {
    pub(super) fn set_interval(&mut self, interval_ms: u64) {
        self.interval_ms = interval_ms;
    }

    /// Queue the tick-stamped dispatch `request` of `orbital`; `None` when it carries no tick. `Some(ms)` when it opened
    /// a window: the host flushes that long from now. Items queued while a window is open join it.
    pub(super) fn queue(&mut self, orbital: &str, request: &EventRequest) -> Option<u64> {
        let tick = request.tick.as_ref()?;
        let item = LiveBroadcastItem {
            event: request.event.clone(),
            payload: request.payload.clone(),
            source: BusEventSource {
                orbital: Some(orbital.to_string()),
                r#trait: request.source_trait.clone(),
                tick: Some(tick.clone()),
                ..Default::default()
            },
            origin_client_id: request.client_id.clone(),
            target: LiveTarget::Peers,
            dispatch: None,
        };
        let key = (
            request.client_id.clone(),
            orbital.to_string(),
            request.event.clone(),
        );
        let opened = self.pending.is_empty();
        match self.pending.iter_mut().find(|(held, _)| *held == key) {
            Some((_, slot)) => *slot = item,
            None => self.pending.push((key, item)),
        }
        opened.then_some(self.interval_ms)
    }

    /// Everything queued, in the order each key first arrived; the window closes.
    pub(super) fn flush(&mut self) -> Vec<LiveBroadcastItem> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(_, item)| item)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::value::Value;

    fn tick(client: Option<&str>, event: &str, x: f64) -> EventRequest {
        EventRequest {
            event: event.to_string(),
            payload: Some(Value::Number(x)),
            client_id: client.map(str::to_string),
            tick: Some("beat".to_string()),
            source_trait: Some("Mover".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn newest_per_key_wins_and_the_first_item_opens_the_window() {
        let mut relay = TickRelay::default();
        assert_eq!(
            relay.queue("World", &tick(Some("a"), "MOVED", 1.0)),
            Some(50)
        );
        assert_eq!(relay.queue("World", &tick(Some("a"), "MOVED", 2.0)), None);
        assert_eq!(relay.queue("World", &tick(Some("b"), "MOVED", 9.0)), None);
        let items = relay.flush();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].payload, Some(Value::Number(2.0)));
        assert_eq!(items[0].origin_client_id.as_deref(), Some("a"));
        assert_eq!(items[0].source.tick.as_deref(), Some("beat"));
        assert_eq!(items[0].source.orbital.as_deref(), Some("World"));
        assert_eq!(items[1].payload, Some(Value::Number(9.0)));
        assert_eq!(items[0].target, LiveTarget::Peers);
    }

    #[test]
    fn control_a_request_without_a_tick_is_not_relayed_and_a_flushed_window_reopens() {
        let mut relay = TickRelay::default();
        let mut plain = tick(Some("a"), "MOVED", 1.0);
        plain.tick = None;
        assert_eq!(relay.queue("World", &plain), None);
        assert!(relay.flush().is_empty());
        relay.set_interval(80);
        assert_eq!(relay.queue("World", &tick(None, "MOVED", 1.0)), Some(80));
        assert_eq!(relay.flush().len(), 1);
        assert_eq!(relay.queue("World", &tick(None, "MOVED", 3.0)), Some(80));
        assert!(relay.flush()[0].origin_client_id.is_none());
    }

    #[test]
    fn different_events_and_orbitals_are_different_keys() {
        let mut relay = TickRelay::default();
        relay.queue("World", &tick(Some("a"), "MOVED", 1.0));
        relay.queue("World", &tick(Some("a"), "TURNED", 1.0));
        relay.queue("Other", &tick(Some("a"), "MOVED", 1.0));
        assert_eq!(relay.flush().len(), 3);
    }
}
