//! Grant atoms, entry problems, and resolving anchored entries against the host's anchors.

use super::*;

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

pub(super) fn valid_glob(glob: &str) -> bool {
    anchor_of(glob).is_some() && !glob.split('/').any(|s| s == "..")
}

/// A grant-relative path: not anchored itself and never climbing with `..`.
pub(super) fn valid_relative(text: &str) -> bool {
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
    #[cfg(feature = "std")]
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

#[cfg(feature = "std")]
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

#[cfg(feature = "std")]
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
    pub(super) fn forms(&self, path: &Path) -> Vec<PathBuf> {
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

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(feature = "std")]
pub(super) fn rooted(segments: &[String], rest: Option<&str>) -> String {
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
    #[cfg(feature = "std")]
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
pub(super) fn relative_segments(path: &str) -> Option<Vec<String>> {
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

pub(super) fn anchor_from_value(value: &Value) -> Option<GrantAnchor> {
    match value {
        Value::String(word) if word == "exe-dir" => Some(GrantAnchor::ExeDir),
        Value::String(word) if word == "cwd" => Some(GrantAnchor::Cwd),
        Value::Object(map) if map.len() == 1 => match map.get("env") {
            Some(Value::String(name)) => Some(GrantAnchor::Env(name.clone().into_inner())),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn anchor_from_sexpr(expr: &SExpression) -> Option<GrantAnchor> {
    match expr {
        SExpression::String(word) if word == "exe-dir" => Some(GrantAnchor::ExeDir),
        SExpression::String(word) if word == "cwd" => Some(GrantAnchor::Cwd),
        SExpression::Object(map) if map.len() == 1 => match map.get("env") {
            Some(SExpression::String(name)) if !name.starts_with('@') => Some(GrantAnchor::Env(name.clone().into_inner())),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `object` has exactly the keys of an anchored path: `anchor` and, optionally, `path`.
pub(super) fn anchored_keys<'a>(mut keys: impl Iterator<Item = &'a String>) -> bool {
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
        Some(Self { anchor, path: path.map(|v| v.into_inner()) })
    }
}

/// An argument written as an anchored path: its literal anchor and its `path` expression (absent for the anchor
/// itself). `Ok(None)` when the argument is not an object of that shape; `Err(())` when it is one whose anchor is
/// not a literal anchor.
pub(super) fn anchored_sexpr(expr: &SExpression) -> Result<Option<(GrantAnchor, Option<&SExpression>)>, ()> {
    let SExpression::Object(map) = expr else { return Ok(None) };
    if !map.contains_key("anchor") || !anchored_keys(map.keys()) {
        return Ok(None);
    }
    let anchor = map.get("anchor").and_then(anchor_from_sexpr).ok_or(())?;
    Ok(Some((anchor, map.get("path").filter(|p| !matches!(p, SExpression::Null)))))
}

#[cfg(feature = "std")]
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

#[cfg(feature = "std")]
pub(super) fn net_target(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(super) fn anchored_net_problem(g: &AnchoredNetGrant) -> Option<String> {
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

#[cfg(feature = "std")]
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

pub(super) fn anchored_exec_label(e: &AnchoredExec) -> String {
    match &e.path {
        Some(path) => format!("{} {path:?}", e.anchor.label()),
        None => e.anchor.label(),
    }
}

pub(super) fn anchored_exec_problem(e: &AnchoredExec) -> Option<String> {
    match (&e.anchor, &e.path) {
        (GrantAnchor::ExeDir, None) => Some("exec exe-dir names a directory, not a command: give the command under it, e.g. exe-dir \"bun\"".to_string()),
        (_, Some(path)) if !valid_relative(path) || path.contains(['*', '?']) => {
            Some(format!("exec {} is not a relative command path without `..` or glob characters", anchored_exec_label(e)))
        }
        _ => None,
    }
}
