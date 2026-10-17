//! The program's `os/` grants (`docs/Almadar_OS_Operators.md` §4): closed by default, matched by pure
//! total functions. The native answerer re-checks them after `realpath`; the validator reads the same data.

use crate::os::OsOp;
use crate::schema::sexpr::SExpression;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FsMode {
    Read,
    Write,
    Create,
    Remove,
    Meta,
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

named_kinds!(FsMode { Read => "read", Write => "write", Create => "create", Remove => "remove", Meta => "meta" });
named_kinds!(NetKind { Tcp => "tcp", Udp => "udp", Listen => "listen" });
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

/// Whether `glob` (absolute, no `..`) matches the already-normalized `path`.
fn glob_matches(glob: &str, path: &[String]) -> bool {
    if !glob.starts_with('/') {
        return false;
    }
    let pattern: Vec<&str> = glob
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if pattern.contains(&"..") {
        return false;
    }
    segments_match(&pattern, path)
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
        extend_missing(&mut self.exec, &other.exec);
        extend_missing(&mut self.env, &other.env);
        extend_missing(&mut self.stdio, &other.stdio);
        extend_missing(&mut self.sys, &other.sys);
        extend_missing(&mut self.desktop, &other.desktop);
    }

    /// Whether `path` may be used in `mode`. The path is matched lexically (`.`/`..` collapsed, refused when
    /// it escapes the root); symlinks are resolved by the caller, which re-checks the real path.
    pub fn fs_allows(&self, path: &Path, mode: FsMode) -> bool {
        let Some(segments) = normalized_segments(path) else {
            return false;
        };
        self.fs
            .iter()
            .any(|g| g.modes.contains(&mode) && glob_matches(&g.glob, &segments))
    }

    /// Whether every path under the directory `root` may be used in `mode`: a grant that lists the mode, ends in
    /// `/**` and matches `root` itself covers `root` and all it contains. Matched lexically like [`fs_allows`].
    pub fn fs_tree_allows(&self, root: &Path, mode: FsMode) -> bool {
        let Some(segments) = normalized_segments(root) else {
            return false;
        };
        self.fs.iter().any(|g| {
            g.modes.contains(&mode) && g.glob.ends_with("/**") && glob_matches(&g.glob, &segments)
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
    glob.starts_with('/') && !glob.split('/').any(|s| s == "..")
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c.is_control())
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
        for (i, name) in self.env.iter().enumerate() {
            if !valid_name(name) || name.contains('=') {
                problem(format!("env[{i}]"), format!("{name:?} is not a variable name"));
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
    Exec { command: String },
    Net { host: String, port: u16, kinds: Vec<NetKind> },
    Env { name: String },
    Stdio { kind: StdioKind },
    Sys { kind: SysKind },
    Desktop { kind: DesktopKind },
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
            OsRequirement::Exec { command } => self.exec_allows(command),
            OsRequirement::Net { host, port, kinds } => {
                kinds.iter().any(|kind| self.net_allows(host, *port, *kind))
            }
            OsRequirement::Env { name } => self.env_allows(name),
            OsRequirement::Stdio { kind } => self.stdio_allows(*kind),
            OsRequirement::Sys { kind } => self.sys_allows(*kind),
            OsRequirement::Desktop { kind } => self.desktop_allows(*kind),
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

enum FsTarget {
    Exact(String),
    Tree(String),
}

fn fs_target(args: &[SExpression], index: usize, deferred: Deferred) -> Result<Option<FsTarget>, usize> {
    if let Some(arg) = args.get(index) {
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
    let (Some(name), Some(port)) = (literal(args, host, deferred)?, literal_port(args, port, deferred)?) else {
        return Ok(Vec::new());
    };
    Ok(vec![OsRequirement::Net { host: name.to_string(), port, kinds: kinds.to_vec() }])
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
    if let Some(command) = literal(args, 0, deferred)? {
        needs.push(OsRequirement::Exec { command: command.to_string() });
    }
    if let Some(SExpression::Object(options)) = args.get(2) {
        for (key, value) in options {
            match (key.as_str(), value) {
                ("cwd", SExpression::String(path)) if !path.starts_with('@') => {
                    needs.push(OsRequirement::Fs { path: path.clone(), mode: FsMode::Read });
                }
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
/// `deferred` is true contributes no requirement instead of being reported as computed.
pub fn os_requirements_deferring(
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
                let tree = matches!(target, FsTarget::Tree(_));
                let path = match target {
                    FsTarget::Exact(p) | FsTarget::Tree(p) => p,
                };
                needs.extend(open_modes(flags).into_iter().map(|mode| {
                    fs_requirement(
                        if tree { FsTarget::Tree(path.clone()) } else { FsTarget::Exact(path.clone()) },
                        mode,
                    )
                }));
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
        Glob => Ok(literal(args, 0, deferred)?
            .map(|glob| OsRequirement::Fs { path: glob_prefix(glob), mode: FsMode::Read })
            .into_iter()
            .collect()),
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
        Close | Read | Write | Seek | Fsync | Cwd | Wait | Kill | Pid | Ppid | WatchProcess
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
