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
    /// The running executable (`current_exe`): what the `exe` anchor names.
    pub exe: Option<PathBuf>,
    pub env: EnvSnapshot,
    /// Each declared bare command name resolved once through the boot `PATH`; a later `PATH` change moves nothing.
    pub tools: ToolPaths,
}

/// The boot `PATH` resolution of bare command names, memoized: a name resolves the first time it is asked for and
/// keeps that answer, found or not, for the life of the host.
#[cfg(feature = "std")]
#[derive(Clone, Default)]
pub struct ToolPaths(std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Result<PathBuf, String>>>>);

#[cfg(feature = "std")]
impl std::fmt::Debug for ToolPaths {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_map().entries(held.iter()).finish()
    }
}

#[cfg(feature = "std")]
impl PartialEq for ToolPaths {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
            || *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                == *other.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(feature = "std")]
impl Eq for ToolPaths {}

#[cfg(feature = "std")]
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The home directory a host may anchor `~/` to: an absolute UTF-8 path with no glob character, as the `env` anchor
/// demands. Anything else leaves `~` unavailable.
#[cfg(feature = "std")]
pub(super) fn usable_home(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(value?);
    let text = path.to_str()?;
    (path.is_absolute() && !text.contains(['*', '?'])).then_some(path)
}

#[cfg(feature = "std")]
impl OsAnchors {
    /// The process's own: `HOME` (`USERPROFILE` on Windows), its working directory, the directory of its executable
    /// and its environment, read once.
    pub fn of_process() -> Self {
        let home = usable_home(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")));
        Self {
            home,
            start: std::env::current_dir().ok(),
            exe_dir: std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)),
            exe: std::env::current_exe().ok(),
            env: EnvSnapshot::of_process(),
            tools: ToolPaths::default(),
        }
    }

    /// The `/`-rooted segments of the directory `anchor` names on this host, or why it names none: an unknown
    /// executable directory, an unset or empty variable, a value that is not an absolute path, or one holding a glob
    /// character (`*`, `?`), which would widen the grant.
    pub fn anchor_segments(&self, anchor: &GrantAnchor) -> Result<Vec<String>, String> {
        let dir = match anchor {
            GrantAnchor::Exe => return Err("exe names the executable file, not a directory".to_string()),
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

    /// The absolute path of the bare command `name` through the `PATH` of the boot environment, resolved the first time
    /// it is asked and kept: the first absolute `PATH` directory holding an executable file of that name. A name that
    /// finds none stays unavailable.
    pub fn resolve_tool(&self, name: &str) -> Result<PathBuf, String> {
        let mut held = self.tools.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        held.entry(name.to_string())
            .or_insert_with(|| {
                let path = self.env.get("PATH").filter(|p| !p.is_empty()).ok_or_else(|| format!("command {name:?} is unavailable: the boot environment has no PATH"))?;
                std::env::split_paths(path)
                    .filter(|dir| dir.is_absolute())
                    .map(|dir| dir.join(name))
                    .find(|candidate| is_executable_file(candidate))
                    .ok_or_else(|| format!("command {name:?} is unavailable: no executable of that name on the PATH at boot"))
            })
            .clone()
    }

    /// What boot resolved for the bare command `name`, or `None` when no program declared it: a name nobody declared is
    /// never looked up.
    pub fn declared_tool(&self, name: &str) -> Option<Result<PathBuf, String>> {
        self.tools.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(name).cloned()
    }

    /// The running executable, or why this host cannot name it.
    pub fn exe_path(&self) -> Result<String, String> {
        let exe = self.exe.as_ref().ok_or_else(|| "this host does not know its own executable".to_string())?;
        let text = exe.to_str().filter(|t| exe.is_absolute() && !t.contains(['*', '?']));
        text.map(str::to_string).ok_or_else(|| format!("exe ({}) is not an absolute UTF-8 path without glob characters", exe.display()))
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

/// An `exec` entry written as a path: absolute, with no `..` segment and no glob character.
pub(super) fn valid_absolute_command(text: &str) -> bool {
    text.starts_with('/') && !text.contains(['*', '?']) && !text.split('/').any(|s| s == "..")
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(feature = "std")]
pub(super) fn rooted(segments: &[String], rest: Option<&str>) -> String {
    let out = host_rooted(segments, rest);
    if out.starts_with('/') { out } else { format!("/{out}") }
}

/// [`rooted`] as the host names the path: a drive-headed list (`C:`, Windows) is `C:/...`, any other is `/...`.
pub(super) fn host_rooted(segments: &[String], rest: Option<&str>) -> String {
    let drive = segments.first().is_some_and(|s| matches!(s.as_bytes(), [l, b':'] if l.is_ascii_uppercase()));
    let mut out = if drive { segments.join("/") } else { format!("/{}", segments.join("/")) };
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
    /// An env anchor whose variable is unset is an optional override, not a mistake, when the program also declares a
    /// `~/` fs entry granting at least the same modes: that entry is the anchor choice the program wrote. A variable that
    /// is set but unusable, and every other anchor, still report.
    fn home_entry_stands_in(&self, anchor: &GrantAnchor, modes: &[FsMode], anchors: &OsAnchors) -> bool {
        let GrantAnchor::Env(name) = anchor else { return false };
        matches!(anchors.env.get(name), None | Some(""))
            && self.fs.iter().any(|f| matches!(anchor_of(&f.glob), Some((Anchor::Home, _))) && modes.iter().all(|m| f.modes.contains(m)))
    }

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
                Err(_) if self.home_entry_stands_in(&g.anchor, &g.modes, anchors) => {}
                Err(why) => problems.push(EntryProblem { path, message: format!("{} {:?} grants nothing: {why}", g.anchor.label(), g.glob) }),
            }
        }
        for (i, e) in self.anchored.exec.iter().enumerate() {
            let path = format!("anchored.exec[{i}]");
            if let Some(message) = anchored_exec_problem(e) {
                problems.push(EntryProblem { path, message });
                continue;
            }
            if e.anchor == GrantAnchor::Exe {
                match anchors.exe_path() {
                    Ok(exe) => added.exec.push(exe),
                    Err(why) => problems.push(EntryProblem { path, message: format!("exec exe grants nothing: {why}") }),
                }
                continue;
            }
            match anchors.anchor_segments(&e.anchor) {
                Ok(segments) => added.exec.push(host_rooted(&segments, e.path.as_deref())),
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
        resolved.exec.clear();
        for (i, command) in self.exec.iter().enumerate() {
            if valid_absolute_command(command) {
                added.exec.push(command.clone());
                continue;
            }
            match anchors.resolve_tool(command) {
                Ok(path) => match path.to_str() {
                    Some(text) => added.exec.push(text.to_string()),
                    None => problems.push(EntryProblem { path: format!("exec[{i}]"), message: format!("command {command:?} resolves to a path that is not UTF-8") }),
                },
                Err(why) => problems.push(EntryProblem { path: format!("exec[{i}]"), message: why }),
            }
        }
        resolved.union(&added);
        resolved.add_canonical_twins(anchors);
        (resolved, problems)
    }

    #[cfg(feature = "std")]
    /// Records, beside every `fs` grant whose literal directory prefix passes through a symlink, the same grant on
    /// the canonical (realpath) prefix, so a host that checks the resolved path of a request finds the grant that
    /// was declared on the unresolved one. The prefix that does not exist yet keeps its text under the canonical
    /// form of its deepest existing ancestor. Nothing else widens: a symlink to a path no grant names stays refused.
    fn add_canonical_twins(&mut self, anchors: &OsAnchors) {
        let twins: Vec<FsGrant> = self
            .fs
            .iter()
            .filter_map(|g| Some(FsGrant { glob: canonical_glob(&g.glob, anchors)?, modes: g.modes.clone() }))
            .collect();
        self.union(&OsGrants { fs: twins, ..OsGrants::default() });
    }
}

#[cfg(feature = "std")]
fn canonical_glob(glob: &str, anchors: &OsAnchors) -> Option<String> {
    let absolute = match anchors.expand(glob) {
        Ok(Some(path)) => path.to_str()?.to_string(),
        Ok(None) => glob.to_string(),
        Err(_) => return None,
    };
    let segments: Vec<&str> = absolute.split('/').filter(|s| !s.is_empty()).collect();
    let literal = segments.iter().take_while(|s| !s.contains(['*', '?'])).count();
    let mut base = PathBuf::from("/");
    base.extend(&segments[..literal]);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let canonical = loop {
        match std::fs::canonicalize(&base) {
            Ok(real) => break real,
            Err(_) => {
                tail.push(base.file_name()?.to_os_string());
                if !base.pop() {
                    return None;
                }
            }
        }
    };
    let mut real = canonical;
    real.extend(tail.iter().rev());
    let mut text = real.to_str()?.trim_end_matches('/').to_string();
    for segment in &segments[literal..] {
        text.push('/');
        text.push_str(segment);
    }
    (text != absolute.trim_end_matches('/')).then_some(text)
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
        Value::String(word) if word == "exe" => Some(GrantAnchor::Exe),
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
        SExpression::String(word) if word == "exe" => Some(GrantAnchor::Exe),
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
pub(super) fn anchored_keys<'a, K: AsRef<str> + 'a>(mut keys: impl Iterator<Item = &'a K>) -> bool {
    keys.all(|k| matches!(k.as_ref(), "anchor" | "path"))
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
        if anchored.anchor == GrantAnchor::Exe {
            return match &anchored.path {
                None => self.exe_path(),
                Some(_) => Err("exe names the executable itself and takes no path".to_string()),
            };
        }
        let base = self.anchor_segments(&anchored.anchor)?;
        let rest = match &anchored.path {
            None => Vec::new(),
            Some(path) => relative_segments(path).ok_or_else(|| {
                format!("{path:?} is not a path relative to {} that stays under it", anchored.anchor.label())
            })?,
        };
        let mut segments = base;
        segments.extend(rest);
        Ok(host_rooted(&segments, None))
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
        (GrantAnchor::Exe, Some(_)) => Some("exec exe names the running executable itself: it takes no path".to_string()),
        (GrantAnchor::Exe, None) => None,
        (GrantAnchor::ExeDir, None) => Some("exec exe-dir names a directory, not a command: give the command under it, e.g. exe-dir \"bun\"".to_string()),
        (_, Some(path)) if !valid_relative(path) || path.contains(['*', '?']) => {
            Some(format!("exec {} is not a relative command path without `..` or glob characters", anchored_exec_label(e)))
        }
        _ => None,
    }
}
