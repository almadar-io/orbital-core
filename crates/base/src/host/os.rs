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
        | DnsResolve | Shutdown | WatchPort | Fetch => OsFamily::Net,
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
    Fetch => ("os/fetch", Single),
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

    /// [`Self::table_entry`] for a running kernel: the typed language-data error when no operator table is installed.
    pub fn try_table_entry(self) -> Result<&'static crate::language::registry::EmbeddedOperator, crate::language_data::LanguageDataError> {
        crate::language::registry::lookup_operator(self.name())?.ok_or_else(|| crate::language_data::LanguageDataError::Invalid {
            path: Default::default(),
            reason: format!("the operator table declares no `{}`", self.name()),
        })
    }

    /// Whether the operation is answered by the host on either side: `os/fetch` is the browser host's only ask, and
    /// `os/recv` and `os/close` read and release the handle of a streamed fetch there (a native host's sockets).
    pub fn runs_on_any_host(self) -> bool {
        matches!(self, OsOp::Fetch | OsOp::Recv | OsOp::Close)
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

/// Whose credential an `os/send` segment names: the app's, or the signed-in viewer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentScope {
    App,
    User,
}

impl SegmentScope {
    pub const NAMES: [&'static str; 2] = ["app", "user"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "app" => Some(Self::App),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// How a credential segment's value is encoded as the host substitutes it: the `encode` field of an `os/send` segment,
/// and the second argument of `credential/byte-length`. The one definition of the three encoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEncode {
    /// Standard padded base64 of the value's UTF-8 bytes.
    Base64,
    /// `encodeURIComponent`.
    Uri,
    /// `application/x-www-form-urlencoded`: a space is `+`.
    Form,
}

impl SegmentEncode {
    pub const NAMES: [&'static str; 3] = ["base64", "uri", "form"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "base64" => Some(Self::Base64),
            "uri" => Some(Self::Uri),
            "form" => Some(Self::Form),
            _ => None,
        }
    }

    pub fn apply(self, value: &str) -> String {
        match self {
            Self::Base64 => crate::codec::base64_encode(value.as_bytes()),
            Self::Uri => crate::codec::encode_uri_component(value),
            Self::Form => crate::codec::form_urlencode_value(value),
        }
    }
}

/// `args` of an `os/send` with the segments of its data operand filled in: a list of `{ bytes, credential, encode? }`
/// objects becomes the one standard-base64 text of their concatenation, each segment being its standard base64 `bytes`
/// followed by the value of the credential it names, passed through its `encode` when it has one (an empty name is
/// bytes alone). A segment's `scope` is `app` (default) or `user`; a user segment reads the credential of `principal`
/// (the signed-in viewer) and is denied with none. `source` reads a credential by name and principal; `None` holds none. The credential value exists only in the returned operand: the program, the journal and
/// the answer never hold it. Any other op, or a data operand that is already bytes, is returned as it is.
pub fn resolve_credential_segments(
    op: OsOp,
    args: &[crate::value::Value],
    principal: Option<&str>,
    source: Option<&dyn Fn(&str, Option<&str>) -> Result<Option<String>, String>>,
) -> Result<Vec<crate::value::Value>, OsError> {
    use crate::value::Value;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let (OsOp::Send, Some(Value::Array(segments))) = (op, args.get(1)) else {
        return Ok(args.to_vec());
    };
    let invalid = |at: usize, what: &str| OsError::new(OsErrorKind::Invalid, format!("{}: segment {} {what}", op.name(), at + 1));
    let denied = |what: &str| OsError::new(OsErrorKind::Denied, format!("{}: {what}", op.name()));
    let mut bytes = Vec::new();
    for (at, segment) in segments.iter().enumerate() {
        let Value::Object(fields) = segment else {
            return Err(invalid(at, "is not a { bytes, credential } object"));
        };
        let (Some(Value::String(text)), Some(Value::String(name))) = (fields.get("bytes"), fields.get("credential")) else {
            return Err(invalid(at, "needs a bytes text and a credential name"));
        };
        bytes.extend(STANDARD.decode(text.as_str()).map_err(|e| invalid(at, &format!("bytes are not standard base64: {e}")))?);
        let encode = match fields.get("encode") {
            None => None,
            Some(Value::String(requested)) => Some(
                SegmentEncode::from_name(requested.as_str())
                    .ok_or_else(|| invalid(at, &format!("encode must be one of {}", SegmentEncode::NAMES.join(", "))))?,
            ),
            Some(_) => return Err(invalid(at, &format!("encode must be one of {}", SegmentEncode::NAMES.join(", ")))),
        };
        let scope = match fields.get("scope") {
            None => SegmentScope::App,
            Some(Value::String(requested)) => SegmentScope::from_name(requested.as_str())
                .ok_or_else(|| invalid(at, &format!("scope must be one of {}", SegmentScope::NAMES.join(", "))))?,
            Some(_) => return Err(invalid(at, &format!("scope must be one of {}", SegmentScope::NAMES.join(", ")))),
        };
        if name.as_str().is_empty() {
            continue;
        }
        let owner = match scope {
            SegmentScope::App => None,
            SegmentScope::User => Some(principal.ok_or_else(|| denied(&format!("segment {} needs a signed-in viewer", at + 1)))?),
        };
        let held = source
            .ok_or_else(|| denied("no credential source is configured"))?(name.as_str(), owner)
            .map_err(|message| OsError::new(OsErrorKind::Io, format!("{}: credential {name}: {message}", op.name())))?;
        match held {
            Some(value) => match encode {
                Some(encode) => bytes.extend_from_slice(encode.apply(&value).as_bytes()),
                None => bytes.extend_from_slice(value.as_bytes()),
            },
            None => return Err(denied(&format!("credential {name} is not held"))),
        }
    }
    let mut resolved = args.to_vec();
    resolved[1] = Value::string(STANDARD.encode(&bytes));
    Ok(resolved)
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
        assert_eq!(OsOp::ALL.len(), 102);
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

    #[derive(serde::Deserialize)]
    struct EncodeVector {
        encode: String,
        input: String,
        expect: String,
    }

    fn encode_vectors() -> Vec<EncodeVector> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/operators/credential-encode-vectors.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("vectors")
    }

    fn held(name: &str, value: &str) -> impl Fn(&str, Option<&str>) -> Result<Option<String>, String> {
        let (name, value) = (name.to_string(), value.to_string());
        move |asked, _| Ok((asked == name).then(|| value.clone()))
    }

    fn send_resolved(segment: serde_json::Value, value: &str) -> Result<Vec<u8>, OsError> {
        use base64::Engine;
        let args: Vec<crate::value::Value> = serde_json::from_value(json!([5, [segment]])).expect("value args");
        let source = held("CRED", value);
        let resolved = resolve_credential_segments(OsOp::Send, &args, None, Some(&source))?;
        let crate::value::Value::String(text) = &resolved[1] else { panic!("bytes operand") };
        Ok(base64::engine::general_purpose::STANDARD.decode(text.as_str()).expect("standard base64"))
    }

    #[test]
    fn a_segment_encode_is_applied_to_the_credential_value_on_every_shared_vector() {
        let vectors = encode_vectors();
        assert!(!vectors.is_empty());
        for v in &vectors {
            let prefix = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"P=");
            let segment = json!({ "bytes": prefix, "credential": "CRED", "encode": v.encode });
            let expected = format!("P={}", v.expect);
            assert_eq!(send_resolved(segment, &v.input).expect("resolved"), expected.into_bytes(), "{} {:?}", v.encode, v.input);
        }
    }

    #[test]
    fn a_segment_without_encode_writes_the_credential_verbatim() {
        let segment = json!({ "bytes": "", "credential": "CRED" });
        assert_eq!(send_resolved(segment, "a b&c").expect("resolved"), b"a b&c".to_vec());
    }

    #[test]
    fn an_unknown_or_non_text_segment_encode_is_refused_and_a_known_one_beside_no_credential_is_inert() {
        for bad in [json!("rot13"), json!("BASE64"), json!(""), json!(7), json!(null)] {
            let segment = json!({ "bytes": "", "credential": "CRED", "encode": bad });
            let error = send_resolved(segment, "x").expect_err("refused");
            assert_eq!(error.kind, OsErrorKind::Invalid, "{bad}");
            assert!(error.message.contains("encode"), "{}", error.message);
        }
        let bytes_alone = json!({ "bytes": "QQ==", "credential": "", "encode": "form" });
        assert_eq!(send_resolved(bytes_alone, "x").expect("resolved"), b"A".to_vec());
    }

    fn send_as(segment: serde_json::Value, principal: Option<&str>) -> Result<Vec<u8>, OsError> {
        use base64::Engine;
        let args: Vec<crate::value::Value> = serde_json::from_value(json!([5, [segment]])).expect("value args");
        let source = |name: &str, owner: Option<&str>| Ok((name == "CRED").then(|| format!("value-of-{}", owner.unwrap_or("app"))));
        let resolved = resolve_credential_segments(OsOp::Send, &args, principal, Some(&source))?;
        let crate::value::Value::String(text) = &resolved[1] else { panic!("bytes operand") };
        Ok(base64::engine::general_purpose::STANDARD.decode(text.as_str()).expect("standard base64"))
    }

    #[test]
    fn a_user_segment_reads_the_viewers_credential_and_two_viewers_get_two_values() {
        let segment = json!({ "bytes": "", "credential": "CRED", "scope": "user" });
        assert_eq!(send_as(segment.clone(), Some("alice")).expect("alice"), b"value-of-alice".to_vec());
        assert_eq!(send_as(segment, Some("bob")).expect("bob"), b"value-of-bob".to_vec());
    }

    #[test]
    fn a_user_segment_without_a_viewer_is_denied_before_any_value_is_read() {
        let segment = json!({ "bytes": "", "credential": "CRED", "scope": "user" });
        let error = send_as(segment, None).expect_err("denied");
        assert_eq!(error.kind, OsErrorKind::Denied);
        assert_eq!(error.message, "os/send: segment 1 needs a signed-in viewer");
    }

    #[test]
    fn an_app_segment_ignores_the_viewer_and_the_default_scope_is_app() {
        for segment in [json!({ "bytes": "", "credential": "CRED" }), json!({ "bytes": "", "credential": "CRED", "scope": "app" })] {
            assert_eq!(send_as(segment.clone(), Some("alice")).expect("viewer"), b"value-of-app".to_vec());
            assert_eq!(send_as(segment, None).expect("no viewer"), b"value-of-app".to_vec());
        }
    }

    #[test]
    fn an_unknown_or_non_text_segment_scope_is_refused() {
        for bad in [json!("team"), json!("USER"), json!(""), json!(1), json!(null)] {
            let segment = json!({ "bytes": "", "credential": "CRED", "scope": bad });
            let error = send_as(segment, Some("alice")).expect_err("refused");
            assert_eq!(error.kind, OsErrorKind::Invalid, "{bad}");
            assert!(error.message.contains("scope"), "{}", error.message);
        }
    }
}
