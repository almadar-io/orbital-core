use super::*;
use std::path::Path;
use {crate::os_host::OsHandles, orbital_core_base::os_grants::OsGrants};

fn s(v: &str) -> Value {
    Value::string(v.to_string())
}

fn all_sys() -> OsGrants {
    OsGrants {
        sys: vec![
            SysKind::Cpu,
            SysKind::Memory,
            SysKind::Disk,
            SysKind::Network,
            SysKind::PowerState,
            SysKind::Identity,
        ],
        ..OsGrants::default()
    }
}

fn safe_desktop() -> OsGrants {
    OsGrants {
        desktop: vec![
            DesktopKind::Notify,
            DesktopKind::Speak,
            DesktopKind::Brightness,
            DesktopKind::Volume,
        ],
        ..OsGrants::default()
    }
}

fn ask(grants: OsGrants, op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let handles = OsHandles::default();
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let host = OsHost {
        grants: &grants,
        handles: &handles,
        on_event: &on_event,
        secrets: None,
    };
    answer(op, args, None, None, &host).expect("sys answers this op")
}

fn field(value: &Value, name: &str) -> Value {
    value
        .as_object()
        .and_then(|m| m.get(name))
        .cloned()
        .expect("field")
}

enum Shape {
    Num,
    Str,
    Bool,
    NumOrNull,
    StrOrNull,
    StrList,
    List(&'static [(&'static str, Shape)]),
    Map(&'static [(&'static str, Shape)]),
    Obj(&'static [(&'static str, Shape)]),
}

const COUNTERS: &[(&str, Shape)] = &[
    ("rxBytes", Shape::Num),
    ("txBytes", Shape::Num),
    ("rxPackets", Shape::Num),
    ("txPackets", Shape::Num),
    ("rxErrors", Shape::Num),
    ("txErrors", Shape::Num),
];

fn conforms(value: &Value, shape: &Shape) -> bool {
    let closed = |map: &ValueMap, fields: &[(&str, Shape)]| {
        map.len() == fields.len()
            && fields
                .iter()
                .all(|(k, sh)| map.get(*k).is_some_and(|v| conforms(v, sh)))
    };
    match (shape, value) {
        (Shape::Num, Value::Number(n)) => n.is_finite(),
        (Shape::Str, Value::String(_)) | (Shape::Bool, Value::Bool(_)) => true,
        (Shape::NumOrNull, Value::Number(_) | Value::Null) => true,
        (Shape::StrOrNull, Value::String(_) | Value::Null) => true,
        (Shape::StrList, Value::Array(items)) => {
            items.iter().all(|v| matches!(v, Value::String(_)))
        }
        (Shape::Obj(fields), Value::Object(map)) => closed(map, fields),
        (Shape::List(fields), Value::Array(items)) => items
            .iter()
            .all(|v| matches!(v, Value::Object(m) if closed(m, fields))),
        (Shape::Map(fields), Value::Object(map)) => map
            .values()
            .all(|v| matches!(v, Value::Object(m) if closed(m, fields))),
        _ => false,
    }
}

#[test]
fn every_sysinfo_op_is_denied_without_the_sys_grant_and_a_wrong_kind_is_not_enough() {
    let ops = [
        (OsOp::CpuUsage, SysKind::Cpu),
        (OsOp::MemoryInfo, SysKind::Memory),
        (OsOp::Uptime, SysKind::Cpu),
        (OsOp::LoadAvg, SysKind::Cpu),
        (OsOp::Hostname, SysKind::Identity),
        (OsOp::Uname, SysKind::Identity),
        (OsOp::DiskUsage, SysKind::Disk),
        (OsOp::Mounts, SysKind::Disk),
        (OsOp::DiskIo, SysKind::Disk),
        (OsOp::NetworkInterfaces, SysKind::Network),
        (OsOp::NetworkStats, SysKind::Network),
        (OsOp::NetworkConnected, SysKind::Network),
        (OsOp::DefaultGateway, SysKind::Network),
        (OsOp::BatteryStatus, SysKind::PowerState),
        (OsOp::AcPower, SysKind::PowerState),
        (OsOp::ThermalZones, SysKind::PowerState),
        (OsOp::Uid, SysKind::Identity),
        (OsOp::Gid, SysKind::Identity),
        (OsOp::Username, SysKind::Identity),
        (OsOp::GroupOf, SysKind::Identity),
    ];
    for (op, kind) in ops {
        let args = [s("/")];
        let none = ask(OsGrants::default(), op, &args).expect_err("denied");
        assert_eq!(none.kind, OsErrorKind::Denied, "{}", op.name());
        let other = if kind == SysKind::Cpu {
            SysKind::Disk
        } else {
            SysKind::Cpu
        };
        let wrong = OsGrants {
            sys: vec![other],
            ..OsGrants::default()
        };
        assert_eq!(
            ask(wrong, op, &args).expect_err("denied").kind,
            OsErrorKind::Denied,
            "{}",
            op.name()
        );
        let right = OsGrants {
            sys: vec![kind],
            ..OsGrants::default()
        };
        let granted = ask(right, op, &args);
        assert!(
            !matches!(&granted, Err(e) if e.kind == OsErrorKind::Denied),
            "{}",
            op.name()
        );
    }
}

#[test]
fn desktop_ops_need_their_grant_and_power_is_separate() {
    let ops = [
        OsOp::Notify,
        OsOp::Speak,
        OsOp::BrightnessGet,
        OsOp::BrightnessSet,
        OsOp::VolumeGet,
        OsOp::VolumeSet,
        OsOp::VolumeMute,
        OsOp::Suspend,
        OsOp::Hibernate,
    ];
    for op in ops {
        let denied = ask(OsGrants::default(), op, &[s("a"), s("b")]).expect_err("denied");
        assert_eq!(denied.kind, OsErrorKind::Denied, "{}", op.name());
    }
    for op in [OsOp::Suspend, OsOp::Hibernate] {
        let without_power = ask(safe_desktop(), op, &[]).expect_err("denied");
        assert_eq!(without_power.kind, OsErrorKind::Denied, "{}", op.name());
    }
}

#[test]
fn desktop_arguments_are_validated_before_any_tool_runs() {
    let g = safe_desktop;
    let invalid = |op, args: &[Value]| ask(g(), op, args).expect_err("invalid").kind;
    assert_eq!(
        invalid(OsOp::BrightnessSet, &[s("x")]),
        OsErrorKind::Invalid
    );
    assert_eq!(
        invalid(OsOp::BrightnessSet, &[Value::Number(101.0)]),
        OsErrorKind::Invalid
    );
    assert_eq!(
        invalid(OsOp::VolumeSet, &[Value::Number(-1.0)]),
        OsErrorKind::Invalid
    );
    assert_eq!(
        invalid(OsOp::VolumeSet, &[Value::Number(f64::NAN)]),
        OsErrorKind::Invalid
    );
    assert_eq!(invalid(OsOp::VolumeMute, &[s("yes")]), OsErrorKind::Invalid);
    assert_eq!(
        invalid(OsOp::Speak, &[Value::Number(1.0)]),
        OsErrorKind::Invalid
    );
    assert_eq!(
        invalid(OsOp::Notify, &[s("t"), s("b"), s("loud")]),
        OsErrorKind::Invalid
    );
    assert_eq!(invalid(OsOp::Notify, &[s("t")]), OsErrorKind::Invalid);
}

#[test]
fn desktop_tools_receive_arguments_as_an_array_never_a_shell_line() {
    let out = run_tool(OsOp::Speak, "/bin/echo", &["a;b", "$(x)", "`y`"]).expect("echo");
    assert_eq!(out, "a;b $(x) `y`\n");
}

#[test]
fn a_missing_tool_is_unsupported_not_a_failure() {
    let chain = [
        tool("orbital-no-such-tool-xyz", &["a"]),
        tool("orbital-no-such-tool-abc", &[]),
    ];
    let error = run_chain(OsOp::Speak, &chain).expect_err("absent");
    assert_eq!(error.kind, OsErrorKind::Unsupported);
    assert!(error.message.contains("orbital-no-such-tool-xyz"));
}

#[test]
fn watch_cron_is_an_unbounded_stream_and_unsupported() {
    let error =
        ask(OsGrants::default(), OsOp::WatchCron, &[s("* * * * *")]).expect_err("unsupported");
    assert_eq!(error.kind, OsErrorKind::Unsupported);
    assert!(error.message.contains("streaming host not implemented"));
}

#[test]
fn the_monotonic_clock_never_goes_backwards() {
    let mut last = 0.0;
    for _ in 0..50 {
        let Value::Number(n) = field(
            &ask(OsGrants::default(), OsOp::ClockMonotonic, &[]).expect("clock"),
            "nanos",
        ) else {
            panic!("nanos")
        };
        assert!(n >= last);
        last = n;
    }
}

#[cfg(unix)]
#[test]
fn identity_matches_libc() {
    let uid = field(&ask(all_sys(), OsOp::Uid, &[]).expect("uid"), "uid");
    let gid = field(&ask(all_sys(), OsOp::Gid, &[]).expect("gid"), "gid");
    assert_eq!(uid, Value::Number(f64::from(unsafe { libc::getuid() })));
    assert_eq!(gid, Value::Number(f64::from(unsafe { libc::getgid() })));
    let Value::String(name) = field(
        &ask(all_sys(), OsOp::Username, &[]).expect("name"),
        "username",
    ) else {
        panic!("username")
    };
    let passwd = unsafe { libc::getpwuid(libc::getuid()) };
    assert!(!passwd.is_null());
    let expected = unsafe { std::ffi::CStr::from_ptr((*passwd).pw_name) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(name, expected);
    assert!(!name.is_empty());
    let group = unsafe { libc::getgrgid(libc::getgid()) };
    assert!(!group.is_null());
    let group_name = unsafe { std::ffi::CStr::from_ptr((*group).gr_name) }
        .to_string_lossy()
        .into_owned();
    let found = ask(all_sys(), OsOp::GroupOf, &[s(&group_name)]).expect("group-of");
    assert_eq!(field(&found, "gid"), gid);
    assert!(matches!(field(&found, "members"), Value::Array(_)));
    let missing = ask(all_sys(), OsOp::GroupOf, &[s("orbital-no-such-group-xyz")])
        .expect_err("not found");
    assert_eq!(missing.kind, OsErrorKind::NotFound);
    assert_eq!(
        ask(all_sys(), OsOp::GroupOf, &[s("a\0b")])
            .expect_err("nul")
            .kind,
        OsErrorKind::Invalid
    );
}

#[test]
fn every_sysinfo_op_returns_its_closed_shape_or_unsupported() {
    let cases: Vec<(OsOp, Vec<Value>, Shape)> = vec![
        (
            OsOp::CpuUsage,
            vec![],
            Shape::Obj(&[("percent", Shape::Num)]),
        ),
        (
            OsOp::MemoryInfo,
            vec![],
            Shape::Obj(&[
                ("total", Shape::Num),
                ("available", Shape::Num),
                ("used", Shape::Num),
                ("usedPercent", Shape::Num),
            ]),
        ),
        (OsOp::Uptime, vec![], Shape::Obj(&[("seconds", Shape::Num)])),
        (
            OsOp::LoadAvg,
            vec![],
            Shape::Obj(&[
                ("one", Shape::Num),
                ("five", Shape::Num),
                ("fifteen", Shape::Num),
            ]),
        ),
        (
            OsOp::Hostname,
            vec![],
            Shape::Obj(&[("hostname", Shape::Str)]),
        ),
        (
            OsOp::Uname,
            vec![],
            Shape::Obj(&[
                ("sysname", Shape::Str),
                ("nodename", Shape::Str),
                ("release", Shape::Str),
                ("version", Shape::Str),
                ("machine", Shape::Str),
            ]),
        ),
        (
            OsOp::DiskUsage,
            vec![s("/")],
            Shape::Obj(&[
                ("path", Shape::Str),
                ("total", Shape::Num),
                ("used", Shape::Num),
                ("available", Shape::Num),
                ("usedPercent", Shape::Num),
                ("filesystem", Shape::Str),
            ]),
        ),
        (
            OsOp::NetworkConnected,
            vec![],
            Shape::Obj(&[("connected", Shape::Bool)]),
        ),
        (
            OsOp::DefaultGateway,
            vec![],
            Shape::Obj(&[("gateway", Shape::StrOrNull)]),
        ),
        (OsOp::AcPower, vec![], Shape::Obj(&[("onAc", Shape::Bool)])),
    ];
    for (op, args, shape) in cases {
        match ask(all_sys(), op, &args) {
            Ok(value) => assert!(conforms(&value, &shape), "{} -> {value:?}", op.name()),
            Err(e) => assert_eq!(
                e.kind,
                OsErrorKind::Unsupported,
                "{}: {}",
                op.name(),
                e.message
            ),
        }
    }
    let listed: Vec<(OsOp, &str, Shape)> = vec![
        (
            OsOp::Mounts,
            "mounts",
            Shape::List(&[
                ("device", Shape::Str),
                ("mountPoint", Shape::Str),
                ("filesystem", Shape::Str),
                ("options", Shape::StrList),
            ]),
        ),
        (
            OsOp::DiskIo,
            "devices",
            Shape::List(&[
                ("device", Shape::Str),
                ("readsCompleted", Shape::Num),
                ("writesCompleted", Shape::Num),
                ("readBytes", Shape::Num),
                ("writeBytes", Shape::Num),
            ]),
        ),
        (
            OsOp::NetworkInterfaces,
            "interfaces",
            Shape::List(&[
                ("name", Shape::Str),
                ("mac", Shape::Str),
                ("up", Shape::Bool),
                ("running", Shape::Bool),
            ]),
        ),
        (OsOp::NetworkStats, "interfaces", Shape::Map(COUNTERS)),
        (
            OsOp::ThermalZones,
            "zones",
            Shape::List(&[
                ("name", Shape::Str),
                ("temperature", Shape::Num),
                ("type", Shape::Str),
            ]),
        ),
    ];
    for (op, key, shape) in listed {
        match ask(all_sys(), op, &[]) {
            Ok(value) => {
                let inner = field(&value, key);
                assert_eq!(value.as_object().map(|m| m.len()), Some(1), "{}", op.name());
                assert!(conforms(&inner, &shape), "{} -> {inner:?}", op.name());
            }
            Err(e) => assert_eq!(
                e.kind,
                OsErrorKind::Unsupported,
                "{}: {}",
                op.name(),
                e.message
            ),
        }
    }
    match ask(all_sys(), OsOp::BatteryStatus, &[]) {
        Ok(value) => assert!(
            conforms(
                &value,
                &Shape::Obj(&[
                    ("present", Shape::Bool),
                    ("percentage", Shape::Num),
                    ("status", Shape::Str),
                    ("timeToEmpty", Shape::NumOrNull),
                    ("timeToFull", Shape::NumOrNull),
                ])
            ),
            "{value:?}"
        ),
        Err(e) => assert_eq!(e.kind, OsErrorKind::Unsupported),
    }
}

#[cfg(unix)]
#[test]
fn the_portable_readings_are_available_on_unix_with_sane_values() {
    for op in [
        OsOp::CpuUsage,
        OsOp::MemoryInfo,
        OsOp::Uptime,
        OsOp::LoadAvg,
        OsOp::Hostname,
        OsOp::Uname,
        OsOp::Mounts,
        OsOp::NetworkInterfaces,
        OsOp::NetworkConnected,
    ] {
        assert!(ask(all_sys(), op, &[]).is_ok(), "{}", op.name());
    }
    let Value::Number(p) = field(
        &ask(all_sys(), OsOp::CpuUsage, &[]).expect("cpu"),
        "percent",
    ) else {
        panic!("percent")
    };
    assert!((0.0..=100.0).contains(&p));
    let mem = ask(all_sys(), OsOp::MemoryInfo, &[]).expect("mem");
    let (Value::Number(total), Value::Number(used), Value::Number(pct)) = (
        field(&mem, "total"),
        field(&mem, "used"),
        field(&mem, "usedPercent"),
    ) else {
        panic!("numbers")
    };
    assert!(total > 0.0 && used <= total && (0.0..=100.0).contains(&pct));
    let disk = ask(all_sys(), OsOp::DiskUsage, &[s("/")]).expect("disk");
    let (Value::Number(dt), Value::Number(du)) = (field(&disk, "total"), field(&disk, "used"))
    else {
        panic!("numbers")
    };
    assert!(dt > 0.0 && du <= dt);
    let Value::String(fs_type) = field(&disk, "filesystem") else {
        panic!("fs")
    };
    assert!(!fs_type.is_empty());
    let none =
        ask(all_sys(), OsOp::DiskUsage, &[s("/orbital-no-such-dir-xyz")]).expect_err("missing");
    assert_eq!(none.kind, OsErrorKind::NotFound);
    assert_eq!(
        ask(all_sys(), OsOp::DiskUsage, &[s("a\0b")])
            .expect_err("nul")
            .kind,
        OsErrorKind::Invalid
    );
}

#[test]
fn proc_stat_gives_the_average_busy_percent_since_boot() {
    let stat = "cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 1 1 1 1 0 0 0 0 0\n";
    assert_eq!(procfs::cpu_percent(stat), Some(20.0));
    assert_eq!(procfs::cpu_percent("cpu 0 0 0 0\n"), Some(0.0));
    assert_eq!(procfs::cpu_percent("intr 1 2 3\n"), None);
    assert_eq!(procfs::cpu_percent("cpu 1 2 3\n"), None);
    assert_eq!(procfs::cpu_percent(""), None);
}

#[test]
fn meminfo_gives_total_and_available_with_the_old_kernel_estimate() {
    let modern = "MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 400 kB\n";
    assert_eq!(procfs::memory(modern), Some((1_024_000.0, 409_600.0)));
    let old = "MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 150 kB\n";
    assert_eq!(procfs::memory(old), Some((1_024_000.0, 307_200.0)));
    assert_eq!(procfs::memory("MemFree: 1 kB\n"), None);
}

#[test]
fn mounts_skip_virtual_file_systems_and_decode_spaces() {
    let raw = "proc /proc proc rw 0 0\n/dev/sda1 / ext4 rw,noatime 0 0\n/dev/sdb1 /mnt/my\\040disk vfat ro 0 0\ntmpfs /run tmpfs rw 0 0\n";
    let mounts = procfs::mounts(raw);
    assert_eq!(mounts.len(), 2);
    assert_eq!(field(&mounts[1], "mountPoint"), s("/mnt/my disk"));
    assert_eq!(
        field(&mounts[0], "options"),
        Value::array(vec![s("rw"), s("noatime")])
    );
}

#[test]
fn the_file_system_of_a_path_is_the_longest_covering_mount() {
    let raw = "/dev/sda1 / ext4 rw 0 0\n/dev/sdb1 /data xfs rw 0 0\n/dev/sdc1 /database btrfs rw 0 0\n";
    assert_eq!(
        procfs::fs_type_for(Path::new("/data/x/y"), raw).as_deref(),
        Some("xfs")
    );
    assert_eq!(
        procfs::fs_type_for(Path::new("/database/x"), raw).as_deref(),
        Some("btrfs")
    );
    assert_eq!(
        procfs::fs_type_for(Path::new("/datafoo"), raw).as_deref(),
        Some("ext4")
    );
    assert_eq!(
        procfs::fs_type_for(Path::new("/etc"), raw).as_deref(),
        Some("ext4")
    );
    assert_eq!(procfs::fs_type_for(Path::new("/etc"), "garbage\n"), None);
}

#[test]
fn diskstats_net_dev_and_route_parse_to_the_closed_shapes() {
    let disks = "   8       0 sda 100 0 2000 0 50 0 1000 0 0 0 0 0 0 0 0\n   8       1 sda1 1 0 1 0 1 0 1 0 0 0 0 0 0 0 0\n 259       0 nvme0n1 7 0 8 0 9 0 10 0 0 0 0 0 0 0 0\n 259       1 nvme0n1p1 7 0 8 0 9 0 10 0 0 0 0 0 0 0 0\n";
    let parsed = procfs::disk_io(disks);
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        field(&parsed[0], "readBytes"),
        Value::Number(2000.0 * 512.0)
    );
    assert_eq!(
        field(&parsed[0], "writeBytes"),
        Value::Number(1000.0 * 512.0)
    );
    assert_eq!(field(&parsed[0], "writesCompleted"), Value::Number(50.0));

    let dev = "Inter-|   Receive\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs\n    lo: 1 1 0 0 0 0 0 0 1 1 0 0 0 0 0 0\neth0:1000 10 2 0 0 0 0 0 2000 20 3 0 0 0 0 0\n";
    let stats = procfs::net_dev(dev);
    assert_eq!(stats.len(), 1);
    let eth = stats.get("eth0").expect("eth0").clone();
    assert_eq!(field(&eth, "rxBytes"), Value::Number(1000.0));
    assert_eq!(field(&eth, "txBytes"), Value::Number(2000.0));
    assert_eq!(field(&eth, "rxErrors"), Value::Number(2.0));
    assert_eq!(field(&eth, "txPackets"), Value::Number(20.0));
    assert!(conforms(&Value::object(stats), &Shape::Map(COUNTERS)));

    let route = "Iface\tDestination\tGateway\tFlags\neth0\t0001A8C0\t00000000\t0001\neth0\t00000000\t0100A8C0\t0003\n";
    assert_eq!(
        procfs::default_gateway(route).as_deref(),
        Some("192.168.0.1")
    );
    assert_eq!(
        procfs::default_gateway("Iface\tDestination\tGateway\n"),
        None
    );
}

#[test]
fn the_first_percent_is_read_from_tool_output() {
    assert_eq!(
        first_percent("Volume: front-left: 65536 / 100% / 0.00 dB"),
        Some(100.0)
    );
    assert_eq!(
        first_percent("Front Left: Playback 38 [58%] [on]"),
        Some(58.0)
    );
    assert_eq!(first_percent("no percent here"), None);
    assert_eq!(first_percent("%"), None);
}
