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
    let mmio = |offset, width| DevResource::Mmio {
        base: 0x0900_0000,
        offset,
        width,
    };
    assert!(g.dev_allows(&mmio(0x18, 4), DevKind::Read));
    assert!(
        g.dev_allows(&mmio(0xffc, 4), DevKind::Write),
        "the last word of the window"
    );
    assert!(
        !g.dev_allows(&mmio(0xffd, 4), DevKind::Read),
        "control: one byte past the window"
    );
    assert!(
        !g.dev_allows(
            &DevResource::Mmio {
                base: 0x0900_1000,
                offset: 0,
                width: 4
            },
            DevKind::Read
        ),
        "control: another device"
    );
    assert!(g.dev_allows(&DevResource::Irq { line: 33 }, DevKind::Ack));
    assert!(
        !g.dev_allows(&DevResource::Irq { line: 34 }, DevKind::Wait),
        "control: another line"
    );
    assert!(
        !g.dev_allows(&DevResource::Irq { line: 33 }, DevKind::Read),
        "control: a register kind on an irq"
    );
    assert!(
        !OsGrants::default().dev_allows(&mmio(0, 1), DevKind::Read),
        "control: closed by default"
    );
    assert!(g.entry_problems().is_empty());
}

#[test]
fn a_dev_target_that_names_no_window_or_line_is_an_entry_problem() {
    let g = OsGrants {
        dev: vec![
            DevGrant {
                target: "mmio:0x10".into(),
                kinds: vec![DevKind::Read],
            },
            DevGrant {
                target: "irq:x".into(),
                kinds: vec![DevKind::Wait],
            },
            DevGrant {
                target: "irq:5".into(),
                kinds: vec![],
            },
            DevGrant {
                target: "mmio:0x10+0".into(),
                kinds: vec![DevKind::Read],
            },
        ],
        ..OsGrants::default()
    };
    let paths: Vec<String> = g.entry_problems().into_iter().map(|p| p.path).collect();
    assert_eq!(paths, vec!["dev[0]", "dev[1]", "dev[2]", "dev[3]"]);
}

#[test]
fn device_calls_need_their_window_or_line() {
    let call = |op, args: serde_json::Value| {
        os_requirements(
            op,
            &serde_json::from_value::<Vec<SExpression>>(args).expect("args"),
        )
    };
    let mmio = |offset, width, kind| OsRequirement::Dev {
        resource: DevResource::Mmio {
            base: 0x0900_0000,
            offset,
            width,
        },
        kind,
    };
    assert_eq!(
        call(OsOp::MmioRead, json!([150994944, 24, 4])),
        Ok(vec![mmio(24, 4, DevKind::Read)])
    );
    assert_eq!(
        call(OsOp::MmioWrite, json!([150994944, 0, 1, 65])),
        Ok(vec![mmio(0, 1, DevKind::Write)])
    );
    assert_eq!(
        call(OsOp::IrqWait, json!([33])),
        Ok(vec![OsRequirement::Dev {
            resource: DevResource::Irq { line: 33 },
            kind: DevKind::Wait
        }])
    );
    assert_eq!(
        call(OsOp::IrqAck, json!([33])),
        Ok(vec![OsRequirement::Dev {
            resource: DevResource::Irq { line: 33 },
            kind: DevKind::Ack
        }])
    );
    assert_eq!(
        call(OsOp::MmioRead, json!([150994944, "@entity.offset", 4])),
        Err(1),
        "control: a computed offset is not literal"
    );
    let deferred =
        |arg: &SExpression| matches!(arg, SExpression::String(s) if s.starts_with("@config."));
    let args: Vec<SExpression> =
        serde_json::from_value(json!(["@config.base", 24, 4])).expect("args");
    assert_eq!(
        os_requirements_deferring(OsOp::MmioRead, &args, &deferred),
        Ok(vec![]),
        "a config base is checked by the host"
    );
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
    OsRequirement::Fs {
        path: path.to_string(),
        mode,
    }
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
            OsRequirement::ExecAnchored {
                anchor: GrantAnchor::Env("ORB_BUN_PATH".into()),
                path: None
            },
            OsRequirement::ExecAnchored {
                anchor: GrantAnchor::ExeDir,
                path: Some("bun".into())
            },
            OsRequirement::Exec {
                command: "bun".into()
            },
        ])
    );
    assert_eq!(
        call(
            OsOp::Spawn,
            json!([["array/nth", [{ "anchor": "exe-dir", "path": "@entity.p" }, "bun"], "@entity.i"], []])
        ),
        Err(0),
        "control: a computed anchored path in the list is still not static"
    );
    assert_eq!(
        call(
            OsOp::Spawn,
            json!([["array/nth", ["let", [["knob_cmds", ["list", { "anchor": "exe-dir", "path": "bun" }, "bun"], "<declared type>"]], "@knob_cmds"], "@entity.cmdCursor"], []])
        ),
        Ok(vec![
            OsRequirement::ExecAnchored {
                anchor: GrantAnchor::ExeDir,
                path: Some("bun".into())
            },
            OsRequirement::Exec {
                command: "bun".into()
            },
        ]),
        "a knob's literal bound by its declaring let is the same list"
    );
    assert_eq!(
        call(
            OsOp::Spawn,
            json!([
                [
                    "array/nth",
                    [
                        "let",
                        [["knob_cmds", ["list", "bun"], "<declared type>"]],
                        "@other"
                    ],
                    "@entity.cmdCursor"
                ],
                []
            ])
        ),
        Err(0),
        "control: a let that does not read its binding back is not a static list"
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
    assert_eq!(
        call(OsOp::ReadFile, json!(["/data/a"])),
        Ok(vec![fs_req("/data/a", FsMode::Read)])
    );
    assert_eq!(
        call(OsOp::WriteFile, json!(["/data/a", "x"])),
        Ok(vec![fs_req("/data/a", FsMode::Write)])
    );
    assert_eq!(
        call(OsOp::Rename, json!(["/a/x", "/b/y"])),
        Ok(vec![
            fs_req("/a/x", FsMode::Remove),
            fs_req("/b/y", FsMode::Create)
        ])
    );
    assert_eq!(
        call(OsOp::Chmod, json!(["/a", 420])),
        Ok(vec![fs_req("/a", FsMode::Meta)])
    );
}

#[test]
fn a_computed_target_names_the_argument_that_is_not_a_literal() {
    assert_eq!(call(OsOp::ReadFile, json!(["@payload.path"])), Err(0));
    assert_eq!(
        call(OsOp::ReadFile, json!([["path-join", "/a", "b"]])),
        Err(0)
    );
    assert_eq!(call(OsOp::Copy, json!(["/a", "@payload.to"])), Err(1));
    assert_eq!(
        call(OsOp::TcpConnect, json!(["db.internal", "@payload.port"])),
        Err(1)
    );
    assert_eq!(call(OsOp::Open, json!(["/a", "@payload.flags"])), Err(1));
}

#[test]
fn open_reads_its_modes_from_the_flags() {
    assert_eq!(
        call(OsOp::Open, json!(["/a", "r"])),
        Ok(vec![fs_req("/a", FsMode::Read)])
    );
    assert_eq!(
        call(OsOp::Open, json!(["/a", "w"])),
        Ok(vec![fs_req("/a", FsMode::Write)])
    );
    assert_eq!(
        call(OsOp::Open, json!(["/a", "w+x"])),
        Ok(vec![
            fs_req("/a", FsMode::Write),
            fs_req("/a", FsMode::Read),
            fs_req("/a", FsMode::Create)
        ])
    );
}

#[test]
fn spawn_needs_the_command_its_cwd_and_each_inherited_stream() {
    let needs = call(
        OsOp::Spawn,
        json!(["git", ["status"], {"cwd": "/repo", "stdout": "inherit", "stderr": "pipe"}]),
    )
    .expect("literal");
    assert_eq!(
        needs,
        vec![
            OsRequirement::Exec {
                command: "git".into()
            },
            fs_req("/repo", FsMode::Read),
            OsRequirement::Stdio {
                kind: StdioKind::Out
            }
        ]
    );
    assert_eq!(call(OsOp::Spawn, json!(["@payload.cmd", []])), Err(0));
    assert_eq!(
        call(OsOp::Spawn, json!(["git", [], {"cwd": "@payload.dir"}])),
        Err(2)
    );
    assert_eq!(
        call(
            OsOp::Spawn,
            json!(["git", [], {"cwd": null, "stdin": "null"}])
        ),
        Ok(vec![OsRequirement::Exec {
            command: "git".into()
        }])
    );
}

#[test]
fn a_spawn_env_entry_that_inherits_the_host_value_needs_that_env_grant() {
    assert_eq!(
        call(
            OsOp::Spawn,
            json!(["git", [], {"env": {"PATH": null, "FOO": "bar", "HOME": null}}])
        ),
        Ok(vec![
            OsRequirement::Exec {
                command: "git".into()
            },
            OsRequirement::Env {
                name: "PATH".into()
            },
            OsRequirement::Env {
                name: "HOME".into()
            },
        ])
    );
    assert_eq!(
        call(OsOp::Spawn, json!(["git", [], {"env": {"FOO": "bar"}}])),
        Ok(vec![OsRequirement::Exec {
            command: "git".into()
        }]),
        "control: a literal value inherits nothing"
    );
}

#[test]
fn a_glob_needs_read_on_its_literal_prefix() {
    assert_eq!(
        call(OsOp::Glob, json!(["/data/**/*.log"])),
        Ok(vec![fs_req("/data", FsMode::Read)])
    );
    assert_eq!(
        call(OsOp::Glob, json!(["/*.log"])),
        Ok(vec![fs_req("/", FsMode::Read)])
    );
}

#[test]
fn sys_desktop_and_net_calls_map_to_their_kind_and_handle_calls_need_nothing() {
    assert_eq!(
        call(OsOp::MemoryInfo, json!([])),
        Ok(vec![OsRequirement::Sys {
            kind: SysKind::Memory
        }])
    );
    assert_eq!(
        call(OsOp::Notify, json!(["t", "b"])),
        Ok(vec![OsRequirement::Desktop {
            kind: DesktopKind::Notify
        }])
    );
    assert_eq!(
        call(OsOp::TcpConnect, json!(["db.internal", 5432])),
        Ok(vec![OsRequirement::Net {
            host: "db.internal".into(),
            port: 5432,
            kinds: vec![NetKind::Tcp]
        }])
    );
    assert_eq!(call(OsOp::Close, json!(["@payload.fd"])), Ok(vec![]));
    assert_eq!(call(OsOp::Pid, json!([])), Ok(vec![]));
    assert_eq!(
        call(OsOp::CurrentExe, json!([])),
        Ok(vec![]),
        "like os/cwd and os/pid, a fact of the process itself"
    );
    assert_eq!(
        call(OsOp::Hostname, json!([])),
        Ok(vec![OsRequirement::Sys {
            kind: SysKind::Identity
        }]),
        "control: a host fact needs sys"
    );
}

#[test]
fn a_requirement_is_the_tagged_wire_shape_and_round_trips() {
    let cases = [
        (
            OsRequirement::Fs {
                path: "/a".into(),
                mode: FsMode::Read,
            },
            json!({"need":"fs","path":"/a","mode":"read"}),
        ),
        (
            OsRequirement::Exec {
                command: "git".into(),
            },
            json!({"need":"exec","command":"git"}),
        ),
        (
            OsRequirement::Net {
                host: "h".into(),
                port: 80,
                kinds: vec![NetKind::Tcp],
            },
            json!({"need":"net","host":"h","port":80,"kinds":["tcp"]}),
        ),
        (
            OsRequirement::Env { name: "FOO".into() },
            json!({"need":"env","name":"FOO"}),
        ),
        (
            OsRequirement::Stdio {
                kind: StdioKind::Out,
            },
            json!({"need":"stdio","kind":"out"}),
        ),
        (
            OsRequirement::Sys {
                kind: SysKind::PowerState,
            },
            json!({"need":"sys","kind":"power-state"}),
        ),
        (
            OsRequirement::Desktop {
                kind: DesktopKind::Notify,
            },
            json!({"need":"desktop","kind":"notify"}),
        ),
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
        fs: vec![FsGrant {
            glob: "/data/**".into(),
            modes: vec![FsMode::Read],
        }],
        exec: vec!["git".into()],
        net: vec![NetGrant {
            target: "db.internal:5432".into(),
            kinds: vec![NetKind::Tcp],
        }],
        sys: vec![SysKind::Memory],
        ..OsGrants::default()
    };
    assert!(g.allows(&fs_req("/data/a", FsMode::Read)));
    assert!(!g.allows(&fs_req("/data/a", FsMode::Write)));
    assert!(g.allows(&OsRequirement::Exec {
        command: "git".into()
    }));
    assert!(!g.allows(&OsRequirement::Exec {
        command: "rm".into()
    }));
    assert!(g.allows(&OsRequirement::Net {
        host: "db.internal".into(),
        port: 5432,
        kinds: vec![NetKind::Udp, NetKind::Tcp]
    }));
    assert!(!g.allows(&OsRequirement::Net {
        host: "db.internal".into(),
        port: 5432,
        kinds: vec![NetKind::Udp]
    }));
    assert!(g.allows(&OsRequirement::Sys {
        kind: SysKind::Memory
    }));
    assert!(!g.allows(&OsRequirement::Desktop {
        kind: DesktopKind::Notify
    }));
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
fn an_egress_origin_grants_no_raw_socket() {
    let schema: crate::schema::OrbitalSchema = serde_json::from_value(json!({
        "name": "p", "version": "1.0.0", "egress": ["https://api.example.com"],
        "orbitals": [{"name": "A", "entity": {"name": "E", "fields": []}, "traits": [], "pages": []}]
    }))
    .expect("schema");
    assert!(schema.program_os_grants().is_empty());
    assert!(schema.orbital_os_grants(&schema.orbitals[0]).is_empty());
}

#[test]
fn every_kind_name_matches_its_serde_spelling() {
    fn spelling<T: Serialize>(kind: &T) -> String {
        serde_json::to_value(kind)
            .expect("ser")
            .as_str()
            .expect("str")
            .to_string()
    }
    for k in FsMode::ALL {
        assert_eq!(k.name(), spelling(k));
    }
    for k in NetKind::ALL {
        assert_eq!(k.name(), spelling(k));
    }
    for k in StdioKind::ALL {
        assert_eq!(k.name(), spelling(k));
    }
    for k in SysKind::ALL {
        assert_eq!(k.name(), spelling(k));
    }
    for k in DesktopKind::ALL {
        assert_eq!(k.name(), spelling(k));
    }
    assert_eq!(
        (
            FsMode::ALL.len(),
            NetKind::ALL.len(),
            StdioKind::ALL.len(),
            SysKind::ALL.len(),
            DesktopKind::ALL.len()
        ),
        (5, 3, 3, 6, 5)
    );
}

#[test]
fn atoms_split_one_mode_or_kind_each_and_each_allows_only_itself() {
    let g = OsGrants {
        fs: vec![FsGrant {
            glob: "/d/**".into(),
            modes: vec![FsMode::Read, FsMode::Write],
        }],
        net: vec![NetGrant {
            target: "h:1".into(),
            kinds: vec![NetKind::Tcp],
        }],
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
            FsGrant {
                glob: "data/**".into(),
                modes: vec![FsMode::Read],
            },
            FsGrant {
                glob: "/a/../b".into(),
                modes: vec![FsMode::Read],
            },
            FsGrant {
                glob: "/ok/**".into(),
                modes: vec![],
            },
        ],
        exec: vec!["git status".into(), "".into()],
        net: vec![
            NetGrant {
                target: "no-port".into(),
                kinds: vec![NetKind::Tcp],
            },
            NetGrant {
                target: "h:1".into(),
                kinds: vec![],
            },
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
        fs: vec![FsGrant {
            glob: "/d/**".into(),
            modes: vec![FsMode::Read],
        }],
        exec: vec!["git".into()],
        net: vec![NetGrant {
            target: "[::1]:80".into(),
            kinds: vec![NetKind::Listen],
        }],
        env: vec!["HOME".into()],
        ..OsGrants::default()
    };
    assert!(good.entry_problems().is_empty());
}

fn grant(glob: &str, modes: &[FsMode]) -> OsGrants {
    OsGrants {
        fs: vec![FsGrant {
            glob: glob.into(),
            modes: modes.to_vec(),
        }],
        ..OsGrants::default()
    }
}

#[test]
fn an_anchored_grant_matches_paths_written_under_the_same_anchor_only() {
    let home = grant("~/.orb/**", &[FsMode::Read]);
    assert!(home.fs_allows(Path::new("~/.orb/package.json"), FsMode::Read));
    assert!(
        !home.fs_allows(Path::new("~/.orb/package.json"), FsMode::Write),
        "control: the mode still counts"
    );
    assert!(
        !home.fs_allows(Path::new("./.orb/package.json"), FsMode::Read),
        "control: another anchor is another place"
    );
    assert!(
        !home.fs_allows(Path::new("/.orb/package.json"), FsMode::Read),
        "control: an absolute path is not under the home anchor"
    );
    assert!(
        !home.fs_allows(Path::new("~/.orb/../.ssh/id"), FsMode::Read),
        "edge: climbing out of the anchored tree is refused"
    );
    assert!(
        !home.fs_allows(Path::new("~/../etc/passwd"), FsMode::Read),
        "edge: climbing above the anchor is refused"
    );
    let start = grant("./**", &[FsMode::Read]);
    assert!(start.fs_tree_allows(Path::new("./"), FsMode::Read));
    assert!(start.fs_allows(Path::new("./docs/a.md"), FsMode::Read));
    assert!(
        start.atoms().len() == 1 && start.entry_problems().is_empty(),
        "an anchored glob is a valid entry"
    );
}

#[test]
fn a_host_checks_an_absolute_path_against_anchored_grants_through_its_anchors() {
    let anchors = OsAnchors {
        home: Some(PathBuf::from("/home/u")),
        start: Some(PathBuf::from("/work/p")),
        ..OsAnchors::default()
    };
    let grants = OsGrants {
        fs: vec![
            FsGrant {
                glob: "~/.orb/**".into(),
                modes: vec![FsMode::Write],
            },
            FsGrant {
                glob: "./**".into(),
                modes: vec![FsMode::Read],
            },
        ],
        ..OsGrants::default()
    };
    assert!(grants.fs_allows_at(Path::new("/home/u/.orb/x.json"), FsMode::Write, &anchors));
    assert!(grants.fs_allows_at(Path::new("/work/p/src/a.ts"), FsMode::Read, &anchors));
    assert!(grants.fs_tree_allows_at(Path::new("/work/p/"), FsMode::Read, &anchors));
    assert!(
        !grants.fs_allows_at(Path::new("/home/u/.ssh/id"), FsMode::Write, &anchors),
        "control: outside the anchored tree"
    );
    assert!(
        !grants.fs_allows_at(Path::new("/work/other/a"), FsMode::Read, &anchors),
        "control: a sibling of the start directory"
    );
    let unknown = OsAnchors::default();
    assert!(
        !grants.fs_allows_at(Path::new("/home/u/.orb/x.json"), FsMode::Write, &unknown),
        "edge: an anchor the host does not know grants nothing"
    );
    assert_eq!(
        anchors.expand("~/.orb/x").unwrap(),
        Some(PathBuf::from("/home/u/.orb/x"))
    );
    assert_eq!(
        anchors.expand("./a").unwrap(),
        Some(PathBuf::from("/work/p/a"))
    );
    assert_eq!(
        anchors.expand("/abs").unwrap(),
        None,
        "control: an absolute path is not anchored"
    );
    assert_eq!(
        anchors.expand("rel").unwrap(),
        None,
        "control: a bare relative path keeps the working-directory rule"
    );
    assert!(
        unknown.expand("~/x").is_err(),
        "edge: an anchor the host cannot place is an error"
    );
}

fn boot(exe_dir: Option<&str>, env: &[(&str, &str)]) -> OsAnchors {
    OsAnchors {
        exe_dir: exe_dir.map(PathBuf::from),
        env: EnvSnapshot::from_pairs(env.iter().map(|(k, v)| (k.to_string(), v.to_string()))),
        ..OsAnchors::default()
    }
}

fn anchored(fs: Vec<AnchoredFsGrant>, exec: Vec<AnchoredExec>) -> OsGrants {
    OsGrants {
        anchored: AnchoredGrants {
            fs,
            exec,
            net: Vec::new(),
        },
        ..OsGrants::default()
    }
}

fn afs(anchor: GrantAnchor, glob: &str) -> AnchoredFsGrant {
    AnchoredFsGrant {
        anchor,
        glob: glob.into(),
        modes: vec![FsMode::Read],
    }
}

fn aexec(anchor: GrantAnchor, path: Option<&str>) -> AnchoredExec {
    AnchoredExec {
        anchor,
        path: path.map(str::to_string),
    }
}

#[test]
fn an_anchored_grant_matches_nothing_until_a_host_resolves_it() {
    let g = anchored(
        vec![afs(GrantAnchor::ExeDir, "agent/**")],
        vec![aexec(GrantAnchor::ExeDir, Some("bun"))],
    );
    assert!(!g.is_empty());
    assert!(!g.fs_allows(Path::new("/opt/orb/agent/cli.js"), FsMode::Read));
    assert!(
        !g.fs_allows(Path::new("/agent/cli.js"), FsMode::Read),
        "control: the relative glob is not read as absolute"
    );
    assert!(!g.exec_allows("bun") && !g.exec_allows("/opt/orb/bun"));
    let (resolved, problems) = g.resolve_anchors(&boot(Some("/opt/orb"), &[]));
    assert!(problems.is_empty(), "{problems:?}");
    assert!(
        resolved.anchored.is_empty(),
        "resolution empties the anchored part"
    );
    assert!(resolved.fs_allows(Path::new("/opt/orb/agent/cli.js"), FsMode::Read));
    assert!(resolved.fs_allows(Path::new("/opt/orb/agent"), FsMode::Read));
    assert!(
        !resolved.fs_allows(Path::new("/opt/orb/agent/cli.js"), FsMode::Write),
        "control: the mode still counts"
    );
    assert!(
        !resolved.fs_allows(Path::new("/opt/orb/other"), FsMode::Read),
        "control: a sibling of the anchored tree"
    );
    assert!(
        !resolved.fs_allows(Path::new("/opt/orb/agent/../../etc/passwd"), FsMode::Read),
        "edge: climbing out is refused"
    );
    assert!(resolved.exec_allows("/opt/orb/bun"));
    assert!(
        !resolved.exec_allows("bun"),
        "control: the anchored command is the resolved path, not the bare name"
    );
}

#[test]
fn an_env_anchor_roots_at_its_boot_value_and_names_the_command_itself_without_a_path() {
    let g = anchored(
        vec![afs(GrantAnchor::Env("ORB_AGENT_DIR".into()), "**")],
        vec![aexec(GrantAnchor::Env("ORB_BUN_PATH".into()), None)],
    );
    let host = boot(
        None,
        &[
            ("ORB_AGENT_DIR", "/srv/agent/"),
            ("ORB_BUN_PATH", "/usr/local/bin/bun"),
        ],
    );
    let (resolved, problems) = g.resolve_anchors(&host);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        resolved.fs[0].glob, "/srv/agent/**",
        "a trailing slash in the value is normalized"
    );
    assert!(resolved.fs_allows(Path::new("/srv/agent/cli.js"), FsMode::Read));
    assert!(
        !resolved.fs_allows(Path::new("/srv/agentx/cli.js"), FsMode::Read),
        "control: a prefix-sharing sibling"
    );
    assert_eq!(resolved.exec, ["/usr/local/bin/bun"]);
    let under = anchored(
        Vec::new(),
        vec![aexec(GrantAnchor::Env("ORB_HOME".into()), Some("bin/bun"))],
    );
    let (resolved, _) = under.resolve_anchors(&boot(None, &[("ORB_HOME", "/o")]));
    assert_eq!(resolved.exec, ["/o/bin/bun"]);
}

#[test]
fn an_unset_relative_or_widening_anchor_grants_nothing_and_is_reported() {
    let env = |name: &str| GrantAnchor::Env(name.into());
    let g = anchored(
        vec![
            afs(env("UNSET"), "**"),
            afs(env("EMPTY"), "**"),
            afs(env("REL"), "**"),
            afs(env("WILD"), "**"),
        ],
        vec![
            aexec(env("UNSET"), None),
            aexec(GrantAnchor::ExeDir, Some("bun")),
        ],
    );
    let host = boot(None, &[("EMPTY", ""), ("REL", "agent"), ("WILD", "/a/*")]);
    let (resolved, problems) = g.resolve_anchors(&host);
    assert!(
        resolved.fs.is_empty() && resolved.exec.is_empty(),
        "nothing is widened: {resolved:?}"
    );
    assert!(!resolved.fs_allows(Path::new("/x"), FsMode::Read));
    let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "anchored.fs[0]",
            "anchored.fs[1]",
            "anchored.fs[2]",
            "anchored.fs[3]",
            "anchored.exec[0]",
            "anchored.exec[1]"
        ]
    );
    assert!(
        problems[0].message.contains("unset"),
        "{}",
        problems[0].message
    );
    assert!(
        problems[2].message.contains("not an absolute path"),
        "{}",
        problems[2].message
    );
    assert!(
        problems[3].message.contains("glob character"),
        "{}",
        problems[3].message
    );
    assert!(
        problems[5].message.contains("executable"),
        "edge: a host that cannot place exe-dir: {}",
        problems[5].message
    );
    let plain = OsGrants {
        exec: vec!["bun".into()],
        ..g.clone()
    };
    let (kept, _) = plain.resolve_anchors(&host);
    assert_eq!(
        kept.exec,
        ["bun"],
        "control: plain entries pass through unresolved anchors untouched"
    );
}

#[test]
fn an_unset_env_anchor_is_silent_when_the_program_declares_a_home_entry_with_the_same_modes() {
    let env = |name: &str| GrantAnchor::Env(name.into());
    let rw = vec![FsMode::Read, FsMode::Write];
    let mut g = anchored(
        vec![AnchoredFsGrant {
            anchor: env("STORE_DIR"),
            glob: "**".into(),
            modes: rw.clone(),
        }],
        Vec::new(),
    );
    g.fs = vec![FsGrant {
        glob: "~/store/**".into(),
        modes: vec![FsMode::Read, FsMode::Write, FsMode::Remove],
    }];
    let (resolved, problems) = g.resolve_anchors(&boot(None, &[]));
    assert!(
        problems.is_empty(),
        "the declared home entry is the anchor choice: {problems:?}"
    );
    assert!(
        resolved.anchored.fs.is_empty(),
        "the unset anchor still grants nothing"
    );

    let (set, problems) = g.resolve_anchors(&boot(None, &[("STORE_DIR", "/custom")]));
    assert!(
        problems.is_empty() && set.fs.iter().any(|f| f.glob == "/custom/**"),
        "control: a set variable resolves as before: {set:?}"
    );

    let (_, relative) = g.resolve_anchors(&boot(None, &[("STORE_DIR", "rel")]));
    assert_eq!(
        relative.len(),
        1,
        "edge: a set but unusable value is still reported: {relative:?}"
    );

    g.fs = vec![FsGrant {
        glob: "~/store/**".into(),
        modes: vec![FsMode::Read],
    }];
    let (_, narrow) = g.resolve_anchors(&boot(None, &[]));
    assert_eq!(
        narrow.len(),
        1,
        "edge: a home entry with fewer modes is no stand-in: {narrow:?}"
    );

    g.fs = vec![FsGrant {
        glob: "/data/**".into(),
        modes: rw,
    }];
    let (_, other) = g.resolve_anchors(&boot(None, &[]));
    assert_eq!(
        other.len(),
        1,
        "edge: only a home entry is the declared anchor: {other:?}"
    );

    let (_, none) = anchored(
        vec![AnchoredFsGrant {
            anchor: env("STORE_DIR"),
            glob: "**".into(),
            modes: vec![FsMode::Read],
        }],
        Vec::new(),
    )
    .resolve_anchors(&boot(None, &[]));
    assert_eq!(
        none.len(),
        1,
        "control: with no other declaration the unset anchor is reported"
    );
}

#[test]
fn a_variable_set_after_boot_never_moves_an_env_anchor() {
    let host = boot(None, &[]);
    std::env::set_var("ORB_GRANT_ANCHOR_TEST_LATE", "/late");
    let g = anchored(
        vec![afs(
            GrantAnchor::Env("ORB_GRANT_ANCHOR_TEST_LATE".into()),
            "**",
        )],
        Vec::new(),
    );
    let (resolved, problems) = g.resolve_anchors(&host);
    std::env::remove_var("ORB_GRANT_ANCHOR_TEST_LATE");
    assert!(resolved.fs.is_empty() && problems.len() == 1);
    assert!(
        !format!("{:?}", boot(None, &[("SECRET", "hunter2")])).contains("hunter2"),
        "the snapshot's Debug names variables only"
    );
}

#[test]
fn anchored_entries_round_trip_split_into_atoms_and_report_bad_shapes() {
    let g = anchored(
        vec![AnchoredFsGrant {
            anchor: GrantAnchor::ExeDir,
            glob: "agent/**".into(),
            modes: vec![FsMode::Read, FsMode::Meta],
        }],
        vec![
            aexec(GrantAnchor::Env("ORB_BUN_PATH".into()), None),
            aexec(GrantAnchor::ExeDir, Some("bun")),
        ],
    );
    let wire = serde_json::to_value(&g).unwrap();
    assert_eq!(
        wire["anchored"],
        json!({"fs": [{"anchor": "exe-dir", "glob": "agent/**", "modes": ["read", "meta"]}],
               "exec": [{"anchor": {"env": "ORB_BUN_PATH"}}, {"anchor": "exe-dir", "path": "bun"}]})
    );
    assert_eq!(serde_json::from_value::<OsGrants>(wire).unwrap(), g);
    assert!(
        serde_json::to_value(OsGrants::default())
            .unwrap()
            .get("anchored")
            .is_none(),
        "control: no anchored key when empty"
    );
    let labels: Vec<String> = g.atoms().into_iter().map(|a| a.label).collect();
    assert_eq!(
        labels,
        [
            r#"fs exe-dir "agent/**" read"#,
            r#"fs exe-dir "agent/**" meta"#,
            "exec env ORB_BUN_PATH",
            r#"exec exe-dir "bun""#
        ]
    );
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
            AnchoredFsGrant {
                anchor: GrantAnchor::ExeDir,
                glob: "a".into(),
                modes: vec![],
            },
            afs(GrantAnchor::Env("A=B".into()), "**"),
        ],
        vec![
            aexec(GrantAnchor::ExeDir, None),
            aexec(GrantAnchor::ExeDir, Some("../bun")),
            aexec(GrantAnchor::ExeDir, Some("b*")),
        ],
    );
    let paths: Vec<String> = bad.entry_problems().into_iter().map(|p| p.path).collect();
    assert_eq!(
        paths,
        [
            "anchored.fs[0]",
            "anchored.fs[1]",
            "anchored.fs[2]",
            "anchored.fs[3]",
            "anchored.fs[4]",
            "anchored.exec[0]",
            "anchored.exec[1]",
            "anchored.exec[2]"
        ]
    );
}

#[test]
fn an_anchored_path_argument_needs_an_anchored_grant_on_the_same_anchor() {
    let cwd_docs = anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new());
    let read = call(
        OsOp::ReadFile,
        json!([{"anchor": "cwd", "path": "docs/Almadar_Gaps.md"}]),
    )
    .expect("static");
    assert_eq!(
        read,
        vec![OsRequirement::FsAnchored {
            anchor: GrantAnchor::Cwd,
            path: "docs/Almadar_Gaps.md".into(),
            mode: FsMode::Read
        }]
    );
    assert!(cwd_docs.allows(&read[0]));
    let outside = call(
        OsOp::ReadFile,
        json!([{"anchor": "cwd", "path": "src/a.rs"}]),
    )
    .unwrap();
    assert!(
        !cwd_docs.allows(&outside[0]),
        "control: outside the anchored glob"
    );
    let escape = call(
        OsOp::ReadFile,
        json!([{"anchor": "cwd", "path": "docs/../../etc/passwd"}]),
    )
    .unwrap();
    assert!(
        !cwd_docs.allows(&escape[0]),
        "edge: climbing above the anchor"
    );
    let other = call(
        OsOp::ReadFile,
        json!([{"anchor": "exe-dir", "path": "docs/a.md"}]),
    )
    .unwrap();
    assert!(
        !cwd_docs.allows(&other[0]),
        "control: another anchor is another place"
    );
    assert!(
        !cwd_docs.allows(&fs_req("docs/a.md", FsMode::Read)),
        "control: a plain path is never covered by an anchored grant"
    );
    let write = call(
        OsOp::WriteFile,
        json!([{"anchor": "cwd", "path": "docs/a.md"}, "x"]),
    )
    .unwrap();
    assert!(
        !cwd_docs.allows(&write[0]),
        "control: the mode still counts"
    );
    let env = anchored(
        vec![afs(GrantAnchor::Env("ORB_AGENT_DIR".into()), "**")],
        Vec::new(),
    );
    let entry = call(
        OsOp::Exists,
        json!([{"anchor": {"env": "ORB_AGENT_DIR"}, "path": "cli.js"}]),
    )
    .unwrap();
    assert!(env.allows(&entry[0]));
    let itself = call(OsOp::Exists, json!([{"anchor": {"env": "ORB_AGENT_DIR"}}])).unwrap();
    assert!(
        env.allows(&itself[0]),
        "a `**` glob covers the anchor itself"
    );
    assert!(
        !env.allows(
            &call(
                OsOp::Exists,
                json!([{"anchor": {"env": "OTHER"}, "path": "cli.js"}])
            )
            .unwrap()[0]
        ),
        "control: another variable"
    );
}

#[test]
fn a_computed_anchored_path_needs_the_tree_under_its_literal_directory() {
    let tree = call(
        OsOp::ReadFile,
        json!([{"anchor": "cwd", "path": ["str/concat", "docs/", "@entity.name"]}]),
    )
    .unwrap();
    assert_eq!(
        tree,
        vec![OsRequirement::FsAnchoredTree {
            anchor: GrantAnchor::Cwd,
            root: "docs/".into(),
            mode: FsMode::Read
        }]
    );
    assert!(anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new()).allows(&tree[0]));
    assert!(
        !anchored(vec![afs(GrantAnchor::Cwd, "src/*.md")], Vec::new()).allows(&tree[0]),
        "control: a glob that cannot reach under the directory"
    );
    let whole = call(
        OsOp::ReadFile,
        json!([{"anchor": "cwd", "path": "@entity.path"}]),
    )
    .unwrap();
    assert_eq!(
        whole,
        vec![OsRequirement::FsAnchoredTree {
            anchor: GrantAnchor::Cwd,
            root: String::new(),
            mode: FsMode::Read
        }]
    );
    assert!(anchored(vec![afs(GrantAnchor::Cwd, "**")], Vec::new()).allows(&whole[0]));
    assert!(
        anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new()).allows(&whole[0]),
        "a narrow glob covers it; the host checks the resolved path"
    );
    assert!(
        !anchored(vec![afs(GrantAnchor::ExeDir, "docs/**")], Vec::new()).allows(&whole[0]),
        "control: another anchor"
    );
    assert_eq!(
        call(
            OsOp::ReadFile,
            json!([{"anchor": "@entity.where", "path": "a"}])
        ),
        Err(0),
        "the anchor itself must be a literal"
    );
    assert_eq!(
        call(
            OsOp::ReadFile,
            json!([{"anchor": {"env": "@entity.n"}, "path": "a"}])
        ),
        Err(0)
    );
    assert_eq!(
        call(
            OsOp::ReadFile,
            json!([{"anchor": "cwd", "path": "a", "extra": 1}])
        ),
        Err(0),
        "control: an object of another shape is not an anchored path"
    );
}

#[test]
fn an_anchored_glob_needs_read_on_the_tree_its_literal_prefix_reaches() {
    let tree = |root: &str| {
        vec![OsRequirement::FsAnchoredTree {
            anchor: GrantAnchor::Cwd,
            root: root.into(),
            mode: FsMode::Read,
        }]
    };
    assert_eq!(
        call(
            OsOp::Glob,
            json!([{"anchor": "cwd", "path": "docs/**/*.md"}])
        ),
        Ok(tree("docs/"))
    );
    assert_eq!(
        call(
            OsOp::Glob,
            json!([{"anchor": "cwd", "path": "*/package.json"}])
        ),
        Ok(tree(""))
    );
    assert_eq!(
        call(OsOp::Glob, json!([{"anchor": "cwd", "path": "docs/a.md"}])),
        Ok(tree("docs/")),
        "edge: no wildcard reaches its directory"
    );
    let computed = call(
        OsOp::Glob,
        json!([{"anchor": "cwd", "path": "@entity.pattern"}]),
    )
    .unwrap();
    assert_eq!(computed, tree(""));
    assert_eq!(
        call(
            OsOp::Glob,
            json!([{"anchor": "cwd", "path": ["str/concat", "src/", "@entity.p"]}])
        ),
        Ok(tree("src/"))
    );
    let manifests = anchored(vec![afs(GrantAnchor::Cwd, "**/package.json")], Vec::new());
    assert!(
        manifests.allows(&computed[0]),
        "a narrow anchored glob covers it; the host checks every result"
    );
    assert!(
        !anchored(
            vec![afs(GrantAnchor::ExeDir, "**/package.json")],
            Vec::new()
        )
        .allows(&computed[0]),
        "control: another anchor"
    );
    let escape = call(OsOp::Glob, json!([{"anchor": "cwd", "path": "../**"}])).unwrap();
    assert!(
        !anchored(vec![afs(GrantAnchor::Cwd, "**")], Vec::new()).allows(&escape[0]),
        "edge: a glob climbing above the anchor is never covered"
    );
    assert_eq!(
        call(OsOp::Glob, json!(["@entity.pattern"])),
        Err(0),
        "control: an unanchored computed glob stays a static error"
    );
    assert_eq!(
        call(
            OsOp::Glob,
            json!([{"anchor": "@entity.where", "path": "*"}])
        ),
        Err(0),
        "the anchor itself must be a literal"
    );
}

#[test]
fn a_computed_anchored_path_is_covered_by_a_narrow_glob_that_can_reach_under_its_directory() {
    let lolo = |modes: Vec<FsMode>| {
        anchored(
            vec![AnchoredFsGrant {
                anchor: GrantAnchor::Cwd,
                glob: "**/*.lolo".into(),
                modes,
            }],
            Vec::new(),
        )
    };
    let read = call(
        OsOp::ReadFile,
        json!([{"anchor": "cwd", "path": "@payload.path"}]),
    )
    .unwrap();
    let write = call(
        OsOp::WriteFile,
        json!([{"anchor": "cwd", "path": "@payload.path"}, "x"]),
    )
    .unwrap();
    let rw = lolo(vec![FsMode::Read, FsMode::Write]);
    assert!(
        rw.allows(&read[0]) && rw.allows(&write[0]),
        "the host enforces the glob on the resolved path"
    );
    assert!(
        !lolo(vec![FsMode::Read]).allows(&write[0]),
        "control: the mode still counts"
    );
    assert!(
        !anchored(
            vec![AnchoredFsGrant {
                anchor: GrantAnchor::ExeDir,
                glob: "**/*.lolo".into(),
                modes: vec![FsMode::Read]
            }],
            Vec::new()
        )
        .allows(&read[0]),
        "control: another anchor"
    );
    let under = call(
        OsOp::ReadFile,
        json!([{"anchor": "cwd", "path": ["str/concat", "src/", "@payload.name"]}]),
    )
    .unwrap();
    assert!(anchored(vec![afs(GrantAnchor::Cwd, "src/*.lolo")], Vec::new()).allows(&under[0]));
    assert!(
        rw.allows(&under[0]),
        "a leading `**` reaches under any directory"
    );
    assert!(
        !anchored(vec![afs(GrantAnchor::Cwd, "docs/*.lolo")], Vec::new()).allows(&under[0]),
        "control: a sibling directory"
    );
    assert!(
        !anchored(vec![afs(GrantAnchor::Cwd, "src")], Vec::new()).allows(&under[0]),
        "edge: a glob naming the directory itself reaches nothing under it"
    );
    assert!(
        !anchored(Vec::new(), Vec::new()).allows(&read[0]),
        "control: no anchored glob at all"
    );
}

#[test]
fn spawn_takes_an_anchored_command_and_cwd() {
    let needs = call(
        OsOp::Spawn,
        json!([{"anchor": "exe-dir", "path": "bun"}, [], {"cwd": {"anchor": "cwd"}}]),
    )
    .unwrap();
    assert_eq!(
        needs,
        vec![
            OsRequirement::ExecAnchored {
                anchor: GrantAnchor::ExeDir,
                path: Some("bun".into())
            },
            OsRequirement::FsAnchored {
                anchor: GrantAnchor::Cwd,
                path: String::new(),
                mode: FsMode::Read
            },
        ]
    );
    let g = anchored(
        Vec::new(),
        vec![
            aexec(GrantAnchor::ExeDir, Some("bun")),
            aexec(GrantAnchor::Env("ORB_BUN_PATH".into()), None),
        ],
    );
    assert!(g.allows(&needs[0]));
    assert!(g.allows(&OsRequirement::ExecAnchored {
        anchor: GrantAnchor::Env("ORB_BUN_PATH".into()),
        path: None
    }));
    assert!(
        !g.allows(&OsRequirement::ExecAnchored {
            anchor: GrantAnchor::ExeDir,
            path: Some("node".into())
        }),
        "control: another command"
    );
    assert!(
        !g.allows(&OsRequirement::Exec {
            command: "bun".into()
        }),
        "control: the bare name is not granted"
    );
    assert_eq!(
        call(
            OsOp::Spawn,
            json!([{"anchor": "exe-dir", "path": "@entity.cmd"}, []])
        ),
        Err(0),
        "a command is never computed"
    );
    assert_eq!(anchored_arg_positions(OsOp::Spawn), [0]);
    assert_eq!(anchored_arg_positions(OsOp::Rename), [0, 1]);
    assert!(anchored_arg_positions(OsOp::Getenv).is_empty());
}

#[test]
fn every_os_operation_reads_its_needs_from_the_operator_table_at_arguments_that_exist() {
    let mut handle_calls = Vec::new();
    for op in OsOp::ALL {
        let needs = os_grant_needs(*op);
        if needs.is_empty() {
            handle_calls.push(op.name());
        }
        for need in needs {
            let positions: Vec<usize> = match need {
                OsGrantNeed::Fs { path, .. } => vec![*path],
                OsGrantNeed::FsGlob { glob } => vec![*glob],
                OsGrantNeed::FsOpen { path, flags } => vec![*path, *flags],
                OsGrantNeed::Exec { command, options } => vec![*command, *options],
                OsGrantNeed::Env { name } => vec![*name],
                OsGrantNeed::Net { host, port, .. } => {
                    std::iter::once(*host).chain(*port).collect()
                }
                OsGrantNeed::Kv { store, .. } => vec![*store],
                OsGrantNeed::Mmio {
                    base,
                    offset,
                    width,
                    ..
                } => vec![*base, *offset, *width],
                OsGrantNeed::Irq { line, .. } => vec![*line],
                OsGrantNeed::Stdio { .. }
                | OsGrantNeed::Sys { .. }
                | OsGrantNeed::Desktop { .. } => Vec::new(),
            };
            for at in positions {
                assert!(
                    at < op.max_args(),
                    "{} names argument {at}, past its {} arguments",
                    op.name(),
                    op.max_args()
                );
            }
        }
    }
    handle_calls.sort_unstable();
    let mut expected = vec![
        "os/close",
        "os/read",
        "os/write",
        "os/seek",
        "os/fsync",
        "os/cwd",
        "os/wait",
        "os/kill",
        "os/pid",
        "os/ppid",
        "os/current-exe",
        "os/watch-process",
        "os/watch-signal",
        "os/environ",
        "os/isatty",
        "os/tty-size",
        "os/send",
        "os/recv",
        "os/recvfrom",
        "os/shutdown",
        "os/tls-upgrade",
        "os/watch-port",
        "os/clock-monotonic",
        "os/watch-cron",
        "os/fetch",
    ];
    expected.sort_unstable();
    assert_eq!(
        handle_calls, expected,
        "only the handle and clock calls need nothing"
    );
    assert_eq!(
        os_grant_needs(OsOp::Rename),
        [
            OsGrantNeed::Fs {
                access: FsMode::Remove,
                path: 0
            },
            OsGrantNeed::Fs {
                access: FsMode::Create,
                path: 1
            }
        ]
    );
    assert_eq!(
        os_grant_needs(OsOp::DnsResolve),
        [OsGrantNeed::Net {
            access: vec![NetKind::Tcp, NetKind::Udp],
            host: 0,
            port: None
        }]
    );
    assert_ne!(
        os_grant_needs(OsOp::ReadFile),
        os_grant_needs(OsOp::WriteFile),
        "control: read and write differ"
    );
}

fn anet(host: NetPart, port: NetPart, kinds: &[NetKind]) -> OsGrants {
    OsGrants {
        anchored: AnchoredGrants {
            net: vec![AnchoredNetGrant {
                host,
                port,
                kinds: kinds.to_vec(),
            }],
            ..AnchoredGrants::default()
        },
        ..OsGrants::default()
    }
}

fn envp(name: &str) -> NetPart {
    NetPart::Anchor(GrantAnchor::Env(name.into()))
}

#[test]
fn a_listen_on_env_host_and_port_needs_the_anchored_net_grant() {
    let g = anet(envp("HOST"), envp("PORT"), &[NetKind::Listen]);
    let needs = call(
        OsOp::TcpListen,
        json!([{"anchor": {"env": "HOST"}}, {"anchor": {"env": "PORT"}}]),
    )
    .unwrap();
    assert_eq!(
        needs,
        vec![OsRequirement::NetAnchored {
            host: envp("HOST"),
            port: envp("PORT"),
            kinds: vec![NetKind::Listen]
        }]
    );
    assert!(g.allows(&needs[0]));
    let tcp = call(
        OsOp::TcpConnect,
        json!([{"anchor": {"env": "HOST"}}, {"anchor": {"env": "PORT"}}]),
    )
    .unwrap();
    assert!(!g.allows(&tcp[0]), "control: another kind");
    let mixed = call(
        OsOp::TcpListen,
        json!(["0.0.0.0", {"anchor": {"env": "PORT"}}]),
    )
    .unwrap();
    assert_eq!(
        mixed,
        vec![OsRequirement::NetAnchored {
            host: NetPart::Literal("0.0.0.0".into()),
            port: envp("PORT"),
            kinds: vec![NetKind::Listen]
        }]
    );
    assert!(
        !g.allows(&mixed[0]),
        "control: a literal host is not the env host"
    );
    assert!(anet(
        NetPart::Literal("0.0.0.0".into()),
        envp("PORT"),
        &[NetKind::Listen]
    )
    .allows(&mixed[0]));
    assert!(
        !g.allows(&OsRequirement::Net {
            host: "localhost".into(),
            port: 80,
            kinds: vec![NetKind::Listen]
        }),
        "control: a literal target"
    );
    assert_eq!(
        call(OsOp::TcpListen, json!([{"anchor": "cwd"}, 80])),
        Err(0),
        "a net part takes only an env anchor"
    );
    assert_eq!(
        call(
            OsOp::TcpListen,
            json!([{"anchor": {"env": "H"}, "path": "x"}, 80])
        ),
        Err(0)
    );
    assert_eq!(anchored_net_args(OsOp::Sendto), Some((2, 3)));
    assert_eq!(anchored_net_args(OsOp::ReadFile), None);
}

#[test]
fn an_anchored_net_grant_resolves_at_boot_and_an_unset_part_grants_nothing() {
    let g = anet(envp("HOST"), envp("PORT"), &[NetKind::Listen]);
    let (resolved, problems) =
        g.resolve_anchors(&boot(None, &[("HOST", "127.0.0.1"), ("PORT", "8080")]));
    assert!(problems.is_empty(), "{problems:?}");
    assert!(resolved.net_allows("127.0.0.1", 8080, NetKind::Listen));
    assert!(
        !resolved.net_allows("127.0.0.1", 8081, NetKind::Listen),
        "control: another port"
    );
    let (v6, _) = g.resolve_anchors(&boot(None, &[("HOST", "::1"), ("PORT", "9")]));
    assert!(v6.net_allows("::1", 9, NetKind::Listen));
    let (none, problems) = g.resolve_anchors(&boot(None, &[("HOST", "127.0.0.1")]));
    assert!(
        none.net.is_empty() && problems.len() == 1 && problems[0].message.contains("PORT is unset"),
        "{problems:?}"
    );
    let (bad, problems) = g.resolve_anchors(&boot(None, &[("HOST", "h"), ("PORT", "http")]));
    assert!(bad.net.is_empty() && problems[0].message.contains("not a port number"));
    let shapes = OsGrants {
        anchored: AnchoredGrants {
            net: vec![
                AnchoredNetGrant {
                    host: NetPart::Literal("h".into()),
                    port: NetPart::Literal("1".into()),
                    kinds: vec![NetKind::Tcp],
                },
                AnchoredNetGrant {
                    host: NetPart::Anchor(GrantAnchor::Cwd),
                    port: envp("P"),
                    kinds: vec![NetKind::Tcp],
                },
                AnchoredNetGrant {
                    host: envp("H"),
                    port: NetPart::Literal("x".into()),
                    kinds: vec![NetKind::Tcp],
                },
                AnchoredNetGrant {
                    host: envp("H"),
                    port: envp("P"),
                    kinds: vec![],
                },
                AnchoredNetGrant {
                    host: envp("H"),
                    port: envp("P"),
                    kinds: vec![NetKind::Udp],
                },
            ],
            ..AnchoredGrants::default()
        },
        ..OsGrants::default()
    };
    let paths: Vec<String> = shapes
        .entry_problems()
        .into_iter()
        .map(|p| p.path)
        .collect();
    assert_eq!(
        paths,
        [
            "anchored.net[0]",
            "anchored.net[1]",
            "anchored.net[2]",
            "anchored.net[3]"
        ]
    );
    assert_eq!(g.atoms()[0].label, "net env HOST, env PORT listen");
}

#[test]
fn a_host_resolves_an_anchored_path_under_its_anchor_or_refuses_it() {
    let host = OsAnchors {
        start: Some(PathBuf::from("/work/p")),
        ..boot(Some("/opt/orb"), &[("D", "/srv/a")])
    };
    let at = |v: serde_json::Value| {
        AnchoredPath::from_value(&serde_json::from_value::<Value>(v).unwrap()).expect("anchored")
    };
    assert_eq!(
        host.resolve_path(&at(json!({"anchor": "cwd", "path": "docs/a.md"}))),
        Ok("/work/p/docs/a.md".into())
    );
    assert_eq!(
        host.resolve_path(&at(json!({"anchor": "exe-dir", "path": "agent/./cli.js"}))),
        Ok("/opt/orb/agent/cli.js".into())
    );
    assert_eq!(
        host.resolve_path(&at(json!({"anchor": {"env": "D"}}))),
        Ok("/srv/a".into())
    );
    assert_eq!(
        host.resolve_path(&at(json!({"anchor": "cwd", "path": "."}))),
        Ok("/work/p".into()),
        "`.` is the anchor itself"
    );
    assert_eq!(
        host.resolve_path(&at(json!({"anchor": "cwd", "path": "./docs"}))),
        Ok("/work/p/docs".into())
    );
    assert!(
        host.resolve_path(&at(json!({"anchor": "cwd", "path": "~/x"})))
            .is_err(),
        "edge: a home form is not relative"
    );
    assert!(
        host.resolve_path(&at(json!({"anchor": "cwd", "path": "../x"})))
            .is_err(),
        "edge: climbing above the anchor"
    );
    assert!(
        host.resolve_path(&at(json!({"anchor": "cwd", "path": "/etc/passwd"})))
            .is_err(),
        "edge: an absolute path is not relative"
    );
    assert!(
        host.resolve_path(&at(json!({"anchor": {"env": "UNSET"}, "path": "a"})))
            .is_err(),
        "edge: an unset variable"
    );
    assert!(
        AnchoredPath::from_value(&Value::String("cwd".into())).is_none(),
        "control: a string is not an anchored path"
    );
    let (resolved, problems) =
        anchored(vec![afs(GrantAnchor::Cwd, "docs/**")], Vec::new()).resolve_anchors(&host);
    assert!(problems.is_empty());
    assert!(resolved.fs_allows(Path::new("/work/p/docs/a.md"), FsMode::Read));
}

fn concat(pieces: &[&str]) -> SExpression {
    let mut items = vec![SExpression::String("str/concat".into())];
    items.extend(pieces.iter().map(|p| SExpression::String((*p).into())));
    SExpression::Array(items.into())
}

#[test]
fn a_concat_with_a_literal_directory_prefix_needs_the_tree_and_anything_else_is_computed() {
    let tree = os_requirements(OsOp::ReadFile, &[concat(&["/data/", "sub/", "@payload.p"])]);
    assert_eq!(
        tree,
        Ok(vec![OsRequirement::FsTree {
            root: "/data/sub/".into(),
            mode: FsMode::Read
        }])
    );
    let open = os_requirements(
        OsOp::Open,
        &[
            concat(&["/data/", "@payload.p"]),
            SExpression::String("w".into()),
        ],
    );
    assert_eq!(
        open,
        Ok(vec![OsRequirement::FsTree {
            root: "/data/".into(),
            mode: FsMode::Write
        }])
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
    let need = |root: &str| OsRequirement::FsTree {
        root: root.into(),
        mode: FsMode::Read,
    };
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

#[test]
fn a_grant_naming_an_unknown_family_or_field_is_refused() {
    assert!(serde_json::from_value::<OsGrants>(
        json!({"files": [{"glob": "/tmp/**", "modes": ["read"]}]})
    )
    .is_err());
    assert!(serde_json::from_value::<OsGrants>(
        json!({"fs": [{"glob": "/tmp/**", "modes": ["read"], "recursive": true}]})
    )
    .is_err());
    assert!(serde_json::from_value::<OsGrants>(json!({"anchored": {"files": []}})).is_err());
}

#[test]
fn control_known_families_and_fields_still_parse() {
    let grants = serde_json::from_value::<OsGrants>(
        json!({"fs": [{"glob": "/tmp/**", "modes": ["read"]}], "env": ["HOME"]}),
    )
    .expect("known families parse");
    assert_eq!(
        grants,
        OsGrants {
            env: vec!["HOME".to_string()],
            ..fs_grants("/tmp/**", &[FsMode::Read])
        }
    );
}

mod static_check {
    use super::*;
    use crate::schema::OrbitalSchema;

    fn orbital(
        name: &str,
        grants: serde_json::Value,
        guard: Option<serde_json::Value>,
        effects: serde_json::Value,
        config: serde_json::Value,
    ) -> serde_json::Value {
        let mut transition =
            json!({ "from": "idle", "to": "idle", "event": "INIT", "effects": effects });
        if let Some(g) = guard {
            transition["guard"] = g;
        }
        let mut o = json!({
            "name": name,
            "entity": { "name": format!("{name}Row"), "persistence": "runtime", "fields": [{ "name": "id", "type": "string", "required": true }] },
            "traits": [{
                "name": format!("{name}Runner"), "linkedEntity": format!("{name}Row"), "config": config,
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "events": [{ "key": "INIT" }], "transitions": [transition] }
            }],
            "pages": []
        });
        if !grants.is_null() {
            o["osGrants"] = grants;
        }
        o
    }

    fn program(app: serde_json::Value, orbitals: Vec<serde_json::Value>) -> OrbitalSchema {
        let mut schema = json!({ "name": "fixture", "version": "1.0.0", "orbitals": orbitals });
        if !app.is_null() {
            schema["osGrants"] = app;
        }
        serde_json::from_value(schema).expect("schema")
    }

    fn one(grants: serde_json::Value, effects: serde_json::Value) -> Vec<OsGrantFinding> {
        os_grant_findings(
            &program(
                json!(null),
                vec![orbital("Orders", grants, None, effects, json!({}))],
            ),
            OsGrantPass::Written,
        )
    }

    fn has(found: &[OsGrantFinding], code: OsGrantFindingCode) -> bool {
        found.iter().any(|f| f.code == code)
    }

    fn read_data() -> serde_json::Value {
        json!({ "fs": [{ "glob": "/data/**", "modes": ["read"] }] })
    }

    // EXEC-GRANTS-1: the kernel static grant check behind program/os-grants.
    #[test]
    fn a_call_outside_the_grants_is_missing_and_inside_is_not() {
        let outside = one(read_data(), json!([["os/read-file", "/etc/passwd"]]));
        assert!(has(&outside, OsGrantFindingCode::Missing));
        let f = outside
            .iter()
            .find(|f| f.code == OsGrantFindingCode::Missing)
            .unwrap();
        assert_eq!(
            (f.orbital.as_deref(), f.call.as_deref(), f.app),
            (Some("Orders"), Some("os/read-file"), Some(true))
        );
        assert_eq!(f.grant.as_deref(), Some("fs: { \"/etc/passwd\": [read] }"));
        assert!(has(
            &one(read_data(), json!([["os/write-file", "/data/a", "x"]])),
            OsGrantFindingCode::Missing
        ));
        assert!(has(
            &one(json!(null), json!([["os/memory-info"]])),
            OsGrantFindingCode::Missing
        ));
        assert!(
            one(read_data(), json!([["os/read-file", "/data/a"]])).is_empty(),
            "control"
        );
    }

    #[test]
    fn a_statically_false_guard_needs_no_grant_and_its_grant_is_unused() {
        let run = |guard: serde_json::Value, effects: serde_json::Value| {
            os_grant_findings(
                &program(
                    json!(null),
                    vec![orbital(
                        "Orders",
                        read_data(),
                        Some(guard),
                        effects,
                        json!({}),
                    )],
                ),
                OsGrantPass::Written,
            )
        };
        let passwd = json!([["os/read-file", "/etc/passwd"]]);
        assert!(!has(
            &run(json!(["!=", "", ""]), passwd.clone()),
            OsGrantFindingCode::Missing
        ));
        assert!(
            has(
                &run(json!(["=", "", ""]), passwd.clone()),
                OsGrantFindingCode::Missing
            ),
            "control: true guard"
        );
        assert!(
            has(
                &run(json!(["=", "@entity.id", ""]), passwd),
                OsGrantFindingCode::Missing
            ),
            "control: undecided guard"
        );
        assert!(has(
            &run(json!(["!=", "", ""]), json!([["os/read-file", "/data/a"]])),
            OsGrantFindingCode::Unused
        ));
    }

    #[test]
    fn grants_are_per_orbital_and_the_app_level_reaches_every_orbital() {
        let read = json!([["os/read-file", "/data/a"]]);
        let split = os_grant_findings(
            &program(
                json!(null),
                vec![
                    orbital("Orders", read_data(), None, read.clone(), json!({})),
                    orbital("Billing", json!(null), None, read.clone(), json!({})),
                ],
            ),
            OsGrantPass::Written,
        );
        let missing: Vec<_> = split
            .iter()
            .filter(|f| f.code == OsGrantFindingCode::Missing)
            .collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].orbital.as_deref(), Some("Billing"));
        let app = os_grant_findings(
            &program(
                read_data(),
                vec![
                    orbital("Orders", json!(null), None, read.clone(), json!({})),
                    orbital("Billing", json!(null), None, read, json!({})),
                ],
            ),
            OsGrantPass::Written,
        );
        assert!(app.is_empty(), "{app:?}");
    }

    #[test]
    fn a_computed_target_is_not_static_and_a_literal_or_handle_is_fine() {
        for effect in [
            json!(["os/read-file", "@payload.path"]),
            json!(["os/read-file", ["str/concat", "@payload.name", "/x"]]),
            json!(["os/spawn", "@payload.cmd", []]),
            json!(["os/tcp-connect", "db.internal", "@payload.port"]),
            json!(["os/getenv", "@payload.name"]),
        ] {
            let found = one(read_data(), json!([effect.clone()]));
            assert!(has(&found, OsGrantFindingCode::TargetNotStatic), "{effect}");
        }
        let found = one(read_data(), json!([["os/read-file", "@payload.path"]]));
        assert_eq!(
            found
                .iter()
                .find(|f| f.code == OsGrantFindingCode::TargetNotStatic)
                .unwrap()
                .argument,
            Some(1)
        );
        let handle = one(json!(null), json!([["os/close", "@payload.fd"]]));
        assert!(handle.is_empty(), "{handle:?}");
        assert!(!has(
            &one(read_data(), json!([["os/read-file", "/data/a"]])),
            OsGrantFindingCode::TargetNotStatic
        ));
    }

    #[test]
    fn a_config_rooted_target_is_deferred_and_a_directory_prefix_is_covered() {
        let config = one(json!(null), json!([["os/spawn", "@config.cmd", []]]));
        assert!(config.is_empty(), "{config:?}");
        for target in [
            json!("@payload.cmd"),
            json!("@entity.cmd"),
            json!(["object/get", "@payload.job", "command"]),
        ] {
            assert!(has(
                &one(json!(null), json!([["os/spawn", target, []]])),
                OsGrantFindingCode::TargetNotStatic
            ));
        }
        let covered = one(
            read_data(),
            json!([["os/read-file", ["str/concat", "/data/", "@payload.name"]]]),
        );
        assert!(covered.is_empty(), "{covered:?}");
        let uncovered = one(
            read_data(),
            json!([["os/read-file", ["str/concat", "/etc/", "@payload.name"]]]),
        );
        assert!(has(&uncovered, OsGrantFindingCode::Missing));
        let wrong_mode = one(
            read_data(),
            json!([[
                "os/write-file",
                ["str/concat", "/data/", "@payload.name"],
                "x"
            ]]),
        );
        assert!(has(&wrong_mode, OsGrantFindingCode::Missing));
        for target in [
            json!(["str/concat", "@payload.name", "/x"]),
            json!(["str/concat", "/data", "@payload.name"]),
            json!(["str/upper", "/data/"]),
        ] {
            assert!(has(
                &one(read_data(), json!([["os/read-file", target]])),
                OsGrantFindingCode::TargetNotStatic
            ));
        }
    }

    #[test]
    fn an_entry_no_call_uses_is_unused_and_an_entry_that_matches_nothing_is_invalid() {
        let grants =
            json!({ "fs": [{ "glob": "/data/**", "modes": ["read", "write"] }], "exec": ["git"] });
        let found = one(grants, json!([["os/read-file", "/data/a"]]));
        let labels: Vec<&str> = found
            .iter()
            .filter(|f| f.code == OsGrantFindingCode::Unused)
            .filter_map(|f| f.entry.as_deref())
            .collect();
        assert_eq!(labels.len(), 2, "{labels:?}");
        assert!(
            labels.iter().any(|m| m.contains("write")) && labels.iter().any(|m| m.contains("exec"))
        );
        assert!(
            !has(
                &one(read_data(), json!([["os/read-file", "/data/a"]])),
                OsGrantFindingCode::Unused
            ),
            "control"
        );
        for grants in [
            json!({ "fs": [{ "glob": "data/**", "modes": ["read"] }] }),
            json!({ "fs": [{ "glob": "/a/../b", "modes": ["read"] }] }),
            json!({ "fs": [{ "glob": "/a/**", "modes": [] }] }),
            json!({ "exec": ["git status"] }),
            json!({ "net": [{ "target": "db.internal", "kinds": ["tcp"] }] }),
            json!({ "env": ["A=B"] }),
        ] {
            assert!(
                has(
                    &one(grants.clone(), json!([])),
                    OsGrantFindingCode::EntryInvalid
                ),
                "{grants}"
            );
        }
        let good = json!({ "fs": [{ "glob": "/a/**", "modes": ["read"] }], "exec": ["git"], "net": [{ "target": "db.internal:5432", "kinds": ["tcp"] }], "env": ["HOME"] });
        assert!(
            !has(&one(good, json!([])), OsGrantFindingCode::EntryInvalid),
            "control"
        );
        let app = os_grant_findings(
            &program(
                json!({ "exec": [""] }),
                vec![orbital("Orders", json!(null), None, json!([]), json!({}))],
            ),
            OsGrantPass::Written,
        );
        assert!(has(&app, OsGrantFindingCode::EntryInvalid));
    }

    #[test]
    fn an_app_entry_is_used_when_any_orbital_uses_it() {
        let read = json!([["os/read-file", "/data/a"]]);
        let used = os_grant_findings(
            &program(
                read_data(),
                vec![
                    orbital("Orders", json!(null), None, read, json!({})),
                    orbital("Billing", json!(null), None, json!([]), json!({})),
                ],
            ),
            OsGrantPass::Written,
        );
        assert!(used.is_empty(), "{used:?}");
        let idle = os_grant_findings(
            &program(
                read_data(),
                vec![orbital("Orders", json!(null), None, json!([]), json!({}))],
            ),
            OsGrantPass::Written,
        );
        let f = idle
            .iter()
            .find(|f| f.code == OsGrantFindingCode::Unused)
            .expect("unused");
        assert_eq!(f.orbital, None);
    }

    #[test]
    fn an_inherited_spawn_env_entry_uses_and_needs_its_env_grant() {
        let spawn = json!([["os/spawn", "git", [], { "env": { "PATH": null } }]]);
        assert!(one(json!({ "exec": ["git"], "env": ["PATH"] }), spawn.clone()).is_empty());
        assert!(has(
            &one(json!({ "exec": ["git"] }), spawn),
            OsGrantFindingCode::Missing
        ));
        let literal = one(
            json!({ "exec": ["git"], "env": ["PATH"] }),
            json!([["os/spawn", "git", [], { "env": { "PATH": "/bin" } }]]),
        );
        assert!(
            has(&literal, OsGrantFindingCode::Unused),
            "control: a literal value uses no env grant"
        );
    }

    #[test]
    fn anchored_calls_are_covered_by_anchored_grants() {
        let grant =
            json!({"anchored": {"fs": [{"anchor": "cwd", "glob": "docs/**", "modes": ["read"]}]}});
        let read = |path: serde_json::Value| json!([["os/read-file", {"anchor": "cwd", "path": path}, {"emit": {"success": "INIT"}}]]);
        assert!(one(grant.clone(), read(json!("docs/Almadar_Gaps.md"))).is_empty());
        assert!(one(
            grant.clone(),
            read(json!(["str/concat", "docs/", "@entity.id"]))
        )
        .is_empty());
        let outside = one(grant.clone(), read(json!("src/a.rs")));
        assert!(
            has(&outside, OsGrantFindingCode::Missing) && has(&outside, OsGrantFindingCode::Unused),
            "control"
        );
        assert!(
            has(
                &one(grant, json!([["os/read-file", "/docs/a.md"]])),
                OsGrantFindingCode::Missing
            ),
            "control: plain path"
        );
        let bad = one(
            json!({"anchored": {"fs": [{"anchor": "cwd", "glob": "/abs/**", "modes": ["read"]}]}}),
            read(json!("a")),
        );
        assert!(has(&bad, OsGrantFindingCode::EntryInvalid));
        let narrow = json!({"anchored": {"fs": [{"anchor": "cwd", "glob": "**/*.lolo", "modes": ["read"]}]}});
        let computed = json!([["os/read-file", {"anchor": "cwd", "path": "@payload.path"}]]);
        assert!(one(narrow, computed.clone()).is_empty());
        let other = json!({"anchored": {"fs": [{"anchor": "exe-dir", "glob": "**/*.lolo", "modes": ["read"]}]}});
        assert!(
            has(&one(other, computed), OsGrantFindingCode::Missing),
            "control"
        );
        let net = json!({"anchored": {"net": [{"host": {"anchor": {"env": "HOST"}}, "port": {"anchor": {"env": "PORT"}}, "kinds": ["listen"]}]}});
        let listen =
            json!([["os/tcp-listen", {"anchor": {"env": "HOST"}}, {"anchor": {"env": "PORT"}}]]);
        assert!(one(net.clone(), listen).is_empty());
        assert!(
            has(
                &one(net, json!([["os/tcp-listen", "0.0.0.0", 8080]])),
                OsGrantFindingCode::Missing
            ),
            "control"
        );
    }

    #[test]
    fn the_finished_pass_checks_every_value_a_config_target_can_take() {
        let knob = json!({ "cmd": { "type": "string", "default": "git" } });
        let run = |grants: serde_json::Value, config: serde_json::Value| {
            os_grant_findings(
                &program(
                    json!(null),
                    vec![orbital(
                        "Orders",
                        grants,
                        None,
                        json!([["os/spawn", "@config.cmd", []]]),
                        config,
                    )],
                ),
                OsGrantPass::Final,
            )
        };
        assert!(run(json!({ "exec": ["git"] }), knob.clone()).is_empty());
        let missing = run(json!({ "exec": ["ls"] }), knob);
        let f = missing
            .iter()
            .find(|f| f.code == OsGrantFindingCode::Missing)
            .expect("missing");
        assert_eq!(f.via, Some(OsGrantVia::Config));
        let lost = run(json!(null), json!({}));
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].code, OsGrantFindingCode::TargetNotStatic);
        assert_eq!(lost[0].via, Some(OsGrantVia::ConfigUnresolvable));
    }
}

#[test]
fn an_unknown_fs_mode_is_refused_when_grants_are_decoded() {
    let refused = serde_json::from_value::<OsGrants>(
        json!({ "fs": [{ "glob": "/data/**", "modes": ["execute"] }] }),
    );
    assert!(refused.is_err());
    let known: OsGrants = serde_json::from_value(
        json!({ "fs": [{ "glob": "/data/**", "modes": ["read", "meta"] }] }),
    )
    .expect("known modes decode");
    assert_eq!(known.fs[0].modes, vec![FsMode::Read, FsMode::Meta]);
}

#[test]
fn the_grants_report_prints_codes_and_via_in_kebab_case_and_reads_back() {
    let finding = OsGrantFinding {
        code: OsGrantFindingCode::TargetNotStatic,
        orbital: Some("Runner".to_string()),
        call: Some("os/spawn".to_string()),
        entry: None,
        site: "Runner/T".to_string(),
        grant: None,
        app: None,
        argument: Some(1),
        via: Some(OsGrantVia::ConfigUnresolvable),
    };
    let report = OsGrantsReport { findings: vec![finding] };
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["findings"][0]["code"], "target-not-static");
    assert_eq!(json["findings"][0]["via"], "config-unresolvable");
    assert!(json["findings"][0].get("grant").is_none(), "an absent field is an absent key: {json}");
    assert_eq!(serde_json::from_value::<OsGrantsReport>(json).unwrap(), report);
    let bad = json!({ "findings": [{ "code": "missing", "site": "s", "via": "elsewhere" }] });
    assert!(serde_json::from_value::<OsGrantsReport>(bad).is_err(), "control: an unknown via is refused");
}
