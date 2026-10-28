//! The `os/` effect family at the kernel: the closed set of operations a host can be asked to perform
//! ([`OsOp`]), the error a host answers with ([`OsError`]) and how an `os/<op>` call site is read.
//! The contract is `docs/Almadar_OS_Operators.md`; the pure `os/path-*` operators are not here (they are
//! evaluator operators, `evaluator/operators/os_path.rs`).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::sexpr::{SExpression, SObject};
use serde::{Deserialize, Serialize};

/// Whether an operation answers once or streams events to its call site's route before its final answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsShape {
    Single,
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    Fs,
    Process,
    Net,
    Sys,
    Kv,
    Dev,
}

/// The one family that answers `op`.
pub fn family_of(op: OsOp) -> OsFamily {
    use OsOp::*;
    match op {
        ReadFile | WriteFile | AppendFile | Open | Close | Read | Write | Seek | Truncate
        | Fsync | Copy | Rename | Remove | Link | Symlink | Readlink | WatchFiles | Mkdir
        | Rmdir | ListDir | Glob | Mktemp | Realpath | Cwd | Chdir | Stat | Lstat | Access
        | Exists | Chmod | Chown | Utimes => OsFamily::Fs,
        Spawn | Wait | Kill | Pid | Ppid | CurrentExe | WatchProcess | WatchSignal | Getenv
        | Setenv | Unsetenv | Environ | WatchEnv | StdinRead | StdoutWrite | StderrWrite
        | Isatty | TtySize => OsFamily::Process,
        TcpConnect | TcpListen | Send | Recv | UdpBind | Sendto | Recvfrom | DnsResolve
        | Shutdown | WatchPort => OsFamily::Net,
        ClockMonotonic | Uid | Gid | Username | GroupOf | WatchCron | CpuUsage | MemoryInfo
        | Uptime | LoadAvg | Hostname | Uname | DiskUsage | Mounts | DiskIo | NetworkInterfaces
        | NetworkStats | NetworkConnected | DefaultGateway | BatteryStatus | AcPower
        | ThermalZones | Notify | Speak | BrightnessGet | BrightnessSet | VolumeGet | VolumeSet
        | VolumeMute | Suspend | Hibernate => OsFamily::Sys,
        KvGet | KvPut | KvDelete | KvScan | KvBatch => OsFamily::Kv,
        MmioRead | MmioWrite | IrqWait | IrqAck => OsFamily::Dev,
    }
}

macro_rules! os_ops {
    ($( $variant:ident => ($name:literal, $min:literal, $max:literal, $shape:ident) ),+ $(,)?) => {
        /// One variant per `os/` effect operation; the host's answerer matches it exhaustively.
        #[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum OsOp {
            $( #[serde(rename = $name)] $variant, )+
        }

        impl OsOp {
            /// Every operation, in contract order.
            pub const ALL: &'static [OsOp] = &[ $( OsOp::$variant, )+ ];

            /// The surface name, `os/<op>`.
            pub fn name(self) -> &'static str {
                match self { $( OsOp::$variant => $name, )+ }
            }

            /// The operation a surface name denotes.
            pub fn from_name(name: &str) -> Option<OsOp> {
                match name { $( $name => Some(OsOp::$variant), )+ _ => None }
            }

            /// The fewest positional arguments the operation takes.
            pub fn min_args(self) -> usize {
                match self { $( OsOp::$variant => $min, )+ }
            }

            /// The most positional arguments the operation takes.
            pub fn max_args(self) -> usize {
                match self { $( OsOp::$variant => $max, )+ }
            }

            /// Whether the operation answers once or streams.
            pub fn shape(self) -> OsShape {
                match self { $( OsOp::$variant => OsShape::$shape, )+ }
            }
        }
    };
}

os_ops! {
    ReadFile => ("os/read-file", 1, 2, Single),
    WriteFile => ("os/write-file", 2, 3, Single),
    AppendFile => ("os/append-file", 2, 2, Single),
    Open => ("os/open", 2, 3, Single),
    Close => ("os/close", 1, 1, Single),
    Read => ("os/read", 2, 2, Single),
    Write => ("os/write", 2, 2, Single),
    Seek => ("os/seek", 3, 3, Single),
    Truncate => ("os/truncate", 2, 2, Single),
    Fsync => ("os/fsync", 1, 1, Single),
    Copy => ("os/copy", 2, 2, Single),
    Rename => ("os/rename", 2, 2, Single),
    Remove => ("os/remove", 1, 1, Single),
    Link => ("os/link", 2, 2, Single),
    Symlink => ("os/symlink", 2, 2, Single),
    Readlink => ("os/readlink", 1, 1, Single),
    WatchFiles => ("os/watch-files", 1, 2, Stream),
    Mkdir => ("os/mkdir", 1, 3, Single),
    Rmdir => ("os/rmdir", 1, 2, Single),
    ListDir => ("os/list-dir", 1, 1, Single),
    Glob => ("os/glob", 1, 1, Single),
    Mktemp => ("os/mktemp", 1, 2, Single),
    Realpath => ("os/realpath", 1, 1, Single),
    Cwd => ("os/cwd", 0, 0, Single),
    Chdir => ("os/chdir", 1, 1, Single),
    Stat => ("os/stat", 1, 1, Single),
    Lstat => ("os/lstat", 1, 1, Single),
    Access => ("os/access", 1, 2, Single),
    Exists => ("os/exists", 1, 1, Single),
    Chmod => ("os/chmod", 2, 2, Single),
    Chown => ("os/chown", 3, 3, Single),
    Utimes => ("os/utimes", 3, 3, Single),
    Spawn => ("os/spawn", 2, 3, Single),
    Wait => ("os/wait", 1, 1, Single),
    Kill => ("os/kill", 2, 2, Single),
    Pid => ("os/pid", 0, 0, Single),
    Ppid => ("os/ppid", 0, 0, Single),
    CurrentExe => ("os/current-exe", 0, 0, Single),
    WatchProcess => ("os/watch-process", 1, 2, Stream),
    WatchSignal => ("os/watch-signal", 1, 1, Stream),
    Getenv => ("os/getenv", 1, 1, Single),
    Setenv => ("os/setenv", 2, 2, Single),
    Unsetenv => ("os/unsetenv", 1, 1, Single),
    Environ => ("os/environ", 0, 0, Single),
    WatchEnv => ("os/watch-env", 1, 1, Stream),
    StdinRead => ("os/stdin-read", 0, 1, Stream),
    StdoutWrite => ("os/stdout-write", 1, 1, Single),
    StderrWrite => ("os/stderr-write", 1, 1, Single),
    Isatty => ("os/isatty", 1, 1, Single),
    TtySize => ("os/tty-size", 1, 1, Single),
    TcpConnect => ("os/tcp-connect", 2, 2, Single),
    TcpListen => ("os/tcp-listen", 2, 2, Stream),
    Send => ("os/send", 2, 2, Single),
    Recv => ("os/recv", 2, 2, Single),
    UdpBind => ("os/udp-bind", 2, 2, Single),
    Sendto => ("os/sendto", 4, 4, Single),
    Recvfrom => ("os/recvfrom", 2, 2, Single),
    DnsResolve => ("os/dns-resolve", 1, 2, Single),
    Shutdown => ("os/shutdown", 1, 1, Single),
    WatchPort => ("os/watch-port", 2, 2, Stream),
    ClockMonotonic => ("os/clock-monotonic", 0, 0, Single),
    Uid => ("os/uid", 0, 0, Single),
    Gid => ("os/gid", 0, 0, Single),
    Username => ("os/username", 0, 0, Single),
    GroupOf => ("os/group-of", 1, 1, Single),
    WatchCron => ("os/watch-cron", 1, 1, Stream),
    CpuUsage => ("os/cpu-usage", 0, 0, Single),
    MemoryInfo => ("os/memory-info", 0, 0, Single),
    Uptime => ("os/uptime", 0, 0, Single),
    LoadAvg => ("os/load-avg", 0, 0, Single),
    Hostname => ("os/hostname", 0, 0, Single),
    Uname => ("os/uname", 0, 0, Single),
    DiskUsage => ("os/disk-usage", 1, 1, Single),
    Mounts => ("os/mounts", 0, 0, Single),
    DiskIo => ("os/disk-io", 0, 0, Single),
    NetworkInterfaces => ("os/network-interfaces", 0, 0, Single),
    NetworkStats => ("os/network-stats", 0, 0, Single),
    NetworkConnected => ("os/network-connected", 0, 0, Single),
    DefaultGateway => ("os/default-gateway", 0, 0, Single),
    BatteryStatus => ("os/battery-status", 0, 0, Single),
    AcPower => ("os/ac-power", 0, 0, Single),
    ThermalZones => ("os/thermal-zones", 0, 0, Single),
    Notify => ("os/notify", 2, 3, Single),
    Speak => ("os/speak", 1, 1, Single),
    BrightnessGet => ("os/brightness-get", 0, 0, Single),
    BrightnessSet => ("os/brightness-set", 1, 1, Single),
    VolumeGet => ("os/volume-get", 0, 0, Single),
    VolumeSet => ("os/volume-set", 1, 1, Single),
    VolumeMute => ("os/volume-mute", 0, 1, Single),
    Suspend => ("os/suspend", 0, 0, Single),
    Hibernate => ("os/hibernate", 0, 0, Single),
    MmioRead => ("os/mmio-read", 3, 3, Single),
    MmioWrite => ("os/mmio-write", 4, 4, Single),
    IrqWait => ("os/irq-wait", 1, 1, Single),
    IrqAck => ("os/irq-ack", 1, 1, Single),
    KvGet => ("kv/get", 2, 2, Single),
    KvPut => ("kv/put", 3, 3, Single),
    KvDelete => ("kv/delete", 2, 2, Single),
    KvScan => ("kv/scan", 2, 3, Single),
    KvBatch => ("kv/batch", 2, 2, Single),
}

impl OsOp {
    /// Whether the operation is one of the `kv/` row-store asks (the host picks the medium; runs on every host).
    pub fn is_kv(self) -> bool {
        matches!(
            self,
            OsOp::KvGet | OsOp::KvPut | OsOp::KvDelete | OsOp::KvScan | OsOp::KvBatch
        )
    }

    /// Whether the operation streams events to its call site's route.
    pub fn is_stream(self) -> bool {
        self.shape() == OsShape::Stream
    }

    /// Whether a keyed call of the operation that declares a completion route may end after the dispatch that
    /// asked it, its outcome arriving later as an event of the route (EXEC-QUEUE-10).
    pub fn defers_completion(self) -> bool {
        (family_of(self) == OsFamily::Fs && self.shape() == OsShape::Single && self != OsOp::Cwd)
            || matches!(
                self,
                OsOp::Wait
                    | OsOp::Send
                    | OsOp::Recv
                    | OsOp::Recvfrom
                    | OsOp::StdinRead
                    | OsOp::IrqWait
            )
    }
}

/// Why a host refused or failed an `os/` operation. `denied` is a refusal by grant; `unsupported` means the
/// host or platform cannot do it.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsErrorKind {
    Denied,
    NotFound,
    Exists,
    Invalid,
    Io,
    Timeout,
    Unsupported,
    Closed,
}

impl OsErrorKind {
    /// The spelling a failure payload carries in its `kind` field.
    pub fn as_str(self) -> &'static str {
        match self {
            OsErrorKind::Denied => "denied",
            OsErrorKind::NotFound => "not-found",
            OsErrorKind::Exists => "exists",
            OsErrorKind::Invalid => "invalid",
            OsErrorKind::Io => "io",
            OsErrorKind::Timeout => "timeout",
            OsErrorKind::Unsupported => "unsupported",
            OsErrorKind::Closed => "closed",
        }
    }

    /// The inverse of [`Self::as_str`]; `None` for a spelling no host sends.
    pub fn from_name(name: &str) -> Option<Self> {
        [
            OsErrorKind::Denied,
            OsErrorKind::NotFound,
            OsErrorKind::Exists,
            OsErrorKind::Invalid,
            OsErrorKind::Io,
            OsErrorKind::Timeout,
            OsErrorKind::Unsupported,
            OsErrorKind::Closed,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == name)
    }
}

/// A host's failure answer to an `os/` operation.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsError {
    pub kind: OsErrorKind,
    pub message: String,
}

impl OsError {
    pub fn new(kind: OsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// An `os/` call site's arguments split from its trailing `{ key?, emit? }` envelope. The last argument is the
/// envelope when it is a non-empty object whose every key is `key` or `emit` and the operation still has its
/// required positional arguments without it.
pub fn split_envelope(op: OsOp, args: &[SExpression]) -> (&[SExpression], Option<&SObject>) {
    if let Some((SExpression::Object(options), positional)) = args.split_last() {
        let is_envelope = !options.is_empty()
            && options.keys().all(|k| k == "key" || k == "emit")
            && positional.len() >= op.min_args();
        if is_envelope {
            return (positional, Some(options));
        }
    }
    (args, None)
}

/// Whether `count` positional arguments are a legal call of `op`.
pub fn accepts_arity(op: OsOp, count: usize) -> bool {
    (op.min_args()..=op.max_args()).contains(&count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sx(v: serde_json::Value) -> Vec<SExpression> {
        serde_json::from_value(v).expect("s-expression array")
    }

    #[test]
    fn every_name_round_trips_through_the_one_table() {
        for op in OsOp::ALL {
            assert_eq!(OsOp::from_name(op.name()), Some(*op));
            assert_eq!(op.name().starts_with("kv/"), op.is_kv());
            assert!(op.name().starts_with("os/") || op.is_kv());
            assert!(op.min_args() <= op.max_args());
        }
        assert_eq!(OsOp::ALL.len(), 100);
        assert_eq!(OsOp::from_name("http"), None);
    }

    #[test]
    fn os_ops_are_exactly_the_table_operators_hosted_by_os_or_kv() {
        use crate::language::registry::operators;
        let mut declared: Vec<&str> = operators()
            .iter()
            .filter(|(_, operator)| matches!(operator.host.as_deref(), Some("os" | "kv")))
            .map(|(name, _)| name.as_str())
            .collect();
        declared.sort_unstable();
        let mut coded: Vec<&str> = OsOp::ALL.iter().map(|op| op.name()).collect();
        coded.sort_unstable();
        assert_eq!(coded, declared);
        let mut unhosted_os: Vec<(&str, bool)> = operators()
            .iter()
            .filter(|(name, operator)| name.starts_with("os/") && operator.host.is_none())
            .map(|(name, operator)| (name.as_str(), operator.has_side_effects))
            .collect();
        unhosted_os.sort_unstable();
        assert!(unhosted_os
            .iter()
            .all(|(name, _)| OsOp::from_name(name).is_none()));
        let effectful: Vec<&str> = unhosted_os
            .iter()
            .filter(|(_, effect)| *effect)
            .map(|(name, _)| *name)
            .collect();
        assert!(effectful.is_empty(), "the pure os/path-* operators are evaluated in eval; every effectful os operator is hosted: {effectful:?}");
    }

    #[test]
    fn names_are_unique_and_unknown_names_are_not_ops() {
        let mut names: Vec<&str> = OsOp::ALL.iter().map(|op| op.name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
        assert_eq!(OsOp::from_name("os/nope"), None);
        assert_eq!(OsOp::from_name("os/path-join"), None);
        assert_eq!(OsOp::from_name("read-file"), None);
        assert_eq!(OsOp::from_name("random-bytes"), None);
    }

    #[test]
    fn finite_filesystem_answers_and_existing_blocking_operations_defer_their_completion() {
        for op in OsOp::ALL
            .iter()
            .copied()
            .filter(|op| family_of(*op) == OsFamily::Fs)
        {
            assert_eq!(
                op.defers_completion(),
                op.shape() == OsShape::Single && op != OsOp::Cwd,
                "{}",
                op.name()
            );
        }
        let other: Vec<_> = OsOp::ALL
            .iter()
            .copied()
            .filter(|op| family_of(*op) != OsFamily::Fs && op.defers_completion())
            .collect();
        assert_eq!(
            other,
            vec![
                OsOp::Wait,
                OsOp::StdinRead,
                OsOp::Send,
                OsOp::Recv,
                OsOp::Recvfrom,
                OsOp::IrqWait
            ]
        );
        assert!(!OsOp::Wait.is_stream() && !OsOp::Recv.is_stream());
        assert!(!OsOp::Spawn.defers_completion() && !OsOp::Sendto.defers_completion());
    }

    #[test]
    fn the_device_family_is_exactly_the_four_device_ops() {
        let dev: Vec<&str> = OsOp::ALL
            .iter()
            .filter(|op| family_of(**op) == OsFamily::Dev)
            .map(|op| op.name())
            .collect();
        assert_eq!(
            dev,
            vec!["os/mmio-read", "os/mmio-write", "os/irq-wait", "os/irq-ack"]
        );
        assert_eq!(
            (OsOp::MmioRead.min_args(), OsOp::MmioRead.max_args()),
            (3, 3)
        );
        assert_eq!(
            (OsOp::MmioWrite.min_args(), OsOp::MmioWrite.max_args()),
            (4, 4)
        );
        assert_eq!((OsOp::IrqWait.min_args(), OsOp::IrqAck.max_args()), (1, 1));
        assert!(dev
            .iter()
            .all(|name| !OsOp::from_name(name).is_some_and(OsOp::is_stream)));
        assert!(OsOp::IrqWait.defers_completion());
        assert!(
            !OsOp::MmioRead.defers_completion() && !OsOp::IrqAck.defers_completion(),
            "control: register access answers at once"
        );
        assert_eq!(
            family_of(OsOp::StdoutWrite),
            OsFamily::Process,
            "control: stdio stays in process"
        );
    }

    #[test]
    fn an_error_kind_name_round_trips() {
        for kind in [
            OsErrorKind::Denied,
            OsErrorKind::NotFound,
            OsErrorKind::Exists,
            OsErrorKind::Invalid,
            OsErrorKind::Io,
            OsErrorKind::Timeout,
            OsErrorKind::Unsupported,
            OsErrorKind::Closed,
        ] {
            assert_eq!(OsErrorKind::from_name(kind.as_str()), Some(kind));
        }
        assert_eq!(OsErrorKind::from_name("unheard-of"), None);
    }

    #[test]
    fn exactly_the_streaming_ops_stream() {
        let streams: Vec<&str> = OsOp::ALL
            .iter()
            .filter(|op| op.is_stream())
            .map(|op| op.name())
            .collect();
        assert_eq!(
            streams,
            vec![
                "os/watch-files",
                "os/watch-process",
                "os/watch-signal",
                "os/watch-env",
                "os/stdin-read",
                "os/tcp-listen",
                "os/watch-port",
                "os/watch-cron",
            ]
        );
        assert!(!OsOp::ReadFile.is_stream());
        assert!(!OsOp::Spawn.is_stream());
    }

    #[test]
    fn an_op_serializes_as_its_surface_name() {
        assert_eq!(
            serde_json::to_value(OsOp::TcpConnect).expect("ser"),
            json!("os/tcp-connect")
        );
        assert_eq!(
            serde_json::from_value::<OsOp>(json!("os/stdin-read")).expect("de"),
            OsOp::StdinRead
        );
        assert!(serde_json::from_value::<OsOp>(json!("os/path-join")).is_err());
    }

    #[test]
    fn an_error_kind_serializes_kebab_and_matches_its_payload_spelling() {
        for kind in [
            OsErrorKind::Denied,
            OsErrorKind::NotFound,
            OsErrorKind::Exists,
            OsErrorKind::Invalid,
            OsErrorKind::Io,
            OsErrorKind::Timeout,
            OsErrorKind::Unsupported,
            OsErrorKind::Closed,
        ] {
            assert_eq!(
                serde_json::to_value(kind).expect("ser"),
                json!(kind.as_str())
            );
        }
    }

    #[test]
    fn a_trailing_key_emit_object_is_the_envelope() {
        let args = sx(json!(["/tmp/a", {"emit": {"success": "OK"}}]));
        let (positional, options) = split_envelope(OsOp::ReadFile, &args);
        assert_eq!(positional.len(), 1);
        assert!(options.is_some());
    }

    #[test]
    fn an_options_object_of_the_op_itself_is_not_the_envelope() {
        let args = sx(json!(["git", ["status"], {"cwd": "/repo", "stdin": "null"}]));
        let (positional, options) = split_envelope(OsOp::Spawn, &args);
        assert_eq!(positional.len(), 3);
        assert!(options.is_none());
        let args = sx(
            json!(["git", ["status"], {"cwd": "/repo"}, {"key": "k", "emit": {"success": "OK"}}]),
        );
        let (positional, options) = split_envelope(OsOp::Spawn, &args);
        assert_eq!(positional.len(), 3);
        assert!(options.is_some());
    }

    #[test]
    fn an_envelope_does_not_stand_in_for_a_required_argument() {
        let args = sx(json!([{"emit": {"success": "OK"}}]));
        let (positional, options) = split_envelope(OsOp::ReadFile, &args);
        assert_eq!(positional.len(), 1);
        assert!(options.is_none());
        let empty = sx(json!([{}]));
        let (positional, options) = split_envelope(OsOp::Cwd, &empty);
        assert_eq!(positional.len(), 1);
        assert!(options.is_none());
    }

    #[test]
    fn arity_is_the_declared_range() {
        assert!(accepts_arity(OsOp::ReadFile, 1));
        assert!(accepts_arity(OsOp::ReadFile, 2));
        assert!(!accepts_arity(OsOp::ReadFile, 0));
        assert!(!accepts_arity(OsOp::ReadFile, 3));
        assert!(accepts_arity(OsOp::Cwd, 0));
        assert!(!accepts_arity(OsOp::Cwd, 1));
    }

    #[test]
    fn current_exe_is_a_single_op_taking_no_argument() {
        assert_eq!(OsOp::from_name("os/current-exe"), Some(OsOp::CurrentExe));
        assert_eq!(OsOp::CurrentExe.name(), "os/current-exe");
        assert!(!OsOp::CurrentExe.is_stream() && !OsOp::CurrentExe.defers_completion());
        assert!(accepts_arity(OsOp::CurrentExe, 0));
        assert!(
            !accepts_arity(OsOp::CurrentExe, 1),
            "control: it takes no path"
        );
    }
}
