//! The `sys` family: monotonic clock, users, system information and desktop actions. `os/watch-cron` is an
//! unbounded stream a synchronous answerer cannot serve, so it answers `unsupported`.
//!
//! Information that a host cannot provide answers `unsupported` with the reason; no value is invented. Reads
//! are bounded ([`MAX_READ`]). `os/cpu-usage` is one reading of the cumulative counters, so `percent` is the
//! average load since boot: the skeleton keeps no per-host state across asks to sample a delta from.
//! Desktop actions run fixed tools with an argument array, never a shell; an absent tool is `unsupported`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::args::{arg_bool, arg_opt, arg_str, denied};
use super::net::io_error;
use super::OsHost;
use crate::resumable::CallSite;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::os_grants::{DesktopKind, SysKind};
use orbital_core_base::value::{Value, ValueMap};
use std::io::Read;
use std::process::{Command, Stdio};

const MAX_READ: u64 = 1024 * 1024;

fn err(op: OsOp, kind: OsErrorKind, what: impl std::fmt::Display) -> OsError {
    OsError::new(kind, format!("{}: {what}", op.name()))
}

fn unsupported(op: OsOp, what: impl std::fmt::Display) -> OsError {
    err(op, OsErrorKind::Unsupported, what)
}

fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = ValueMap::default();
    for (key, value) in pairs {
        map.insert(key.to_string().into(), value);
    }
    Value::object(map)
}

fn num(v: f64) -> Value {
    Value::Number(v)
}

fn text(v: impl Into<String>) -> Value {
    Value::string(v.into())
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn percent_of(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        round1(part / whole * 100.0)
    } else {
        0.0
    }
}

fn missing(op: OsOp, path: &str, e: &std::io::Error) -> OsError {
    if e.kind() == std::io::ErrorKind::NotFound {
        unsupported(op, format!("{path} is not available on this host"))
    } else {
        io_error(op, e)
    }
}

fn read_small(op: OsOp, path: &str) -> Result<String, OsError> {
    let file = std::fs::File::open(path).map_err(|e| missing(op, path, &e))?;
    let mut out = String::new();
    file.take(MAX_READ)
        .read_to_string(&mut out)
        .map_err(|e| missing(op, path, &e))?;
    Ok(out)
}

fn read_trimmed(path: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut out = String::new();
    file.take(MAX_READ).read_to_string(&mut out).ok()?;
    Some(out.trim().to_string())
}

fn read_f64(path: &str) -> Option<f64> {
    read_trimmed(path)?.parse().ok()
}

fn dir_names(op: OsOp, path: &str) -> Result<Vec<String>, OsError> {
    let entries = std::fs::read_dir(path).map_err(|e| missing(op, path, &e))?;
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

fn sys_kind(op: OsOp) -> Option<SysKind> {
    use orbital_core_base::os::OsOp::*;
    match op {
        Uid | Gid | Username | GroupOf | Hostname | Uname => Some(SysKind::Identity),
        CpuUsage | Uptime | LoadAvg => Some(SysKind::Cpu),
        MemoryInfo => Some(SysKind::Memory),
        ProcessList => Some(SysKind::Processes),
        DiskUsage | Mounts | DiskIo => Some(SysKind::Disk),
        NetworkInterfaces | NetworkStats | NetworkConnected | DefaultGateway => {
            Some(SysKind::Network)
        }
        BatteryStatus | AcPower | ThermalZones => Some(SysKind::PowerState),
        _ => None,
    }
}

fn sys_label(kind: SysKind) -> &'static str {
    match kind {
        SysKind::Cpu => "cpu",
        SysKind::Memory => "memory",
        SysKind::Disk => "disk",
        SysKind::Network => "network",
        SysKind::PowerState => "power-state",
        SysKind::Identity => "identity",
        SysKind::Processes => "processes",
        SysKind::SelfSignal => "self-signal",
        SysKind::ProcessSignal => "process-signal",
    }
}

fn desktop_kind(op: OsOp) -> Option<DesktopKind> {
    use orbital_core_base::os::OsOp::*;
    match op {
        Notify => Some(DesktopKind::Notify),
        Speak => Some(DesktopKind::Speak),
        BrightnessGet | BrightnessSet => Some(DesktopKind::Brightness),
        VolumeGet | VolumeSet | VolumeMute => Some(DesktopKind::Volume),
        Suspend | Hibernate => Some(DesktopKind::Power),
        _ => None,
    }
}

fn desktop_label(kind: DesktopKind) -> &'static str {
    match kind {
        DesktopKind::Notify => "notify",
        DesktopKind::Speak => "speak",
        DesktopKind::Brightness => "brightness",
        DesktopKind::Volume => "volume",
        DesktopKind::Power => "power",
    }
}

/// Parsers of the Linux kernel's text interfaces; pure, so they are checked against fixtures on any host.
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod procfs {
    use super::{round1, text, Value, ValueMap};
    use std::net::Ipv4Addr;
    use std::path::Path;

    pub(super) fn cpu_percent(stat: &str) -> Option<f64> {
        let line = stat.lines().next()?;
        let mut fields = line.split_whitespace();
        if fields.next()? != "cpu" {
            return None;
        }
        let ticks: Vec<u64> = fields
            .map(|p| p.parse().ok())
            .collect::<Option<Vec<u64>>>()?;
        if ticks.len() < 4 {
            return None;
        }
        let total: u64 = ticks.iter().take(8).sum();
        let idle = ticks[3] + ticks.get(4).copied().unwrap_or(0);
        if total == 0 {
            return Some(0.0);
        }
        Some(round1(100.0 * (1.0 - idle as f64 / total as f64)))
    }

    /// `(total, available)` in bytes; a kernel without `MemAvailable` gets the free + buffers + cached estimate.
    pub(super) fn memory(meminfo: &str) -> Option<(f64, f64)> {
        let (mut total, mut available, mut free, mut buffers, mut cached) = (None, None, 0, 0, 0);
        for line in meminfo.lines() {
            let mut parts = line.split_whitespace();
            let (Some(key), Some(kb)) = (
                parts.next(),
                parts.next().and_then(|v| v.parse::<u64>().ok()),
            ) else {
                continue;
            };
            match key {
                "MemTotal:" => total = Some(kb),
                "MemAvailable:" => available = Some(kb),
                "MemFree:" => free = kb,
                "Buffers:" => buffers = kb,
                "Cached:" => cached = kb,
                _ => {}
            }
        }
        let total = total?;
        let available = available.unwrap_or(free + buffers + cached);
        Some((total as f64 * 1024.0, available as f64 * 1024.0))
    }

    /// The fields `os/process-list` reads from `/proc/<pid>/stat`.
    #[derive(Debug, PartialEq)]
    pub(super) struct ProcessStat {
        pub(super) name: String,
        pub(super) ppid: u32,
        pub(super) user_ticks: u64,
        pub(super) system_ticks: u64,
        pub(super) start_ticks: u64,
        pub(super) rss_pages: u64,
    }

    /// The name is everything between the first `(` and the last `)`: it may itself hold spaces and parentheses.
    pub(super) fn process_stat(stat: &str) -> Option<ProcessStat> {
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        if close < open {
            return None;
        }
        let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        // `rest[0]` is field 3 (state); ppid is field 4, utime 14, stime 15, starttime 22, rss 24.
        let field = |n: usize| rest.get(n - 3).and_then(|v| v.parse::<u64>().ok());
        Some(ProcessStat {
            name: stat[open + 1..close].to_string(),
            ppid: u32::try_from(field(4)?).ok()?,
            user_ticks: field(14)?,
            system_ticks: field(15)?,
            start_ticks: field(22)?,
            rss_pages: field(24)?,
        })
    }

    /// `/proc/<pid>/cmdline`: the arguments, each ended by a NUL.
    pub(super) fn cmdline(raw: &[u8]) -> Vec<String> {
        let body = raw.strip_suffix(&[0]).unwrap_or(raw);
        if body.is_empty() {
            return Vec::new();
        }
        body.split(|b| *b == 0).map(|arg| String::from_utf8_lossy(arg).into_owned()).collect()
    }

    /// The boot time in seconds since the epoch (`btime` of `/proc/stat`).
    pub(super) fn boot_time(stat: &str) -> Option<f64> {
        stat.lines().find_map(|line| line.strip_prefix("btime ")?.trim().parse::<f64>().ok())
    }

    pub(super) fn uptime(raw: &str) -> Option<f64> {
        raw.split_whitespace().next()?.parse().ok()
    }

    fn unescape(field: &str) -> String {
        field.replace("\\040", " ")
    }

    fn is_virtual(fs_type: &str) -> bool {
        matches!(
            fs_type,
            "proc"
                | "sysfs"
                | "devtmpfs"
                | "devpts"
                | "tmpfs"
                | "securityfs"
                | "cgroup"
                | "cgroup2"
                | "pstore"
                | "debugfs"
                | "tracefs"
                | "hugetlbfs"
                | "mqueue"
                | "fusectl"
                | "binfmt_misc"
                | "configfs"
                | "efivarfs"
                | "autofs"
                | "overlay"
        )
    }

    pub(super) fn mounts(raw: &str) -> Vec<Value> {
        let mut out = Vec::new();
        for line in raw.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 || is_virtual(parts[2]) {
                continue;
            }
            let mut mount = ValueMap::default();
            mount.insert("device".into(), text(unescape(parts[0])));
            mount.insert("mountPoint".into(), text(unescape(parts[1])));
            mount.insert("filesystem".into(), text(parts[2]));
            mount.insert(
                "options".into(),
                Value::Array(parts[3].split(',').map(text).collect()),
            );
            out.push(Value::object(mount));
        }
        out
    }

    /// The type of the mount whose point is the longest prefix of `path`.
    pub(super) fn fs_type_for(path: &Path, raw_mounts: &str) -> Option<String> {
        let mut best: Option<(usize, &str)> = None;
        for line in raw_mounts.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }
            let point = unescape(parts[1]);
            if path.starts_with(&point) {
                let depth = Path::new(&point).components().count();
                if best.is_none_or(|(d, _)| depth >= d) {
                    best = Some((depth, parts[2]));
                }
            }
        }
        best.map(|(_, t)| t.to_string())
    }

    fn is_physical_disk(name: &str) -> bool {
        (name.starts_with("nvme") && name.contains('n') && !name.contains('p'))
            || ((name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd"))
                && name.len() == 3)
    }

    pub(super) fn disk_io(raw: &str) -> Vec<Value> {
        let mut out = Vec::new();
        for line in raw.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 14 || !is_physical_disk(f[2]) {
                continue;
            }
            let at = |i: usize| f[i].parse::<f64>().unwrap_or(0.0);
            let mut stat = ValueMap::default();
            stat.insert("device".into(), text(f[2]));
            stat.insert("readsCompleted".into(), Value::Number(at(3)));
            stat.insert("writesCompleted".into(), Value::Number(at(7)));
            stat.insert("readBytes".into(), Value::Number(at(5) * 512.0));
            stat.insert("writeBytes".into(), Value::Number(at(9) * 512.0));
            out.push(Value::object(stat));
        }
        out
    }

    pub(super) fn net_dev(raw: &str) -> ValueMap {
        let mut stats = ValueMap::default();
        for line in raw.lines().skip(2) {
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let f: Vec<&str> = rest.split_whitespace().collect();
            if name == "lo" || f.len() < 16 {
                continue;
            }
            let at = |i: usize| f[i].parse::<f64>().unwrap_or(0.0);
            let mut counters = ValueMap::default();
            counters.insert("rxBytes".into(), Value::Number(at(0)));
            counters.insert("txBytes".into(), Value::Number(at(8)));
            counters.insert("rxPackets".into(), Value::Number(at(1)));
            counters.insert("txPackets".into(), Value::Number(at(9)));
            counters.insert("rxErrors".into(), Value::Number(at(2)));
            counters.insert("txErrors".into(), Value::Number(at(10)));
            stats.insert(name.to_string().into(), Value::object(counters));
        }
        stats
    }

    pub(super) fn default_gateway(route: &str) -> Option<String> {
        for line in route.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 3 && f[1] == "00000000" {
                let raw = u32::from_str_radix(f[2], 16).ok()?;
                return Some(Ipv4Addr::from(raw.to_le_bytes()).to_string());
            }
        }
        None
    }
}

pub(super) struct Iface {
    name: String,
    mac: String,
    up: bool,
    running: bool,
}

/// One row of `os/process-list`. `user` is the owner as the platform names it (a user id on Unix, a SID on
/// Windows) and `own` whether that is the asker. Memory, CPU time and the start time are `None` when the host
/// refuses them for a process the asker does not own; `args` is empty when the arguments are not readable.
pub(super) struct Process {
    pid: u32,
    ppid: u32,
    name: String,
    command: String,
    args: Vec<String>,
    user: String,
    own: bool,
    rss_bytes: Option<f64>,
    cpu_seconds: Option<f64>,
    started_at: Option<f64>,
}

/// Whether the running process `pid` belongs to the user the program runs as; `None` when there is no such process.
#[cfg(unix)]
pub(super) fn process_is_own(pid: u32) -> Option<bool> {
    native::process_owner(pid).map(|owner| owner == unsafe { libc::getuid() })
}

#[cfg(windows)]
pub(super) fn process_is_own(pid: u32) -> Option<bool> {
    windows::process_is_own(pid)
}

#[cfg(windows)]
pub(super) fn terminate(op: OsOp, pid: u32) -> Result<(), OsError> {
    windows::terminate(op, pid)
}

/// A Windows file time (100 ns ticks since 1601) as seconds since 1970.
#[cfg_attr(not(windows), allow(dead_code))]
fn filetime_unix_seconds(ticks: u64) -> f64 {
    ticks as f64 / 1e7 - 11_644_473_600.0
}

/// A span of Windows file-time ticks (100 ns each) as seconds.
#[cfg_attr(not(windows), allow(dead_code))]
fn filetime_seconds(ticks: u64) -> f64 {
    ticks as f64 / 1e7
}

#[cfg(windows)]
mod windows;

/// `KERN_PROCARGS2` of one process: the argument count, the executable path, NUL padding, then the arguments.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn procargs2(raw: &[u8]) -> Vec<String> {
    let Some((count, rest)) = raw.split_first_chunk::<4>() else {
        return Vec::new();
    };
    let count = usize::try_from(i32::from_ne_bytes(*count)).unwrap_or(0);
    let Some(path_end) = rest.iter().position(|b| *b == 0) else {
        return Vec::new();
    };
    let after_path = &rest[path_end..];
    let Some(first) = after_path.iter().position(|b| *b != 0) else {
        return Vec::new();
    };
    after_path[first..]
        .split(|b| *b == 0)
        .take(count)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect()
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::ffi::{c_void, CStr, CString};

    fn process_args(pid: libc::pid_t) -> Vec<String> {
        let mut argmax: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        let sized = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 2, (&mut argmax as *mut libc::c_int).cast(), &mut len, std::ptr::null_mut(), 0)
        };
        let Ok(capacity) = usize::try_from(argmax) else {
            return Vec::new();
        };
        if sized != 0 || capacity == 0 {
            return Vec::new();
        }
        let mut raw = vec![0u8; capacity];
        let mut len = raw.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let read = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, raw.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
        };
        if read != 0 {
            return Vec::new();
        }
        raw.truncate(len);
        procargs2(&raw)
    }

    pub(super) fn process_owner(pid: u32) -> Option<u32> {
        let mut short: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(libc::pid_t::try_from(pid).ok()?, libc::PROC_PIDT_SHORTBSDINFO, 0, (&mut short as *mut libc::proc_bsdshortinfo).cast(), size)
        };
        (got == size).then_some(short.pbsi_uid)
    }

    /// `sizeof(struct kinfo_proc)`; its first field is the start time (`kp_proc.p_starttime`, a `timeval`).
    const KINFO_PROC_SIZE: usize = 648;

    /// A process the asker does not own: no memory, CPU time or arguments; a process that has ended has no row.
    fn other_users_process(pid: libc::pid_t, listed: u32) -> Option<Process> {
        let mut short: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let short_size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO, 0, (&mut short as *mut libc::proc_bsdshortinfo).cast(), short_size)
        };
        if got != short_size {
            return None;
        }
        let mut entry = [0u8; KINFO_PROC_SIZE];
        let mut len = entry.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
        let read = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 4, entry.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
        };
        if read != 0 || len != KINFO_PROC_SIZE {
            return None;
        }
        let seconds = i64::from_ne_bytes(*entry.first_chunk::<8>()?);
        let micros = i32::from_ne_bytes(*entry[8..].first_chunk::<4>()?);
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let path_len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        Some(Process {
            pid: listed,
            ppid: short.pbsi_ppid,
            // The short name fills its field with no terminator when it is as long as the field.
            name: String::from_utf8_lossy(&short.pbsi_comm.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect::<Vec<u8>>()).into_owned(),
            command: usize::try_from(path_len)
                .ok()
                .map(|n| String::from_utf8_lossy(&path[..n]).into_owned())
                .unwrap_or_default(),
            args: Vec::new(),
            user: short.pbsi_uid.to_string(),
            own: short.pbsi_uid == unsafe { libc::getuid() },
            rss_bytes: None,
            cpu_seconds: None,
            started_at: Some(seconds as f64 + f64::from(micros) / 1e6),
        })
    }

    #[allow(deprecated)]
    pub(super) fn processes(op: OsOp) -> Result<Vec<Process>, OsError> {
        let pid_size = std::mem::size_of::<libc::pid_t>();
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let Ok(count) = usize::try_from(count) else {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        };
        // Processes may start between the two asks: leave room for them.
        let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
        let filled = unsafe {
            libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * pid_size) as libc::c_int)
        };
        let Ok(filled) = usize::try_from(filled) else {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        };
        pids.truncate(filled);
        let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
        if unsafe { libc::mach_timebase_info(&mut timebase) } != 0 || timebase.denom == 0 {
            return Err(err(op, OsErrorKind::Io, "the host gave no time base"));
        }
        let nanos_per_tick = f64::from(timebase.numer) / f64::from(timebase.denom);
        let me = unsafe { libc::getuid() };
        let mut out = Vec::with_capacity(pids.len());
        for pid in pids {
            let Ok(listed) = u32::try_from(pid) else { continue };
            let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let bsd_size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            let got = unsafe {
                libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&mut bsd as *mut libc::proc_bsdinfo).cast(), bsd_size)
            };
            // The full record is the owner's and root's alone: another user's process is read through the short
            // record and the kernel's process entry, which every user may ask for.
            if got != bsd_size {
                if let Some(other) = other_users_process(pid, listed) {
                    out.push(other);
                }
                continue;
            }
            let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
            let task_size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
            let measured = unsafe {
                libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&mut task as *mut libc::proc_taskinfo).cast(), task_size)
            } == task_size;
            let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            let path_len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
            let command = usize::try_from(path_len)
                .ok()
                .map(|n| String::from_utf8_lossy(&path[..n]).into_owned())
                .unwrap_or_default();
            let c_text = |raw: &[libc::c_char]| unsafe { CStr::from_ptr(raw.as_ptr()) }.to_string_lossy().into_owned();
            let long_name = c_text(&bsd.pbi_name);
            out.push(Process {
                pid: listed,
                ppid: bsd.pbi_ppid,
                name: if long_name.is_empty() { c_text(&bsd.pbi_comm) } else { long_name },
                command,
                args: process_args(pid),
                user: bsd.pbi_uid.to_string(),
                own: bsd.pbi_uid == me,
                rss_bytes: measured.then_some(task.pti_resident_size as f64),
                cpu_seconds: measured
                    .then_some((task.pti_total_user + task.pti_total_system) as f64 * nanos_per_tick / 1e9),
                started_at: Some(bsd.pbi_start_tvsec as f64 + bsd.pbi_start_tvusec as f64 / 1e6),
            });
        }
        Ok(out)
    }

    fn sysctl_into<T>(op: OsOp, name: &str, value: &mut T) -> Result<(), OsError> {
        let cname =
            CString::new(name).map_err(|_| err(op, OsErrorKind::Invalid, "bad sysctl name"))?;
        let mut len = std::mem::size_of::<T>();
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                (value as *mut T).cast::<c_void>(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io_error(op, &std::io::Error::last_os_error()))
        }
    }

    #[allow(deprecated)]
    pub(super) fn cpu_percent(op: OsOp) -> Result<f64, OsError> {
        let mut info = libc::host_cpu_load_info { cpu_ticks: [0; 4] };
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        let rc = unsafe {
            libc::host_statistics(
                libc::mach_host_self(),
                libc::HOST_CPU_LOAD_INFO,
                (&mut info as *mut libc::host_cpu_load_info).cast(),
                &mut count,
            )
        };
        if rc != 0 {
            return Err(err(
                op,
                OsErrorKind::Io,
                format!("host_statistics failed ({rc})"),
            ));
        }
        let total: f64 = info.cpu_ticks.iter().map(|t| f64::from(*t)).sum();
        let idle = f64::from(info.cpu_ticks[libc::CPU_STATE_IDLE as usize]);
        Ok(if total > 0.0 {
            round1(100.0 * (1.0 - idle / total))
        } else {
            0.0
        })
    }

    #[allow(deprecated)]
    pub(super) fn memory(op: OsOp) -> Result<(f64, f64), OsError> {
        let mut total: u64 = 0;
        sysctl_into(op, "hw.memsize", &mut total)?;
        let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        let mut count = libc::HOST_VM_INFO64_COUNT;
        let rc = unsafe {
            libc::host_statistics64(
                libc::mach_host_self(),
                libc::HOST_VM_INFO64,
                (&mut stats as *mut libc::vm_statistics64).cast(),
                &mut count,
            )
        };
        if rc != 0 {
            return Err(err(
                op,
                OsErrorKind::Io,
                format!("host_statistics64 failed ({rc})"),
            ));
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let pages = f64::from(stats.free_count)
            + f64::from(stats.inactive_count)
            + f64::from(stats.purgeable_count);
        Ok((total as f64, pages * page.max(0) as f64))
    }

    pub(super) fn uptime_seconds(op: OsOp) -> Result<f64, OsError> {
        let mut boot: libc::timeval = unsafe { std::mem::zeroed() };
        sysctl_into(op, "kern.boottime", &mut boot)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| err(op, OsErrorKind::Io, e))?
            .as_secs_f64();
        let booted = boot.tv_sec as f64 + f64::from(boot.tv_usec) / 1e6;
        Ok((now - booted).max(0.0))
    }

    pub(super) fn mounts(op: OsOp) -> Result<Value, OsError> {
        let mut table: *mut libc::statfs = std::ptr::null_mut();
        let n = unsafe { libc::getmntinfo(&mut table, libc::MNT_NOWAIT) };
        if n <= 0 || table.is_null() {
            return Err(err(op, OsErrorKind::Io, "getmntinfo returned no mounts"));
        }
        let entries = unsafe { std::slice::from_raw_parts(table, n as usize) };
        let field = |raw: &[libc::c_char]| {
            unsafe { CStr::from_ptr(raw.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        let mut out = Vec::new();
        for entry in entries {
            let fs_type = field(&entry.f_fstypename);
            if matches!(fs_type.as_str(), "devfs" | "autofs") {
                continue;
            }
            let flags = entry.f_flags as libc::c_int;
            let mut options = vec![text(if flags & libc::MNT_RDONLY != 0 {
                "ro"
            } else {
                "rw"
            })];
            if flags & libc::MNT_NOSUID != 0 {
                options.push(text("nosuid"));
            }
            if flags & libc::MNT_NOEXEC != 0 {
                options.push(text("noexec"));
            }
            out.push(object([
                ("device", text(field(&entry.f_mntfromname))),
                ("mountPoint", text(field(&entry.f_mntonname))),
                ("filesystem", text(fs_type)),
                ("options", Value::array(options)),
            ]));
        }
        Ok(Value::array(out))
    }

    struct Seen {
        iface: Iface,
        loopback: bool,
        ipv4: bool,
    }

    fn walk(op: OsOp) -> Result<Vec<Seen>, OsError> {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        }
        let mut seen: Vec<Seen> = Vec::new();
        let mut cur = head;
        while !cur.is_null() {
            let ifa = unsafe { &*cur };
            cur = ifa.ifa_next;
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let flags = ifa.ifa_flags as libc::c_int;
            let at = match seen.iter().position(|s| s.iface.name == name) {
                Some(i) => i,
                None => {
                    seen.push(Seen {
                        iface: Iface {
                            name,
                            mac: String::new(),
                            up: flags & libc::IFF_UP != 0,
                            running: flags & libc::IFF_RUNNING != 0,
                        },
                        loopback: flags & libc::IFF_LOOPBACK != 0,
                        ipv4: false,
                    });
                    seen.len() - 1
                }
            };
            if ifa.ifa_addr.is_null() {
                continue;
            }
            let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
            if family == libc::AF_INET {
                seen[at].ipv4 = true;
            } else if family == libc::AF_LINK {
                let base = ifa.ifa_addr.cast::<u8>();
                let sdl = ifa.ifa_addr.cast::<libc::sockaddr_dl>();
                let (nlen, alen) =
                    unsafe { (usize::from((*sdl).sdl_nlen), usize::from((*sdl).sdl_alen)) };
                let start = std::mem::offset_of!(libc::sockaddr_dl, sdl_data) + nlen;
                let bytes: Vec<String> = (0..alen)
                    .map(|i| format!("{:02x}", unsafe { *base.add(start + i) }))
                    .collect();
                seen[at].iface.mac = bytes.join(":");
            }
        }
        unsafe { libc::freeifaddrs(head) };
        Ok(seen)
    }

    pub(super) fn interfaces(op: OsOp) -> Result<Vec<Iface>, OsError> {
        Ok(walk(op)?
            .into_iter()
            .filter(|s| !s.loopback)
            .map(|s| s.iface)
            .collect())
    }

    /// Connected: an up and running, non-loopback interface that carries an IPv4 address.
    pub(super) fn connected(op: OsOp) -> Result<bool, OsError> {
        Ok(walk(op)?
            .iter()
            .any(|s| !s.loopback && s.iface.up && s.iface.running && s.ipv4))
    }

    pub(super) fn filesystem_of(op: OsOp, path: &str) -> Result<String, OsError> {
        let cpath = CString::new(path)
            .map_err(|_| err(op, OsErrorKind::Invalid, "the path holds a NUL byte"))?;
        let mut info: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(cpath.as_ptr(), &mut info) } != 0 {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        }
        Ok(unsafe { CStr::from_ptr(info.f_fstypename.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::*;

    #[cfg(unix)]
    pub(super) fn process_owner(pid: u32) -> Option<u32> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(format!("/proc/{pid}")).ok().map(|owner| owner.uid())
    }

    #[cfg(windows)]
    pub(super) fn processes(op: OsOp) -> Result<Vec<Process>, OsError> {
        super::windows::processes(op)
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn processes(op: OsOp) -> Result<Vec<Process>, OsError> {
        Err(unsupported(op, "the process table is not readable on this host"))
    }

    #[cfg(unix)]
    pub(super) fn processes(op: OsOp) -> Result<Vec<Process>, OsError> {
        use std::os::unix::fs::MetadataExt;
        let boot = procfs::boot_time(&read_small(op, "/proc/stat")?)
            .ok_or_else(|| err(op, OsErrorKind::Io, "/proc/stat has no btime"))?;
        let (ticks, page) = unsafe { (libc::sysconf(libc::_SC_CLK_TCK), libc::sysconf(libc::_SC_PAGESIZE)) };
        if ticks <= 0 || page <= 0 {
            return Err(err(op, OsErrorKind::Io, "the host gave no clock tick or page size"));
        }
        let (ticks, page) = (ticks as f64, page as f64);
        let mut out = Vec::new();
        for entry in dir_names(op, "/proc")? {
            let Ok(pid) = entry.parse::<u32>() else { continue };
            // A process that ended since the listing has no stat file and no row.
            let Some(raw) = read_trimmed(&format!("/proc/{pid}/stat")) else { continue };
            let stat = procfs::process_stat(&raw)
                .ok_or_else(|| err(op, OsErrorKind::Io, format!("/proc/{pid}/stat is not in the expected format")))?;
            let Ok(owner) = std::fs::metadata(format!("/proc/{pid}")) else { continue };
            let args = std::fs::read(format!("/proc/{pid}/cmdline")).map(|raw| procfs::cmdline(&raw)).unwrap_or_default();
            let command = std::fs::read_link(format!("/proc/{pid}/exe"))
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(Process {
                pid,
                ppid: stat.ppid,
                name: stat.name,
                command,
                args,
                user: owner.uid().to_string(),
                own: owner.uid() == unsafe { libc::getuid() },
                rss_bytes: Some(stat.rss_pages as f64 * page),
                cpu_seconds: Some((stat.user_ticks + stat.system_ticks) as f64 / ticks),
                started_at: Some(boot + stat.start_ticks as f64 / ticks),
            });
        }
        Ok(out)
    }

    pub(super) fn cpu_percent(op: OsOp) -> Result<f64, OsError> {
        procfs::cpu_percent(&read_small(op, "/proc/stat")?).ok_or_else(|| {
            err(
                op,
                OsErrorKind::Io,
                "/proc/stat is not in the expected format",
            )
        })
    }

    pub(super) fn memory(op: OsOp) -> Result<(f64, f64), OsError> {
        procfs::memory(&read_small(op, "/proc/meminfo")?)
            .ok_or_else(|| err(op, OsErrorKind::Io, "/proc/meminfo has no MemTotal"))
    }

    pub(super) fn uptime_seconds(op: OsOp) -> Result<f64, OsError> {
        procfs::uptime(&read_small(op, "/proc/uptime")?).ok_or_else(|| {
            err(
                op,
                OsErrorKind::Io,
                "/proc/uptime is not in the expected format",
            )
        })
    }

    pub(super) fn mounts(op: OsOp) -> Result<Value, OsError> {
        Ok(Value::Array(
            procfs::mounts(&read_small(op, "/proc/mounts")?).into(),
        ))
    }

    pub(super) fn interfaces(op: OsOp) -> Result<Vec<Iface>, OsError> {
        let mut out = Vec::new();
        for name in dir_names(op, "/sys/class/net")? {
            if name == "lo" {
                continue;
            }
            let base = format!("/sys/class/net/{name}");
            let flags = read_trimmed(&format!("{base}/flags"))
                .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0);
            out.push(Iface {
                mac: read_trimmed(&format!("{base}/address")).unwrap_or_default(),
                up: flags & 0x1 != 0,
                running: flags & 0x40 != 0,
                name,
            });
        }
        Ok(out)
    }

    pub(super) fn connected(op: OsOp) -> Result<bool, OsError> {
        for name in dir_names(op, "/sys/class/net")? {
            if name == "lo" {
                continue;
            }
            let base = format!("/sys/class/net/{name}");
            if read_trimmed(&format!("{base}/operstate")).as_deref() == Some("up")
                && read_trimmed(&format!("{base}/carrier")).as_deref() == Some("1")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn filesystem_of(op: OsOp, path: &str) -> Result<String, OsError> {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
        procfs::fs_type_for(&real, &read_small(op, "/proc/mounts")?)
            .ok_or_else(|| unsupported(op, "no mount covers the path"))
    }
}

#[cfg(unix)]
mod posix {
    use super::*;
    use std::ffi::{CStr, CString};

    const MAX_NSS_BUFFER: usize = 1024 * 1024;

    fn initial_buffer(name: libc::c_int) -> usize {
        let n = unsafe { libc::sysconf(name) };
        usize::try_from(n).unwrap_or(0).clamp(1024, 65_536)
    }

    fn lookup_error(op: OsOp, rc: libc::c_int) -> OsError {
        io_error(op, &std::io::Error::from_raw_os_error(rc))
    }

    pub(super) fn uid() -> f64 {
        f64::from(unsafe { libc::getuid() })
    }

    pub(super) fn gid() -> f64 {
        f64::from(unsafe { libc::getgid() })
    }

    pub(super) fn username(op: OsOp) -> Result<String, OsError> {
        let uid = unsafe { libc::getuid() };
        let mut size = initial_buffer(libc::_SC_GETPW_R_SIZE_MAX);
        loop {
            let mut buf = vec![0 as libc::c_char; size];
            let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
            let mut found: *mut libc::passwd = std::ptr::null_mut();
            let rc =
                unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
            if rc == libc::ERANGE && size < MAX_NSS_BUFFER {
                size *= 2;
                continue;
            }
            if rc != 0 {
                return Err(lookup_error(op, rc));
            }
            if found.is_null() {
                return Err(err(
                    op,
                    OsErrorKind::NotFound,
                    format!("no passwd entry for uid {uid}"),
                ));
            }
            return Ok(unsafe { CStr::from_ptr(pwd.pw_name) }
                .to_string_lossy()
                .into_owned());
        }
    }

    pub(super) fn group_of(op: OsOp, name: &str) -> Result<(f64, Vec<String>), OsError> {
        let cname = CString::new(name)
            .map_err(|_| err(op, OsErrorKind::Invalid, "the group name holds a NUL byte"))?;
        let mut size = initial_buffer(libc::_SC_GETGR_R_SIZE_MAX);
        loop {
            let mut buf = vec![0 as libc::c_char; size];
            let mut grp: libc::group = unsafe { std::mem::zeroed() };
            let mut found: *mut libc::group = std::ptr::null_mut();
            let rc = unsafe {
                libc::getgrnam_r(
                    cname.as_ptr(),
                    &mut grp,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut found,
                )
            };
            if rc == libc::ERANGE && size < MAX_NSS_BUFFER {
                size *= 2;
                continue;
            }
            if rc != 0 {
                return Err(lookup_error(op, rc));
            }
            if found.is_null() {
                return Err(err(
                    op,
                    OsErrorKind::NotFound,
                    format!("no group named {name}"),
                ));
            }
            let mut members = Vec::new();
            let mut at = grp.gr_mem;
            while !at.is_null() && unsafe { !(*at).is_null() } {
                members.push(
                    unsafe { CStr::from_ptr(*at) }
                        .to_string_lossy()
                        .into_owned(),
                );
                at = unsafe { at.add(1) };
            }
            return Ok((f64::from(grp.gr_gid), members));
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(super) fn load_avg(op: OsOp) -> Result<[f64; 3], OsError> {
        let mut avg = [0f64; 3];
        let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
        if n == 3 {
            Ok(avg)
        } else {
            Err(unsupported(op, "the load averages are not available"))
        }
    }

    #[cfg(any(target_os = "android", test))]
    pub(super) fn parse_proc_loadavg(op: OsOp, contents: &str) -> Result<[f64; 3], OsError> {
        let mut words = contents.split_whitespace();
        let mut avg = [0.0; 3];
        for value in &mut avg {
            let word = words.next().ok_or_else(|| err(op, OsErrorKind::Io, "incomplete /proc/loadavg"))?;
            *value = word.parse::<f64>().map_err(|_| err(op, OsErrorKind::Io, "invalid /proc/loadavg"))?;
            if !value.is_finite() || *value < 0.0 {
                return Err(err(op, OsErrorKind::Io, "invalid /proc/loadavg"));
            }
        }
        Ok(avg)
    }

    #[cfg(target_os = "android")]
    pub(super) fn load_avg(op: OsOp) -> Result<[f64; 3], OsError> {
        parse_proc_loadavg(op, &read_small(op, "/proc/loadavg")?)
    }

    pub(super) fn hostname(op: OsOp) -> Result<String, OsError> {
        let mut buf = [0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
        if rc != 0 {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
    }

    pub(super) fn uname(op: OsOp) -> Result<[String; 5], OsError> {
        let mut info: libc::utsname = unsafe { std::mem::zeroed() };
        if unsafe { libc::uname(&mut info) } != 0 {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        }
        let field = |raw: &[libc::c_char]| {
            unsafe { CStr::from_ptr(raw.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        Ok([
            field(&info.sysname),
            field(&info.nodename),
            field(&info.release),
            field(&info.version),
            field(&info.machine),
        ])
    }

    /// `(total, used, available)` bytes of the file system holding `path`.
    pub(super) fn disk_space(op: OsOp, path: &str) -> Result<(f64, f64, f64), OsError> {
        let cpath = CString::new(path)
            .map_err(|_| err(op, OsErrorKind::Invalid, "the path holds a NUL byte"))?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(cpath.as_ptr(), &mut st) } != 0 {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        }
        let unit = st.f_frsize as f64;
        let total = st.f_blocks as f64 * unit;
        let free = st.f_bfree as f64 * unit;
        let available = st.f_bavail as f64 * unit;
        Ok((total, (total - free).max(0.0), available))
    }
}

#[cfg(not(unix))]
mod posix {
    use super::*;

    fn none(op: OsOp) -> OsError {
        unsupported(op, "this platform has no POSIX interface")
    }

    pub(super) fn uid() -> f64 {
        0.0
    }

    pub(super) fn gid() -> f64 {
        0.0
    }

    pub(super) fn username(op: OsOp) -> Result<String, OsError> {
        Err(none(op))
    }

    pub(super) fn group_of(op: OsOp, _name: &str) -> Result<(f64, Vec<String>), OsError> {
        Err(none(op))
    }

    pub(super) fn load_avg(op: OsOp) -> Result<[f64; 3], OsError> {
        Err(none(op))
    }

    pub(super) fn hostname(op: OsOp) -> Result<String, OsError> {
        Err(none(op))
    }

    pub(super) fn uname(op: OsOp) -> Result<[String; 5], OsError> {
        Err(none(op))
    }

    pub(super) fn disk_space(op: OsOp, _path: &str) -> Result<(f64, f64, f64), OsError> {
        Err(none(op))
    }
}


pub(super) fn answer(
    op: OsOp,
    args: &[Value],
    _key: Option<&str>,
    _site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Option<Result<Value, OsError>> {
    if let Some(kind) = sys_kind(op) {
        if !host.grants.sys_allows(kind) {
            return Some(Err(denied(
                op,
                &format!("the sys grant {} is missing", sys_label(kind)),
            )));
        }
    }
    if let Some(kind) = desktop_kind(op) {
        if !host.grants.desktop_allows(kind) {
            return Some(Err(denied(
                op,
                &format!("the desktop grant {} is missing", desktop_label(kind)),
            )));
        }
    }
    Some(match op {
        OsOp::ClockMonotonic => Ok(clock_monotonic()),
        OsOp::Uid => uid(),
        OsOp::Gid => gid(),
        OsOp::Username => username(op),
        OsOp::GroupOf => group_of(op, args),
        OsOp::WatchCron => Err(unsupported(op, "streaming host not implemented")),
        OsOp::CpuUsage => cpu_usage(op),
        OsOp::MemoryInfo => memory_info(op),
        OsOp::ProcessList => process_list(op),
        OsOp::Uptime => uptime(op),
        OsOp::LoadAvg => load_avg(op),
        OsOp::Hostname => hostname(op),
        OsOp::Uname => uname(op),
        OsOp::DiskUsage => disk_usage(op, args),
        OsOp::Mounts => mounts(op),
        OsOp::DiskIo => disk_io(op),
        OsOp::NetworkInterfaces => network_interfaces(op),
        OsOp::NetworkStats => network_stats(op),
        OsOp::NetworkConnected => network_connected(op),
        OsOp::DefaultGateway => default_gateway(op),
        OsOp::BatteryStatus => battery_status(op),
        OsOp::AcPower => ac_power(op),
        OsOp::ThermalZones => thermal_zones(op),
        OsOp::Notify => notify(op, args),
        OsOp::Speak => speak(op, args),
        OsOp::BrightnessGet => brightness_get(op),
        OsOp::BrightnessSet => brightness_set(op, args),
        OsOp::VolumeGet => volume_get(op),
        OsOp::VolumeSet => volume_set(op, args),
        OsOp::VolumeMute => volume_mute(op, args),
        OsOp::Suspend => suspend(op),
        OsOp::Hibernate => hibernate(op),
        _ => return None,
    })
}

mod info;
mod power;
mod desktop;
use info::*;
use power::*;
use desktop::*;

#[cfg(test)]
mod tests;
