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
        TcpConnect | TcpListen | TlsUpgrade | Send | Recv | UdpBind | Sendto | Recvfrom
        | DnsResolve | Shutdown | WatchPort => OsFamily::Net,
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
    ($( $variant:ident => ($name:literal, $shape:ident) ),+ $(,)?) => {
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

            /// Whether the operation answers once or streams.
            pub fn shape(self) -> OsShape {
                match self { $( OsOp::$variant => OsShape::$shape, )+ }
            }
        }
    };
}

os_ops! {
    ReadFile => ("os/read-file", Single),
    WriteFile => ("os/write-file", Single),
    AppendFile => ("os/append-file", Single),
    Open => ("os/open", Single),
    Close => ("os/close", Single),
    Read => ("os/read", Single),
    Write => ("os/write", Single),
    Seek => ("os/seek", Single),
    Truncate => ("os/truncate", Single),
    Fsync => ("os/fsync", Single),
    Copy => ("os/copy", Single),
    Rename => ("os/rename", Single),
    Remove => ("os/remove", Single),
    Link => ("os/link", Single),
    Symlink => ("os/symlink", Single),
    Readlink => ("os/readlink", Single),
    WatchFiles => ("os/watch-files", Stream),
    Mkdir => ("os/mkdir", Single),
    Rmdir => ("os/rmdir", Single),
    ListDir => ("os/list-dir", Single),
    Glob => ("os/glob", Single),
    Mktemp => ("os/mktemp", Single),
    Realpath => ("os/realpath", Single),
    Cwd => ("os/cwd", Single),
    Chdir => ("os/chdir", Single),
    Stat => ("os/stat", Single),
    Lstat => ("os/lstat", Single),
    Access => ("os/access", Single),
    Exists => ("os/exists", Single),
    Chmod => ("os/chmod", Single),
    Chown => ("os/chown", Single),
    Utimes => ("os/utimes", Single),
    Spawn => ("os/spawn", Single),
    Wait => ("os/wait", Single),
    Kill => ("os/kill", Single),
    Pid => ("os/pid", Single),
    Ppid => ("os/ppid", Single),
    CurrentExe => ("os/current-exe", Single),
    WatchProcess => ("os/watch-process", Stream),
    WatchSignal => ("os/watch-signal", Stream),
    Getenv => ("os/getenv", Single),
    Setenv => ("os/setenv", Single),
    Unsetenv => ("os/unsetenv", Single),
    Environ => ("os/environ", Single),
    WatchEnv => ("os/watch-env", Stream),
    StdinRead => ("os/stdin-read", Stream),
    StdoutWrite => ("os/stdout-write", Single),
    StderrWrite => ("os/stderr-write", Single),
    Isatty => ("os/isatty", Single),
    TtySize => ("os/tty-size", Single),
    TcpConnect => ("os/tcp-connect", Single),
    TcpListen => ("os/tcp-listen", Stream),
    TlsUpgrade => ("os/tls-upgrade", Single),
    Send => ("os/send", Single),
    Recv => ("os/recv", Single),
    UdpBind => ("os/udp-bind", Single),
    Sendto => ("os/sendto", Single),
    Recvfrom => ("os/recvfrom", Single),
    DnsResolve => ("os/dns-resolve", Single),
    Shutdown => ("os/shutdown", Single),
    WatchPort => ("os/watch-port", Stream),
    ClockMonotonic => ("os/clock-monotonic", Single),
    Uid => ("os/uid", Single),
    Gid => ("os/gid", Single),
    Username => ("os/username", Single),
    GroupOf => ("os/group-of", Single),
    WatchCron => ("os/watch-cron", Stream),
    CpuUsage => ("os/cpu-usage", Single),
    MemoryInfo => ("os/memory-info", Single),
    Uptime => ("os/uptime", Single),
    LoadAvg => ("os/load-avg", Single),
    Hostname => ("os/hostname", Single),
    Uname => ("os/uname", Single),
    DiskUsage => ("os/disk-usage", Single),
    Mounts => ("os/mounts", Single),
    DiskIo => ("os/disk-io", Single),
    NetworkInterfaces => ("os/network-interfaces", Single),
    NetworkStats => ("os/network-stats", Single),
    NetworkConnected => ("os/network-connected", Single),
    DefaultGateway => ("os/default-gateway", Single),
    BatteryStatus => ("os/battery-status", Single),
    AcPower => ("os/ac-power", Single),
    ThermalZones => ("os/thermal-zones", Single),
    Notify => ("os/notify", Single),
    Speak => ("os/speak", Single),
    BrightnessGet => ("os/brightness-get", Single),
    BrightnessSet => ("os/brightness-set", Single),
    VolumeGet => ("os/volume-get", Single),
    VolumeSet => ("os/volume-set", Single),
    VolumeMute => ("os/volume-mute", Single),
    Suspend => ("os/suspend", Single),
    Hibernate => ("os/hibernate", Single),
    MmioRead => ("os/mmio-read", Single),
    MmioWrite => ("os/mmio-write", Single),
    IrqWait => ("os/irq-wait", Single),
    IrqAck => ("os/irq-ack", Single),
    KvGet => ("kv/get", Single),
    KvPut => ("kv/put", Single),
    KvDelete => ("kv/delete", Single),
    KvScan => ("kv/scan", Single),
    KvBatch => ("kv/batch", Single),
}

impl OsOp {
    /// The fewest positional arguments the operation takes: the operator table's `minArity`.
    pub fn min_args(self) -> usize {
        self.table_entry().min_arity as usize
    }

    /// The most positional arguments the operation takes: the operator table's `maxArity` less its trailing
    /// `options` parameter, which the envelope carries.
    pub fn max_args(self) -> usize {
        let max = self.table_entry().max_arity.unwrap_or_else(|| panic!("the operator table bounds the arity of `{}`", self.name()));
        max as usize - 1
    }

    fn table_entry(self) -> &'static crate::language::registry::EmbeddedOperator {
        crate::language::registry::get_operator(self.name())
            .unwrap_or_else(|| panic!("the operator table declares no `{}`", self.name()))
    }

    /// Whether the operation is one of the `kv/` row-store asks (the host picks the medium; runs on every host).
    pub fn is_kv(self) -> bool {
        matches!(
            self,
            OsOp::KvGet | OsOp::KvPut | OsOp::KvDelete | OsOp::KvScan | OsOp::KvBatch
        )
    }

    /// Whether the kernel owns the operation: it answers it itself and a host implements no part of it.
    pub fn is_kernel_owned(self) -> bool {
        self == OsOp::TlsUpgrade
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
    Tls,
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
            OsErrorKind::Tls => "tls",
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
            OsErrorKind::Tls,
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
        assert_eq!(OsOp::ALL.len(), 101);
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
    fn every_os_operator_ends_in_the_options_parameter_the_envelope_carries() {
        use crate::language::registry::get_operator;
        for op in OsOp::ALL {
            let entry = get_operator(op.name()).expect("table entry");
            let last = entry.params.last().expect("a parameter");
            assert_eq!(last.name, "options", "{}", op.name());
            assert_eq!(op.max_args() + 1, entry.max_arity.expect("bounded") as usize, "{}", op.name());
            assert_eq!(op.min_args(), entry.min_arity as usize, "{}", op.name());
        }
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
            OsErrorKind::Tls,
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
            OsErrorKind::Tls,
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
        assert!(!accepts_arity(OsOp::ReadFile, 0));
        assert!(!accepts_arity(OsOp::ReadFile, 2), "the table declares a path and the options");
        assert!(accepts_arity(OsOp::Spawn, 3) && !accepts_arity(OsOp::Spawn, 2), "the table requires the config");
        assert!(accepts_arity(OsOp::VolumeMute, 1) && !accepts_arity(OsOp::VolumeMute, 0));
        assert!(accepts_arity(OsOp::WatchProcess, 2) && !accepts_arity(OsOp::WatchProcess, 1));
        assert!(accepts_arity(OsOp::Exists, 1) && !accepts_arity(OsOp::Exists, 2));
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
