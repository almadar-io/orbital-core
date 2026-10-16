//! The `sys` family: monotonic clock, users, system information and desktop actions. `os/watch-cron` is an
//! unbounded stream a synchronous answerer cannot serve, so it answers `unsupported`.
//!
//! Information that a host cannot provide answers `unsupported` with the reason; no value is invented. Reads
//! are bounded ([`MAX_READ`]). `os/cpu-usage` is one reading of the cumulative counters, so `percent` is the
//! average load since boot: the skeleton keeps no per-host state across asks to sample a delta from.
//! Desktop actions run fixed tools with an argument array, never a shell; an absent tool is `unsupported`.

use super::args::{arg_bool, arg_opt, arg_str, denied};
use super::net::io_error;
use super::{DesktopKind, OsHost, SysKind};
use crate::evaluator::{Value, ValueMap};
use crate::runtime::os::{OsError, OsErrorKind, OsOp};
use crate::runtime::resumable::CallSite;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Instant;

const MAX_READ: u64 = 1024 * 1024;

fn err(op: OsOp, kind: OsErrorKind, what: impl std::fmt::Display) -> OsError {
    OsError::new(kind, format!("{}: {what}", op.name()))
}

fn unsupported(op: OsOp, what: impl std::fmt::Display) -> OsError {
    err(op, OsErrorKind::Unsupported, what)
}

fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = ValueMap::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

fn num(v: f64) -> Value {
    Value::Number(v)
}

fn text(v: impl Into<String>) -> Value {
    Value::String(v.into())
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
    use OsOp::*;
    match op {
        Uid | Gid | Username | GroupOf | Hostname | Uname => Some(SysKind::Identity),
        CpuUsage | Uptime | LoadAvg => Some(SysKind::Cpu),
        MemoryInfo => Some(SysKind::Memory),
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
    }
}

fn desktop_kind(op: OsOp) -> Option<DesktopKind> {
    use OsOp::*;
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
            let mut mount = ValueMap::new();
            mount.insert("device".to_string(), text(unescape(parts[0])));
            mount.insert("mountPoint".to_string(), text(unescape(parts[1])));
            mount.insert("filesystem".to_string(), text(parts[2]));
            mount.insert(
                "options".to_string(),
                Value::Array(parts[3].split(',').map(text).collect()),
            );
            out.push(Value::Object(mount));
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
            let mut stat = ValueMap::new();
            stat.insert("device".to_string(), text(f[2]));
            stat.insert("readsCompleted".to_string(), Value::Number(at(3)));
            stat.insert("writesCompleted".to_string(), Value::Number(at(7)));
            stat.insert("readBytes".to_string(), Value::Number(at(5) * 512.0));
            stat.insert("writeBytes".to_string(), Value::Number(at(9) * 512.0));
            out.push(Value::Object(stat));
        }
        out
    }

    pub(super) fn net_dev(raw: &str) -> ValueMap {
        let mut stats = ValueMap::new();
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
            let mut counters = ValueMap::new();
            counters.insert("rxBytes".to_string(), Value::Number(at(0)));
            counters.insert("txBytes".to_string(), Value::Number(at(8)));
            counters.insert("rxPackets".to_string(), Value::Number(at(1)));
            counters.insert("txPackets".to_string(), Value::Number(at(9)));
            counters.insert("rxErrors".to_string(), Value::Number(at(2)));
            counters.insert("txErrors".to_string(), Value::Number(at(10)));
            stats.insert(name.to_string(), Value::Object(counters));
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

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::ffi::{c_void, CStr, CString};

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
                ("options", Value::Array(options)),
            ]));
        }
        Ok(Value::Array(out))
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
        Ok(Value::Array(procfs::mounts(&read_small(
            op,
            "/proc/mounts",
        )?)))
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

    pub(super) fn load_avg(op: OsOp) -> Result<[f64; 3], OsError> {
        let mut avg = [0f64; 3];
        let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
        if n == 3 {
            Ok(avg)
        } else {
            Err(unsupported(op, "the load averages are not available"))
        }
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

fn clock_monotonic() -> Value {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    object([("nanos", num(origin.elapsed().as_nanos() as f64))])
}

fn uid() -> Result<Value, OsError> {
    Ok(object([("uid", num(posix::uid()))]))
}

fn gid() -> Result<Value, OsError> {
    Ok(object([("gid", num(posix::gid()))]))
}

fn username(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("username", text(posix::username(op)?))]))
}

fn group_of(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let (gid, members) = posix::group_of(op, arg_str(args, 0, op)?)?;
    Ok(object([
        ("gid", num(gid)),
        (
            "members",
            Value::Array(members.into_iter().map(text).collect()),
        ),
    ]))
}

fn cpu_usage(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("percent", num(native::cpu_percent(op)?))]))
}

fn memory_info(op: OsOp) -> Result<Value, OsError> {
    let (total, available) = native::memory(op)?;
    let used = (total - available).max(0.0);
    Ok(object([
        ("total", num(total)),
        ("available", num(available)),
        ("used", num(used)),
        ("usedPercent", num(percent_of(used, total))),
    ]))
}

fn uptime(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("seconds", num(native::uptime_seconds(op)?))]))
}

fn load_avg(op: OsOp) -> Result<Value, OsError> {
    let [one, five, fifteen] = posix::load_avg(op)?;
    Ok(object([
        ("one", num(one)),
        ("five", num(five)),
        ("fifteen", num(fifteen)),
    ]))
}

fn hostname(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("hostname", text(posix::hostname(op)?))]))
}

fn uname(op: OsOp) -> Result<Value, OsError> {
    let [sysname, nodename, release, version, machine] = posix::uname(op)?;
    Ok(object([
        ("sysname", text(sysname)),
        ("nodename", text(nodename)),
        ("release", text(release)),
        ("version", text(version)),
        ("machine", text(machine)),
    ]))
}

fn disk_usage(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let path = arg_str(args, 0, op)?;
    let (total, used, available) = posix::disk_space(op, path)?;
    Ok(object([
        ("path", text(path)),
        ("total", num(total)),
        ("used", num(used)),
        ("available", num(available)),
        ("usedPercent", num(percent_of(used, total))),
        ("filesystem", text(native::filesystem_of(op, path)?)),
    ]))
}

fn mounts(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("mounts", native::mounts(op)?)]))
}

fn disk_io(op: OsOp) -> Result<Value, OsError> {
    let devices = procfs::disk_io(&read_small(op, "/proc/diskstats")?);
    Ok(object([("devices", Value::Array(devices))]))
}

fn network_interfaces(op: OsOp) -> Result<Value, OsError> {
    let list = native::interfaces(op)?
        .into_iter()
        .map(|i| {
            object([
                ("name", text(i.name)),
                ("mac", text(i.mac)),
                ("up", Value::Bool(i.up)),
                ("running", Value::Bool(i.running)),
            ])
        })
        .collect();
    Ok(object([("interfaces", Value::Array(list))]))
}

fn network_stats(op: OsOp) -> Result<Value, OsError> {
    let stats = procfs::net_dev(&read_small(op, "/proc/net/dev")?);
    Ok(object([("interfaces", Value::Object(stats))]))
}

fn network_connected(op: OsOp) -> Result<Value, OsError> {
    Ok(object([("connected", Value::Bool(native::connected(op)?))]))
}

fn default_gateway(op: OsOp) -> Result<Value, OsError> {
    let gateway = procfs::default_gateway(&read_small(op, "/proc/net/route")?);
    Ok(object([("gateway", gateway.map_or(Value::Null, text))]))
}

const POWER_SUPPLY: &str = "/sys/class/power_supply";

fn battery_status(op: OsOp) -> Result<Value, OsError> {
    let names = dir_names(op, POWER_SUPPLY)?;
    let Some(name) = names.iter().find(|n| n.starts_with("BAT")) else {
        return Ok(object([
            ("present", Value::Bool(false)),
            ("percentage", num(0.0)),
            ("status", text("unknown")),
            ("timeToEmpty", Value::Null),
            ("timeToFull", Value::Null),
        ]));
    };
    let base = format!("{POWER_SUPPLY}/{name}");
    let percentage = read_f64(&format!("{base}/capacity"))
        .ok_or_else(|| unsupported(op, format!("{base}/capacity is not readable")))?;
    let raw_status = read_trimmed(&format!("{base}/status"))
        .unwrap_or_default()
        .to_lowercase();
    let status = match raw_status.as_str() {
        "charging" => "charging",
        "discharging" => "discharging",
        "full" => "full",
        "not charging" => "not_charging",
        _ => "unknown",
    };
    let rate =
        read_f64(&format!("{base}/power_now")).or_else(|| read_f64(&format!("{base}/current_now")));
    let (now, full) = if read_f64(&format!("{base}/power_now")).is_some() {
        (
            read_f64(&format!("{base}/energy_now")),
            read_f64(&format!("{base}/energy_full")),
        )
    } else {
        (
            read_f64(&format!("{base}/charge_now")),
            read_f64(&format!("{base}/charge_full")),
        )
    };
    let hours = |amount: Option<f64>| match (amount, rate) {
        (Some(a), Some(r)) if r > 0.0 => Value::Number((a / r * 3600.0).round()),
        _ => Value::Null,
    };
    let to_empty = if status == "discharging" {
        hours(now)
    } else {
        Value::Null
    };
    let to_full = match (status, full, now) {
        ("charging", Some(f), Some(n)) => hours(Some((f - n).max(0.0))),
        _ => Value::Null,
    };
    Ok(object([
        ("present", Value::Bool(true)),
        ("percentage", num(percentage)),
        ("status", text(status)),
        ("timeToEmpty", to_empty),
        ("timeToFull", to_full),
    ]))
}

fn ac_power(op: OsOp) -> Result<Value, OsError> {
    let mut mains = 0;
    let mut online = false;
    for name in dir_names(op, POWER_SUPPLY)? {
        let base = format!("{POWER_SUPPLY}/{name}");
        if read_trimmed(&format!("{base}/type")).as_deref() == Some("Mains") {
            mains += 1;
            online |= read_trimmed(&format!("{base}/online")).as_deref() == Some("1");
        }
    }
    if mains == 0 {
        return Err(unsupported(op, "the host reports no mains power supply"));
    }
    Ok(object([("onAc", Value::Bool(online))]))
}

fn thermal_zones(op: OsOp) -> Result<Value, OsError> {
    const THERMAL: &str = "/sys/class/thermal";
    let mut zones = Vec::new();
    for name in dir_names(op, THERMAL)? {
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let base = format!("{THERMAL}/{name}");
        let Some(milli) = read_f64(&format!("{base}/temp")) else {
            continue;
        };
        zones.push(object([
            ("name", text(name)),
            ("temperature", num(milli / 1000.0)),
            (
                "type",
                text(read_trimmed(&format!("{base}/type")).unwrap_or_default()),
            ),
        ]));
    }
    Ok(object([("zones", Value::Array(zones))]))
}

fn run_tool(op: OsOp, program: &str, args: &[&str]) -> Result<String, OsError> {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err(err(
            op,
            OsErrorKind::Io,
            format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(unsupported(op, format!("{program} is not installed")))
        }
        Err(e) => Err(io_error(op, &e)),
    }
}

/// The first tool of `chain` that is installed runs; none installed is `unsupported`.
fn run_chain(op: OsOp, chain: &[(&str, Vec<String>)]) -> Result<String, OsError> {
    for (program, args) in chain {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        match run_tool(op, program, &borrowed) {
            Err(e) if e.kind == OsErrorKind::Unsupported => continue,
            other => return other,
        }
    }
    let names: Vec<&str> = chain.iter().map(|(p, _)| *p).collect();
    Err(unsupported(
        op,
        format!("no supported tool is installed ({})", names.join(", ")),
    ))
}

fn tool(program: &'static str, args: &[&str]) -> (&'static str, Vec<String>) {
    (program, args.iter().map(|a| (*a).to_string()).collect())
}

fn osascript(script: &str) -> (&'static str, Vec<String>) {
    tool("osascript", &["-e", script])
}

fn level_arg(args: &[Value], i: usize, op: OsOp) -> Result<f64, OsError> {
    match args.get(i) {
        Some(Value::Number(n)) if n.is_finite() && (0.0..=100.0).contains(n) => Ok(*n),
        _ => Err(err(
            op,
            OsErrorKind::Invalid,
            format!("argument {} must be a number from 0 to 100", i + 1),
        )),
    }
}

fn first_percent(raw: &str) -> Option<f64> {
    let at = raw.find('%')?;
    let digits: String = raw[..at]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

fn notify(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let title = arg_str(args, 0, op)?;
    let body = arg_str(args, 1, op)?;
    let urgency = match arg_opt(args, 2) {
        None => "normal",
        Some(_) => match arg_str(args, 2, op)? {
            u @ ("low" | "normal" | "critical") => u,
            _ => {
                return Err(err(
                    op,
                    OsErrorKind::Invalid,
                    "the urgency must be low, normal or critical",
                ))
            }
        },
    };
    run_chain(
        op,
        &[
            tool("notify-send", &["--urgency", urgency, "--", title, body]),
            tool(
                "osascript",
                &[
                    "-e",
                    "on run argv",
                    "-e",
                    "display notification (item 2 of argv) with title (item 1 of argv)",
                    "-e",
                    "end run",
                    "--",
                    title,
                    body,
                ],
            ),
        ],
    )?;
    Ok(object([]))
}

fn speak(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let words = arg_str(args, 0, op)?;
    run_chain(
        op,
        &[
            tool("espeak-ng", &["--", words]),
            tool("espeak", &["--", words]),
            tool("say", &["--", words]),
        ],
    )?;
    Ok(object([]))
}

fn brightness_get(op: OsOp) -> Result<Value, OsError> {
    const BACKLIGHT: &str = "/sys/class/backlight";
    for name in dir_names(op, BACKLIGHT)? {
        let base = format!("{BACKLIGHT}/{name}");
        if let (Some(current), Some(max)) = (
            read_f64(&format!("{base}/brightness")),
            read_f64(&format!("{base}/max_brightness")),
        ) {
            if max > 0.0 {
                return Ok(object([("level", num((current / max * 100.0).round()))]));
            }
        }
    }
    Err(unsupported(op, "no backlight device reports a brightness"))
}

fn brightness_set(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let level = level_arg(args, 0, op)?;
    run_chain(
        op,
        &[tool(
            "brightnessctl",
            &["set", &format!("{}%", level as u64)],
        )],
    )?;
    Ok(object([]))
}

fn volume_get(op: OsOp) -> Result<Value, OsError> {
    let raw = run_chain(
        op,
        &[
            tool("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]),
            tool("amixer", &["get", "Master"]),
            osascript("output volume of (get volume settings)"),
        ],
    )?;
    let level = first_percent(&raw)
        .or_else(|| raw.trim().parse::<f64>().ok())
        .ok_or_else(|| unsupported(op, "the host reports no output volume"))?;
    Ok(object([("level", num(level))]))
}

fn volume_set(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let percent = level_arg(args, 0, op)? as u64;
    run_chain(
        op,
        &[
            tool(
                "pactl",
                &["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")],
            ),
            tool("amixer", &["set", "Master", &format!("{percent}%")]),
            osascript(&format!("set volume output volume {percent}")),
        ],
    )?;
    Ok(object([]))
}

fn volume_mute(op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    let muted = arg_bool(args, 0, op)?;
    run_chain(
        op,
        &[
            tool(
                "pactl",
                &[
                    "set-sink-mute",
                    "@DEFAULT_SINK@",
                    if muted { "1" } else { "0" },
                ],
            ),
            tool(
                "amixer",
                &["set", "Master", if muted { "mute" } else { "unmute" }],
            ),
            osascript(if muted {
                "set volume output muted true"
            } else {
                "set volume output muted false"
            }),
        ],
    )?;
    Ok(object([]))
}

fn suspend(op: OsOp) -> Result<Value, OsError> {
    run_chain(
        op,
        &[
            tool("systemctl", &["suspend"]),
            tool("pm-suspend", &[]),
            tool("pmset", &["sleepnow"]),
        ],
    )?;
    Ok(object([]))
}

fn hibernate(op: OsOp) -> Result<Value, OsError> {
    run_chain(
        op,
        &[tool("systemctl", &["hibernate"]), tool("pm-hibernate", &[])],
    )?;
    Ok(object([]))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::os_host::{OsGrants, OsHandles};
    use std::path::Path;

    fn s(v: &str) -> Value {
        Value::String(v.to_string())
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
            Value::Array(vec![s("rw"), s("noatime")])
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
        assert!(conforms(&Value::Object(stats), &Shape::Map(COUNTERS)));

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
}
