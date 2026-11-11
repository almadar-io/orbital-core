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
    Mem,
    Kernel,
    Ffi,
    Gpu,
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
        ClockMonotonic | Uid | Gid | Username | GroupOf | WatchCron | CpuUsage | MemoryInfo | ProcessList
        | Uptime | LoadAvg | Hostname | Uname | DiskUsage | Mounts | DiskIo | NetworkInterfaces
        | NetworkStats | NetworkConnected | DefaultGateway | BatteryStatus | AcPower
        | ThermalZones | Notify | Speak | BrightnessGet | BrightnessSet | VolumeGet | VolumeSet
        | VolumeMute | Suspend | Hibernate => OsFamily::Sys,
        KvGet | KvPut | KvDelete | KvScan | KvBatch => OsFamily::Kv,
        MmioRead | MmioWrite | IrqWait | IrqAck => OsFamily::Dev,
        ArenaNew | ArenaDrop | Alloc | Free | Borrow | Release | MemWrite | Freeze | AtomicLoad | AtomicStore
        | AtomicAdd | AtomicCas => OsFamily::Mem,
        QueueFront | QueueBack => OsFamily::Kernel,
        FfiCall => OsFamily::Ffi,
        GpuDeviceOpen | GpuDeviceClose | GpuBufferCreate | GpuTextureCreate | GpuSamplerCreate | GpuQueryCreate
        | GpuShaderCreate | GpuPipelineCreate | GpuBindGroupCreate | GpuSubmit | GpuRelease => OsFamily::Gpu,
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
    ProcessList => ("os/process-list", Single),
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
    ArenaNew => ("mem/arena-new", Single),
    ArenaDrop => ("mem/arena-drop", Single),
    Alloc => ("mem/alloc", Single),
    Free => ("mem/free", Single),
    Borrow => ("mem/borrow", Single),
    Release => ("mem/release", Single),
    MemWrite => ("mem/write", Single),
    Freeze => ("mem/freeze", Single),
    AtomicLoad => ("mem/atomic-load", Single),
    AtomicStore => ("mem/atomic-store", Single),
    AtomicAdd => ("mem/atomic-add", Single),
    AtomicCas => ("mem/atomic-cas", Single),
    QueueFront => ("kernel/queue-front", Single),
    QueueBack => ("kernel/queue-back", Single),
    FfiCall => ("ffi/call", Single),
    GpuDeviceOpen => ("gpu/device-open", Single),
    GpuDeviceClose => ("gpu/device-close", Single),
    GpuBufferCreate => ("gpu/buffer-create", Single),
    GpuTextureCreate => ("gpu/texture-create", Single),
    GpuSamplerCreate => ("gpu/sampler-create", Single),
    GpuQueryCreate => ("gpu/query-create", Single),
    GpuShaderCreate => ("gpu/shader-create", Single),
    GpuPipelineCreate => ("gpu/pipeline-create", Single),
    GpuBindGroupCreate => ("gpu/bind-group-create", Single),
    GpuSubmit => ("gpu/submit", Single),
    GpuRelease => ("gpu/release", Single),
}

impl OsOp {
    /// The fewest positional arguments the operation takes: the operator table's `minArity`.
    pub fn min_args(self) -> Result<usize, crate::language_data::LanguageDataError> {
        Ok(self.try_table_entry()?.min_arity as usize)
    }

    /// The most positional arguments the operation takes: the operator table's `maxArity` less its trailing
    /// `options` parameter, which the envelope carries.
    pub fn max_args(self) -> Result<usize, crate::language_data::LanguageDataError> {
        let entry = self.try_table_entry()?;
        let max = entry.max_arity.ok_or_else(|| crate::language_data::LanguageDataError::Invalid {
            path: Default::default(),
            reason: format!("the operator table bounds the arity of `{}`", self.name()),
        })?;
        Ok(max as usize - 1)
    }

    /// The operator table's entry for the operation: the typed language-data error when no table is installed or it declares none.
    pub fn try_table_entry(self) -> Result<&'static crate::language::registry::EmbeddedOperator, crate::language_data::LanguageDataError> {
        crate::language::registry::lookup_operator(self.name())?.ok_or_else(|| crate::language_data::LanguageDataError::Invalid {
            path: Default::default(),
            reason: format!("the operator table declares no `{}`", self.name()),
        })
    }

    /// Whether the operation is answered by the host on either side: `os/fetch` is the browser host's only ask, and
    /// `os/recv` and `os/close` read and release the handle of a streamed fetch there (a native host's sockets); the
    /// `mem/` and `kernel/` families are the kernel's own, so every kernel runs them; the `gpu/` family is answered by a
    /// host of any kind.
    pub fn runs_on_any_host(self) -> bool {
        matches!(self, OsOp::Fetch | OsOp::Recv | OsOp::Close) || matches!(family_of(self), OsFamily::Mem | OsFamily::Kernel | OsFamily::Gpu)
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
        self == OsOp::TlsUpgrade || matches!(family_of(self), OsFamily::Mem | OsFamily::Kernel)
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
                    | OsOp::FfiCall
                    | OsOp::GpuDeviceOpen
                    | OsOp::GpuSubmit
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
    Borrowed,
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
            OsErrorKind::Borrowed => "borrowed",
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
            OsErrorKind::Borrowed,
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
pub fn split_envelope(op: OsOp, args: &[SExpression]) -> Result<(&[SExpression], Option<&SObject>), crate::language_data::LanguageDataError> {
    if let Some((SExpression::Object(options), positional)) = args.split_last() {
        let is_envelope = !options.is_empty()
            && options.keys().all(|k| k == "key" || k == "emit")
            && positional.len() >= op.min_args()?;
        if is_envelope {
            return Ok((positional, Some(options)));
        }
    }
    Ok((args, None))
}

/// Whether `count` positional arguments are a legal call of `op`.
pub fn accepts_arity(op: OsOp, count: usize) -> Result<bool, crate::language_data::LanguageDataError> {
    Ok((op.min_args()?..=op.max_args()?).contains(&count))
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

/// An `os/` answer with each field the operator table declares `bytes` made a bytes value: a host that answers over
/// JSON writes it as standard padded base64 text, and this is where the kernel checks it (EXEC-EVAL-10). A bytes
/// field that is neither bytes nor standard base64 text is an `Io` failure, never read as empty.
pub fn answer_with_declared_bytes(op: OsOp, answer: crate::value::Value) -> Result<crate::value::Value, OsError> {
    use crate::language::registry::{try_get_operator, OperatorTypeRef, OperatorTypeRefKind};
    use crate::value::Value;
    if !matches!(answer, Value::Object(_)) {
        return Ok(answer);
    }
    let Value::Object(mut fields) = answer else { unreachable!("matched an object above") };
    let produces = try_get_operator(op.name()).and_then(|operator| operator.effect.as_ref()).and_then(|effect| effect.produces.as_ref());
    let Some(OperatorTypeRef::Tagged(OperatorTypeRefKind::Object { fields: declared, .. })) = produces else {
        return Ok(Value::Object(fields));
    };
    for (name, kind) in declared {
        if kind.kind_name() != "bytes" {
            continue;
        }
        let Some(Value::String(text)) = fields.get(name.as_str()) else {
            continue;
        };
        let data = crate::codec::base64_decode(text)
            .map_err(|e| OsError::new(OsErrorKind::Io, format!("{}: the answer field {name} is not standard base64: {e}", op.name())))?;
        fields.insert(name.as_str().into(), Value::bytes(data));
    }
    Ok(Value::Object(fields))
}

/// The `denied` answers a host or kernel gave, each message once with how often, in first-seen order.
#[derive(Debug, Default)]
pub struct OsDenials(crate::sync::Mutex<Vec<(String, u64)>>);

impl OsDenials {
    pub fn note(&self, error: &OsError) {
        if error.kind != OsErrorKind::Denied {
            return;
        }
        let mut seen = self.0.lock();
        match seen.iter_mut().find(|(message, _)| *message == error.message) {
            Some((_, count)) => *count += 1,
            None => seen.push((error.message.clone(), 1)),
        }
    }

    pub fn entries(&self) -> Vec<(String, u64)> {
        self.0.lock().clone()
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
        let (Some(segment_bytes @ (Value::String(_) | Value::Bytes(_))), Some(Value::String(name))) = (fields.get("bytes"), fields.get("credential")) else {
            return Err(invalid(at, "needs a bytes text and a credential name"));
        };
        bytes.extend_from_slice(&segment_bytes.bytes_view().map_err(|e| invalid(at, &format!("bytes are not standard base64: {e}")))?);
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
    resolved[1] = Value::bytes(bytes);
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sx(v: serde_json::Value) -> Vec<SExpression> {
        serde_json::from_value(v).expect("s-expression array")
    }

    fn table_naming(names: impl Iterator<Item = &'static str>) -> crate::language_data::OperatorData {
        let operators: serde_json::Map<String, serde_json::Value> = names
            .map(|name| (name.to_string(), json!({ "category": "os", "minArity": 0, "maxArity": 1 })))
            .collect();
        crate::language_data::OperatorData {
            running: String::new(),
            std_version: "0.0.0".into(),
            root: Default::default(),
            operators: json!({ "categories": { "os": { "target": ["rust"] } }, "operators": operators }).to_string(),
        }
    }

    #[test]
    fn g_orbital_core_137_a_table_missing_an_os_operator_is_refused_with_a_typed_error() {
        let full = table_naming(OsOp::ALL.iter().map(|op| op.name()));
        assert_eq!(crate::language_data::certify_operator_data(&full, |_| true), Ok(()));
        let older = table_naming(OsOp::ALL.iter().filter(|op| **op != OsOp::GpuDeviceOpen).map(|op| op.name()));
        let refused = crate::language_data::certify_operator_data(&older, |_| true);
        assert!(refused.is_err(), "a table lacking `gpu/device-open` must be refused at load, not panic later in min_args");
    }

    #[test]
    fn every_name_round_trips_through_the_one_table() {
        for op in OsOp::ALL {
            assert_eq!(OsOp::from_name(op.name()), Some(*op));
            assert_eq!(op.name().starts_with("kv/"), op.is_kv());
            assert_eq!(op.name().starts_with("mem/"), family_of(*op) == OsFamily::Mem);
            assert_eq!(op.name().starts_with("kernel/"), family_of(*op) == OsFamily::Kernel);
            assert_eq!(op.name().starts_with("ffi/"), family_of(*op) == OsFamily::Ffi);
            assert_eq!(op.name().starts_with("gpu/"), family_of(*op) == OsFamily::Gpu);
            assert!(
                op.name().starts_with("os/")
                    || op.name().starts_with("mem/")
                    || op.name().starts_with("kernel/")
                    || op.name().starts_with("ffi/")
                    || op.name().starts_with("gpu/")
                    || op.is_kv()
            );
            assert!(op.min_args().expect("table") <= op.max_args().expect("table"));
        }
        assert_eq!(OsOp::ALL.len(), 129);
        assert_eq!(OsOp::from_name("http"), None);
    }

    #[test]
    fn os_ops_are_exactly_the_effect_operators_hosted_by_os_kv_or_kernel() {
        use crate::language::registry::operators;
        let mut declared: Vec<&str> = operators()
            .iter()
            .filter(|(_, operator)| {
                operator.has_side_effects
                    && matches!(operator.host.as_deref(), Some("os" | "kv" | "kernel"))
            })
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
            assert_eq!(op.max_args().expect("table") + 1, entry.max_arity.expect("bounded") as usize, "{}", op.name());
            assert_eq!(op.min_args().expect("table"), entry.min_arity as usize, "{}", op.name());
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
                OsOp::IrqWait,
                OsOp::FfiCall,
                OsOp::GpuDeviceOpen,
                OsOp::GpuSubmit
            ]
        );
        assert!(!OsOp::Wait.is_stream() && !OsOp::Recv.is_stream());
        assert!(!OsOp::Spawn.defers_completion() && !OsOp::Sendto.defers_completion());
    }

    #[test]
    fn the_mem_family_is_exactly_the_twelve_kernel_owned_mem_ops() {
        let mem: Vec<&str> = OsOp::ALL.iter().filter(|op| family_of(**op) == OsFamily::Mem).map(|op| op.name()).collect();
        assert_eq!(
            mem,
            vec![
                "mem/arena-new",
                "mem/arena-drop",
                "mem/alloc",
                "mem/free",
                "mem/borrow",
                "mem/release",
                "mem/write",
                "mem/freeze",
                "mem/atomic-load",
                "mem/atomic-store",
                "mem/atomic-add",
                "mem/atomic-cas"
            ]
        );
        assert!(OsOp::ALL.iter().filter(|op| op.is_kernel_owned()).all(|op| {
            op == &OsOp::TlsUpgrade || op.name().starts_with("mem/") || op.name().starts_with("kernel/")
        }));
        assert!(!OsOp::ReadFile.is_kernel_owned(), "control: a host-answered op");
        assert!(OsOp::Alloc.runs_on_any_host() && !OsOp::ReadFile.runs_on_any_host());
        for op in [OsOp::Freeze, OsOp::AtomicLoad, OsOp::AtomicStore, OsOp::AtomicAdd, OsOp::AtomicCas] {
            assert!(op.runs_on_any_host() && op.is_kernel_owned(), "{}", op.name());
        }
    }

    #[test]
    fn the_kernel_family_is_exactly_the_two_queue_ops() {
        let kernel: Vec<&str> = OsOp::ALL.iter().filter(|op| family_of(**op) == OsFamily::Kernel).map(|op| op.name()).collect();
        assert_eq!(kernel, vec!["kernel/queue-front", "kernel/queue-back"]);
        assert!(OsOp::QueueFront.is_kernel_owned() && OsOp::QueueBack.runs_on_any_host());
        assert!(!OsOp::Stat.runs_on_any_host(), "control: a host-answered op");
    }

    #[test]
    fn the_ffi_family_is_exactly_the_one_host_answered_call() {
        let ffi: Vec<&str> = OsOp::ALL.iter().filter(|op| family_of(**op) == OsFamily::Ffi).map(|op| op.name()).collect();
        assert_eq!(ffi, vec!["ffi/call"]);
        assert!(OsOp::FfiCall.defers_completion() && !OsOp::FfiCall.is_stream());
        assert!(!OsOp::FfiCall.is_kernel_owned() && !OsOp::FfiCall.runs_on_any_host(), "a host answers it");
        assert!(!OsOp::Pid.defers_completion(), "control: an answer that never defers");
    }

    #[test]
    fn the_gpu_family_is_exactly_the_eleven_host_answered_ops() {
        let gpu: Vec<&str> = OsOp::ALL.iter().filter(|op| family_of(**op) == OsFamily::Gpu).map(|op| op.name()).collect();
        assert_eq!(
            gpu,
            vec![
                "gpu/device-open",
                "gpu/device-close",
                "gpu/buffer-create",
                "gpu/texture-create",
                "gpu/sampler-create",
                "gpu/query-create",
                "gpu/shader-create",
                "gpu/pipeline-create",
                "gpu/bind-group-create",
                "gpu/submit",
                "gpu/release"
            ]
        );
        for op in [OsOp::GpuDeviceOpen, OsOp::GpuSubmit] {
            assert!(op.defers_completion(), "{}", op.name());
        }
        for op in [OsOp::GpuDeviceClose, OsOp::GpuBufferCreate, OsOp::GpuRelease] {
            assert!(!op.defers_completion(), "{}", op.name());
        }
        assert!(OsOp::ALL.iter().filter(|op| family_of(**op) == OsFamily::Gpu).all(|op| {
            op.runs_on_any_host() && !op.is_kernel_owned() && !op.is_stream()
        }));
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
            (OsOp::MmioRead.min_args().expect("table"), OsOp::MmioRead.max_args().expect("table")),
            (3, 3)
        );
        assert_eq!(
            (OsOp::MmioWrite.min_args().expect("table"), OsOp::MmioWrite.max_args().expect("table")),
            (4, 4)
        );
        assert_eq!((OsOp::IrqWait.min_args().expect("table"), OsOp::IrqAck.max_args().expect("table")), (1, 1));
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
        let (positional, options) = split_envelope(OsOp::ReadFile, &args).expect("table");
        assert_eq!(positional.len(), 1);
        assert!(options.is_some());
    }

    #[test]
    fn an_options_object_of_the_op_itself_is_not_the_envelope() {
        let args = sx(json!(["git", ["status"], {"cwd": "/repo", "stdin": "null"}]));
        let (positional, options) = split_envelope(OsOp::Spawn, &args).expect("table");
        assert_eq!(positional.len(), 3);
        assert!(options.is_none());
        let args = sx(
            json!(["git", ["status"], {"cwd": "/repo"}, {"key": "k", "emit": {"success": "OK"}}]),
        );
        let (positional, options) = split_envelope(OsOp::Spawn, &args).expect("table");
        assert_eq!(positional.len(), 3);
        assert!(options.is_some());
    }

    #[test]
    fn an_envelope_does_not_stand_in_for_a_required_argument() {
        let args = sx(json!([{"emit": {"success": "OK"}}]));
        let (positional, options) = split_envelope(OsOp::ReadFile, &args).expect("table");
        assert_eq!(positional.len(), 1);
        assert!(options.is_none());
        let empty = sx(json!([{}]));
        let (positional, options) = split_envelope(OsOp::Cwd, &empty).expect("table");
        assert_eq!(positional.len(), 1);
        assert!(options.is_none());
    }

    #[test]
    fn arity_is_the_declared_range() {
        assert!(accepts_arity(OsOp::ReadFile, 1).expect("table"));
        assert!(!accepts_arity(OsOp::ReadFile, 0).expect("table"));
        assert!(!accepts_arity(OsOp::ReadFile, 2).expect("table"), "the table declares a path and the options");
        assert!(accepts_arity(OsOp::Spawn, 3).expect("table") && !accepts_arity(OsOp::Spawn, 2).expect("table"), "the table requires the config");
        assert!(accepts_arity(OsOp::VolumeMute, 1).expect("table") && !accepts_arity(OsOp::VolumeMute, 0).expect("table"));
        assert!(accepts_arity(OsOp::WatchProcess, 2).expect("table") && !accepts_arity(OsOp::WatchProcess, 1).expect("table"));
        assert!(accepts_arity(OsOp::Exists, 1).expect("table") && !accepts_arity(OsOp::Exists, 2).expect("table"));
        assert!(accepts_arity(OsOp::Cwd, 0).expect("table"));
        assert!(!accepts_arity(OsOp::Cwd, 1).expect("table"));
    }

    #[test]
    fn current_exe_is_a_single_op_taking_no_argument() {
        assert_eq!(OsOp::from_name("os/current-exe"), Some(OsOp::CurrentExe));
        assert_eq!(OsOp::CurrentExe.name(), "os/current-exe");
        assert!(!OsOp::CurrentExe.is_stream() && !OsOp::CurrentExe.defers_completion());
        assert!(accepts_arity(OsOp::CurrentExe, 0).expect("table"));
        assert!(
            !accepts_arity(OsOp::CurrentExe, 1).expect("table"),
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
        let args: Vec<crate::value::Value> = serde_json::from_value(json!([5, [segment]])).expect("value args");
        let source = held("CRED", value);
        let resolved = resolve_credential_segments(OsOp::Send, &args, None, Some(&source))?;
        let crate::value::Value::Bytes(data) = &resolved[1] else { panic!("bytes operand") };
        Ok(data.to_vec())
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
    fn a_declared_bytes_answer_field_becomes_bytes_and_an_undeclared_one_is_left_alone() {
        use crate::value::Value;
        let answer: Value = serde_json::from_value(json!({ "data": "aGk=", "eof": false })).expect("answer");
        let read = answer_with_declared_bytes(OsOp::ReadFile, answer.clone()).expect("converted");
        assert_eq!(read.get("data"), Some(&Value::bytes(b"hi".to_vec())));
        assert_eq!(read.get("eof"), Some(&Value::Bool(false)));
        let wrote: Value = serde_json::from_value(json!({ "bytes": "aGk=" })).expect("answer");
        assert_eq!(answer_with_declared_bytes(OsOp::WriteFile, wrote.clone()).expect("untouched"), wrote);
        let bad: Value = serde_json::from_value(json!({ "data": "not base64!" })).expect("answer");
        assert_eq!(answer_with_declared_bytes(OsOp::ReadFile, bad).expect_err("refused").kind, OsErrorKind::Io);
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
        let args: Vec<crate::value::Value> = serde_json::from_value(json!([5, [segment]])).expect("value args");
        let source = |name: &str, owner: Option<&str>| Ok((name == "CRED").then(|| format!("value-of-{}", owner.unwrap_or("app"))));
        let resolved = resolve_credential_segments(OsOp::Send, &args, principal, Some(&source))?;
        let crate::value::Value::Bytes(data) = &resolved[1] else { panic!("bytes operand") };
        Ok(data.to_vec())
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
