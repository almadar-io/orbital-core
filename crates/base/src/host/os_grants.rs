//! The program's `os/` grants (`docs/Almadar_OS_Operators.md` §4): closed by default, matched by pure
//! total functions. The native answerer re-checks them after `realpath`; the validator reads the same data.

use crate::os::{OsError, OsErrorKind, OsOp};
use crate::schema::sexpr::SExpression;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KvMode {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetKind {
    Tcp,
    Udp,
    Listen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StdioKind {
    In,
    Out,
    Err,
}

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

/// A path glob (`**` any number of segments, `*` and `?` within one) and the modes it grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsGrant {
    pub glob: String,
    pub modes: Vec<FsMode>,
}

/// An exact `host:port` and the kinds of traffic it grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetGrant {
    pub target: String,
    pub kinds: Vec<NetKind>,
}

/// A device resource and the kinds of access it grants: `mmio:<base>+<length>` (a register window) or
/// `irq:<line>`, numbers decimal or `0x` hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

enum DevTarget {
    Mmio { base: u64, length: u64 },
    Irq { line: u64 },
}

fn dev_number(text: &str) -> Option<u64> {
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn dev_target(target: &str) -> Option<DevTarget> {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchoredFsGrant {
    pub anchor: GrantAnchor,
    pub glob: String,
    pub modes: Vec<FsMode>,
}

/// A command at an anchor: `path` under it, or with no `path` the anchor's own value (an `env` anchor naming the
/// executable itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchoredExec {
    pub anchor: GrantAnchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// One half of an anchored `net` target: a literal host or port, or an `env` anchor whose boot value is it.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnchoredGrants {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fs: Vec<AnchoredFsGrant>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exec: Vec<AnchoredExec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<AnchoredNetGrant>,
}

impl AnchoredGrants {
    pub fn is_empty(&self) -> bool {
        self.fs.is_empty() && self.exec.is_empty() && self.net.is_empty()
    }
}

/// Every grant a program declares; the default grants nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OsGrants {
    pub fs: Vec<FsGrant>,
    pub exec: Vec<String>,
    pub net: Vec<NetGrant>,
    pub env: Vec<String>,
    pub stdio: Vec<StdioKind>,
    pub sys: Vec<SysKind>,
    pub desktop: Vec<DesktopKind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dev: Vec<DevGrant>,
    /// `kv: { <store>: [read, write] }`: a store name, or a `*`/`?` glob over one, and the modes it grants.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub kv: BTreeMap<String, Vec<KvMode>>,
    #[serde(skip_serializing_if = "AnchoredGrants::is_empty")]
    pub anchored: AnchoredGrants,
}

/// The absolute path as its `/`-separated segments with `.` dropped and `..` collapsed; `None` for a
/// relative path, a non-UTF-8 segment, a platform prefix, or a `..` that climbs above the root.
fn normalized_segments(path: &Path) -> Option<Vec<String>> {
    let mut segments: Vec<String> = Vec::new();
    let mut rooted = false;
    for component in path.components() {
        match component {
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                segments.pop()?;
            }
            Component::Normal(part) => segments.push(part.to_str()?.to_string()),
            Component::Prefix(_) => return None,
        }
    }
    rooted.then_some(segments)
}

fn segment_matches(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|skip| segment_matches(rest, &text[skip..])),
        Some(('?', rest)) => text
            .split_first()
            .is_some_and(|(_, tail)| segment_matches(rest, tail)),
        Some((c, rest)) => text
            .split_first()
            .is_some_and(|(t, tail)| t == c && segment_matches(rest, tail)),
    }
}

fn segments_match(pattern: &[&str], path: &[String]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((segment, rest)) => path.split_first().is_some_and(|(head, tail)| {
            let pattern_chars: Vec<char> = segment.chars().collect();
            let text_chars: Vec<char> = head.chars().collect();
            segment_matches(&pattern_chars, &text_chars) && segments_match(rest, tail)
        }),
    }
}

/// Where a grant glob or a path is rooted: the filesystem root, the user's home (`~/`) or the directory the host
/// started the program in (`./`). An anchored form matches only a form under the same anchor; a host checks an
/// absolute path against anchored grants through its [`OsAnchors`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Root,
    Home,
    Start,
}

/// The anchor of `text` and the part after it; `None` for a bare relative path.
fn anchor_of(text: &str) -> Option<(Anchor, &str)> {
    if let Some(rest) = text.strip_prefix("~/") {
        Some((Anchor::Home, rest))
    } else if text == "~" {
        Some((Anchor::Home, ""))
    } else if let Some(rest) = text.strip_prefix("./") {
        Some((Anchor::Start, rest))
    } else if text == "." {
        Some((Anchor::Start, ""))
    } else if text.starts_with('/') {
        Some((Anchor::Root, text))
    } else {
        None
    }
}

/// [`normalized_segments`] for any anchored form: a home or start form is normalized under its anchor and refused
/// when a `..` climbs above it.
fn anchored_segments(path: &Path) -> Option<(Anchor, Vec<String>)> {
    let text = path.to_str()?;
    match anchor_of(text)? {
        (Anchor::Root, _) => Some((Anchor::Root, normalized_segments(path)?)),
        (anchor, rest) => {
            let mut segments: Vec<String> = Vec::new();
            for component in Path::new(rest).components() {
                match component {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        segments.pop()?;
                    }
                    Component::Normal(part) => segments.push(part.to_str()?.to_string()),
                    Component::RootDir | Component::Prefix(_) => return None,
                }
            }
            Some((anchor, segments))
        }
    }
}

/// Whether `glob` (anchored, no `..`) matches the already-normalized `path` under `anchor`.
fn glob_matches(glob: &str, anchor: Anchor, path: &[String]) -> bool {
    let Some((glob_anchor, rest)) = anchor_of(glob) else {
        return false;
    };
    if glob_anchor != anchor {
        return false;
    }
    let pattern: Vec<&str> = rest
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if pattern.contains(&"..") {
        return false;
    }
    segments_match(&pattern, path)
}

/// Whether the anchor-relative `glob` (no `..`) matches the already-normalized relative `path`.
fn relative_glob_matches(glob: &str, path: &[String]) -> bool {
    if !valid_relative(glob) {
        return false;
    }
    let pattern: Vec<&str> = glob.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    segments_match(&pattern, path)
}

/// Whether the anchor-relative `glob` (no `..`) can match some path strictly under the normalized directory `root`.
/// A computed path under `root` is then accepted statically and the host checks the glob on the resolved path.
fn relative_glob_reaches_under(glob: &str, root: &[String]) -> bool {
    if !valid_relative(glob) {
        return false;
    }
    let mut pattern: &[&str] = &glob.split('/').filter(|s| !s.is_empty() && *s != ".").collect::<Vec<_>>();
    for segment in root {
        match pattern.split_first() {
            Some((&"**", _)) => return true,
            Some((head, rest)) if segment_matches(&head.chars().collect::<Vec<_>>(), &segment.chars().collect::<Vec<_>>()) => pattern = rest,
            _ => return false,
        }
    }
    !pattern.is_empty()
}

fn split_target(target: &str) -> Option<(&str, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then_some(())?;
    Some((host, port.parse().ok()?))
}

fn extend_missing<T: Clone + PartialEq>(into: &mut Vec<T>, from: &[T]) {
    for item in from {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

impl OsGrants {
    pub fn is_empty(&self) -> bool {
        self == &OsGrants::default()
    }

    /// Adds every entry of `other` this set does not already hold; a glob or target already present gains
    /// the other entry's modes or kinds.
    pub fn union(&mut self, other: &OsGrants) {
        for grant in &other.fs {
            match self.fs.iter_mut().find(|g| g.glob == grant.glob) {
                Some(held) => extend_missing(&mut held.modes, &grant.modes),
                None => self.fs.push(grant.clone()),
            }
        }
        for grant in &other.net {
            match self.net.iter_mut().find(|g| g.target == grant.target) {
                Some(held) => extend_missing(&mut held.kinds, &grant.kinds),
                None => self.net.push(grant.clone()),
            }
        }
        for grant in &other.dev {
            match self.dev.iter_mut().find(|g| g.target == grant.target) {
                Some(held) => extend_missing(&mut held.kinds, &grant.kinds),
                None => self.dev.push(grant.clone()),
            }
        }
        for grant in &other.anchored.fs {
            match self
                .anchored
                .fs
                .iter_mut()
                .find(|g| g.anchor == grant.anchor && g.glob == grant.glob)
            {
                Some(held) => extend_missing(&mut held.modes, &grant.modes),
                None => self.anchored.fs.push(grant.clone()),
            }
        }
        extend_missing(&mut self.anchored.exec, &other.anchored.exec);
        for grant in &other.anchored.net {
            match self
                .anchored
                .net
                .iter_mut()
                .find(|g| g.host == grant.host && g.port == grant.port)
            {
                Some(held) => extend_missing(&mut held.kinds, &grant.kinds),
                None => self.anchored.net.push(grant.clone()),
            }
        }
        extend_missing(&mut self.exec, &other.exec);
        extend_missing(&mut self.env, &other.env);
        extend_missing(&mut self.stdio, &other.stdio);
        extend_missing(&mut self.sys, &other.sys);
        extend_missing(&mut self.desktop, &other.desktop);
        for (store, modes) in &other.kv {
            extend_missing(self.kv.entry(store.clone()).or_default(), modes);
        }
    }

    /// Whether `path` may be used in `mode`. The path is matched lexically (`.`/`..` collapsed, refused when
    /// it escapes the root); symlinks are resolved by the caller, which re-checks the real path.
    pub fn fs_allows(&self, path: &Path, mode: FsMode) -> bool {
        let Some((anchor, segments)) = anchored_segments(path) else {
            return false;
        };
        self.fs
            .iter()
            .any(|g| g.modes.contains(&mode) && glob_matches(&g.glob, anchor, &segments))
    }

    /// [`Self::fs_allows`] for a host holding an absolute `path`: also matched in its `~/` and `./` forms when it lies
    /// under the home or start directory the host knows, so an anchored grant covers it.
    pub fn fs_allows_at(&self, path: &Path, mode: FsMode, anchors: &OsAnchors) -> bool {
        anchors.forms(path).iter().any(|form| self.fs_allows(form, mode))
    }

    /// [`Self::fs_tree_allows`] for a host holding an absolute `root`, as [`Self::fs_allows_at`].
    pub fn fs_tree_allows_at(&self, root: &Path, mode: FsMode, anchors: &OsAnchors) -> bool {
        anchors.forms(root).iter().any(|form| self.fs_tree_allows(form, mode))
    }

    /// Whether every path under the directory `root` may be used in `mode`: a grant that lists the mode, ends in
    /// `/**` and matches `root` itself covers `root` and all it contains. Matched lexically like [`fs_allows`].
    pub fn fs_tree_allows(&self, root: &Path, mode: FsMode) -> bool {
        let Some((anchor, segments)) = anchored_segments(root) else {
            return false;
        };
        self.fs.iter().any(|g| {
            g.modes.contains(&mode) && g.glob.ends_with("/**") && glob_matches(&g.glob, anchor, &segments)
        })
    }

    /// The one-grant set that matches exactly what `glob` matches, for reading.
    pub fn read_matcher(glob: &str) -> OsGrants {
        OsGrants {
            fs: vec![FsGrant { glob: glob.to_string(), modes: vec![FsMode::Read] }],
            ..OsGrants::default()
        }
    }

    pub fn exec_allows(&self, command: &str) -> bool {
        self.exec.iter().any(|c| c == command)
    }

    pub fn net_allows(&self, host: &str, port: u16, kind: NetKind) -> bool {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        self.net.iter().any(|g| {
            g.kinds.contains(&kind)
                && split_target(&g.target)
                    .is_some_and(|(h, p)| p == port && h.eq_ignore_ascii_case(host))
        })
    }

    pub fn env_allows(&self, name: &str) -> bool {
        self.env.iter().any(|n| n == name)
    }

    pub fn stdio_allows(&self, kind: StdioKind) -> bool {
        self.stdio.contains(&kind)
    }

    pub fn sys_allows(&self, kind: SysKind) -> bool {
        self.sys.contains(&kind)
    }

    pub fn desktop_allows(&self, kind: DesktopKind) -> bool {
        self.desktop.contains(&kind)
    }

    /// Whether `resource` may be used for `kind`: a register access lies wholly inside a granted window, an irq line
    /// is granted exactly.
    pub fn dev_allows(&self, resource: &DevResource, kind: DevKind) -> bool {
        self.dev.iter().any(|g| {
            g.kinds.contains(&kind)
                && match (dev_target(&g.target), resource) {
                    (Some(DevTarget::Mmio { base, length }), DevResource::Mmio { base: at, offset, width }) => {
                        matches!(kind, DevKind::Read | DevKind::Write)
                            && at.checked_add(*offset).and_then(|start| start.checked_add(*width)).is_some_and(|end| {
                                *at + *offset >= base && end <= base + length
                            })
                    }
                    (Some(DevTarget::Irq { line }), DevResource::Irq { line: asked }) => {
                        matches!(kind, DevKind::Wait | DevKind::Ack) && line == *asked
                    }
                    _ => false,
                }
        })
    }

    /// Whether the kv `store` may be used in `mode`: an entry naming it exactly or by a one-segment glob.
    pub fn kv_allows(&self, store: &str, mode: KvMode) -> bool {
        let text: Vec<char> = store.chars().collect();
        self.kv.iter().any(|(glob, modes)| {
            let pattern: Vec<char> = glob.chars().collect();
            modes.contains(&mode) && segment_matches(&pattern, &text)
        })
    }
}


/// One declared entry (a glob with one mode, a target with one kind, a command, …) as a grant set of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantAtom {
    /// Where it sits in the set, e.g. `fs[0]`.
    pub path: String,
    /// How it reads in a message, e.g. `fs "/data/**" write`.
    pub label: String,
    pub grants: OsGrants,
}

/// A declared entry whose shape cannot match anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryProblem {
    pub path: String,
    pub message: String,
}

fn valid_glob(glob: &str) -> bool {
    anchor_of(glob).is_some() && !glob.split('/').any(|s| s == "..")
}

/// A grant-relative path: not anchored itself and never climbing with `..`.
fn valid_relative(text: &str) -> bool {
    !text.is_empty() && anchor_of(text).is_none() && !text.split('/').any(|s| s == "..")
}

/// The process environment as it was when the host booted, so a variable the program sets later never moves an
/// anchored grant. Its `Debug` names the variables only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EnvSnapshot(std::collections::BTreeMap<String, String>);

impl std::fmt::Debug for EnvSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl EnvSnapshot {
    /// Every variable of this process whose name and value are UTF-8.
    pub fn of_process() -> Self {
        Self::from_pairs(
            std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
        )
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(pairs.into_iter().collect())
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

/// The directories a host anchors `~/` and `./` to (the user's home and the directory it started the program in),
/// and what a grant's declared anchors resolve to: the running executable's directory and the boot environment.
/// A host without one of them grants nothing under that anchor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsAnchors {
    pub home: Option<PathBuf>,
    pub start: Option<PathBuf>,
    pub exe_dir: Option<PathBuf>,
    pub env: EnvSnapshot,
}

impl OsAnchors {
    /// The process's own: `HOME` (`USERPROFILE` on Windows), its working directory, the directory of its executable
    /// and its environment, read once.
    pub fn of_process() -> Self {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
        Self {
            home,
            start: std::env::current_dir().ok(),
            exe_dir: std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)),
            env: EnvSnapshot::of_process(),
        }
    }

    /// The `/`-rooted segments of the directory `anchor` names on this host, or why it names none: an unknown
    /// executable directory, an unset or empty variable, a value that is not an absolute path, or one holding a glob
    /// character (`*`, `?`), which would widen the grant.
    pub fn anchor_segments(&self, anchor: &GrantAnchor) -> Result<Vec<String>, String> {
        let dir = match anchor {
            GrantAnchor::ExeDir => self
                .exe_dir
                .clone()
                .ok_or_else(|| "this host does not know the directory of its executable".to_string())?,
            GrantAnchor::Cwd => self
                .start
                .clone()
                .ok_or_else(|| "this host does not know the directory the program was invoked in".to_string())?,
            GrantAnchor::Env(name) => match self.env.get(name) {
                None | Some("") => return Err(format!("env {name} is unset")),
                Some(value) if !Path::new(value).is_absolute() => {
                    return Err(format!("env {name} is {value:?}, which is not an absolute path"))
                }
                Some(value) => PathBuf::from(value),
            },
        };
        let segments = normalized_segments(&dir)
            .ok_or_else(|| format!("{} ({}) is not an absolute UTF-8 path", anchor.label(), dir.display()))?;
        if segments.iter().any(|s| s.contains(['*', '?'])) {
            return Err(format!("{} ({}) holds a glob character", anchor.label(), dir.display()));
        }
        Ok(segments)
    }

    /// The absolute path a `~/` or `./` form names; `None` for any other form (an absolute path, or a bare relative
    /// one, which keeps the working-directory rule). An anchor this host does not know is an error.
    pub fn expand(&self, text: &str) -> Result<Option<PathBuf>, String> {
        let (anchor, rest) = match anchor_of(text) {
            Some((Anchor::Home, rest)) => (&self.home, rest),
            Some((Anchor::Start, rest)) => (&self.start, rest),
            _ => return Ok(None),
        };
        match anchor {
            Some(dir) => Ok(Some(if rest.is_empty() { dir.clone() } else { dir.join(rest) })),
            None => Err(format!("{text}: this host has no {} directory", if text.starts_with('~') { "home" } else { "start" })),
        }
    }

    /// `path` and, when it lies under the home or start directory, its `~/` and `./` forms.
    fn forms(&self, path: &Path) -> Vec<PathBuf> {
        let mut forms = vec![path.to_path_buf()];
        let under = |dir: &Option<PathBuf>, prefix: &str| {
            let dir = dir.as_ref()?;
            let rest = path.strip_prefix(dir).ok()?;
            Some(PathBuf::from(format!("{prefix}{}", rest.display())))
        };
        forms.extend(under(&self.home, "~/"));
        forms.extend(under(&self.start, "./"));
        forms
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn rooted(segments: &[String], rest: Option<&str>) -> String {
    let mut out = format!("/{}", segments.join("/"));
    if let Some(rest) = rest {
        if !segments.is_empty() {
            out.push('/');
        }
        out.push_str(rest);
    }
    out
}

impl OsGrants {
    /// The plain grant set a host checks against: every anchored entry resolved through `anchors` into an absolute
    /// `fs` glob or `exec` command, with the anchored part emptied. An entry whose anchor this host cannot place, or
    /// whose shape is invalid, grants nothing and is returned as a problem for the host to report.
    pub fn resolve_anchors(&self, anchors: &OsAnchors) -> (OsGrants, Vec<EntryProblem>) {
        let mut problems = Vec::new();
        let mut added = OsGrants::default();
        for (i, g) in self.anchored.fs.iter().enumerate() {
            let path = format!("anchored.fs[{i}]");
            if !valid_relative(&g.glob) {
                problems.push(EntryProblem { path, message: format!("{} {:?} is not a relative glob without `..`", g.anchor.label(), g.glob) });
                continue;
            }
            match anchors.anchor_segments(&g.anchor) {
                Ok(segments) => added.fs.push(FsGrant { glob: rooted(&segments, Some(&g.glob)), modes: g.modes.clone() }),
                Err(why) => problems.push(EntryProblem { path, message: format!("{} {:?} grants nothing: {why}", g.anchor.label(), g.glob) }),
            }
        }
        for (i, e) in self.anchored.exec.iter().enumerate() {
            let path = format!("anchored.exec[{i}]");
            if let Some(message) = anchored_exec_problem(e) {
                problems.push(EntryProblem { path, message });
                continue;
            }
            match anchors.anchor_segments(&e.anchor) {
                Ok(segments) => added.exec.push(rooted(&segments, e.path.as_deref())),
                Err(why) => problems.push(EntryProblem { path, message: format!("exec {} grants nothing: {why}", anchored_exec_label(e)) }),
            }
        }
        for (i, g) in self.anchored.net.iter().enumerate() {
            let path = format!("anchored.net[{i}]");
            if let Some(message) = anchored_net_problem(g) {
                problems.push(EntryProblem { path, message });
                continue;
            }
            match (anchors.net_part(&g.host), anchors.net_part(&g.port)) {
                (Ok(host), Ok(port)) => match port.parse::<u16>() {
                    Ok(port) => added.net.push(NetGrant { target: net_target(&host, port), kinds: g.kinds.clone() }),
                    Err(_) => problems.push(EntryProblem {
                        path,
                        message: format!("net {} grants nothing: the port {port:?} is not a port number", g.label()),
                    }),
                },
                (Err(why), _) | (_, Err(why)) => {
                    problems.push(EntryProblem { path, message: format!("net {} grants nothing: {why}", g.label()) })
                }
            }
        }
        let mut resolved = OsGrants { anchored: AnchoredGrants::default(), ..self.clone() };
        resolved.union(&added);
        (resolved, problems)
    }
}

/// An `os/` path or command argument written against an anchor, `{ anchor: "cwd", path: "docs/a.md" }`: `path` is
/// relative to the anchor and may not climb above it; without `path` it names the anchor itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredPath {
    pub anchor: GrantAnchor,
    pub path: Option<String>,
}

/// The relative segments of `path` with `.` dropped and `..` collapsed; `None` for an absolute or `~/` form or a
/// `..` that climbs above the anchor. `.` and `./a` name the anchor and `a` under it.
fn relative_segments(path: &str) -> Option<Vec<String>> {
    if path.starts_with('/') || path == "~" || path.starts_with("~/") {
        return None;
    }
    let mut segments: Vec<String> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other.to_string()),
        }
    }
    Some(segments)
}

fn anchor_from_value(value: &Value) -> Option<GrantAnchor> {
    match value {
        Value::String(word) if word == "exe-dir" => Some(GrantAnchor::ExeDir),
        Value::String(word) if word == "cwd" => Some(GrantAnchor::Cwd),
        Value::Object(map) if map.len() == 1 => match map.get("env") {
            Some(Value::String(name)) => Some(GrantAnchor::Env(name.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn anchor_from_sexpr(expr: &SExpression) -> Option<GrantAnchor> {
    match expr {
        SExpression::String(word) if word == "exe-dir" => Some(GrantAnchor::ExeDir),
        SExpression::String(word) if word == "cwd" => Some(GrantAnchor::Cwd),
        SExpression::Object(map) if map.len() == 1 => match map.get("env") {
            Some(SExpression::String(name)) if !name.starts_with('@') => Some(GrantAnchor::Env(name.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `object` has exactly the keys of an anchored path: `anchor` and, optionally, `path`.
fn anchored_keys<'a>(mut keys: impl Iterator<Item = &'a String>) -> bool {
    keys.all(|k| k == "anchor" || k == "path")
}

impl AnchoredPath {
    /// The evaluated argument a host receives; `None` when it is not an anchored path.
    pub fn from_value(value: &Value) -> Option<Self> {
        let Value::Object(map) = value else { return None };
        if !anchored_keys(map.keys()) {
            return None;
        }
        let anchor = anchor_from_value(map.get("anchor")?)?;
        let path = match map.get("path") {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) => Some(p.clone()),
            Some(_) => return None,
        };
        Some(Self { anchor, path })
    }
}

/// An argument written as an anchored path: its literal anchor and its `path` expression (absent for the anchor
/// itself). `Ok(None)` when the argument is not an object of that shape; `Err(())` when it is one whose anchor is
/// not a literal anchor.
fn anchored_sexpr(expr: &SExpression) -> Result<Option<(GrantAnchor, Option<&SExpression>)>, ()> {
    let SExpression::Object(map) = expr else { return Ok(None) };
    if !map.contains_key("anchor") || !anchored_keys(map.keys()) {
        return Ok(None);
    }
    let anchor = map.get("anchor").and_then(anchor_from_sexpr).ok_or(())?;
    Ok(Some((anchor, map.get("path").filter(|p| !matches!(p, SExpression::Null)))))
}

impl OsAnchors {
    /// The absolute path an anchored path names on this host, or why it names none.
    pub fn resolve_path(&self, anchored: &AnchoredPath) -> Result<String, String> {
        let base = self.anchor_segments(&anchored.anchor)?;
        let rest = match &anchored.path {
            None => Vec::new(),
            Some(path) => relative_segments(path).ok_or_else(|| {
                format!("{path:?} is not a path relative to {} that stays under it", anchored.anchor.label())
            })?,
        };
        let mut segments = base;
        segments.extend(rest);
        Ok(rooted(&segments, None))
    }
}

fn net_target(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn anchored_net_problem(g: &AnchoredNetGrant) -> Option<String> {
    let part = |p: &NetPart| match p {
        NetPart::Anchor(GrantAnchor::Env(_)) => None,
        NetPart::Anchor(other) => Some(format!("net {} takes only `env` anchors, not {}", g.label(), other.label())),
        NetPart::Literal(text) if text.is_empty() => Some(format!("net {} has an empty part", g.label())),
        NetPart::Literal(_) => None,
    };
    if matches!((&g.host, &g.port), (NetPart::Literal(_), NetPart::Literal(_))) {
        return Some(format!("net {} anchors nothing: write it as \"host:port\"", g.label()));
    }
    if let NetPart::Literal(port) = &g.port {
        if port.parse::<u16>().is_err() {
            return Some(format!("net {} has a port that is not a port number", g.label()));
        }
    }
    part(&g.host).or_else(|| part(&g.port)).or_else(|| g.kinds.is_empty().then(|| format!("net {} grants no kind", g.label())))
}

impl OsAnchors {
    /// The text a net part names on this host: the literal, or the boot value of its `env` anchor.
    pub fn net_part(&self, part: &NetPart) -> Result<String, String> {
        match part {
            NetPart::Literal(text) => Ok(text.clone()),
            NetPart::Anchor(GrantAnchor::Env(name)) => match self.env.get(name) {
                None | Some("") => Err(format!("env {name} is unset")),
                Some(value) => Ok(value.trim_start_matches('[').trim_end_matches(']').to_string()),
            },
            NetPart::Anchor(other) => Err(format!("{} is not a net anchor", other.label())),
        }
    }
}

fn anchored_exec_label(e: &AnchoredExec) -> String {
    match &e.path {
        Some(path) => format!("{} {path:?}", e.anchor.label()),
        None => e.anchor.label(),
    }
}

fn anchored_exec_problem(e: &AnchoredExec) -> Option<String> {
    match (&e.anchor, &e.path) {
        (GrantAnchor::ExeDir, None) => Some("exec exe-dir names a directory, not a command: give the command under it, e.g. exe-dir \"bun\"".to_string()),
        (_, Some(path)) if !valid_relative(path) || path.contains(['*', '?']) => {
            Some(format!("exec {} is not a relative command path without `..` or glob characters", anchored_exec_label(e)))
        }
        _ => None,
    }
}

impl OsGrants {
    /// Every entry split to one mode / kind each, each as a grant set of its own.
    pub fn atoms(&self) -> Vec<GrantAtom> {
        let mut out = Vec::new();
        for (i, g) in self.fs.iter().enumerate() {
            for mode in &g.modes {
                out.push(GrantAtom {
                    path: format!("fs[{i}]"),
                    label: format!("fs {:?} {}", g.glob, mode.name()),
                    grants: OsGrants {
                        fs: vec![FsGrant { glob: g.glob.clone(), modes: vec![*mode] }],
                        ..OsGrants::default()
                    },
                });
            }
        }
        for (i, command) in self.exec.iter().enumerate() {
            out.push(GrantAtom {
                path: format!("exec[{i}]"),
                label: format!("exec {command:?}"),
                grants: OsGrants { exec: vec![command.clone()], ..OsGrants::default() },
            });
        }
        for (i, g) in self.net.iter().enumerate() {
            for kind in &g.kinds {
                out.push(GrantAtom {
                    path: format!("net[{i}]"),
                    label: format!("net {:?} {}", g.target, kind.name()),
                    grants: OsGrants {
                        net: vec![NetGrant { target: g.target.clone(), kinds: vec![*kind] }],
                        ..OsGrants::default()
                    },
                });
            }
        }
        for (i, name) in self.env.iter().enumerate() {
            out.push(GrantAtom {
                path: format!("env[{i}]"),
                label: format!("env {name:?}"),
                grants: OsGrants { env: vec![name.clone()], ..OsGrants::default() },
            });
        }
        for (i, kind) in self.stdio.iter().enumerate() {
            out.push(GrantAtom {
                path: format!("stdio[{i}]"),
                label: format!("stdio {}", kind.name()),
                grants: OsGrants { stdio: vec![*kind], ..OsGrants::default() },
            });
        }
        for (i, kind) in self.sys.iter().enumerate() {
            out.push(GrantAtom {
                path: format!("sys[{i}]"),
                label: format!("sys {}", kind.name()),
                grants: OsGrants { sys: vec![*kind], ..OsGrants::default() },
            });
        }
        for (i, kind) in self.desktop.iter().enumerate() {
            out.push(GrantAtom {
                path: format!("desktop[{i}]"),
                label: format!("desktop {}", kind.name()),
                grants: OsGrants { desktop: vec![*kind], ..OsGrants::default() },
            });
        }
        for (i, g) in self.dev.iter().enumerate() {
            for kind in &g.kinds {
                out.push(GrantAtom {
                    path: format!("dev[{i}]"),
                    label: format!("dev {:?} {}", g.target, kind.name()),
                    grants: OsGrants {
                        dev: vec![DevGrant { target: g.target.clone(), kinds: vec![*kind] }],
                        ..OsGrants::default()
                    },
                });
            }
        }
        for (store, modes) in &self.kv {
            for mode in modes {
                out.push(GrantAtom {
                    path: format!("kv[{store:?}]"),
                    label: format!("kv {store:?} {}", mode.name()),
                    grants: OsGrants { kv: BTreeMap::from([(store.clone(), vec![*mode])]), ..OsGrants::default() },
                });
            }
        }
        for (i, g) in self.anchored.fs.iter().enumerate() {
            for mode in &g.modes {
                out.push(GrantAtom {
                    path: format!("anchored.fs[{i}]"),
                    label: format!("fs {} {:?} {}", g.anchor.label(), g.glob, mode.name()),
                    grants: OsGrants {
                        anchored: AnchoredGrants {
                            fs: vec![AnchoredFsGrant { anchor: g.anchor.clone(), glob: g.glob.clone(), modes: vec![*mode] }],
                            ..AnchoredGrants::default()
                        },
                        ..OsGrants::default()
                    },
                });
            }
        }
        for (i, g) in self.anchored.net.iter().enumerate() {
            for kind in &g.kinds {
                out.push(GrantAtom {
                    path: format!("anchored.net[{i}]"),
                    label: format!("net {} {}", g.label(), kind.name()),
                    grants: OsGrants {
                        anchored: AnchoredGrants {
                            net: vec![AnchoredNetGrant { host: g.host.clone(), port: g.port.clone(), kinds: vec![*kind] }],
                            ..AnchoredGrants::default()
                        },
                        ..OsGrants::default()
                    },
                });
            }
        }
        for (i, e) in self.anchored.exec.iter().enumerate() {
            out.push(GrantAtom {
                path: format!("anchored.exec[{i}]"),
                label: format!("exec {}", anchored_exec_label(e)),
                grants: OsGrants {
                    anchored: AnchoredGrants { exec: vec![e.clone()], ..AnchoredGrants::default() },
                    ..OsGrants::default()
                },
            });
        }
        out
    }

    /// The entries that cannot match anything: a glob that is not absolute or climbs with `..`, a command
    /// or variable name that is empty or holds whitespace, a `net` target that is not `host:port`, an
    /// entry with no modes or kinds.
    pub fn entry_problems(&self) -> Vec<EntryProblem> {
        let mut out = Vec::new();
        let mut problem = |path: String, message: String| out.push(EntryProblem { path, message });
        for (i, g) in self.fs.iter().enumerate() {
            if !valid_glob(&g.glob) {
                problem(format!("fs[{i}]"), format!("{:?} is not an absolute glob without `..`", g.glob));
            }
            if g.modes.is_empty() {
                problem(format!("fs[{i}]"), format!("{:?} grants no mode", g.glob));
            }
        }
        for (store, modes) in &self.kv {
            if store.is_empty() || !store.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '*' | '?')) {
                problem(format!("kv[{store:?}]"), format!("{store:?} is not a store name or a glob over one"));
            }
            if modes.is_empty() {
                problem(format!("kv[{store:?}]"), format!("{store:?} grants no mode"));
            }
        }
        for (i, command) in self.exec.iter().enumerate() {
            if !valid_name(command) {
                problem(format!("exec[{i}]"), format!("{command:?} is not one command name"));
            }
        }
        for (i, g) in self.net.iter().enumerate() {
            if split_target(&g.target).is_none() {
                problem(format!("net[{i}]"), format!("{:?} is not `host:port`", g.target));
            }
            if g.kinds.is_empty() {
                problem(format!("net[{i}]"), format!("{:?} grants no kind", g.target));
            }
        }
        for (i, g) in self.dev.iter().enumerate() {
            if dev_target(&g.target).is_none() {
                problem(format!("dev[{i}]"), format!("{:?} is not `mmio:<base>+<length>` or `irq:<line>`", g.target));
            }
            if g.kinds.is_empty() {
                problem(format!("dev[{i}]"), format!("{:?} grants no kind", g.target));
            }
        }
        for (i, name) in self.env.iter().enumerate() {
            if !valid_name(name) || name.contains('=') {
                problem(format!("env[{i}]"), format!("{name:?} is not a variable name"));
            }
        }
        let anchor_problem = |anchor: &GrantAnchor| match anchor {
            GrantAnchor::Env(name) if !valid_name(name) || name.contains('=') => {
                Some(format!("env {name:?} is not a variable name"))
            }
            _ => None,
        };
        for (i, g) in self.anchored.fs.iter().enumerate() {
            let at = format!("anchored.fs[{i}]");
            if let Some(message) = anchor_problem(&g.anchor) {
                problem(at.clone(), message);
            }
            if !valid_relative(&g.glob) {
                problem(at.clone(), format!("{} {:?} is not a relative glob without `..`", g.anchor.label(), g.glob));
            }
            if g.modes.is_empty() {
                problem(at, format!("{} {:?} grants no mode", g.anchor.label(), g.glob));
            }
        }
        for (i, e) in self.anchored.exec.iter().enumerate() {
            let at = format!("anchored.exec[{i}]");
            if let Some(message) = anchor_problem(&e.anchor) {
                problem(at.clone(), message);
            }
            if let Some(message) = anchored_exec_problem(e) {
                problem(at, message);
            }
        }
        for (i, g) in self.anchored.net.iter().enumerate() {
            let at = format!("anchored.net[{i}]");
            for part in [&g.host, &g.port] {
                if let NetPart::Anchor(anchor) = part {
                    if let Some(message) = anchor_problem(anchor) {
                        problem(at.clone(), message);
                    }
                }
            }
            if let Some(message) = anchored_net_problem(g) {
                problem(at, message);
            }
        }
        out
    }
}

/// One thing an `os/` call needs from the grants, read from its literal arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "need", rename_all = "kebab-case")]
pub enum OsRequirement {
    Fs { path: String, mode: FsMode },
    /// Every path under `root` (which ends in `/`): the target is a `str/concat` whose static prefix is
    /// `root` and whose rest is computed. Covered only by a grant glob ending in `/**` that contains `root`.
    FsTree { root: String, mode: FsMode },
    /// A path written against an anchor (`path` relative to it, empty for the anchor itself): covered only by an
    /// anchored `fs` grant on the same anchor whose glob matches it.
    FsAnchored { anchor: GrantAnchor, path: String, mode: FsMode },
    /// Every path under `root` (empty, or ending in `/`) of an anchor: the anchored path's `path` is computed past
    /// that literal directory. Covered by an anchored glob on the same anchor that can match under `root` (`**`, or a
    /// narrow one such as `**/*.lolo`); the host checks the glob on the resolved path and answers `denied` outside it.
    FsAnchoredTree { anchor: GrantAnchor, root: String, mode: FsMode },
    Exec { command: String },
    /// A host and port of which at least one is written against an `env` anchor: covered only by an anchored `net`
    /// grant with the same parts and one of the kinds.
    NetAnchored { host: NetPart, port: NetPart, kinds: Vec<NetKind> },
    /// A command written against an anchor: covered only by the same anchored `exec` entry.
    ExecAnchored { anchor: GrantAnchor, path: Option<String> },
    Net { host: String, port: u16, kinds: Vec<NetKind> },
    Env { name: String },
    Stdio { kind: StdioKind },
    Sys { kind: SysKind },
    Desktop { kind: DesktopKind },
    Kv { store: String, mode: KvMode },
    Dev { resource: DevResource, kind: DevKind },
}

impl OsRequirement {
    /// The requirement of a `spawn` `env` entry whose value is `null`: the child inherits that variable from the
    /// host, so the `env` grant must name it. The one derivation the validator and the host's check share.
    pub fn inherited_env(name: &str) -> Self {
        OsRequirement::Env { name: name.to_string() }
    }
}

impl OsGrants {
    /// Whether the grants cover `need`.
    pub fn allows(&self, need: &OsRequirement) -> bool {
        match need {
            OsRequirement::Fs { path, mode } => self.fs_allows(Path::new(path), *mode),
            OsRequirement::FsTree { root, mode } => self.fs_tree_allows(Path::new(root), *mode),
            OsRequirement::FsAnchored { anchor, path, mode } => relative_segments(path).is_some_and(|segments| {
                self.anchored.fs.iter().any(|g| {
                    g.anchor == *anchor && g.modes.contains(mode) && relative_glob_matches(&g.glob, &segments)
                })
            }),
            OsRequirement::FsAnchoredTree { anchor, root, mode } => relative_segments(root).is_some_and(|segments| {
                self.anchored.fs.iter().any(|g| {
                    g.anchor == *anchor && g.modes.contains(mode) && relative_glob_reaches_under(&g.glob, &segments)
                })
            }),
            OsRequirement::Exec { command } => self.exec_allows(command),
            OsRequirement::NetAnchored { host, port, kinds } => self.anchored.net.iter().any(|g| {
                g.host == *host && g.port == *port && kinds.iter().any(|k| g.kinds.contains(k))
            }),
            OsRequirement::ExecAnchored { anchor, path } => self
                .anchored
                .exec
                .iter()
                .any(|e| e.anchor == *anchor && e.path == *path),
            OsRequirement::Net { host, port, kinds } => {
                kinds.iter().any(|kind| self.net_allows(host, *port, *kind))
            }
            OsRequirement::Env { name } => self.env_allows(name),
            OsRequirement::Stdio { kind } => self.stdio_allows(*kind),
            OsRequirement::Sys { kind } => self.sys_allows(*kind),
            OsRequirement::Desktop { kind } => self.desktop_allows(*kind),
            OsRequirement::Kv { store, mode } => self.kv_allows(store, *mode),
            OsRequirement::Dev { resource, kind } => self.dev_allows(resource, *kind),
        }
    }
}

/// A target argument the caller cannot read at this point (`deferred`) contributes no requirement.
type Deferred<'a> = &'a dyn Fn(&SExpression) -> bool;

fn literal<'a>(args: &'a [SExpression], index: usize, deferred: Deferred) -> Result<Option<&'a str>, usize> {
    match args.get(index) {
        None => Ok(None),
        Some(arg) if deferred(arg) => Ok(None),
        Some(SExpression::String(s)) if !s.starts_with('@') => Ok(Some(s)),
        Some(_) => Err(index),
    }
}

fn literal_port(args: &[SExpression], index: usize, deferred: Deferred) -> Result<Option<u16>, usize> {
    match args.get(index) {
        None => Ok(None),
        Some(arg) if deferred(arg) => Ok(None),
        Some(SExpression::Number(n)) if n.fract() == 0.0 && (0.0..=65535.0).contains(n) => {
            Ok(Some(*n as u16))
        }
        Some(_) => Err(index),
    }
}

fn literal_int(args: &[SExpression], index: usize, deferred: Deferred) -> Result<Option<u64>, usize> {
    match args.get(index) {
        None => Ok(None),
        Some(arg) if deferred(arg) => Ok(None),
        Some(SExpression::Number(n)) if n.fract() == 0.0 && *n >= 0.0 && *n <= u64::MAX as f64 => Ok(Some(*n as u64)),
        Some(_) => Err(index),
    }
}

fn mmio_need(args: &[SExpression], kind: DevKind, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    let (base, offset, width) = (literal_int(args, 0, deferred)?, literal_int(args, 1, deferred)?, literal_int(args, 2, deferred)?);
    Ok(match (base, offset, width) {
        (Some(base), Some(offset), Some(width)) => {
            vec![OsRequirement::Dev { resource: DevResource::Mmio { base, offset, width }, kind }]
        }
        _ => Vec::new(),
    })
}

fn irq_need(args: &[SExpression], kind: DevKind, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    Ok(literal_int(args, 0, deferred)?
        .map(|line| OsRequirement::Dev { resource: DevResource::Irq { line }, kind })
        .into_iter()
        .collect())
}

/// The leading pieces of a `str/concat` target that are known before the call runs, cut after the last
/// literal piece that ends in `/`: a literal string, or (for a caller that defers config reads) a piece for
/// which `deferred` is true. `None` when the target is not a `str/concat` or has no such prefix, so the part
/// a computed tail can reach is always bounded by a directory.
pub fn concat_static_prefix<'a>(
    expr: &'a SExpression,
    deferred: &dyn Fn(&SExpression) -> bool,
) -> Option<&'a [SExpression]> {
    let SExpression::Array(arr) = expr else {
        return None;
    };
    if arr.first().and_then(SExpression::as_str) != Some("str/concat") {
        return None;
    }
    let pieces = &arr[1..];
    let mut last_dir = None;
    for (i, piece) in pieces.iter().enumerate() {
        match piece {
            SExpression::String(s) if !s.starts_with('@') => {
                if s.ends_with('/') {
                    last_dir = Some(i + 1);
                }
            }
            _ if deferred(piece) => {}
            _ => break,
        }
    }
    last_dir.map(|end| &pieces[..end])
}

#[derive(Clone)]
enum FsTarget {
    Exact(String),
    Tree(String),
    AnchoredExact(GrantAnchor, String),
    AnchoredTree(GrantAnchor, String),
}

/// The target of an anchored path argument: its literal `path` exactly, a `str/concat` `path` as the tree under its
/// literal directory prefix, or any other computed `path` as the anchor's whole tree.
fn anchored_target(anchor: GrantAnchor, path: Option<&SExpression>, deferred: Deferred) -> FsTarget {
    match path {
        None => FsTarget::AnchoredExact(anchor, String::new()),
        Some(SExpression::String(p)) if !p.starts_with('@') => FsTarget::AnchoredExact(anchor, p.clone()),
        Some(expr) => {
            let root: String = concat_static_prefix(expr, deferred)
                .filter(|prefix| !prefix.iter().any(deferred))
                .map(|prefix| prefix.iter().filter_map(SExpression::as_str).collect())
                .unwrap_or_default();
            FsTarget::AnchoredTree(anchor, root)
        }
    }
}

fn fs_target(args: &[SExpression], index: usize, deferred: Deferred) -> Result<Option<FsTarget>, usize> {
    if let Some(arg) = args.get(index) {
        if let Some((anchor, path)) = anchored_sexpr(arg).map_err(|()| index)? {
            return Ok(Some(anchored_target(anchor, path, deferred)));
        }
        if matches!(arg, SExpression::Array(a) if a.first().and_then(SExpression::as_str) == Some("str/concat")) {
            let prefix = concat_static_prefix(arg, deferred).ok_or(index)?;
            if prefix.iter().any(deferred) {
                return Ok(None);
            }
            let root: String = prefix.iter().filter_map(SExpression::as_str).collect();
            return Ok(Some(FsTarget::Tree(root)));
        }
    }
    Ok(literal(args, index, deferred)?.map(|path| FsTarget::Exact(path.to_string())))
}

fn fs_requirement(target: FsTarget, mode: FsMode) -> OsRequirement {
    match target {
        FsTarget::Exact(path) => OsRequirement::Fs { path, mode },
        FsTarget::Tree(root) => OsRequirement::FsTree { root, mode },
        FsTarget::AnchoredExact(anchor, path) => OsRequirement::FsAnchored { anchor, path, mode },
        FsTarget::AnchoredTree(anchor, root) => OsRequirement::FsAnchoredTree { anchor, root, mode },
    }
}

fn fs_need(args: &[SExpression], index: usize, mode: FsMode, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    Ok(fs_target(args, index, deferred)?
        .map(|target| fs_requirement(target, mode))
        .into_iter()
        .collect())
}

fn net_need(
    args: &[SExpression],
    host: usize,
    port: usize,
    kinds: &[NetKind],
    deferred: Deferred,
) -> Result<Vec<OsRequirement>, usize> {
    let anchored_part = |index: usize| -> Result<Option<NetPart>, usize> {
        match args.get(index) {
            Some(expr @ SExpression::Object(map)) if map.contains_key("anchor") => match anchored_sexpr(expr) {
                Ok(Some((anchor @ GrantAnchor::Env(_), None))) => Ok(Some(NetPart::Anchor(anchor))),
                _ => Err(index),
            },
            _ => Ok(None),
        }
    };
    let (host_anchor, port_anchor) = (anchored_part(host)?, anchored_part(port)?);
    if host_anchor.is_none() && port_anchor.is_none() {
        let (Some(name), Some(port)) = (literal(args, host, deferred)?, literal_port(args, port, deferred)?) else {
            return Ok(Vec::new());
        };
        return Ok(vec![OsRequirement::Net { host: name.to_string(), port, kinds: kinds.to_vec() }]);
    }
    let host_part = match host_anchor {
        Some(part) => Some(part),
        None => literal(args, host, deferred)?.map(|h| NetPart::Literal(h.to_string())),
    };
    let port_part = match port_anchor {
        Some(part) => Some(part),
        None => literal_port(args, port, deferred)?.map(|p| NetPart::Literal(p.to_string())),
    };
    let (Some(host), Some(port)) = (host_part, port_part) else {
        return Ok(Vec::new());
    };
    Ok(vec![OsRequirement::NetAnchored { host, port, kinds: kinds.to_vec() }])
}

/// The prefix of a glob up to its first wildcard segment, as an absolute path.
fn glob_prefix(glob: &str) -> String {
    let fixed: Vec<&str> = glob
        .split('/')
        .filter(|s| !s.is_empty())
        .take_while(|s| !s.contains(['*', '?']))
        .collect();
    format!("/{}", fixed.join("/"))
}

/// An anchored glob reads the tree its literal directory prefix reaches (the segments before the first wildcard,
/// never the last one): a narrow anchored grant that can match under it covers it, and the host checks every match.
fn anchored_glob_need(anchor: GrantAnchor, pattern: Option<&SExpression>, deferred: Deferred) -> OsRequirement {
    let written = match pattern {
        Some(SExpression::String(p)) if !p.starts_with('@') => p.clone(),
        Some(expr) => match anchored_target(anchor.clone(), Some(expr), deferred) {
            FsTarget::AnchoredTree(_, root) => format!("{root}*"),
            _ => String::new(),
        },
        None => String::new(),
    };
    let mut segments: Vec<&str> = written.split('/').collect();
    segments.pop();
    let root: String = segments
        .into_iter()
        .take_while(|s| !s.contains(['*', '?']))
        .map(|s| format!("{s}/"))
        .collect();
    OsRequirement::FsAnchoredTree { anchor, root, mode: FsMode::Read }
}

fn open_modes(flags: &str) -> Vec<FsMode> {
    let mut modes = Vec::new();
    let writes = flags.starts_with(['w', 'a']);
    modes.push(if writes { FsMode::Write } else { FsMode::Read });
    if flags.contains('+') && writes {
        modes.push(FsMode::Read);
    }
    if flags.contains('x') {
        modes.push(FsMode::Create);
    }
    modes
}

fn spawn_needs(args: &[SExpression], deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    let mut needs = Vec::new();
    match args.first().map(anchored_sexpr) {
        Some(Err(())) => return Err(0),
        Some(Ok(Some((anchor, path)))) => match path {
            None => needs.push(OsRequirement::ExecAnchored { anchor, path: None }),
            Some(SExpression::String(p)) if !p.starts_with('@') => {
                needs.push(OsRequirement::ExecAnchored { anchor, path: Some(p.clone()) })
            }
            Some(_) => return Err(0),
        },
        _ => {
            if let Some(command) = literal(args, 0, deferred)? {
                needs.push(OsRequirement::Exec { command: command.to_string() });
            }
        }
    }
    if let Some(SExpression::Object(options)) = args.get(2) {
        for (key, value) in options {
            match (key.as_str(), value) {
                ("cwd", SExpression::String(path)) if !path.starts_with('@') => {
                    needs.push(OsRequirement::Fs { path: path.clone(), mode: FsMode::Read });
                }
                ("cwd", SExpression::Object(_)) => match anchored_sexpr(value) {
                    Ok(Some((anchor, path))) => {
                        needs.push(fs_requirement(anchored_target(anchor, path, deferred), FsMode::Read))
                    }
                    _ => return Err(2),
                },
                ("cwd", SExpression::Null) => {}
                ("cwd", value) if deferred(value) => {}
                ("cwd", _) => return Err(2),
                ("env", SExpression::Object(vars)) => {
                    for (name, value) in vars {
                        if matches!(value, SExpression::Null) {
                            needs.push(OsRequirement::inherited_env(name));
                        }
                    }
                }
                ("stdin" | "stdout" | "stderr", SExpression::String(mode)) if mode == "inherit" => {
                    needs.push(OsRequirement::Stdio {
                        kind: match key.as_str() {
                            "stdin" => StdioKind::In,
                            "stdout" => StdioKind::Out,
                            _ => StdioKind::Err,
                        },
                    });
                }
                _ => {}
            }
        }
    }
    Ok(needs)
}

/// What the `os/` operation `op` needs from the grants, read from its positional arguments (the call's
/// `{ key, emit }` envelope already split off). `Err(index)` names the first target argument that is not a
/// literal. Calls that act on a handle (`fd`, `pid`, `sock`) need nothing: the grant was checked where the
/// handle was made.
pub fn os_requirements(op: OsOp, args: &[SExpression]) -> Result<Vec<OsRequirement>, usize> {
    os_requirements_deferring(op, args, &|_| false)
}

/// [`os_requirements`] for a caller that checks the program before every target is known: an argument for which
/// `deferred` is true contributes no requirement instead of being reported as computed. An argument that picks
/// from a literal list (`(array/nth (list a b) i)`) needs what each element it can pick needs: every element for
/// a computed index, the one at a literal index.
pub fn os_requirements_deferring(
    op: OsOp,
    args: &[SExpression],
    deferred: &dyn Fn(&SExpression) -> bool,
) -> Result<Vec<OsRequirement>, usize> {
    let mut expansions: Vec<Vec<SExpression>> = vec![Vec::new()];
    for arg in args {
        let choices = literal_list_pick(arg).unwrap_or_else(|| vec![arg.clone()]);
        expansions = expansions
            .into_iter()
            .flat_map(|prefix| {
                choices.iter().map(move |choice| {
                    let mut next = prefix.clone();
                    next.push(choice.clone());
                    next
                })
            })
            .collect();
    }
    let mut needs = Vec::new();
    for expansion in &expansions {
        for need in requirements_of(op, expansion, deferred)? {
            if !needs.contains(&need) {
                needs.push(need);
            }
        }
    }
    Ok(needs)
}

/// The elements `(array/nth <literal list> <index>)` can yield: all of them for a computed index, the one at a
/// literal whole index (none past the end). The list is a `(list …)` or a data array whose head is not a string (a
/// substituted config list of objects, which needs no `list` marker). `None` when `expr` is not such a pick.
fn literal_list_pick(expr: &SExpression) -> Option<Vec<SExpression>> {
    let SExpression::Array(arr) = expr else {
        return None;
    };
    if !matches!(
        arr.first().and_then(SExpression::as_str),
        Some("array/nth" | "nth")
    ) {
        return None;
    }
    let mut source = arr.get(1)?;
    // A list carrying its declared type (`["typed", list, type]`) is that list.
    if let SExpression::Array(parts) = source {
        if parts.len() == 3 && parts[0].as_str() == Some("typed") {
            source = &parts[1];
        }
    }
    let SExpression::Array(list) = source else {
        return None;
    };
    let items = match list.first().and_then(SExpression::as_str) {
        Some("list") => &list[1..],
        Some(_) => return None,
        None if !list.is_empty() && !source.is_call() => &list[..],
        None => return None,
    };
    Some(match arr.get(2) {
        Some(SExpression::Number(at)) if *at >= 0.0 && at.fract() == 0.0 => {
            items.get(*at as usize).cloned().into_iter().collect()
        }
        _ => items.to_vec(),
    })
}

/// The positional arguments of `op` that may be written as an anchored path (`{ anchor, path }`): the file targets,
/// and the command of `os/spawn`. A host resolves them to absolute paths before the call runs; the `cwd` option of
/// `os/spawn` may be one too.
pub fn anchored_arg_positions(op: OsOp) -> &'static [usize] {
    use OsOp::*;
    match op {
        ReadFile | Readlink | WatchFiles | ListDir | Realpath | Chdir | Stat | Lstat | Access | Exists | WriteFile
        | AppendFile | Truncate | Open | Remove | Rmdir | Mkdir | Mktemp | Chmod | Chown | Utimes | Spawn => &[0],
        Copy | Rename | Link => &[0, 1],
        Symlink => &[1],
        _ => &[],
    }
}

/// The host and port positions of `op` that may be written as an `env` anchor (`{ anchor: { env: NAME } }`); a host
/// resolves them to the boot value (the port as a number) before the call runs.
pub fn anchored_net_args(op: OsOp) -> Option<(usize, usize)> {
    use OsOp::*;
    match op {
        TcpConnect | TcpListen | UdpBind => Some((0, 1)),
        Sendto => Some((2, 3)),
        _ => None,
    }
}

/// The call's arguments with every anchored path (`{ anchor, path }`) at a path or command position, the anchored
/// `cwd` of `os/spawn`, and every `env`-anchored host or port replaced by what it names on this host. The grant
/// check then runs on the plain value against the boot-resolved grants.
pub fn resolve_anchored_args(op: OsOp, args: &[Value], anchors: &OsAnchors) -> Result<Vec<Value>, OsError> {
    let refuse = |why: String| OsError::new(OsErrorKind::Denied, format!("{}: {why}", op.name()));
    // An `env` anchor that is unset or empty names nothing: the target is absent, not refused.
    let unset = |anchor: &GrantAnchor| match anchor {
        GrantAnchor::Env(name) if anchors.env.get(name).is_none_or(str::is_empty) => {
            Some(OsError::new(OsErrorKind::NotFound, format!("{}: env {name} is unset", op.name())))
        }
        _ => None,
    };
    let path = |value: &Value| -> Result<Option<Value>, OsError> {
        match AnchoredPath::from_value(value) {
            Some(anchored) => match unset(&anchored.anchor) {
                Some(absent) => Err(absent),
                None => anchors.resolve_path(&anchored).map(|p| Some(Value::String(p))).map_err(refuse),
            },
            None => Ok(None),
        }
    };
    let mut out = args.to_vec();
    for &at in anchored_arg_positions(op) {
        if let Some(value) = out.get(at) {
            if let Some(resolved) = path(value)? {
                out[at] = resolved;
            }
        }
    }
    if op == OsOp::Spawn {
        if let Some(Value::Object(options)) = out.get_mut(2) {
            if let Some(cwd) = options.get("cwd") {
                if let Some(resolved) = path(cwd)? {
                    options.insert("cwd".to_string(), resolved);
                }
            }
        }
    }
    if let Some((host_at, port_at)) = anchored_net_args(op) {
        for (at, is_port) in [(host_at, false), (port_at, true)] {
            let Some(AnchoredPath { anchor, path: None }) = out.get(at).and_then(AnchoredPath::from_value) else {
                continue;
            };
            if let Some(absent) = unset(&anchor) {
                return Err(absent);
            }
            let text = anchors.net_part(&NetPart::Anchor(anchor)).map_err(refuse)?;
            out[at] = if is_port {
                let port: u16 = text.parse().map_err(|_| refuse(format!("the port {text:?} is not a port number")))?;
                Value::Number(f64::from(port))
            } else {
                Value::String(text)
            };
        }
    }
    Ok(out)
}

fn kv_need(args: &[SExpression], mode: KvMode, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    Ok(literal(args, 0, deferred)?
        .map(|store| OsRequirement::Kv { store: store.to_string(), mode })
        .into_iter()
        .collect())
}

fn requirements_of(
    op: OsOp,
    args: &[SExpression],
    deferred: &dyn Fn(&SExpression) -> bool,
) -> Result<Vec<OsRequirement>, usize> {
    use OsOp::*;
    let sys = |kind| Ok(vec![OsRequirement::Sys { kind }]);
    let desktop = |kind| Ok(vec![OsRequirement::Desktop { kind }]);
    match op {
        ReadFile | Readlink | WatchFiles | ListDir | Realpath | Chdir | Stat | Lstat | Access | Exists => {
            fs_need(args, 0, FsMode::Read, deferred)
        }
        WriteFile | AppendFile | Truncate => fs_need(args, 0, FsMode::Write, deferred),
        Open => {
            let mut needs = Vec::new();
            if let (Some(target), Some(flags)) = (fs_target(args, 0, deferred)?, literal(args, 1, deferred)?) {
                needs.extend(open_modes(flags).into_iter().map(|mode| fs_requirement(target.clone(), mode)));
            }
            Ok(needs)
        }
        Copy => fs_need(args, 0, FsMode::Read, deferred).and_then(|mut n| {
            n.extend(fs_need(args, 1, FsMode::Create, deferred)?);
            Ok(n)
        }),
        Rename => fs_need(args, 0, FsMode::Remove, deferred).and_then(|mut n| {
            n.extend(fs_need(args, 1, FsMode::Create, deferred)?);
            Ok(n)
        }),
        Link => fs_need(args, 0, FsMode::Write, deferred).and_then(|mut n| {
            n.extend(fs_need(args, 1, FsMode::Create, deferred)?);
            Ok(n)
        }),
        Symlink => fs_need(args, 1, FsMode::Create, deferred),
        Remove | Rmdir => fs_need(args, 0, FsMode::Remove, deferred),
        Mkdir | Mktemp => fs_need(args, 0, FsMode::Create, deferred),
        Chmod | Chown | Utimes => fs_need(args, 0, FsMode::Meta, deferred),
        Glob => match args.first().map(anchored_sexpr).transpose().map_err(|()| 0usize)?.flatten() {
            Some((anchor, pattern)) => Ok(vec![anchored_glob_need(anchor, pattern, deferred)]),
            None => Ok(literal(args, 0, deferred)?
                .map(|glob| OsRequirement::Fs { path: glob_prefix(glob), mode: FsMode::Read })
                .into_iter()
                .collect()),
        },
        Spawn => spawn_needs(args, deferred),
        Getenv | Setenv | Unsetenv | WatchEnv => Ok(literal(args, 0, deferred)?
            .map(|name| OsRequirement::Env { name: name.to_string() })
            .into_iter()
            .collect()),
        StdinRead => Ok(vec![OsRequirement::Stdio { kind: StdioKind::In }]),
        StdoutWrite => Ok(vec![OsRequirement::Stdio { kind: StdioKind::Out }]),
        StderrWrite => Ok(vec![OsRequirement::Stdio { kind: StdioKind::Err }]),
        TcpConnect => net_need(args, 0, 1, &[NetKind::Tcp], deferred),
        TcpListen => net_need(args, 0, 1, &[NetKind::Listen], deferred),
        UdpBind => net_need(args, 0, 1, &[NetKind::Udp], deferred),
        Sendto => net_need(args, 2, 3, &[NetKind::Udp], deferred),
        DnsResolve => Ok(literal(args, 0, deferred)?
            .map(|host| OsRequirement::Net {
                host: host.to_string(),
                port: 0,
                kinds: vec![NetKind::Tcp, NetKind::Udp],
            })
            .into_iter()
            .collect()),
        Uid | Gid | Username | GroupOf | Hostname | Uname => sys(SysKind::Identity),
        CpuUsage | Uptime | LoadAvg => sys(SysKind::Cpu),
        MemoryInfo => sys(SysKind::Memory),
        DiskUsage | Mounts | DiskIo => sys(SysKind::Disk),
        NetworkInterfaces | NetworkStats | NetworkConnected | DefaultGateway => sys(SysKind::Network),
        BatteryStatus | AcPower | ThermalZones => sys(SysKind::PowerState),
        Notify => desktop(DesktopKind::Notify),
        Speak => desktop(DesktopKind::Speak),
        BrightnessGet | BrightnessSet => desktop(DesktopKind::Brightness),
        VolumeGet | VolumeSet | VolumeMute => desktop(DesktopKind::Volume),
        Suspend | Hibernate => desktop(DesktopKind::Power),
        KvGet | KvScan => kv_need(args, KvMode::Read, deferred),
        KvPut | KvDelete | KvBatch => kv_need(args, KvMode::Write, deferred),
        MmioRead => mmio_need(args, DevKind::Read, deferred),
        MmioWrite => mmio_need(args, DevKind::Write, deferred),
        IrqWait => irq_need(args, DevKind::Wait, deferred),
        IrqAck => irq_need(args, DevKind::Ack, deferred),
        Close | Read | Write | Seek | Fsync | Cwd | Wait | Kill | Pid | Ppid | CurrentExe | WatchProcess
        | WatchSignal | Environ | Isatty | TtySize | Send | Recv | Recvfrom | Shutdown | WatchPort
        | ClockMonotonic | WatchCron => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fs_grants(glob: &str, modes: &[FsMode]) -> OsGrants {
        OsGrants {
            fs: vec![FsGrant {
                glob: glob.to_string(),
                modes: modes.to_vec(),
            }],
            ..OsGrants::default()
        }
    }

    fn allows(grants: &OsGrants, path: &str, mode: FsMode) -> bool {
        grants.fs_allows(Path::new(path), mode)
    }

    #[test]
    fn empty_grants_deny_everything() {
        let g = OsGrants::default();
        assert!(g.is_empty());
        for mode in [
            FsMode::Read,
            FsMode::Write,
            FsMode::Create,
            FsMode::Remove,
            FsMode::Meta,
        ] {
            assert!(!allows(&g, "/", mode));
            assert!(!allows(&g, "/data/a", mode));
        }
        assert!(!g.exec_allows("git"));
        assert!(!g.exec_allows(""));
        for kind in [NetKind::Tcp, NetKind::Udp, NetKind::Listen] {
            assert!(!g.net_allows("localhost", 80, kind));
        }
        assert!(!g.env_allows("HOME"));
        for kind in [StdioKind::In, StdioKind::Out, StdioKind::Err] {
            assert!(!g.stdio_allows(kind));
        }
        for kind in [
            SysKind::Cpu,
            SysKind::Memory,
            SysKind::Disk,
            SysKind::Network,
            SysKind::PowerState,
            SysKind::Identity,
        ] {
            assert!(!g.sys_allows(kind));
        }
        for kind in [
            DesktopKind::Notify,
            DesktopKind::Speak,
            DesktopKind::Brightness,
            DesktopKind::Volume,
            DesktopKind::Power,
        ] {
            assert!(!g.desktop_allows(kind));
        }
    }

    #[test]
    fn a_double_star_glob_allows_the_subtree_and_nothing_beside_it() {
        let g = fs_grants("/data/**", &[FsMode::Read]);
        assert!(allows(&g, "/data/a/b", FsMode::Read));
        assert!(allows(&g, "/data/a", FsMode::Read));
        assert!(allows(&g, "/data", FsMode::Read));
        assert!(!allows(&g, "/database", FsMode::Read));
        assert!(!allows(&g, "/database/a", FsMode::Read));
        assert!(!allows(&g, "/other/data/a", FsMode::Read));
        assert!(!allows(&g, "/", FsMode::Read));
    }

    #[test]
    fn dot_dot_is_collapsed_and_an_escape_is_refused() {
        let g = fs_grants("/data/**", &[FsMode::Read]);
        assert!(!allows(&g, "/data/../etc/passwd", FsMode::Read));
        assert!(allows(&g, "/data/a/../b", FsMode::Read));
        assert!(allows(&g, "/etc/../data/a", FsMode::Read));
        assert!(allows(&g, "/data/./a", FsMode::Read));
        assert!(!allows(&g, "/../data/a", FsMode::Read));
        assert!(!allows(&g, "/data/../../data/a", FsMode::Read));
        let everything = fs_grants("/**", &[FsMode::Read]);
        assert!(!allows(&everything, "/..", FsMode::Read));
        assert!(allows(&everything, "/a/..", FsMode::Read));
    }

    #[test]
    fn a_relative_path_is_denied() {
        let g = fs_grants("/**", &[FsMode::Read]);
        assert!(!allows(&g, "data/a", FsMode::Read));
        assert!(!allows(&g, "", FsMode::Read));
        assert!(!allows(&g, "./a", FsMode::Read));
    }

    #[test]
    fn a_relative_or_dot_dot_glob_matches_nothing() {
        assert!(!allows(
            &fs_grants("data/**", &[FsMode::Read]),
            "/data/a",
            FsMode::Read
        ));
        assert!(!allows(
            &fs_grants("/data/../etc/**", &[FsMode::Read]),
            "/etc/passwd",
            FsMode::Read
        ));
        assert!(!allows(&fs_grants("", &[FsMode::Read]), "/a", FsMode::Read));
    }

    #[test]
    fn a_mode_is_granted_only_when_listed() {
        let g = fs_grants("/data/**", &[FsMode::Read, FsMode::Write]);
        assert!(allows(&g, "/data/a", FsMode::Read));
        assert!(allows(&g, "/data/a", FsMode::Write));
        assert!(!allows(&g, "/data/a", FsMode::Create));
        assert!(!allows(&g, "/data/a", FsMode::Remove));
        assert!(!allows(&g, "/data/a", FsMode::Meta));
        assert!(!allows(
            &fs_grants("/data/**", &[]),
            "/data/a",
            FsMode::Read
        ));
    }

    #[test]
    fn modes_of_separate_grants_do_not_mix_across_globs() {
        let g = OsGrants {
            fs: vec![
                FsGrant {
                    glob: "/a/**".into(),
                    modes: vec![FsMode::Read],
                },
                FsGrant {
                    glob: "/b/**".into(),
                    modes: vec![FsMode::Write],
                },
            ],
            ..OsGrants::default()
        };
        assert!(allows(&g, "/a/x", FsMode::Read));
        assert!(!allows(&g, "/a/x", FsMode::Write));
        assert!(allows(&g, "/b/x", FsMode::Write));
        assert!(!allows(&g, "/b/x", FsMode::Read));
    }

    #[test]
    fn a_single_star_stays_inside_one_segment() {
        let g = fs_grants("/logs/*.log", &[FsMode::Read]);
        assert!(allows(&g, "/logs/app.log", FsMode::Read));
        assert!(allows(&g, "/logs/.log", FsMode::Read));
        assert!(!allows(&g, "/logs/app.txt", FsMode::Read));
        assert!(!allows(&g, "/logs/sub/app.log", FsMode::Read));
        assert!(!allows(&g, "/logs", FsMode::Read));
    }

    #[test]
    fn a_question_mark_is_exactly_one_character() {
        let g = fs_grants("/t/f?.txt", &[FsMode::Read]);
        assert!(allows(&g, "/t/f1.txt", FsMode::Read));
        assert!(!allows(&g, "/t/f.txt", FsMode::Read));
        assert!(!allows(&g, "/t/f12.txt", FsMode::Read));
    }

    #[test]
    fn a_double_star_in_the_middle_spans_any_depth_including_none() {
        let g = fs_grants("/home/**/cache", &[FsMode::Read]);
        assert!(allows(&g, "/home/cache", FsMode::Read));
        assert!(allows(&g, "/home/u/x/cache", FsMode::Read));
        assert!(!allows(&g, "/home/u/x/cache/y", FsMode::Read));
        assert!(!allows(&g, "/home/u/x", FsMode::Read));
    }

    #[test]
    fn a_trailing_slash_and_doubled_slashes_do_not_change_the_match() {
        let g = fs_grants("/data/**", &[FsMode::Read]);
        assert!(allows(&g, "/data//a/", FsMode::Read));
        assert!(!allows(&g, "//database", FsMode::Read));
    }

    #[test]
    fn exec_is_an_exact_string() {
        let g = OsGrants {
            exec: vec!["git".into()],
            ..OsGrants::default()
        };
        assert!(g.exec_allows("git"));
        assert!(!g.exec_allows("git "));
        assert!(!g.exec_allows("/usr/bin/git"));
        assert!(!g.exec_allows("Git"));
        assert!(!g.exec_allows("git status"));
        assert!(!g.exec_allows(""));
    }

    #[test]
    fn net_matches_host_port_and_kind() {
        let g = OsGrants {
            net: vec![NetGrant {
                target: "db.internal:5432".into(),
                kinds: vec![NetKind::Tcp],
            }],
            ..OsGrants::default()
        };
        assert!(g.net_allows("db.internal", 5432, NetKind::Tcp));
        assert!(g.net_allows("DB.Internal", 5432, NetKind::Tcp));
        assert!(!g.net_allows("db.internal", 5433, NetKind::Tcp));
        assert!(!g.net_allows("db.internal", 5432, NetKind::Udp));
        assert!(!g.net_allows("db.internal", 5432, NetKind::Listen));
        assert!(!g.net_allows("evil.db.internal", 5432, NetKind::Tcp));
        assert!(!g.net_allows("internal", 5432, NetKind::Tcp));
    }

    #[test]
    fn net_handles_ipv6_literals_and_ignores_malformed_entries() {
        let g = OsGrants {
            net: vec![
                NetGrant {
                    target: "[::1]:8080".into(),
                    kinds: vec![NetKind::Listen],
                },
                NetGrant {
                    target: "no-port".into(),
                    kinds: vec![NetKind::Tcp],
                },
                NetGrant {
                    target: "host:notaport".into(),
                    kinds: vec![NetKind::Tcp],
                },
                NetGrant {
                    target: ":80".into(),
                    kinds: vec![NetKind::Tcp],
                },
            ],
            ..OsGrants::default()
        };
        assert!(g.net_allows("::1", 8080, NetKind::Listen));
        assert!(g.net_allows("[::1]", 8080, NetKind::Listen));
        assert!(!g.net_allows("::1", 8080, NetKind::Tcp));
        assert!(!g.net_allows("no-port", 80, NetKind::Tcp));
        assert!(!g.net_allows("host", 80, NetKind::Tcp));
        assert!(!g.net_allows("", 80, NetKind::Tcp));
    }

    #[test]
    fn env_is_an_exact_name() {
        let g = OsGrants {
            env: vec!["HOME".into()],
            ..OsGrants::default()
        };
        assert!(g.env_allows("HOME"));
        assert!(!g.env_allows("home"));
        assert!(!g.env_allows("HOME2"));
        assert!(!g.env_allows(""));
    }

    #[test]
    fn stdio_sys_and_desktop_grant_only_what_is_listed() {
        let g = OsGrants {
            stdio: vec![StdioKind::Out],
            sys: vec![SysKind::Memory, SysKind::PowerState],
            desktop: vec![DesktopKind::Notify],
            ..OsGrants::default()
        };
        assert!(g.stdio_allows(StdioKind::Out));
        assert!(!g.stdio_allows(StdioKind::In));
        assert!(!g.stdio_allows(StdioKind::Err));
        assert!(g.sys_allows(SysKind::Memory));
        assert!(g.sys_allows(SysKind::PowerState));
        assert!(!g.sys_allows(SysKind::Cpu));
        assert!(!g.sys_allows(SysKind::Identity));
        assert!(g.desktop_allows(DesktopKind::Notify));
        assert!(!g.desktop_allows(DesktopKind::Power));
        assert!(!g.desktop_allows(DesktopKind::Speak));
    }

    #[test]
    fn dev_grants_cover_a_register_window_and_an_irq_line_only() {
        let g: OsGrants = serde_json::from_value(json!({
            "dev": [
                {"target": "mmio:0x09000000+0x1000", "kinds": ["read", "write"]},
                {"target": "irq:33", "kinds": ["wait", "ack"]}
            ]
        }))
        .expect("dev grants");
        let mmio = |offset, width| DevResource::Mmio { base: 0x0900_0000, offset, width };
        assert!(g.dev_allows(&mmio(0x18, 4), DevKind::Read));
        assert!(g.dev_allows(&mmio(0xffc, 4), DevKind::Write), "the last word of the window");
        assert!(!g.dev_allows(&mmio(0xffd, 4), DevKind::Read), "control: one byte past the window");
        assert!(!g.dev_allows(&DevResource::Mmio { base: 0x0900_1000, offset: 0, width: 4 }, DevKind::Read), "control: another device");
        assert!(g.dev_allows(&DevResource::Irq { line: 33 }, DevKind::Ack));
        assert!(!g.dev_allows(&DevResource::Irq { line: 34 }, DevKind::Wait), "control: another line");
        assert!(!g.dev_allows(&DevResource::Irq { line: 33 }, DevKind::Read), "control: a register kind on an irq");
        assert!(!OsGrants::default().dev_allows(&mmio(0, 1), DevKind::Read), "control: closed by default");
        assert!(g.entry_problems().is_empty());
    }

    #[test]
    fn a_dev_target_that_names_no_window_or_line_is_an_entry_problem() {
        let g = OsGrants {
            dev: vec![
                DevGrant { target: "mmio:0x10".into(), kinds: vec![DevKind::Read] },
                DevGrant { target: "irq:x".into(), kinds: vec![DevKind::Wait] },
                DevGrant { target: "irq:5".into(), kinds: vec![] },
                DevGrant { target: "mmio:0x10+0".into(), kinds: vec![DevKind::Read] },
            ],
            ..OsGrants::default()
        };
        let paths: Vec<String> = g.entry_problems().into_iter().map(|p| p.path).collect();
        assert_eq!(paths, vec!["dev[0]", "dev[1]", "dev[2]", "dev[3]"]);
    }

    #[test]
    fn device_calls_need_their_window_or_line() {
        let call = |op, args: serde_json::Value| os_requirements(op, &serde_json::from_value::<Vec<SExpression>>(args).expect("args"));
        let mmio = |offset, width, kind| OsRequirement::Dev { resource: DevResource::Mmio { base: 0x0900_0000, offset, width }, kind };
        assert_eq!(call(OsOp::MmioRead, json!([150994944, 24, 4])), Ok(vec![mmio(24, 4, DevKind::Read)]));
        assert_eq!(call(OsOp::MmioWrite, json!([150994944, 0, 1, 65])), Ok(vec![mmio(0, 1, DevKind::Write)]));
        assert_eq!(call(OsOp::IrqWait, json!([33])), Ok(vec![OsRequirement::Dev { resource: DevResource::Irq { line: 33 }, kind: DevKind::Wait }]));
        assert_eq!(call(OsOp::IrqAck, json!([33])), Ok(vec![OsRequirement::Dev { resource: DevResource::Irq { line: 33 }, kind: DevKind::Ack }]));
        assert_eq!(call(OsOp::MmioRead, json!([150994944, "@entity.offset", 4])), Err(1), "control: a computed offset is not literal");
        let deferred = |arg: &SExpression| matches!(arg, SExpression::String(s) if s.starts_with("@config."));
        let args: Vec<SExpression> = serde_json::from_value(json!(["@config.base", 24, 4])).expect("args");
        assert_eq!(os_requirements_deferring(OsOp::MmioRead, &args, &deferred), Ok(vec![]), "a config base is checked by the host");
    }

    #[test]
    fn grants_deserialize_from_the_documented_spellings() {
        let g: OsGrants = serde_json::from_value(json!({
            "fs": [{"glob": "/data/**", "modes": ["read", "create"]}],
            "exec": ["git"],
            "net": [{"target": "db.internal:5432", "kinds": ["tcp", "listen"]}],
            "env": ["HOME"],
            "stdio": ["out", "err"],
            "sys": ["power-state"],
            "desktop": ["notify", "power"]
        }))
        .expect("grants");
        assert!(allows(&g, "/data/x", FsMode::Create));
        assert!(g.net_allows("db.internal", 5432, NetKind::Listen));
        assert!(g.sys_allows(SysKind::PowerState));
        assert!(g.desktop_allows(DesktopKind::Power));
        assert!(!g.is_empty());
        let empty: OsGrants = serde_json::from_value(json!({})).expect("empty");
        assert!(empty.is_empty());
        assert!(serde_json::from_value::<OsGrants>(json!({"sys": ["gpu"]})).is_err());
    }

    fn call(op: OsOp, args: serde_json::Value) -> Result<Vec<OsRequirement>, usize> {
        let args: Vec<SExpression> = serde_json::from_value(args).expect("args");
        os_requirements(op, &args)
    }

    fn fs_req(path: &str, mode: FsMode) -> OsRequirement {
        OsRequirement::Fs { path: path.to_string(), mode }
    }

    #[test]
    fn a_target_picked_from_a_literal_list_needs_every_element_it_can_pick() {
        let env = |name: &str| OsRequirement::Env {
            name: name.to_string(),
        };
        assert_eq!(
            call(
                OsOp::Getenv,
                json!([["array/nth", ["list", "PATH", "HOME"], "@entity.cursor"]])
            ),
            Ok(vec![env("PATH"), env("HOME")])
        );
        assert_eq!(
            call(
                OsOp::Spawn,
                json!([["array/nth", ["list", "gh", "git"], "@entity.i"], []])
            ),
            Ok(vec![
                OsRequirement::Exec {
                    command: "gh".into()
                },
                OsRequirement::Exec {
                    command: "git".into()
                }
            ])
        );
        assert_eq!(
            call(
                OsOp::ReadFile,
                json!([["nth", ["list", "/data/a", "/data/b"], "@entity.i"]])
            ),
            Ok(vec![
                fs_req("/data/a", FsMode::Read),
                fs_req("/data/b", FsMode::Read)
            ])
        );
    }

    #[test]
    fn a_command_picked_from_a_config_fed_mixed_list_needs_each_anchored_or_plain_command() {
        let picked = call(
            OsOp::Spawn,
            json!([["array/nth", [{ "anchor": { "env": "ORB_BUN_PATH" } }, { "anchor": "exe-dir", "path": "bun" }, "bun"], "@entity.cmdCursor"], []]),
        );
        assert_eq!(
            picked,
            Ok(vec![
                OsRequirement::ExecAnchored { anchor: GrantAnchor::Env("ORB_BUN_PATH".into()), path: None },
                OsRequirement::ExecAnchored { anchor: GrantAnchor::ExeDir, path: Some("bun".into()) },
                OsRequirement::Exec { command: "bun".into() },
            ])
        );
        assert_eq!(
            call(OsOp::Spawn, json!([["array/nth", [{ "anchor": "exe-dir", "path": "@entity.p" }, "bun"], "@entity.i"], []])),
            Err(0),
            "control: a computed anchored path in the list is still not static"
        );
        assert_eq!(
            call(
                OsOp::Spawn,
                json!([["array/nth", ["typed", ["list", { "anchor": "exe-dir", "path": "bun" }, "bun"], "<declared type>"], "@entity.cmdCursor"], []])
            ),
            Ok(vec![
                OsRequirement::ExecAnchored { anchor: GrantAnchor::ExeDir, path: Some("bun".into()) },
                OsRequirement::Exec { command: "bun".into() },
            ]),
            "a marked list carrying its declared type is the same list"
        );
    }

    #[test]
    fn a_literal_index_into_a_literal_list_needs_only_that_element() {
        assert_eq!(
            call(
                OsOp::Getenv,
                json!([["array/nth", ["list", "PATH", "HOME"], 1]])
            ),
            Ok(vec![OsRequirement::Env {
                name: "HOME".into()
            }])
        );
        assert_eq!(
            call(OsOp::Getenv, json!([["array/nth", ["list", "PATH"], 3]])),
            Ok(vec![])
        );
    }

    #[test]
    fn control_a_pick_from_a_computed_list_or_a_computed_element_is_still_not_static() {
        assert_eq!(
            call(OsOp::Getenv, json!([["array/nth", "@entity.names", 0]])),
            Err(0)
        );
        assert_eq!(
            call(
                OsOp::Getenv,
                json!([["array/nth", ["list", "PATH", "@entity.other"], "@entity.i"]])
            ),
            Err(0)
        );
        assert_eq!(
            call(OsOp::Getenv, json!([["str/concat", "A", "@entity.x"]])),
            Err(0)
        );
    }

    #[test]
    fn a_file_call_needs_its_literal_path_in_the_mode_the_host_checks() {
        assert_eq!(call(OsOp::ReadFile, json!(["/data/a"])), Ok(vec![fs_req("/data/a", FsMode::Read)]));
        assert_eq!(call(OsOp::WriteFile, json!(["/data/a", "x"])), Ok(vec![fs_req("/data/a", FsMode::Write)]));
        assert_eq!(
            call(OsOp::Rename, json!(["/a/x", "/b/y"])),
            Ok(vec![fs_req("/a/x", FsMode::Remove), fs_req("/b/y", FsMode::Create)])
        );
        assert_eq!(call(OsOp::Chmod, json!(["/a", 420])), Ok(vec![fs_req("/a", FsMode::Meta)]));
    }

    #[test]
    fn a_computed_target_names_the_argument_that_is_not_a_literal() {
        assert_eq!(call(OsOp::ReadFile, json!(["@payload.path"])), Err(0));
        assert_eq!(call(OsOp::ReadFile, json!([["path-join", "/a", "b"]])), Err(0));
        assert_eq!(call(OsOp::Copy, json!(["/a", "@payload.to"])), Err(1));
        assert_eq!(call(OsOp::TcpConnect, json!(["db.internal", "@payload.port"])), Err(1));
        assert_eq!(call(OsOp::Open, json!(["/a", "@payload.flags"])), Err(1));
    }

    #[test]
    fn open_reads_its_modes_from_the_flags() {
        assert_eq!(call(OsOp::Open, json!(["/a", "r"])), Ok(vec![fs_req("/a", FsMode::Read)]));
        assert_eq!(call(OsOp::Open, json!(["/a", "w"])), Ok(vec![fs_req("/a", FsMode::Write)]));
        assert_eq!(
            call(OsOp::Open, json!(["/a", "w+x"])),
            Ok(vec![fs_req("/a", FsMode::Write), fs_req("/a", FsMode::Read), fs_req("/a", FsMode::Create)])
        );
    }

    #[test]
    fn spawn_needs_the_command_its_cwd_and_each_inherited_stream() {
        let needs = call(OsOp::Spawn, json!(["git", ["status"], {"cwd": "/repo", "stdout": "inherit", "stderr": "pipe"}]))
            .expect("literal");
        assert_eq!(
            needs,
            vec![
                OsRequirement::Exec { command: "git".into() },
                fs_req("/repo", FsMode::Read),
                OsRequirement::Stdio { kind: StdioKind::Out }
            ]
        );
        assert_eq!(call(OsOp::Spawn, json!(["@payload.cmd", []])), Err(0));
        assert_eq!(call(OsOp::Spawn, json!(["git", [], {"cwd": "@payload.dir"}])), Err(2));
        assert_eq!(
            call(OsOp::Spawn, json!(["git", [], {"cwd": null, "stdin": "null"}])),
            Ok(vec![OsRequirement::Exec { command: "git".into() }])
        );
    }

    #[test]
    fn a_spawn_env_entry_that_inherits_the_host_value_needs_that_env_grant() {
        assert_eq!(
            call(OsOp::Spawn, json!(["git", [], {"env": {"PATH": null, "FOO": "bar", "HOME": null}}])),
            Ok(vec![
                OsRequirement::Exec { command: "git".into() },
                OsRequirement::Env { name: "PATH".into() },
                OsRequirement::Env { name: "HOME".into() },
            ])
        );
        assert_eq!(
            call(OsOp::Spawn, json!(["git", [], {"env": {"FOO": "bar"}}])),
            Ok(vec![OsRequirement::Exec { command: "git".into() }]),
            "control: a literal value inherits nothing"
        );
    }

    #[test]
    fn a_glob_needs_read_on_its_literal_prefix() {
        assert_eq!(call(OsOp::Glob, json!(["/data/**/*.log"])), Ok(vec![fs_req("/data", FsMode::Read)]));
        assert_eq!(call(OsOp::Glob, json!(["/*.log"])), Ok(vec![fs_req("/", FsMode::Read)]));
    }

    #[test]
    fn sys_desktop_and_net_calls_map_to_their_kind_and_handle_calls_need_nothing() {
        assert_eq!(call(OsOp::MemoryInfo, json!([])), Ok(vec![OsRequirement::Sys { kind: SysKind::Memory }]));
        assert_eq!(call(OsOp::Notify, json!(["t", "b"])), Ok(vec![OsRequirement::Desktop { kind: DesktopKind::Notify }]));
        assert_eq!(
            call(OsOp::TcpConnect, json!(["db.internal", 5432])),
            Ok(vec![OsRequirement::Net { host: "db.internal".into(), port: 5432, kinds: vec![NetKind::Tcp] }])
        );
        assert_eq!(call(OsOp::Close, json!(["@payload.fd"])), Ok(vec![]));
        assert_eq!(call(OsOp::Pid, json!([])), Ok(vec![]));
        assert_eq!(call(OsOp::CurrentExe, json!([])), Ok(vec![]), "like os/cwd and os/pid, a fact of the process itself");
        assert_eq!(call(OsOp::Hostname, json!([])), Ok(vec![OsRequirement::Sys { kind: SysKind::Identity }]), "control: a host fact needs sys");
    }

    #[test]
    fn a_requirement_is_the_tagged_wire_shape_and_round_trips() {
        let cases = [
            (OsRequirement::Fs { path: "/a".into(), mode: FsMode::Read }, json!({"need":"fs","path":"/a","mode":"read"})),
            (OsRequirement::Exec { command: "git".into() }, json!({"need":"exec","command":"git"})),
            (
                OsRequirement::Net { host: "h".into(), port: 80, kinds: vec![NetKind::Tcp] },
                json!({"need":"net","host":"h","port":80,"kinds":["tcp"]}),
            ),
            (OsRequirement::Env { name: "FOO".into() }, json!({"need":"env","name":"FOO"})),
            (OsRequirement::Stdio { kind: StdioKind::Out }, json!({"need":"stdio","kind":"out"})),
            (OsRequirement::Sys { kind: SysKind::PowerState }, json!({"need":"sys","kind":"power-state"})),
            (OsRequirement::Desktop { kind: DesktopKind::Notify }, json!({"need":"desktop","kind":"notify"})),
        ];
        for (need, wire) in cases {
            assert_eq!(serde_json::to_value(&need).unwrap(), wire);
            assert_eq!(serde_json::from_value::<OsRequirement>(wire).unwrap(), need);
        }
        assert!(serde_json::from_value::<OsRequirement>(json!({"need":"fs","path":"/a"})).is_err());
        assert!(serde_json::from_value::<OsRequirement>(json!({"need":"nope"})).is_err());
    }

    #[test]
    fn allows_is_the_one_dispatch_over_the_family_matchers() {
        let g = OsGrants {
            fs: vec![FsGrant { glob: "/data/**".into(), modes: vec![FsMode::Read] }],
            exec: vec!["git".into()],
            net: vec![NetGrant { target: "db.internal:5432".into(), kinds: vec![NetKind::Tcp] }],
            sys: vec![SysKind::Memory],
            ..OsGrants::default()
        };
        assert!(g.allows(&fs_req("/data/a", FsMode::Read)));
        assert!(!g.allows(&fs_req("/data/a", FsMode::Write)));
        assert!(g.allows(&OsRequirement::Exec { command: "git".into() }));
        assert!(!g.allows(&OsRequirement::Exec { command: "rm".into() }));
        assert!(g.allows(&OsRequirement::Net { host: "db.internal".into(), port: 5432, kinds: vec![NetKind::Udp, NetKind::Tcp] }));
        assert!(!g.allows(&OsRequirement::Net { host: "db.internal".into(), port: 5432, kinds: vec![NetKind::Udp] }));
        assert!(g.allows(&OsRequirement::Sys { kind: SysKind::Memory }));
        assert!(!g.allows(&OsRequirement::Desktop { kind: DesktopKind::Notify }));
    }

    #[test]
    fn an_orbital_holds_the_app_level_plus_its_own_and_the_program_the_union() {
        let schema: crate::schema::OrbitalSchema = serde_json::from_value(json!({
            "name": "p", "version": "1.0.0",
            "osGrants": {"stdio": ["out"]},
            "orbitals": [
                {"name": "A", "entity": {"name": "E", "fields": []}, "traits": [], "pages": [],
                 "osGrants": {"exec": ["git"]}},
                {"name": "B", "entity": {"name": "F", "fields": []}, "traits": [], "pages": []}
            ]
        }))
        .expect("schema");
        let a = schema.orbital_os_grants(&schema.orbitals[0]);
        assert!(a.exec_allows("git") && a.stdio_allows(StdioKind::Out));
        let b = schema.orbital_os_grants(&schema.orbitals[1]);
        assert!(!b.exec_allows("git") && b.stdio_allows(StdioKind::Out));
        let program = schema.program_os_grants();
        assert!(program.exec_allows("git") && program.stdio_allows(StdioKind::Out));
        assert!(schema.os_grants_of([&schema.orbitals[1]]).exec.is_empty());
    }

    #[test]
    fn a_protocol_driver_target_in_egress_is_a_tcp_net_grant_and_nothing_else_is() {
        let schema: crate::schema::OrbitalSchema = serde_json::from_value(json!({
            "name": "p", "version": "1.0.0",
            "egress": ["redis://cache.internal:6379", "rediss://tls.internal", "postgres://db.internal", "https://api.example.com"],
            "orbitals": [{"name": "A", "entity": {"name": "E", "fields": []}, "traits": [], "pages": []}]
        }))
        .expect("schema");
        for grants in [schema.orbital_os_grants(&schema.orbitals[0]), schema.program_os_grants()] {
            assert!(grants.net_allows("cache.internal", 6379, NetKind::Tcp));
            assert!(grants.net_allows("tls.internal", 6379, NetKind::Tcp), "the default port of the scheme");
            assert!(grants.net_allows("db.internal", 5432, NetKind::Tcp));
            assert!(!grants.net_allows("cache.internal", 6380, NetKind::Tcp), "another port");
            assert!(!grants.net_allows("cache.internal", 6379, NetKind::Udp), "another kind");
            assert!(!grants.net_allows("api.example.com", 443, NetKind::Tcp), "an http origin grants no raw socket");
        }
        let plain: crate::schema::OrbitalSchema = serde_json::from_value(json!({
            "name": "p", "version": "1.0.0", "egress": ["https://api.example.com"],
            "orbitals": [{"name": "A", "entity": {"name": "E", "fields": []}, "traits": [], "pages": []}]
        }))
        .expect("schema");
        assert!(plain.program_os_grants().is_empty());
    }

    #[test]
    fn every_kind_name_matches_its_serde_spelling() {
        fn spelling<T: Serialize>(kind: &T) -> String {
            serde_json::to_value(kind).expect("ser").as_str().expect("str").to_string()
        }
        for k in FsMode::ALL { assert_eq!(k.name(), spelling(k)); }
        for k in NetKind::ALL { assert_eq!(k.name(), spelling(k)); }
        for k in StdioKind::ALL { assert_eq!(k.name(), spelling(k)); }
        for k in SysKind::ALL { assert_eq!(k.name(), spelling(k)); }
        for k in DesktopKind::ALL { assert_eq!(k.name(), spelling(k)); }
        assert_eq!((FsMode::ALL.len(), NetKind::ALL.len(), StdioKind::ALL.len(), SysKind::ALL.len(), DesktopKind::ALL.len()), (5, 3, 3, 6, 5));
    }

    #[test]
    fn atoms_split_one_mode_or_kind_each_and_each_allows_only_itself() {
        let g = OsGrants {
            fs: vec![FsGrant { glob: "/d/**".into(), modes: vec![FsMode::Read, FsMode::Write] }],
            net: vec![NetGrant { target: "h:1".into(), kinds: vec![NetKind::Tcp] }],
            exec: vec!["git".into()],
            ..OsGrants::default()
        };
        let atoms = g.atoms();
        assert_eq!(atoms.len(), 4);
        assert_eq!(atoms[0].path, "fs[0]");
        assert!(atoms[0].grants.fs_allows(Path::new("/d/x"), FsMode::Read));
        assert!(!atoms[0].grants.fs_allows(Path::new("/d/x"), FsMode::Write));
        assert!(atoms[1].grants.fs_allows(Path::new("/d/x"), FsMode::Write));
        assert_eq!(atoms[2].path, "exec[0]");
        assert_eq!(atoms[3].path, "net[0]");
        assert!(OsGrants::default().atoms().is_empty());
    }

    #[test]
    fn entry_problems_name_every_shape_that_cannot_match() {
        let bad = OsGrants {
            fs: vec![
                FsGrant { glob: "data/**".into(), modes: vec![FsMode::Read] },
                FsGrant { glob: "/a/../b".into(), modes: vec![FsMode::Read] },
                FsGrant { glob: "/ok/**".into(), modes: vec![] },
            ],
            exec: vec!["git status".into(), "".into()],
            net: vec![
                NetGrant { target: "no-port".into(), kinds: vec![NetKind::Tcp] },
                NetGrant { target: "h:1".into(), kinds: vec![] },
            ],
            env: vec!["A=B".into(), "".into()],
            ..OsGrants::default()
        };
        let paths: Vec<String> = bad.entry_problems().into_iter().map(|p| p.path).collect();
        assert_eq!(
            paths,
            ["fs[0]", "fs[1]", "fs[2]", "exec[0]", "exec[1]", "net[0]", "net[1]", "env[0]", "env[1]"]
        );
        let good = OsGrants {
            fs: vec![FsGrant { glob: "/d/**".into(), modes: vec![FsMode::Read] }],
            exec: vec!["git".into()],
            net: vec![NetGrant { target: "[::1]:80".into(), kinds: vec![NetKind::Listen] }],
            env: vec!["HOME".into()],
            ..OsGrants::default()
        };
        assert!(good.entry_problems().is_empty());
    }

    fn grant(glob: &str, modes: &[FsMode]) -> OsGrants {
        OsGrants { fs: vec![FsGrant { glob: glob.into(), modes: modes.to_vec() }], ..OsGrants::default() }
    }

    #[test]
    fn an_anchored_grant_matches_paths_written_under_the_same_anchor_only() {
        let home = grant("~/.orb/**", &[FsMode::Read]);
        assert!(home.fs_allows(Path::new("~/.orb/package.json"), FsMode::Read));
        assert!(!home.fs_allows(Path::new("~/.orb/package.json"), FsMode::Write), "control: the mode still counts");
        assert!(!home.fs_allows(Path::new("./.orb/package.json"), FsMode::Read), "control: another anchor is another place");
        assert!(!home.fs_allows(Path::new("/.orb/package.json"), FsMode::Read), "control: an absolute path is not under the home anchor");
        assert!(!home.fs_allows(Path::new("~/.orb/../.ssh/id"), FsMode::Read), "edge: climbing out of the anchored tree is refused");
        assert!(!home.fs_allows(Path::new("~/../etc/passwd"), FsMode::Read), "edge: climbing above the anchor is refused");
        let start = grant("./**", &[FsMode::Read]);
        assert!(start.fs_tree_allows(Path::new("./"), FsMode::Read));
        assert!(start.fs_allows(Path::new("./docs/a.md"), FsMode::Read));
        assert!(start.atoms().len() == 1 && start.entry_problems().is_empty(), "an anchored glob is a valid entry");
    }

    #[test]
    fn a_host_checks_an_absolute_path_against_anchored_grants_through_its_anchors() {
        let anchors = OsAnchors { home: Some(PathBuf::from("/home/u")), start: Some(PathBuf::from("/work/p")), ..OsAnchors::default() };
        let grants = OsGrants {
            fs: vec![
                FsGrant { glob: "~/.orb/**".into(), modes: vec![FsMode::Write] },
                FsGrant { glob: "./**".into(), modes: vec![FsMode::Read] },
            ],
            ..OsGrants::default()
        };
        assert!(grants.fs_allows_at(Path::new("/home/u/.orb/x.json"), FsMode::Write, &anchors));
        assert!(grants.fs_allows_at(Path::new("/work/p/src/a.ts"), FsMode::Read, &anchors));
        assert!(grants.fs_tree_allows_at(Path::new("/work/p/"), FsMode::Read, &anchors));
        assert!(!grants.fs_allows_at(Path::new("/home/u/.ssh/id"), FsMode::Write, &anchors), "control: outside the anchored tree");
        assert!(!grants.fs_allows_at(Path::new("/work/other/a"), FsMode::Read, &anchors), "control: a sibling of the start directory");
        let unknown = OsAnchors::default();
        assert!(!grants.fs_allows_at(Path::new("/home/u/.orb/x.json"), FsMode::Write, &unknown), "edge: an anchor the host does not know grants nothing");
        assert_eq!(anchors.expand("~/.orb/x").unwrap(), Some(PathBuf::from("/home/u/.orb/x")));
        assert_eq!(anchors.expand("./a").unwrap(), Some(PathBuf::from("/work/p/a")));
        assert_eq!(anchors.expand("/abs").unwrap(), None, "control: an absolute path is not anchored");
        assert_eq!(anchors.expand("rel").unwrap(), None, "control: a bare relative path keeps the working-directory rule");
        assert!(unknown.expand("~/x").is_err(), "edge: an anchor the host cannot place is an error");
    }

    fn boot(exe_dir: Option<&str>, env: &[(&str, &str)]) -> OsAnchors {
        OsAnchors {
            exe_dir: exe_dir.map(PathBuf::from),
            env: EnvSnapshot::from_pairs(env.iter().map(|(k, v)| (k.to_string(), v.to_string()))),
            ..OsAnchors::default()
        }
    }

    fn anchored(fs: Vec<AnchoredFsGrant>, exec: Vec<AnchoredExec>) -> OsGrants {
        OsGrants { anchored: AnchoredGrants { fs, exec, net: Vec::new() }, ..OsGrants::default() }
    }

    fn afs(anchor: GrantAnchor, glob: &str) -> AnchoredFsGrant {
        AnchoredFsGrant { anchor, glob: glob.into(), modes: vec![FsMode::Read] }
    }

    fn aexec(anchor: GrantAnchor, path: Option<&str>) -> AnchoredExec {
        AnchoredExec { anchor, path: path.map(str::to_string) }
    }

    #[test]
    fn an_anchored_grant_matches_nothing_until_a_host_resolves_it() {
        let g = anchored(
            vec![afs(GrantAnchor::ExeDir, "agent/**")],
            vec![aexec(GrantAnchor::ExeDir, Some("bun"))],
        );
        assert!(!g.is_empty());
        assert!(!g.fs_allows(Path::new("/opt/orb/agent/cli.js"), FsMode::Read));
        assert!(!g.fs_allows(Path::new("/agent/cli.js"), FsMode::Read), "control: the relative glob is not read as absolute");
        assert!(!g.exec_allows("bun") && !g.exec_allows("/opt/orb/bun"));
        let (resolved, problems) = g.resolve_anchors(&boot(Some("/opt/orb"), &[]));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(resolved.anchored.is_empty(), "resolution empties the anchored part");
        assert!(resolved.fs_allows(Path::new("/opt/orb/agent/cli.js"), FsMode::Read));
        assert!(resolved.fs_allows(Path::new("/opt/orb/agent"), FsMode::Read));
        assert!(!resolved.fs_allows(Path::new("/opt/orb/agent/cli.js"), FsMode::Write), "control: the mode still counts");
        assert!(!resolved.fs_allows(Path::new("/opt/orb/other"), FsMode::Read), "control: a sibling of the anchored tree");
        assert!(!resolved.fs_allows(Path::new("/opt/orb/agent/../../etc/passwd"), FsMode::Read), "edge: climbing out is refused");
        assert!(resolved.exec_allows("/opt/orb/bun"));
        assert!(!resolved.exec_allows("bun"), "control: the anchored command is the resolved path, not the bare name");
    }

    #[test]
    fn an_env_anchor_roots_at_its_boot_value_and_names_the_command_itself_without_a_path() {
        let g = anchored(
            vec![afs(GrantAnchor::Env("ORB_AGENT_DIR".into()), "**")],
            vec![aexec(GrantAnchor::Env("ORB_BUN_PATH".into()), None)],
        );
        let host = boot(None, &[("ORB_AGENT_DIR", "/srv/agent/"), ("ORB_BUN_PATH", "/usr/local/bin/bun")]);
        let (resolved, problems) = g.resolve_anchors(&host);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(resolved.fs[0].glob, "/srv/agent/**", "a trailing slash in the value is normalized");
        assert!(resolved.fs_allows(Path::new("/srv/agent/cli.js"), FsMode::Read));
        assert!(!resolved.fs_allows(Path::new("/srv/agentx/cli.js"), FsMode::Read), "control: a prefix-sharing sibling");
        assert_eq!(resolved.exec, ["/usr/local/bin/bun"]);
        let under = anchored(Vec::new(), vec![aexec(GrantAnchor::Env("ORB_HOME".into()), Some("bin/bun"))]);
        let (resolved, _) = under.resolve_anchors(&boot(None, &[("ORB_HOME", "/o")]));
        assert_eq!(resolved.exec, ["/o/bin/bun"]);
    }

    #[test]
    fn an_unset_relative_or_widening_anchor_grants_nothing_and_is_reported() {
        let env = |name: &str| GrantAnchor::Env(name.into());
        let g = anchored(
            vec![afs(env("UNSET"), "**"), afs(env("EMPTY"), "**"), afs(env("REL"), "**"), afs(env("WILD"), "**")],
            vec![aexec(env("UNSET"), None), aexec(GrantAnchor::ExeDir, Some("bun"))],
        );
        let host = boot(None, &[("EMPTY", ""), ("REL", "agent"), ("WILD", "/a/*")]);
        let (resolved, problems) = g.resolve_anchors(&host);
        assert!(resolved.fs.is_empty() && resolved.exec.is_empty(), "nothing is widened: {resolved:?}");
        assert!(!resolved.fs_allows(Path::new("/x"), FsMode::Read));
        let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["anchored.fs[0]", "anchored.fs[1]", "anchored.fs[2]", "anchored.fs[3]", "anchored.exec[0]", "anchored.exec[1]"]);
        assert!(problems[0].message.contains("unset"), "{}", problems[0].message);
        assert!(problems[2].message.contains("not an absolute path"), "{}", problems[2].message);
        assert!(problems[3].message.contains("glob character"), "{}", problems[3].message);
        assert!(problems[5].message.contains("executable"), "edge: a host that cannot place exe-dir: {}", problems[5].message);
        let plain = OsGrants { exec: vec!["bun".into()], ..g.clone() };
        let (kept, _) = plain.resolve_anchors(&host);
        assert_eq!(kept.exec, ["bun"], "control: plain entries pass through unresolved anchors untouched");
    }

    #[test]
    fn a_variable_set_after_boot_never_moves_an_env_anchor() {
        let host = boot(None, &[]);
        std::env::set_var("ORB_GRANT_ANCHOR_TEST_LATE", "/late");
        let g = anchored(vec![afs(GrantAnchor::Env("ORB_GRANT_ANCHOR_TEST_LATE".into()), "**")], Vec::new());
        let (resolved, problems) = g.resolve_anchors(&host);
        std::env::remove_var("ORB_GRANT_ANCHOR_TEST_LATE");
        assert!(resolved.fs.is_empty() && problems.len() == 1);
        assert!(!format!("{:?}", boot(None, &[("SECRET", "hunter2")])).contains("hunter2"), "the snapshot's Debug names variables only");
    }

    #[test]
    fn anchored_entries_round_trip_split_into_atoms_and_report_bad_shapes() {
        let g = anchored(
            vec![AnchoredFsGrant { anchor: GrantAnchor::ExeDir, glob: "agent/**".into(), modes: vec![FsMode::Read, FsMode::Meta] }],
            vec![aexec(GrantAnchor::Env("ORB_BUN_PATH".into()), None), aexec(GrantAnchor::ExeDir, Some("bun"))],
        );
        let wire = serde_json::to_value(&g).unwrap();
        assert_eq!(
            wire["anchored"],
            json!({"fs": [{"anchor": "exe-dir", "glob": "agent/**", "modes": ["read", "meta"]}],
                   "exec": [{"anchor": {"env": "ORB_BUN_PATH"}}, {"anchor": "exe-dir", "path": "bun"}]})
        );
        assert_eq!(serde_json::from_value::<OsGrants>(wire).unwrap(), g);
        assert!(serde_json::to_value(OsGrants::default()).unwrap().get("anchored").is_none(), "control: no anchored key when empty");
        let labels: Vec<String> = g.atoms().into_iter().map(|a| a.label).collect();
        assert_eq!(labels, [r#"fs exe-dir "agent/**" read"#, r#"fs exe-dir "agent/**" meta"#, "exec env ORB_BUN_PATH", r#"exec exe-dir "bun""#]);
        assert!(g.entry_problems().is_empty());
        let mut merged = OsGrants::default();
        merged.union(&g);
        merged.union(&g);
        assert_eq!(merged, g, "union is idempotent over anchored entries");
        let bad = anchored(
            vec![
                afs(GrantAnchor::ExeDir, "/abs/**"),
                afs(GrantAnchor::ExeDir, "../up/**"),
                afs(GrantAnchor::ExeDir, "~/x"),
                AnchoredFsGrant { anchor: GrantAnchor::ExeDir, glob: "a".into(), modes: vec![] },
                afs(GrantAnchor::Env("A=B".into()), "**"),
            ],
            vec![aexec(GrantAnchor::ExeDir, None), aexec(GrantAnchor::ExeDir, Some("../bun")), aexec(GrantAnchor::ExeDir, Some("b*"))],
        );
        let paths: Vec<String> = bad.entry_problems().into_iter().map(|p| p.path).collect();
        assert_eq!(
            paths,
            ["anchored.fs[0]", "anchored.fs[1]", "anchored.fs[2]", "anchored.fs[3]", "anchored.fs[4]", "anchored.exec[0]", "anchored.exec[1]", "anchored.exec[2]"]
        );
    }

    #[test]
    fn an_anchored_path_argument_needs_an_anchored_grant_on_the_same_anchor() {
        let cwd_docs = anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new());
        let read = call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": "docs/Almadar_Gaps.md"}])).expect("static");
        assert_eq!(read, vec![OsRequirement::FsAnchored { anchor: GrantAnchor::Cwd, path: "docs/Almadar_Gaps.md".into(), mode: FsMode::Read }]);
        assert!(cwd_docs.allows(&read[0]));
        let outside = call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": "src/a.rs"}])).unwrap();
        assert!(!cwd_docs.allows(&outside[0]), "control: outside the anchored glob");
        let escape = call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": "docs/../../etc/passwd"}])).unwrap();
        assert!(!cwd_docs.allows(&escape[0]), "edge: climbing above the anchor");
        let other = call(OsOp::ReadFile, json!([{"anchor": "exe-dir", "path": "docs/a.md"}])).unwrap();
        assert!(!cwd_docs.allows(&other[0]), "control: another anchor is another place");
        assert!(!cwd_docs.allows(&fs_req("docs/a.md", FsMode::Read)), "control: a plain path is never covered by an anchored grant");
        let write = call(OsOp::WriteFile, json!([{"anchor": "cwd", "path": "docs/a.md"}, "x"])).unwrap();
        assert!(!cwd_docs.allows(&write[0]), "control: the mode still counts");
        let env = anchored(vec![afs(GrantAnchor::Env("ORB_AGENT_DIR".into()), "**")], Vec::new());
        let entry = call(OsOp::Exists, json!([{"anchor": {"env": "ORB_AGENT_DIR"}, "path": "cli.js"}])).unwrap();
        assert!(env.allows(&entry[0]));
        let itself = call(OsOp::Exists, json!([{"anchor": {"env": "ORB_AGENT_DIR"}}])).unwrap();
        assert!(env.allows(&itself[0]), "a `**` glob covers the anchor itself");
        assert!(!env.allows(&call(OsOp::Exists, json!([{"anchor": {"env": "OTHER"}, "path": "cli.js"}])).unwrap()[0]), "control: another variable");
    }

    #[test]
    fn a_computed_anchored_path_needs_the_tree_under_its_literal_directory() {
        let tree = call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": ["str/concat", "docs/", "@entity.name"]}])).unwrap();
        assert_eq!(tree, vec![OsRequirement::FsAnchoredTree { anchor: GrantAnchor::Cwd, root: "docs/".into(), mode: FsMode::Read }]);
        assert!(anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new()).allows(&tree[0]));
        assert!(!anchored(vec![afs(GrantAnchor::Cwd, "src/*.md")], Vec::new()).allows(&tree[0]), "control: a glob that cannot reach under the directory");
        let whole = call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": "@entity.path"}])).unwrap();
        assert_eq!(whole, vec![OsRequirement::FsAnchoredTree { anchor: GrantAnchor::Cwd, root: String::new(), mode: FsMode::Read }]);
        assert!(anchored(vec![afs(GrantAnchor::Cwd, "**")], Vec::new()).allows(&whole[0]));
        assert!(anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new()).allows(&whole[0]), "a narrow glob covers it; the host checks the resolved path");
        assert!(!anchored(vec![afs(GrantAnchor::ExeDir, "docs/**")], Vec::new()).allows(&whole[0]), "control: another anchor");
        assert_eq!(call(OsOp::ReadFile, json!([{"anchor": "@entity.where", "path": "a"}])), Err(0), "the anchor itself must be a literal");
        assert_eq!(call(OsOp::ReadFile, json!([{"anchor": {"env": "@entity.n"}, "path": "a"}])), Err(0));
        assert_eq!(
            call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": "a", "extra": 1}])),
            Err(0),
            "control: an object of another shape is not an anchored path"
        );
    }

    #[test]
    fn an_anchored_glob_needs_read_on_the_tree_its_literal_prefix_reaches() {
        let tree = |root: &str| vec![OsRequirement::FsAnchoredTree { anchor: GrantAnchor::Cwd, root: root.into(), mode: FsMode::Read }];
        assert_eq!(call(OsOp::Glob, json!([{"anchor": "cwd", "path": "docs/**/*.md"}])), Ok(tree("docs/")));
        assert_eq!(call(OsOp::Glob, json!([{"anchor": "cwd", "path": "*/package.json"}])), Ok(tree("")));
        assert_eq!(call(OsOp::Glob, json!([{"anchor": "cwd", "path": "docs/a.md"}])), Ok(tree("docs/")), "edge: no wildcard reaches its directory");
        let computed = call(OsOp::Glob, json!([{"anchor": "cwd", "path": "@entity.pattern"}])).unwrap();
        assert_eq!(computed, tree(""));
        assert_eq!(call(OsOp::Glob, json!([{"anchor": "cwd", "path": ["str/concat", "src/", "@entity.p"]}])), Ok(tree("src/")));
        let manifests = anchored(vec![afs(GrantAnchor::Cwd, "**/package.json")], Vec::new());
        assert!(manifests.allows(&computed[0]), "a narrow anchored glob covers it; the host checks every result");
        assert!(!anchored(vec![afs(GrantAnchor::ExeDir, "**/package.json")], Vec::new()).allows(&computed[0]), "control: another anchor");
        let escape = call(OsOp::Glob, json!([{"anchor": "cwd", "path": "../**"}])).unwrap();
        assert!(!anchored(vec![afs(GrantAnchor::Cwd, "**")], Vec::new()).allows(&escape[0]), "edge: a glob climbing above the anchor is never covered");
        assert_eq!(call(OsOp::Glob, json!(["@entity.pattern"])), Err(0), "control: an unanchored computed glob stays a static error");
        assert_eq!(call(OsOp::Glob, json!([{"anchor": "@entity.where", "path": "*"}])), Err(0), "the anchor itself must be a literal");
    }

    #[test]
    fn a_computed_anchored_path_is_covered_by_a_narrow_glob_that_can_reach_under_its_directory() {
        let lolo = |modes: Vec<FsMode>| anchored(vec![AnchoredFsGrant { anchor: GrantAnchor::Cwd, glob: "**/*.lolo".into(), modes }], Vec::new());
        let read = call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": "@payload.path"}])).unwrap();
        let write = call(OsOp::WriteFile, json!([{"anchor": "cwd", "path": "@payload.path"}, "x"])).unwrap();
        let rw = lolo(vec![FsMode::Read, FsMode::Write]);
        assert!(rw.allows(&read[0]) && rw.allows(&write[0]), "the host enforces the glob on the resolved path");
        assert!(!lolo(vec![FsMode::Read]).allows(&write[0]), "control: the mode still counts");
        assert!(!anchored(vec![AnchoredFsGrant { anchor: GrantAnchor::ExeDir, glob: "**/*.lolo".into(), modes: vec![FsMode::Read] }], Vec::new()).allows(&read[0]), "control: another anchor");
        let under = call(OsOp::ReadFile, json!([{"anchor": "cwd", "path": ["str/concat", "src/", "@payload.name"]}])).unwrap();
        assert!(anchored(vec![afs(GrantAnchor::Cwd, "src/*.lolo")], Vec::new()).allows(&under[0]));
        assert!(rw.allows(&under[0]), "a leading `**` reaches under any directory");
        assert!(!anchored(vec![afs(GrantAnchor::Cwd, "docs/*.lolo")], Vec::new()).allows(&under[0]), "control: a sibling directory");
        assert!(!anchored(vec![afs(GrantAnchor::Cwd, "src")], Vec::new()).allows(&under[0]), "edge: a glob naming the directory itself reaches nothing under it");
        assert!(!anchored(Vec::new(), Vec::new()).allows(&read[0]), "control: no anchored glob at all");
    }

    #[test]
    fn spawn_takes_an_anchored_command_and_cwd() {
        let needs = call(OsOp::Spawn, json!([{"anchor": "exe-dir", "path": "bun"}, [], {"cwd": {"anchor": "cwd"}}])).unwrap();
        assert_eq!(
            needs,
            vec![
                OsRequirement::ExecAnchored { anchor: GrantAnchor::ExeDir, path: Some("bun".into()) },
                OsRequirement::FsAnchored { anchor: GrantAnchor::Cwd, path: String::new(), mode: FsMode::Read },
            ]
        );
        let g = anchored(Vec::new(), vec![aexec(GrantAnchor::ExeDir, Some("bun")), aexec(GrantAnchor::Env("ORB_BUN_PATH".into()), None)]);
        assert!(g.allows(&needs[0]));
        assert!(g.allows(&OsRequirement::ExecAnchored { anchor: GrantAnchor::Env("ORB_BUN_PATH".into()), path: None }));
        assert!(!g.allows(&OsRequirement::ExecAnchored { anchor: GrantAnchor::ExeDir, path: Some("node".into()) }), "control: another command");
        assert!(!g.allows(&OsRequirement::Exec { command: "bun".into() }), "control: the bare name is not granted");
        assert_eq!(call(OsOp::Spawn, json!([{"anchor": "exe-dir", "path": "@entity.cmd"}, []])), Err(0), "a command is never computed");
        assert_eq!(anchored_arg_positions(OsOp::Spawn), [0]);
        assert_eq!(anchored_arg_positions(OsOp::Rename), [0, 1]);
        assert!(anchored_arg_positions(OsOp::Getenv).is_empty());
    }

    fn anet(host: NetPart, port: NetPart, kinds: &[NetKind]) -> OsGrants {
        OsGrants {
            anchored: AnchoredGrants { net: vec![AnchoredNetGrant { host, port, kinds: kinds.to_vec() }], ..AnchoredGrants::default() },
            ..OsGrants::default()
        }
    }

    fn envp(name: &str) -> NetPart {
        NetPart::Anchor(GrantAnchor::Env(name.into()))
    }

    #[test]
    fn a_listen_on_env_host_and_port_needs_the_anchored_net_grant() {
        let g = anet(envp("HOST"), envp("PORT"), &[NetKind::Listen]);
        let needs = call(OsOp::TcpListen, json!([{"anchor": {"env": "HOST"}}, {"anchor": {"env": "PORT"}}])).unwrap();
        assert_eq!(needs, vec![OsRequirement::NetAnchored { host: envp("HOST"), port: envp("PORT"), kinds: vec![NetKind::Listen] }]);
        assert!(g.allows(&needs[0]));
        let tcp = call(OsOp::TcpConnect, json!([{"anchor": {"env": "HOST"}}, {"anchor": {"env": "PORT"}}])).unwrap();
        assert!(!g.allows(&tcp[0]), "control: another kind");
        let mixed = call(OsOp::TcpListen, json!(["0.0.0.0", {"anchor": {"env": "PORT"}}])).unwrap();
        assert_eq!(mixed, vec![OsRequirement::NetAnchored { host: NetPart::Literal("0.0.0.0".into()), port: envp("PORT"), kinds: vec![NetKind::Listen] }]);
        assert!(!g.allows(&mixed[0]), "control: a literal host is not the env host");
        assert!(anet(NetPart::Literal("0.0.0.0".into()), envp("PORT"), &[NetKind::Listen]).allows(&mixed[0]));
        assert!(!g.allows(&OsRequirement::Net { host: "localhost".into(), port: 80, kinds: vec![NetKind::Listen] }), "control: a literal target");
        assert_eq!(call(OsOp::TcpListen, json!([{"anchor": "cwd"}, 80])), Err(0), "a net part takes only an env anchor");
        assert_eq!(call(OsOp::TcpListen, json!([{"anchor": {"env": "H"}, "path": "x"}, 80])), Err(0));
        assert_eq!(anchored_net_args(OsOp::Sendto), Some((2, 3)));
        assert_eq!(anchored_net_args(OsOp::ReadFile), None);
    }

    #[test]
    fn an_anchored_net_grant_resolves_at_boot_and_an_unset_part_grants_nothing() {
        let g = anet(envp("HOST"), envp("PORT"), &[NetKind::Listen]);
        let (resolved, problems) = g.resolve_anchors(&boot(None, &[("HOST", "127.0.0.1"), ("PORT", "8080")]));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(resolved.net_allows("127.0.0.1", 8080, NetKind::Listen));
        assert!(!resolved.net_allows("127.0.0.1", 8081, NetKind::Listen), "control: another port");
        let (v6, _) = g.resolve_anchors(&boot(None, &[("HOST", "::1"), ("PORT", "9")]));
        assert!(v6.net_allows("::1", 9, NetKind::Listen));
        let (none, problems) = g.resolve_anchors(&boot(None, &[("HOST", "127.0.0.1")]));
        assert!(none.net.is_empty() && problems.len() == 1 && problems[0].message.contains("PORT is unset"), "{problems:?}");
        let (bad, problems) = g.resolve_anchors(&boot(None, &[("HOST", "h"), ("PORT", "http")]));
        assert!(bad.net.is_empty() && problems[0].message.contains("not a port number"));
        let shapes = OsGrants {
            anchored: AnchoredGrants {
                net: vec![
                    AnchoredNetGrant { host: NetPart::Literal("h".into()), port: NetPart::Literal("1".into()), kinds: vec![NetKind::Tcp] },
                    AnchoredNetGrant { host: NetPart::Anchor(GrantAnchor::Cwd), port: envp("P"), kinds: vec![NetKind::Tcp] },
                    AnchoredNetGrant { host: envp("H"), port: NetPart::Literal("x".into()), kinds: vec![NetKind::Tcp] },
                    AnchoredNetGrant { host: envp("H"), port: envp("P"), kinds: vec![] },
                    AnchoredNetGrant { host: envp("H"), port: envp("P"), kinds: vec![NetKind::Udp] },
                ],
                ..AnchoredGrants::default()
            },
            ..OsGrants::default()
        };
        let paths: Vec<String> = shapes.entry_problems().into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["anchored.net[0]", "anchored.net[1]", "anchored.net[2]", "anchored.net[3]"]);
        assert_eq!(g.atoms()[0].label, "net env HOST, env PORT listen");
    }

    #[test]
    fn a_host_resolves_an_anchored_path_under_its_anchor_or_refuses_it() {
        let host = OsAnchors { start: Some(PathBuf::from("/work/p")), ..boot(Some("/opt/orb"), &[("D", "/srv/a")]) };
        let at = |v: serde_json::Value| AnchoredPath::from_value(&serde_json::from_value::<Value>(v).unwrap()).expect("anchored");
        assert_eq!(host.resolve_path(&at(json!({"anchor": "cwd", "path": "docs/a.md"}))), Ok("/work/p/docs/a.md".into()));
        assert_eq!(host.resolve_path(&at(json!({"anchor": "exe-dir", "path": "agent/./cli.js"}))), Ok("/opt/orb/agent/cli.js".into()));
        assert_eq!(host.resolve_path(&at(json!({"anchor": {"env": "D"}}))), Ok("/srv/a".into()));
        assert_eq!(host.resolve_path(&at(json!({"anchor": "cwd", "path": "."}))), Ok("/work/p".into()), "`.` is the anchor itself");
        assert_eq!(host.resolve_path(&at(json!({"anchor": "cwd", "path": "./docs"}))), Ok("/work/p/docs".into()));
        assert!(host.resolve_path(&at(json!({"anchor": "cwd", "path": "~/x"}))).is_err(), "edge: a home form is not relative");
        assert!(host.resolve_path(&at(json!({"anchor": "cwd", "path": "../x"}))).is_err(), "edge: climbing above the anchor");
        assert!(host.resolve_path(&at(json!({"anchor": "cwd", "path": "/etc/passwd"}))).is_err(), "edge: an absolute path is not relative");
        assert!(host.resolve_path(&at(json!({"anchor": {"env": "UNSET"}, "path": "a"}))).is_err(), "edge: an unset variable");
        assert!(AnchoredPath::from_value(&Value::String("cwd".into())).is_none(), "control: a string is not an anchored path");
        let (resolved, problems) = anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new()).resolve_anchors(&host);
        assert!(problems.is_empty());
        assert!(resolved.fs_allows(Path::new("/work/p/docs/a.md"), FsMode::Read));
    }

    fn concat(pieces: &[&str]) -> SExpression {
        let mut items = vec![SExpression::String("str/concat".into())];
        items.extend(pieces.iter().map(|p| SExpression::String((*p).into())));
        SExpression::Array(items)
    }

    #[test]
    fn a_concat_with_a_literal_directory_prefix_needs_the_tree_and_anything_else_is_computed() {
        let tree = os_requirements(OsOp::ReadFile, &[concat(&["/data/", "sub/", "@payload.p"])]);
        assert_eq!(
            tree,
            Ok(vec![OsRequirement::FsTree { root: "/data/sub/".into(), mode: FsMode::Read }])
        );
        let open = os_requirements(OsOp::Open, &[concat(&["/data/", "@payload.p"]), SExpression::String("w".into())]);
        assert_eq!(
            open,
            Ok(vec![OsRequirement::FsTree { root: "/data/".into(), mode: FsMode::Write }])
        );
        for computed in [
            concat(&["/data", "@payload.p"]),
            concat(&["@payload.p", "/x"]),
            concat(&["@config.root", "/", "@payload.p"]),
        ] {
            assert_eq!(os_requirements(OsOp::ReadFile, &[computed]), Err(0));
        }
        let deferring = os_requirements_deferring(
            OsOp::ReadFile,
            &[concat(&["@config.root", "/", "@payload.p"])],
            &|p| p.as_str().is_some_and(|s| s.starts_with("@config.")),
        );
        assert_eq!(deferring, Ok(vec![]));
    }

    #[test]
    fn a_tree_is_covered_only_by_a_double_star_grant_that_contains_its_root() {
        let need = |root: &str| OsRequirement::FsTree { root: root.into(), mode: FsMode::Read };
        let g = fs_grants("/data/**", &[FsMode::Read]);
        assert!(g.allows(&need("/data/")));
        assert!(g.allows(&need("/data/sub/")));
        assert!(!g.allows(&need("/")));
        assert!(!g.allows(&need("/database/")));
        assert!(!g.allows(&need("/data/../etc/")));
        assert!(g.allows(&need("/data/../data/x/")));
        assert!(!g.allows(&need("data/")));
        assert!(!fs_grants("/data/*", &[FsMode::Read]).allows(&need("/data/")));
        assert!(!fs_grants("/data/**/x", &[FsMode::Read]).allows(&need("/data/x/")));
        assert!(!fs_grants("/data/**", &[FsMode::Write]).allows(&need("/data/")));
        assert!(fs_grants("/**", &[FsMode::Read]).allows(&need("/")));
    }
}
