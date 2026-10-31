//! What an `os/` call requires from its literal arguments, and whether the grants allow it.

use super::*;

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
    /// A host chosen per call at a literal `port`: covered only by a host-pattern `net` grant (`*.example.com:443`) at
    /// that port; the host that arrives is checked against the pattern by the host when the call runs.
    NetPattern { port: u16, kinds: Vec<NetKind> },
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
            #[cfg(feature = "std")]
            OsRequirement::Fs { path, mode } => self.fs_allows(Path::new(path), *mode),
            #[cfg(feature = "std")]
            OsRequirement::FsTree { root, mode } => self.fs_tree_allows(Path::new(root), *mode),
            #[cfg(not(feature = "std"))]
            OsRequirement::Fs { .. } | OsRequirement::FsTree { .. } => false,
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
            OsRequirement::Net { host, port, kinds } => kinds.iter().any(|kind| {
                self.net_allows(host, *port, *kind)
                    || (host_pattern_suffix(host).is_some() && self.net_names_pattern(host, *port, *kind))
            }),
            OsRequirement::NetPattern { port, kinds } => self.net_pattern_allows(*port, kinds),
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
pub(super) type Deferred<'a> = &'a dyn Fn(&SExpression) -> bool;

pub(super) fn literal<'a>(args: &'a [SExpression], index: usize, deferred: Deferred) -> Result<Option<&'a str>, usize> {
    match args.get(index) {
        None => Ok(None),
        Some(arg) if deferred(arg) => Ok(None),
        Some(SExpression::String(s)) if !s.starts_with('@') => Ok(Some(s)),
        Some(_) => Err(index),
    }
}

pub(super) fn literal_port(args: &[SExpression], index: usize, deferred: Deferred) -> Result<Option<u16>, usize> {
    match args.get(index) {
        None => Ok(None),
        Some(arg) if deferred(arg) => Ok(None),
        Some(SExpression::Number(n)) if n.fract() == 0.0 && (0.0..=65535.0).contains(n) => {
            Ok(Some(*n as u16))
        }
        Some(_) => Err(index),
    }
}

pub(super) fn literal_int(args: &[SExpression], index: usize, deferred: Deferred) -> Result<Option<u64>, usize> {
    match args.get(index) {
        None => Ok(None),
        Some(arg) if deferred(arg) => Ok(None),
        Some(SExpression::Number(n)) if n.fract() == 0.0 && *n >= 0.0 && *n <= u64::MAX as f64 => Ok(Some(*n as u64)),
        Some(_) => Err(index),
    }
}

pub(super) fn mmio_need(
    args: &[SExpression],
    (base, offset, width): (usize, usize, usize),
    kind: DevKind,
    deferred: Deferred,
) -> Result<Vec<OsRequirement>, usize> {
    let (base, offset, width) = (literal_int(args, base, deferred)?, literal_int(args, offset, deferred)?, literal_int(args, width, deferred)?);
    Ok(match (base, offset, width) {
        (Some(base), Some(offset), Some(width)) => {
            vec![OsRequirement::Dev { resource: DevResource::Mmio { base, offset, width }, kind }]
        }
        _ => Vec::new(),
    })
}

pub(super) fn irq_need(args: &[SExpression], line: usize, kind: DevKind, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    Ok(literal_int(args, line, deferred)?
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
pub(super) enum FsTarget {
    Exact(String),
    Tree(String),
    AnchoredExact(GrantAnchor, String),
    AnchoredTree(GrantAnchor, String),
}

/// The target of an anchored path argument: its literal `path` exactly, a `str/concat` `path` as the tree under its
/// literal directory prefix, or any other computed `path` as the anchor's whole tree. A `path` that depends on a
/// deferred piece reaches only the tree under its leading literal directory; with none it is not known before the
/// finished program, so it has no target (`None`).
pub(super) fn anchored_target(anchor: GrantAnchor, path: Option<&SExpression>, deferred: Deferred) -> Option<FsTarget> {
    match path {
        None => Some(FsTarget::AnchoredExact(anchor, String::new())),
        Some(SExpression::String(p)) if !p.starts_with('@') => Some(FsTarget::AnchoredExact(anchor, p.clone().into_inner())),
        Some(expr) => {
            let depends_on_deferred = deferred(expr)
                || matches!(expr, SExpression::Array(arr)
                    if arr.first().and_then(SExpression::as_str) == Some("str/concat") && arr[1..].iter().any(deferred));
            let root: String = if depends_on_deferred {
                let SExpression::Array(arr) = expr else {
                    return None;
                };
                let leading: String = arr[1..]
                    .iter()
                    .map_while(|piece| match piece {
                        SExpression::String(s) if !s.starts_with('@') => Some(s.as_str()),
                        _ => None,
                    })
                    .collect();
                let root = leading.rfind('/').map(|at| leading[..=at].to_string()).unwrap_or_default();
                if root.is_empty() {
                    return None;
                }
                root
            } else {
                concat_static_prefix(expr, deferred)
                    .map(|prefix| prefix.iter().filter_map(SExpression::as_str).collect())
                    .unwrap_or_default()
            };
            Some(FsTarget::AnchoredTree(anchor, root))
        }
    }
}

pub(super) fn fs_target(args: &[SExpression], index: usize, deferred: Deferred) -> Result<Option<FsTarget>, usize> {
    if let Some(arg) = args.get(index) {
        if let Some((anchor, path)) = anchored_sexpr(arg).map_err(|()| index)? {
            return Ok(anchored_target(anchor, path, deferred));
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

pub(super) fn fs_requirement(target: FsTarget, mode: FsMode) -> OsRequirement {
    match target {
        FsTarget::Exact(path) => OsRequirement::Fs { path, mode },
        FsTarget::Tree(root) => OsRequirement::FsTree { root, mode },
        FsTarget::AnchoredExact(anchor, path) => OsRequirement::FsAnchored { anchor, path, mode },
        FsTarget::AnchoredTree(anchor, root) => OsRequirement::FsAnchoredTree { anchor, root, mode },
    }
}

pub(super) fn fs_need(args: &[SExpression], index: usize, mode: FsMode, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    Ok(fs_target(args, index, deferred)?
        .map(|target| fs_requirement(target, mode))
        .into_iter()
        .collect())
}

pub(super) fn net_need(
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
        let name = match literal(args, host, deferred) {
            Ok(name) => name,
            Err(at) => {
                return match literal_port(args, port, deferred) {
                    Ok(Some(port)) => Ok(vec![OsRequirement::NetPattern { port, kinds: kinds.to_vec() }]),
                    _ => Err(at),
                };
            }
        };
        let (Some(name), Some(port)) = (name, literal_port(args, port, deferred)?) else {
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
pub(super) fn glob_prefix(glob: &str) -> String {
    let fixed: Vec<&str> = glob
        .split('/')
        .filter(|s| !s.is_empty())
        .take_while(|s| !s.contains(['*', '?']))
        .collect();
    format!("/{}", fixed.join("/"))
}

/// An anchored glob reads the tree its literal directory prefix reaches (the segments before the first wildcard,
/// never the last one): a narrow anchored grant that can match under it covers it, and the host checks every match.
pub(super) fn anchored_glob_need(anchor: GrantAnchor, pattern: Option<&SExpression>, deferred: Deferred) -> Option<OsRequirement> {
    let written = match pattern {
        Some(SExpression::String(p)) if !p.starts_with('@') => p.clone(),
        Some(expr) => (match anchored_target(anchor.clone(), Some(expr), deferred)? {
            FsTarget::AnchoredTree(_, root) => format!("{root}*"),
            _ => String::new(),
        }).into(),
        None => String::new().into(),
    };
    let mut segments: Vec<&str> = written.split('/').collect();
    segments.pop();
    let root: String = segments
        .into_iter()
        .take_while(|s| !s.contains(['*', '?']))
        .map(|s| format!("{s}/"))
        .collect();
    Some(OsRequirement::FsAnchoredTree { anchor, root, mode: FsMode::Read })
}

pub(super) fn open_modes(flags: &str) -> Vec<FsMode> {
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

pub(super) fn spawn_needs(args: &[SExpression], command: usize, options: usize, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    let mut needs = Vec::new();
    match args.get(command).map(anchored_sexpr) {
        Some(Err(())) => return Err(command),
        Some(Ok(Some((anchor, path)))) => match path {
            None => needs.push(OsRequirement::ExecAnchored { anchor, path: None }),
            Some(SExpression::String(p)) if !p.starts_with('@') => {
                needs.push(OsRequirement::ExecAnchored { anchor, path: Some(p.clone().into_inner()) })
            }
            Some(_) => return Err(command),
        },
        _ => {
            if let Some(command) = literal(args, command, deferred)? {
                needs.push(OsRequirement::Exec { command: command.to_string() });
            }
        }
    }
    if let Some(SExpression::Object(option_map)) = args.get(options) {
        for (key, value) in option_map.iter() {
            match (key.as_str(), value) {
                ("cwd", SExpression::String(path)) if !path.starts_with('@') => {
                    needs.push(OsRequirement::Fs { path: path.clone().into_inner(), mode: FsMode::Read });
                }
                ("cwd", SExpression::Object(_)) => match anchored_sexpr(value) {
                    Ok(Some((anchor, path))) => {
                        needs.extend(anchored_target(anchor, path, deferred).map(|target| fs_requirement(target, FsMode::Read)))
                    }
                    _ => return Err(options),
                },
                ("cwd", SExpression::Null) => {}
                ("cwd", value) if deferred(value) => {}
                ("cwd", _) => return Err(options),
                ("env", SExpression::Object(vars)) => {
                    for (name, value) in vars.iter() {
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
pub(super) fn literal_list_pick(expr: &SExpression) -> Option<Vec<SExpression>> {
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
    if let Some(literal) = source.declared_knob_literal() {
        source = literal;
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
/// and the command of `os/spawn`, as the operator table's `grant` names them. A host resolves them to absolute
/// paths before the call runs; the `cwd` option of `os/spawn` may be one too.
pub fn anchored_arg_positions(op: OsOp) -> Vec<usize> {
    anchored_positions_of(os_grant_needs(op))
}

fn anchored_positions_of(needs: &[OsGrantNeed]) -> Vec<usize> {
    let mut at: Vec<usize> = needs
        .iter()
        .filter_map(|need| match need {
            OsGrantNeed::Fs { path, .. } | OsGrantNeed::FsOpen { path, .. } => Some(*path),
            OsGrantNeed::Exec { command, .. } => Some(*command),
            _ => None,
        })
        .collect();
    at.sort_unstable();
    at.dedup();
    at
}

/// The host and port positions of `op` that may be written as an `env` anchor (`{ anchor: { env: NAME } }`); a host
/// resolves them to the boot value (the port as a number) before the call runs.
pub fn anchored_net_args(op: OsOp) -> Option<(usize, usize)> {
    anchored_net_of(os_grant_needs(op))
}

fn anchored_net_of(needs: &[OsGrantNeed]) -> Option<(usize, usize)> {
    needs.iter().find_map(|need| match need {
        OsGrantNeed::Net { host, port: Some(port), .. } => Some((*host, *port)),
        _ => None,
    })
}

#[cfg(feature = "std")]
/// The call's arguments with every anchored path (`{ anchor, path }`) at a path or command position, the anchored
/// `cwd` of `os/spawn`, and every `env`-anchored host or port replaced by what it names on this host. The grant
/// check then runs on the plain value against the boot-resolved grants.
pub fn resolve_anchored_args(op: OsOp, args: &[Value], anchors: &OsAnchors) -> Result<Vec<Value>, OsError> {
    let refuse = |why: String| OsError::new(OsErrorKind::Denied, format!("{}: {why}", op.name()));
    let needs = try_os_grant_needs(op)
        .map_err(|e| OsError::new(OsErrorKind::Unsupported, format!("{}: {}: {e}", op.name(), e.code())))?;
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
                None => anchors.resolve_path(&anchored).map(|p| Some(Value::string(p))).map_err(refuse),
            },
            None => Ok(None),
        }
    };
    let mut out = args.to_vec();
    for at in anchored_positions_of(needs) {
        if let Some(value) = out.get(at) {
            if let Some(resolved) = path(value)? {
                out[at] = resolved;
            }
        }
    }
    for need in needs {
        let OsGrantNeed::Exec { options: at, .. } = need else { continue };
        if let Some(Value::Object(options)) = out.get_mut(*at) {
            if let Some(cwd) = options.get("cwd") {
                if let Some(resolved) = path(cwd)? {
                    options.insert("cwd".into(), resolved);
                }
            }
        }
    }
    if let Some((host_at, port_at)) = anchored_net_of(needs) {
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
                Value::string(text)
            };
        }
    }
    Ok(out)
}

pub(super) fn kv_need(args: &[SExpression], store: usize, mode: KvMode, deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    Ok(literal(args, store, deferred)?
        .map(|store| OsRequirement::Kv { store: store.to_string(), mode })
        .into_iter()
        .collect())
}

pub(super) fn requirements_of(
    op: OsOp,
    args: &[SExpression],
    deferred: &dyn Fn(&SExpression) -> bool,
) -> Result<Vec<OsRequirement>, usize> {
    let mut needs = Vec::new();
    for need in os_grant_needs(op) {
        needs.extend(need_of(need, args, deferred)?);
    }
    Ok(needs)
}

pub(super) fn need_of(need: &OsGrantNeed, args: &[SExpression], deferred: Deferred) -> Result<Vec<OsRequirement>, usize> {
    match need {
        OsGrantNeed::Fs { access, path } => fs_need(args, *path, *access, deferred),
        OsGrantNeed::FsGlob { glob } => match args.get(*glob).map(anchored_sexpr).transpose().map_err(|()| *glob)?.flatten() {
            Some((anchor, pattern)) => Ok(anchored_glob_need(anchor, pattern, deferred).into_iter().collect()),
            None if matches!(args.get(*glob), Some(SExpression::Array(a)) if a.first().and_then(SExpression::as_str) == Some("str/concat")) => {
                fs_need(args, *glob, FsMode::Read, deferred)
            }
            None => Ok(literal(args, *glob, deferred)?
                .map(|text| OsRequirement::Fs { path: glob_prefix(text), mode: FsMode::Read })
                .into_iter()
                .collect()),
        },
        OsGrantNeed::FsOpen { path, flags } => {
            let mut needs = Vec::new();
            if let (Some(target), Some(flags)) = (fs_target(args, *path, deferred)?, literal(args, *flags, deferred)?) {
                needs.extend(open_modes(flags).into_iter().map(|mode| fs_requirement(target.clone(), mode)));
            }
            Ok(needs)
        }
        OsGrantNeed::Exec { command, options } => spawn_needs(args, *command, *options, deferred),
        OsGrantNeed::Env { name } => Ok(literal(args, *name, deferred)?
            .map(|name| OsRequirement::Env { name: name.to_string() })
            .into_iter()
            .collect()),
        OsGrantNeed::Stdio { access } => Ok(vec![OsRequirement::Stdio { kind: *access }]),
        OsGrantNeed::Net { access, host, port: Some(port) } => net_need(args, *host, *port, access, deferred),
        OsGrantNeed::Net { access, host, port: None } => Ok(literal(args, *host, deferred)?
            .map(|name| OsRequirement::Net { host: name.to_string(), port: 0, kinds: access.clone() })
            .into_iter()
            .collect()),
        OsGrantNeed::Sys { access } => Ok(vec![OsRequirement::Sys { kind: *access }]),
        OsGrantNeed::Desktop { access } => Ok(vec![OsRequirement::Desktop { kind: *access }]),
        OsGrantNeed::Kv { access, store } => kv_need(args, *store, *access, deferred),
        OsGrantNeed::Mmio { access, base, offset, width } => mmio_need(args, (*base, *offset, *width), *access, deferred),
        OsGrantNeed::Irq { access, line } => irq_need(args, *line, *access, deferred),
    }
}
