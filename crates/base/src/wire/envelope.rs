//! The cross-instance envelope: what a cross-unit `emit` becomes when its declared `listens` target runs in
//! another deployment unit (`docs/Almadar_Sandbox.md` Contracts 1, EXEC-QUEUE-9). The kernel only builds
//! envelopes and judges inbound ones; transport, routing and mailboxes belong to the host.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::codec::{hex, sha256};
use crate::value::{Value, ValueMap};
use serde::{Deserialize, Serialize};

/// The most hops a causal chain of envelopes may take; the next one is refused `hop-cap`.
pub const HOP_CAP: u32 = 16;

/// The event the emitting trait hears, through a declared `listens`, when its envelope is refused.
pub const DELIVERY_REJECTED: &str = "DELIVERY_REJECTED";

/// One trait (or a whole orbital) of one instance. `instance` is the deployment unit's name.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceRef {
    pub instance: String,
    pub orbital: String,
    #[serde(rename = "trait", default, skip_serializing_if = "Option::is_none")]
    pub trait_name: Option<String>,
}

/// One delivery between instances. `id` is `envelope_id(from.instance, causal_id, dispatch_seq, emit_ordinal)`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub id: String,
    pub from: InstanceRef,
    pub to: InstanceRef,
    pub event: String,
    pub payload: Value,
    pub causal_id: String,
    pub hop: u32,
}

/// Why an envelope was not taken.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeliverRefusalKind {
    MailboxFull,
    HopCap,
    UnknownInstance,
    Denied,
    Down,
}

impl DeliverRefusalKind {
    /// The spelling a `DELIVERY_REJECTED` payload carries as `reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MailboxFull => "mailbox-full",
            Self::HopCap => "hop-cap",
            Self::UnknownInstance => "unknown-instance",
            Self::Denied => "denied",
            Self::Down => "down",
        }
    }
}

/// A refused envelope: the reason and the envelope's id.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverRefusal {
    pub kind: DeliverRefusalKind,
    pub id: String,
}

/// The host took the envelope (it will be, or already was, dispatched on the receiving side).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted;

/// What a receiving kernel decided about an inbound envelope.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum EnvelopeVerdict {
    Accepted,
    Duplicate,
    Rejected(DeliverRefusal),
}

/// One refused delivery of an emit, as the emitting step records it: the payload of its `DELIVERY_REJECTED`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRefused {
    pub id: String,
    pub to: InstanceRef,
    pub event: String,
    pub reason: DeliverRefusalKind,
    pub causal_id: String,
}

impl DeliveryRefused {
    /// `{ id, to: { instance, orbital, trait? }, event, reason, causalId }`.
    pub fn payload(&self) -> Value {
        let mut to = ValueMap::default();
        to.insert(
            "instance".to_string(),
            Value::string(self.to.instance.clone()),
        );
        to.insert(
            "orbital".to_string(),
            Value::string(self.to.orbital.clone()),
        );
        if let Some(trait_name) = &self.to.trait_name {
            to.insert("trait".to_string(), Value::string(trait_name.clone()));
        }
        let mut payload = ValueMap::default();
        payload.insert("id".to_string(), Value::string(self.id.clone()));
        payload.insert("to".to_string(), Value::object(to));
        payload.insert("event".to_string(), Value::string(self.event.clone()));
        payload.insert(
            "reason".to_string(),
            Value::string(self.reason.as_str().to_string()),
        );
        payload.insert(
            "causalId".to_string(),
            Value::string(self.causal_id.clone()),
        );
        Value::object(payload)
    }
}

/// `sha256(from_instance ‖ causal_id ‖ dispatch_seq ‖ emit_ordinal)` in hex, the parts joined by the
/// unit separator (0x1f) so no two tuples share a preimage. Every part is a journaled value, so a replay
/// derives the same id.
pub fn envelope_id(
    from_instance: &str,
    causal_id: &str,
    dispatch_seq: u64,
    emit_ordinal: u32,
) -> String {
    let preimage =
        format!("{from_instance}\u{1f}{causal_id}\u{1f}{dispatch_seq}\u{1f}{emit_ordinal}");
    hex(&sha256(preimage.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_stable_hex_and_changes_with_each_part() {
        let id = envelope_id("api", "c1", 3, 0);
        assert_eq!(id, envelope_id("api", "c1", 3, 0));
        assert_eq!(id.len(), 64);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(id, envelope_id("web", "c1", 3, 0));
        assert_ne!(id, envelope_id("api", "c2", 3, 0));
        assert_ne!(id, envelope_id("api", "c1", 4, 0));
        assert_ne!(id, envelope_id("api", "c1", 3, 1));
        assert_ne!(envelope_id("a", "bc", 1, 0), envelope_id("ab", "c", 1, 0));
    }

    #[test]
    fn refusal_kinds_use_their_wire_spellings() {
        for (kind, word) in [
            (DeliverRefusalKind::MailboxFull, "mailbox-full"),
            (DeliverRefusalKind::HopCap, "hop-cap"),
            (DeliverRefusalKind::UnknownInstance, "unknown-instance"),
            (DeliverRefusalKind::Denied, "denied"),
            (DeliverRefusalKind::Down, "down"),
        ] {
            assert_eq!(kind.as_str(), word);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{word}\""));
        }
    }

    #[test]
    fn an_envelope_round_trips_and_its_verdicts_are_tagged() {
        let envelope = Envelope {
            id: "i".into(),
            from: InstanceRef {
                instance: "api".into(),
                orbital: "A".into(),
                trait_name: Some("T".into()),
            },
            to: InstanceRef {
                instance: "web".into(),
                orbital: "B".into(),
                trait_name: None,
            },
            event: "E".into(),
            payload: Value::Bool(true),
            causal_id: "c".into(),
            hop: 2,
        };
        let text = serde_json::to_string(&envelope).unwrap();
        assert!(
            text.contains("\"causalId\":\"c\"") && text.contains("\"trait\":\"T\""),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<Envelope>(&text).unwrap(), envelope);
        let rejected = EnvelopeVerdict::Rejected(DeliverRefusal {
            kind: DeliverRefusalKind::HopCap,
            id: "i".into(),
        });
        let text = serde_json::to_string(&rejected).unwrap();
        assert_eq!(
            serde_json::from_str::<EnvelopeVerdict>(&text).unwrap(),
            rejected
        );
        assert_eq!(
            serde_json::to_string(&EnvelopeVerdict::Duplicate).unwrap(),
            "{\"verdict\":\"duplicate\"}"
        );
    }
}
