//! Path, glob and target matching, and what a grant set allows.

use super::*;

#[cfg(feature = "std")]
/// The absolute path as its `/`-separated segments with `.` dropped and `..` collapsed; `None` for a
/// relative path, a non-UTF-8 segment, a platform prefix, or a `..` that climbs above the root.
pub(super) fn normalized_segments(path: &Path) -> Option<Vec<String>> {
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

pub(super) fn segment_matches(pattern: &[char], text: &[char]) -> bool {
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

pub(super) fn segments_match(pattern: &[&str], path: &[String]) -> bool {
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
pub(super) enum Anchor {
    Root,
    Home,
    Start,
}

/// The anchor of `text` and the part after it; `None` for a bare relative path.
pub(super) fn anchor_of(text: &str) -> Option<(Anchor, &str)> {
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

#[cfg(feature = "std")]
/// [`normalized_segments`] for any anchored form: a home or start form is normalized under its anchor and refused
/// when a `..` climbs above it.
pub(super) fn anchored_segments(path: &Path) -> Option<(Anchor, Vec<String>)> {
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

#[cfg(feature = "std")]
/// Whether `glob` (anchored, no `..`) matches the already-normalized `path` under `anchor`.
pub(super) fn glob_matches(glob: &str, anchor: Anchor, path: &[String]) -> bool {
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
pub(super) fn relative_glob_matches(glob: &str, path: &[String]) -> bool {
    if !valid_relative(glob) {
        return false;
    }
    let pattern: Vec<&str> = glob.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    segments_match(&pattern, path)
}

/// Whether the anchor-relative `glob` (no `..`) can match some path strictly under the normalized directory `root`.
/// A computed path under `root` is then accepted statically and the host checks the glob on the resolved path.
pub(super) fn relative_glob_reaches_under(glob: &str, root: &[String]) -> bool {
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

/// The literal suffix of a host-pattern grant host (`*.notify.windows.com` gives `notify.windows.com`): a leading
/// `*.` and at least two more labels of letters, digits, `-` or `_`, the last not all digits. `None` for anything else,
/// including a `*` anywhere but the first label.
pub(super) fn host_pattern_suffix(host: &str) -> Option<&str> {
    let suffix = host.strip_prefix("*.")?;
    let labels: Vec<&str> = suffix.split('.').collect();
    let label = |l: &str| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    (labels.len() >= 2
        && labels.iter().all(|l| label(l))
        && !labels[labels.len() - 1].chars().all(|c| c.is_ascii_digit()))
    .then_some(suffix)
}

/// Whether a grant host covers `host`: an exact name (case-insensitive), or a pattern `*.<suffix>` that `host`
/// matches with exactly one non-empty label before `<suffix>` (the TLS wildcard rule: `*.x.y` covers `a.x.y`, not
/// `a.b.x.y` and not `x.y`). A pattern is never an exact name.
pub(super) fn grant_host_covers(grant_host: &str, host: &str) -> bool {
    if grant_host.contains('*') {
        return host_pattern_suffix(grant_host).is_some_and(|suffix| {
            let host = host.to_ascii_lowercase();
            host.strip_suffix(&suffix.to_ascii_lowercase())
                .and_then(|rest| rest.strip_suffix('.'))
                .is_some_and(|label| {
                    !label.is_empty()
                        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                })
        });
    }
    grant_host.eq_ignore_ascii_case(host)
}

pub(super) fn split_target(target: &str) -> Option<(&str, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then_some(())?;
    Some((host, port.parse().ok()?))
}

pub(super) fn extend_missing<T: Clone + PartialEq>(into: &mut Vec<T>, from: &[T]) {
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
        extend_missing(&mut self.credentials, &other.credentials);
        for (store, modes) in &other.kv {
            extend_missing(self.kv.entry(store.clone()).or_default(), modes);
        }
    }

    #[cfg(feature = "std")]
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

    #[cfg(feature = "std")]
    /// [`Self::fs_allows`] for a host holding an absolute `path`: also matched in its `~/` and `./` forms when it lies
    /// under the home or start directory the host knows, so an anchored grant covers it.
    pub fn fs_allows_at(&self, path: &Path, mode: FsMode, anchors: &OsAnchors) -> bool {
        anchors.forms(path).iter().any(|form| self.fs_allows(form, mode))
    }

    #[cfg(feature = "std")]
    /// [`Self::fs_tree_allows`] for a host holding an absolute `root`, as [`Self::fs_allows_at`].
    pub fn fs_tree_allows_at(&self, root: &Path, mode: FsMode, anchors: &OsAnchors) -> bool {
        anchors.forms(root).iter().any(|form| self.fs_tree_allows(form, mode))
    }

    #[cfg(feature = "std")]
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
                    .is_some_and(|(h, p)| p == port && grant_host_covers(h, host))
        })
    }

    /// Whether a grant of `kind` names `host` at any port: the check for a TLS server name, which carries no port.
    pub fn net_names_host(&self, host: &str, kind: NetKind) -> bool {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        self.net.iter().any(|g| {
            g.kinds.contains(&kind) && split_target(&g.target).is_some_and(|(h, _)| grant_host_covers(h, host))
        })
    }

    /// Whether a grant of one of `kinds` is a host pattern at `port`: what covers a call whose host is chosen per
    /// call. The host itself is checked by [`Self::net_allows`] when the call runs.
    pub fn net_pattern_allows(&self, port: u16, kinds: &[NetKind]) -> bool {
        self.net.iter().any(|g| {
            kinds.iter().any(|k| g.kinds.contains(k))
                && split_target(&g.target).is_some_and(|(h, p)| p == port && host_pattern_suffix(h).is_some())
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

    pub fn credentials_allow(&self, kind: CredentialKind) -> bool {
        self.credentials.contains(&kind)
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
