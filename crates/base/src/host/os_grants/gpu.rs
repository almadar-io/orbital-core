//! The `gpu` grant: a device bounded in bytes and the kinds of work the program may do on it.

use super::*;

/// What a gpu call does: `open` a device, run `compute` or `render` commands on it, `readback` a buffer to the host.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuKind {
    Open,
    Compute,
    Render,
    Readback,
}

named_kinds!(GpuKind { Open => "open", Compute => "compute", Render => "render", Readback => "readback" });

/// A device and the kinds of work it grants: `device:<maxBytes>`, a device holding at most that many bytes.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuGrant {
    pub target: String,
    pub kinds: Vec<GpuKind>,
}

/// What a `gpu` grant's target names: a device bounded by `max_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuTarget {
    pub max_bytes: u64,
}

/// The device a `gpu` grant target names; `None` for a target that is not `device:<positive decimal>`.
pub fn gpu_target(target: &str) -> Option<GpuTarget> {
    let digits = target.strip_prefix("device:")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<u64>()
        .ok()
        .filter(|max| *max > 0)
        .map(|max_bytes| GpuTarget { max_bytes })
}

impl OsGrants {
    /// Whether some granted device lists `kind`.
    pub fn gpu_allows(&self, kind: GpuKind) -> bool {
        self.gpu
            .iter()
            .any(|g| g.kinds.contains(&kind) && gpu_target(&g.target).is_some())
    }
}
