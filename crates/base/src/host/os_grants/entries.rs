//! A grant set listed as atoms, and the entries it cannot honor.

use super::*;

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
        for (i, kind) in self.credentials.iter().enumerate() {
            out.push(GrantAtom {
                path: format!("credentials[{i}]"),
                label: format!("credentials {}", kind.name()),
                grants: OsGrants { credentials: vec![*kind], ..OsGrants::default() },
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
            match split_target(&g.target) {
                None => problem(format!("net[{i}]"), format!("{:?} is not `host:port`", g.target)),
                Some((host, _)) if host.contains('*') && host_pattern_suffix(host).is_none() => problem(
                    format!("net[{i}]"),
                    format!("{:?} is not a host pattern: only a leading `*.` followed by at least two literal labels (`*.example.com:443`)", g.target),
                ),
                Some(_) => {}
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
