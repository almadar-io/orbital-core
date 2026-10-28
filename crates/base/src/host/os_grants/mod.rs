//! The program's `os/` grants (`docs/Almadar_OS_Operators.md` §4): closed by default, matched by pure
//! total functions. The native answerer re-checks them after `realpath`; the validator reads the same data.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

#[cfg(feature = "std")]
use crate::os::{OsError, OsErrorKind};
use crate::os::OsOp;
use crate::schema::sexpr::SExpression;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(feature = "std")]
use std::path::{Component, Path, PathBuf};

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FsMode {
    Read,
    Write,
    Create,
    Remove,
    Meta,
}

/// What a `kv/` call does to a store: `read` (get, scan) or `write` (put, delete, batch).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KvMode {
    Read,
    Write,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetKind {
    Tcp,
    Udp,
    Listen,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StdioKind {
    In,
    Out,
    Err,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SysKind {
    Cpu,
    Memory,
    Disk,
    Network,
    PowerState,
    Identity,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesktopKind {
    Notify,
    Speak,
    Brightness,
    Volume,
    Power,
}

macro_rules! named_kinds {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            /// Every kind, in declaration order.
            pub const ALL: &'static [$ty] = &[$($ty::$variant),+];

            /// The `.lolo` / `.orb` spelling.
            pub fn name(self) -> &'static str {
                match self { $($ty::$variant => $name),+ }
            }
        }
    };
}

/// What a device call does: read or write a register in an `mmio` window, wait for or acknowledge an `irq` line.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DevKind {
    Read,
    Write,
    Wait,
    Ack,
}

named_kinds!(FsMode { Read => "read", Write => "write", Create => "create", Remove => "remove", Meta => "meta" });
named_kinds!(KvMode { Read => "read", Write => "write" });
named_kinds!(NetKind { Tcp => "tcp", Udp => "udp", Listen => "listen" });
named_kinds!(DevKind { Read => "read", Write => "write", Wait => "wait", Ack => "ack" });
named_kinds!(StdioKind { In => "in", Out => "out", Err => "err" });
named_kinds!(SysKind {
    Cpu => "cpu",
    Memory => "memory",
    Disk => "disk",
    Network => "network",
    PowerState => "power-state",
    Identity => "identity",
});
named_kinds!(DesktopKind {
    Notify => "notify",
    Speak => "speak",
    Brightness => "brightness",
    Volume => "volume",
    Power => "power",
});

/// One thing an `os/` or `kv/` call needs from the grants, as the operator table declares it (`grant` in the
/// operator table): the access and which positional arguments hold the path, host, store or line. Reading the
/// literal arguments, anchors and glob matching are the kernel's.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum OsGrantNeed {
    Fs { access: FsMode, path: usize },
    FsGlob { glob: usize },
    FsOpen { path: usize, flags: usize },
    Exec { command: usize, options: usize },
    Env { name: usize },
    Stdio { access: StdioKind },
    Net {
        access: Vec<NetKind>,
        host: usize,
        #[serde(default)]
        port: Option<usize>,
    },
    Sys { access: SysKind },
    Desktop { access: DesktopKind },
    Kv { access: KvMode, store: usize },
    Mmio { access: DevKind, base: usize, offset: usize, width: usize },
    Irq { access: DevKind, line: usize },
}

/// The needs the installed operator table declares for `op`; empty for a call on a handle, whose grant was checked
/// where the handle was made.
pub fn os_grant_needs(op: OsOp) -> &'static [OsGrantNeed] {
    crate::language::registry::get_operator(op.name())
        .and_then(|operator| operator.grant.as_deref())
        .unwrap_or_else(|| panic!("the operator table declares no `grant` for `{}`", op.name()))
}

/// A path glob (`**` any number of segments, `*` and `?` within one) and the modes it grants.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsGrant {
    pub glob: String,
    pub modes: Vec<FsMode>,
}

/// An exact `host:port` and the kinds of traffic it grants.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetGrant {
    pub target: String,
    pub kinds: Vec<NetKind>,
}

/// A device resource and the kinds of access it grants: `mmio:<base>+<length>` (a register window) or
/// `irq:<line>`, numbers decimal or `0x` hex.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevGrant {
    pub target: String,
    pub kinds: Vec<DevKind>,
}

/// The device resource a call touches: `width` bytes at `offset` into the window based at `base`, or an irq line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resource", rename_all = "kebab-case")]
pub enum DevResource {
    Mmio { base: u64, offset: u64, width: u64 },
    Irq { line: u64 },
}

/// What a `dev` grant's target names: a register window or an interrupt line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevTarget {
    Mmio { base: u64, length: u64 },
    Irq { line: u64 },
}

fn dev_number(text: &str) -> Option<u64> {
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// The window or line a `dev` grant target names; `None` for a target that names neither.
pub fn dev_target(target: &str) -> Option<DevTarget> {
    if let Some(window) = target.strip_prefix("mmio:") {
        let (base, length) = window.split_once('+')?;
        let (base, length) = (dev_number(base)?, dev_number(length)?);
        (length > 0 && base.checked_add(length).is_some()).then_some(DevTarget::Mmio { base, length })
    } else {
        target.strip_prefix("irq:").and_then(dev_number).map(|line| DevTarget::Irq { line })
    }
}

/// Where an anchored grant or path is rooted, resolved once when the host boots: the directory of the running
/// executable, the directory the program was invoked in, or the value of an environment variable, which must be an
/// absolute path.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantAnchor {
    ExeDir,
    Cwd,
    Env(String),
}

impl GrantAnchor {
    /// The `.lolo` spelling: `exe-dir` or `env NAME`.
    pub fn label(&self) -> String {
        match self {
            GrantAnchor::ExeDir => "exe-dir".to_string(),
            GrantAnchor::Cwd => "cwd".to_string(),
            GrantAnchor::Env(name) => format!("env {name}"),
        }
    }
}

/// A glob relative to its anchor and the modes it grants.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchoredFsGrant {
    pub anchor: GrantAnchor,
    pub glob: String,
    pub modes: Vec<FsMode>,
}

/// A command at an anchor: `path` under it, or with no `path` the anchor's own value (an `env` anchor naming the
/// executable itself).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchoredExec {
    pub anchor: GrantAnchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// One half of an anchored `net` target: a literal host or port, or an `env` anchor whose boot value is it.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetPart {
    Literal(String),
    Anchor(GrantAnchor),
}

impl NetPart {
    pub fn label(&self) -> String {
        match self {
            NetPart::Literal(text) => format!("{text:?}"),
            NetPart::Anchor(anchor) => anchor.label(),
        }
    }
}

/// A `net` target whose host, port or both come from an anchor, and the kinds of traffic it grants.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchoredNetGrant {
    pub host: NetPart,
    pub port: NetPart,
    pub kinds: Vec<NetKind>,
}

impl AnchoredNetGrant {
    fn label(&self) -> String {
        format!("{}, {}", self.host.label(), self.port.label())
    }
}

/// The `fs`, `exec` and `net` entries rooted at an anchor. They match nothing until a host resolves them
/// ([`OsGrants::resolve_anchors`]) into plain entries.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnchoredGrants {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<AnchoredFsGrant>>"))]
    pub fs: Vec<AnchoredFsGrant>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<AnchoredExec>>"))]
    pub exec: Vec<AnchoredExec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<AnchoredNetGrant>>"))]
    pub net: Vec<AnchoredNetGrant>,
}

impl AnchoredGrants {
    pub fn is_empty(&self) -> bool {
        self.fs.is_empty() && self.exec.is_empty() && self.net.is_empty()
    }
}

/// Every grant a program declares; the default grants nothing.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OsGrants {
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<FsGrant>>"))]
    pub fs: Vec<FsGrant>,
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<String>>"))]
    pub exec: Vec<String>,
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<NetGrant>>"))]
    pub net: Vec<NetGrant>,
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<String>>"))]
    pub env: Vec<String>,
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<StdioKind>>"))]
    pub stdio: Vec<StdioKind>,
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<SysKind>>"))]
    pub sys: Vec<SysKind>,
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<DesktopKind>>"))]
    pub desktop: Vec<DesktopKind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<DevGrant>>"))]
    pub dev: Vec<DevGrant>,
    /// `kv: { <store>: [read, write] }`: a store name, or a `*`/`?` glob over one, and the modes it grants.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<BTreeMap<String, Vec<KvMode>>>"))]
    pub kv: BTreeMap<String, Vec<KvMode>>,
    #[serde(skip_serializing_if = "AnchoredGrants::is_empty")]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<AnchoredGrants>"))]
    pub anchored: AnchoredGrants,
}

mod matching;
mod anchors;
mod entries;
mod requirements;
use matching::*;
use anchors::*;
pub use anchors::{AnchoredPath, EntryProblem, EnvSnapshot, GrantAtom, OsAnchors};
pub use requirements::{
    anchored_arg_positions, anchored_net_args, concat_static_prefix, os_requirements,
    os_requirements_deferring, resolve_anchored_args, OsRequirement,
};

#[cfg(test)]
mod tests;
