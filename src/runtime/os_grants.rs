//! The program's `os/` grants (`docs/Almadar_OS_Operators.md` §4): closed by default, matched by pure
//! total functions. The native answerer re-checks them after `realpath`; the validator reads the same data.

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
}
