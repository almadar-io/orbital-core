//! The frame delta a response carries (K5, EXEC-DIST-7): `(session, from, to, ops)` against the kernel's client view
//! record. Ops are absolute and idempotent, so a repeat changes nothing; a client applies a delta only at `from`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::value::{Value, ValueMap};
use serde::de::{self, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One absolute change to a client's frames. On the wire an array headed by its name:
/// `["put", frame, {fields}]`, `["set", frame, field, value]`, `["del", frame]`, `["clear"]`.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameOp {
    /// A frame the client does not hold yet, whole.
    Put {
        frame: String,
        fields: ValueMap,
    },
    /// These fields of these frames, absolute; a frame's other fields stay, and a frame never seen is made.
    Set { frames: Vec<(String, ValueMap)> },
    Del {
        frame: String,
    },
    /// Forget every frame (a snapshot starts with it).
    Clear,
}

/// The change a response makes to the frames its client holds. `session` names the record the versions belong to;
/// `None` is an unversioned delta (a request served by a session that keeps no record), applied wherever it lands.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub from: u64,
    pub to: u64,
    pub ops: Vec<FrameOp>,
    /// The session's bound identity (EXEC-IDENTITY-4), present when it changed since the last delta of this record (a
    /// snapshot always carries it); absent leaves the client's mirror as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<FrameIdentity>,
}

/// The `[identity]` row a session is bound to: `user` is its `@user`, absent for the anonymous viewer.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub user: Option<Value>,
}

impl FrameDelta {
    /// An unversioned delta setting these frames' fields.
    pub fn absolute(frames: impl IntoIterator<Item = (String, ValueMap)>) -> Self {
        let ops: Vec<FrameOp> = frames
            .into_iter()
            .fold(Vec::new(), |mut ops, (frame, fields)| {
                FrameOp::push_set(&mut ops, frame, fields);
                ops
            });
        Self {
            session: None,
            from: 0,
            to: 0,
            ops,
            identity: None,
        }
    }

    /// The frames a client holds after applying these ops to `frames`, version aside.
    pub fn fold_into(&self, frames: &mut std::collections::BTreeMap<String, ValueMap>) {
        for op in &self.ops {
            match op {
                FrameOp::Clear => frames.clear(),
                FrameOp::Del { frame } => {
                    frames.remove(frame);
                }
                FrameOp::Put { frame, fields } => {
                    frames.insert(frame.clone(), fields.clone());
                }
                FrameOp::Set { frames: sets } => {
                    for (frame, fields) in sets {
                        frames.entry(frame.clone()).or_default().extend(fields.clone());
                    }
                }
            }
        }
    }

    /// Whether this delta starts from nothing: a snapshot (`from` 0 and a leading `clear`).
    pub fn is_snapshot(&self) -> bool {
        self.from == 0 && matches!(self.ops.first(), Some(FrameOp::Clear))
    }
}

struct SetFrames<'a>(&'a [(String, ValueMap)]);

impl Serialize for SetFrames<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (frame, fields) in self.0 {
            map.serialize_entry(frame, fields)?;
        }
        map.end()
    }
}

impl FrameOp {
    /// Add `fields` of `frame` to the `set` that ends `ops`, or start one.
    pub fn push_set(ops: &mut Vec<FrameOp>, frame: String, fields: ValueMap) {
        if fields.is_empty() {
            return;
        }
        if let Some(FrameOp::Set { frames }) = ops.last_mut() {
            match frames.iter_mut().find(|(held, _)| *held == frame) {
                Some((_, held)) => held.extend(fields),
                None => frames.push((frame, fields)),
            }
        } else {
            ops.push(FrameOp::Set { frames: vec![(frame, fields)] });
        }
    }
}

impl Serialize for FrameOp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            FrameOp::Put { frame, fields } => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element("put")?;
                seq.serialize_element(frame)?;
                seq.serialize_element(fields)?;
                seq.end()
            }
            FrameOp::Set { frames } => {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element("set")?;
                seq.serialize_element(&SetFrames(frames))?;
                seq.end()
            }
            FrameOp::Del { frame } => {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element("del")?;
                seq.serialize_element(frame)?;
                seq.end()
            }
            FrameOp::Clear => {
                let mut seq = serializer.serialize_seq(Some(1))?;
                seq.serialize_element("clear")?;
                seq.end()
            }
        }
    }
}

struct FrameOpVisitor;

impl<'de> Visitor<'de> for FrameOpVisitor {
    type Value = FrameOp;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a frame op: [\"put\", frame, fields], [\"set\", frame, field, value], [\"del\", frame] or [\"clear\"]")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FrameOp, A::Error> {
        fn next<'de, T: Deserialize<'de>, A: SeqAccess<'de>>(
            seq: &mut A,
            what: &str,
        ) -> Result<T, A::Error> {
            seq.next_element()?
                .ok_or_else(|| de::Error::custom(format!("frame op is missing its {what}")))
        }
        let name: String = next(&mut seq, "name")?;
        let op = match name.as_str() {
            "put" => FrameOp::Put {
                frame: next(&mut seq, "frame")?,
                fields: next(&mut seq, "fields")?,
            },
            "set" => FrameOp::Set {
                frames: next::<std::collections::BTreeMap<String, ValueMap>, A>(&mut seq, "frames")?.into_iter().collect(),
            },
            "del" => FrameOp::Del {
                frame: next(&mut seq, "frame")?,
            },
            "clear" => FrameOp::Clear,
            other => return Err(de::Error::custom(format!("unknown frame op `{other}`"))),
        };
        if seq.next_element::<de::IgnoredAny>()?.is_some() {
            return Err(de::Error::custom(format!(
                "frame op `{name}` has extra elements"
            )));
        }
        Ok(op)
    }
}

impl<'de> Deserialize<'de> for FrameOp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(FrameOpVisitor)
    }
}
