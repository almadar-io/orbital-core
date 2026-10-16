//! What an effect puts on the bus when its `emit.success` fires.
//!
//! **The payload invariant:** an event's declared payload type IS its access
//! path. Declare `Survey` and the payload is the row, read as `?name`; declare
//! `{ row : Survey }` and it is read as `?row.name`. Wrapping that is not in
//! the type is a lie, and a field in the type that is not in the payload reads
//! `undefined`. Reading FEWER fields than are emitted is fine, so the relation
//! is `declared ⊆ produced`.
//!
//! This module is the ONE place that answers "what does operator X emit?" for
//! both compiler layers. It lives in `orbital-core` because
//! `orbital-lolo` (L1) and `orbital-compiler` (L2) are siblings — L1 dev-depends
//! on L2, so it cannot reach the baked operator registry, and CLAUDE.md's rule
//! is that a check both layers need lives in the crate they share.
//!
//! The two layers see different amounts of the program, and the split is
//! deliberate:
//!
//! - **L1** knows one file. It can decide a call site whose entity is a literal
//!   in the same file, and it catches the common authoring mistake at parse
//!   time.
//! - **L2** knows the whole program post-inline, including `@rebindable` traits
//!   whose entity is only fixed once a consumer binds it. Those are inherently
//!   L2 and L1 stays silent on them.
//!
//! The contracts below are LANGUAGE invariants, not registry trivia, but the
//! registry (`canonical-operators.json`) states them too. To keep the two from
//! drifting, `orbital-compiler` carries a test that asserts this table agrees
//! with the baked registry — drift fails a build rather than waiting to be
//! noticed in a corpus sweep.

use crate::runtime::os::OsOp;

/// The shape an operator's success emit carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProducedPayload {
    /// The persisted row itself, FLAT — its own fields at the payload root.
    /// `persist create` / `persist update`: both executors emit the row with
    /// no wrapper (`EffectExecutor.ts`'s `successPayload = persisted ?? data`;
    /// `orbital-core`'s `do_persist` → `executed_with_value`).
    EntityRow,
    /// A fixed set of field names, e.g. `persist delete`'s `{ id, deleted }`
    /// or `fetch`'s `{ data, totalCount }` wrapper.
    Fields(&'static [&'static str]),
    /// A fixed set of typed fields (`http`'s reply, failure and stream event),
    /// each possibly carrying nested typed fields.
    Shape(&'static [ShapeField]),
    /// Not decidable here — no contract, an opaque `any`, or a call site this
    /// layer cannot narrow. Callers MUST treat this as "do not check" rather
    /// than as "produces nothing".
    Opaque,
}

/// One typed field of a [`ProducedPayload::Shape`]. `field_type` speaks the
/// payload-declaration vocabulary (`string`, `number`, `object`, `array`);
/// `fields` types an object's members, empty when it has none to state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeField {
    pub name: &'static str,
    pub field_type: &'static str,
    pub fields: &'static [ShapeField],
}

const fn field(name: &'static str, field_type: &'static str) -> ShapeField {
    ShapeField { name, field_type, fields: &[] }
}

/// What distinguishes a call site within one operator's contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallVariant<'a> {
    /// The operator has one contract, or the call site carries no literal
    /// action (the legacy 3-element `persist`).
    Plain,
    /// `persist`'s literal action keyword (`create`, `update`, `delete`, ...).
    PersistAction(&'a str),
    /// An `http` call site that carries `paginate`.
    HttpPaginate,
    HttpPresign,
    /// An `http` call whose auth is `oauth-client` with this op.
    HttpOAuth(crate::oauth::client::OAuthOp),
}

/// `persist delete` hands back the deleted row's id plus a tombstone flag —
/// never the row.
pub const PERSIST_DELETE_FIELDS: &[&str] = &["id", "deleted"];

/// `fetch` / `ref` wrap their rows: authors read `?data`, and `totalCount` is
/// the pre-pagination count so a paginating consumer needs no second call.
pub const FETCH_WRAPPER_FIELDS: &[&str] = &["data", "totalCount"];

/// The reply an `http` call hands back, as `last` carries it inside a paged payload.
const HTTP_REPLY_BODY_SHAPE: &[ShapeField] = &[
    field("status", "number"),
    field("headers", "object"),
    field("body", "string"),
];

/// `http` hands back the reply: its status, its lower-cased headers, its body and the call's key (`null` unkeyed).
pub const HTTP_REPLY_SHAPE: &[ShapeField] = &[
    field("status", "number"),
    field("headers", "object"),
    field("body", "string"),
    field("key", "string"),
];

/// `browser/beacon` hands back how the host took the request, never a collector reply: `sent`, the route taken
/// (`queue` | `fetch`), the keepalive fetch's `status` (`null` when the queue accepted it or the fetch threw) and
/// the call's key (`null` unkeyed).
pub const HTTP_BEACON_SHAPE: &[ShapeField] = &[
    field("sent", "boolean"),
    field("via", "string"),
    field("status", "number"),
    field("key", "string"),
];

/// `browser/page-facts` hands back the page facts flat: whether there is a page, and each fact (a fact that was
/// not requested keeps its default and is not readable).
pub const PAGE_FACTS_SHAPE: &[ShapeField] = &[
    field("available", "boolean"),
    field("hostname", "string"),
    field("referrer", "string"),
    field("screenWidth", "number"),
];

pub const HTTP_PRESIGN_SHAPE: &[ShapeField] = &[field("url", "string"), field("key", "string")];

/// `http` with `paginate` hands back the pages it walked, the collected items and the last reply.
pub const HTTP_PAGED_SHAPE: &[ShapeField] = &[
    field("pages", "number"),
    field("items", "array"),
    ShapeField { name: "last", field_type: "object", fields: HTTP_REPLY_BODY_SHAPE },
    field("key", "string"),
];

/// An `http` failure: the message, its kind (`timeout`, `failed`, `refused`, `unconfigured`, `oauth`,
/// `unavailable`), the credential a missing one names, the leg that failed (`phase`: `grant`, `request` or
/// `null`), the status of a failed grant reply (`grantStatus`), the public `code` and closed `reason` of an
/// `oauth` failure (each `null` for every other kind) and the call's key.
pub const HTTP_FAILURE_SHAPE: &[ShapeField] = &[
    field("error", "string"),
    field("kind", "string"),
    field("credential", "string"),
    field("phase", "string"),
    field("grantStatus", "number"),
    field("grantBody", "string"),
    field("code", "string"),
    field("reason", "string"),
    field("key", "string"),
];

/// One `http` `stream` event handed to `emit.onMessage`.
pub const HTTP_STREAM_EVENT_SHAPE: &[ShapeField] = &[
    field("data", "string"),
    field("event", "string"),
    field("id", "string"),
    field("key", "string"),
];

/// `random-bytes` hands back the bytes as base64url without padding.
pub const RANDOM_BYTES_SHAPE: &[ShapeField] = &[field("value", "string")];

/// `credential-status` and `credential-forget` hand back whether the credential is connected.
pub const CREDENTIAL_SHAPE: &[ShapeField] = &[field("connected", "boolean")];

/// `oauth-authorize` hands back the URL to send the user to and the state the callback returns.
pub const OAUTH_AUTHORIZE_SHAPE: &[ShapeField] = &[field("authUrl", "string"), field("state", "string"), field("key", "string")];

/// An `oauth-authorize` failure: the OAuth failure beside the call's key.
pub const OAUTH_AUTHORIZE_FAILURE_SHAPE: &[ShapeField] =
    &[field("error", "string"), field("code", "string"), field("reason", "string"), field("key", "string")];

/// `oauth-connection-status`, `oauth-connection-forget` and an `oauth-client` `connect`: a caller's connection row.
pub const OAUTH_CONNECTION_SHAPE: &[ShapeField] = &[
    field("provider", "string"),
    field("connected", "boolean"),
    field("account", "string"),
    field("avatarUrl", "string"),
    field("scope", "array"),
    field("connectedAt", "string"),
];

pub const OAUTH_CONNECT_SHAPE: &[ShapeField] = &[
    field("provider", "string"),
    field("connected", "boolean"),
    field("account", "string"),
    field("avatarUrl", "string"),
    field("scope", "array"),
    field("connectedAt", "string"),
    field("key", "string"),
];

pub const OAUTH_DISCOVERY_SHAPE: &[ShapeField] = &[field("issuer", "string"), field("key", "string")];

pub const OAUTH_CODE_SHAPE: &[ShapeField] = &[
    field("accessToken", "string"),
    field("refreshToken", "string"),
    field("expiresIn", "number"),
    field("tokenType", "string"),
    field("scope", "array"),
    field("subject", "string"),
    field("key", "string"),
];

pub const OAUTH_REFRESH_SHAPE: &[ShapeField] =
    &[field("accessToken", "string"), field("expiresIn", "number"), field("key", "string")];

pub const OAUTH_REVOKE_SHAPE: &[ShapeField] = &[field("revoked", "boolean"), field("key", "string")];

pub const OAUTH_USERINFO_SHAPE: &[ShapeField] = &[
    field("sub", "string"),
    field("email", "string"),
    field("name", "string"),
    field("picture", "string"),
    field("key", "string"),
];

/// An OAuth effect's failure: the fixed safe message, the public code and the closed reason.
pub const OAUTH_FAILURE_SHAPE: &[ShapeField] =
    &[field("error", "string"), field("code", "string"), field("reason", "string")];

/// A host effect's failure: the message.
pub const HOST_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string")];

/// A keyed `call-service` failure: the message and the call's key.
pub const KEYED_CALL_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("key", "string")];

/// `persist batch` hands back a summary of the attempted operations.
pub const PERSIST_BATCH_FIELDS: &[&str] = &["operations", "completedCount", "totalCount"];

const OS_NONE: &[ShapeField] = &[];
const OS_DATA: &[ShapeField] = &[field("data", "string")];
const OS_BYTES: &[ShapeField] = &[field("bytes", "number")];
const OS_FD: &[ShapeField] = &[field("fd", "number")];
const OS_READ: &[ShapeField] = &[field("data", "string"), field("eof", "boolean")];
const OS_OFFSET: &[ShapeField] = &[field("offset", "number")];
const OS_TARGET: &[ShapeField] = &[field("target", "string")];
const OS_ENTRIES: &[ShapeField] = &[field("entries", "array")];
const OS_PATHS: &[ShapeField] = &[field("paths", "array")];
const OS_PATH: &[ShapeField] = &[field("path", "string")];
const OS_STAT: &[ShapeField] = &[
    field("kind", "string"),
    field("size", "number"),
    field("mode", "number"),
    field("uid", "number"),
    field("gid", "number"),
    field("atime", "number"),
    field("mtime", "number"),
    field("ctime", "number"),
];
const OS_OK: &[ShapeField] = &[field("ok", "boolean")];
const OS_SPAWNED: &[ShapeField] = &[
    field("pid", "number"),
    field("stdin", "number"),
    field("stdout", "number"),
    field("stderr", "number"),
];
const OS_EXIT: &[ShapeField] = &[field("code", "number"), field("signal", "string")];
const OS_PID: &[ShapeField] = &[field("pid", "number")];
const OS_VALUE: &[ShapeField] = &[field("value", "string")];
const OS_NAMES: &[ShapeField] = &[field("names", "array")];
const OS_EOF: &[ShapeField] = &[field("eof", "boolean")];
const OS_TTY_SIZE: &[ShapeField] = &[field("rows", "number"), field("cols", "number")];
const OS_SOCK: &[ShapeField] = &[field("sock", "number")];
const OS_ACCEPTED: &[ShapeField] = &[field("sock", "number"), field("peer", "string")];
const OS_RECVFROM: &[ShapeField] = &[field("data", "string"), field("peer", "string")];
const OS_ADDRESSES: &[ShapeField] = &[field("addresses", "array")];
const OS_NANOS: &[ShapeField] = &[field("nanos", "number")];
const OS_UID: &[ShapeField] = &[field("uid", "number")];
const OS_GID: &[ShapeField] = &[field("gid", "number")];
const OS_USERNAME: &[ShapeField] = &[field("username", "string")];
const OS_GROUP: &[ShapeField] = &[field("gid", "number"), field("members", "array")];
const OS_EVENT_FILE: &[ShapeField] = &[
    field("path", "string"),
    field("name", "string"),
    field("dir", "string"),
    field("timestamp", "number"),
    field("kind", "string"),
];
const OS_EVENT_PROCESS: &[ShapeField] = &[
    field("pid", "number"),
    field("name", "string"),
    field("exitCode", "number"),
    field("signal", "string"),
    field("args", "array"),
    field("duration", "number"),
];
const OS_EVENT_SIGNAL: &[ShapeField] = &[field("signal", "string")];
const OS_EVENT_ENV: &[ShapeField] = &[field("name", "string"), field("value", "string")];
const OS_EVENT_PORT: &[ShapeField] = &[field("port", "number"), field("open", "boolean")];
const OS_EVENT_CRON: &[ShapeField] = &[field("scheduledAt", "number")];

/// An `os/` failure: the message and its closed kind (`denied`, `not-found`, `exists`, `invalid`, `io`,
/// `timeout`, `unsupported`, `closed`).
pub const OS_FAILURE_SHAPE: &[ShapeField] = &[field("error", "string"), field("kind", "string")];

/// What one `os/` operation emits: its final answer on `emit.success`, and each event of a stream on
/// `emit.onMessage` (opaque for a single-answer op). The system-information and desktop ops state no
/// payload in the contract, so theirs is opaque.
struct OsContract {
    success: ProducedPayload,
    event: ProducedPayload,
}

fn os_contract(op: OsOp) -> OsContract {
    let single = |shape: &'static [ShapeField]| OsContract {
        success: ProducedPayload::Shape(shape),
        event: ProducedPayload::Opaque,
    };
    let stream = |success: &'static [ShapeField], event: &'static [ShapeField]| OsContract {
        success: ProducedPayload::Shape(success),
        event: ProducedPayload::Shape(event),
    };
    let unstated = || OsContract { success: ProducedPayload::Opaque, event: ProducedPayload::Opaque };
    match op {
        OsOp::ReadFile => single(OS_DATA),
        OsOp::WriteFile | OsOp::AppendFile | OsOp::Write | OsOp::Send | OsOp::Sendto | OsOp::StdoutWrite
        | OsOp::StderrWrite => single(OS_BYTES),
        OsOp::Open => single(OS_FD),
        OsOp::Read | OsOp::Recv => single(OS_READ),
        OsOp::Seek => single(OS_OFFSET),
        OsOp::Close | OsOp::Truncate | OsOp::Fsync | OsOp::Copy | OsOp::Rename | OsOp::Remove | OsOp::Link
        | OsOp::Symlink | OsOp::Mkdir | OsOp::Rmdir | OsOp::Chdir | OsOp::Chmod | OsOp::Chown | OsOp::Utimes
        | OsOp::Kill | OsOp::Setenv | OsOp::Unsetenv | OsOp::Shutdown => single(OS_NONE),
        OsOp::Readlink => single(OS_TARGET),
        OsOp::WatchFiles => stream(OS_NONE, OS_EVENT_FILE),
        OsOp::ListDir => single(OS_ENTRIES),
        OsOp::Glob => single(OS_PATHS),
        OsOp::Mktemp | OsOp::Realpath | OsOp::Cwd => single(OS_PATH),
        OsOp::Stat | OsOp::Lstat => single(OS_STAT),
        OsOp::Access | OsOp::Exists | OsOp::Isatty => single(OS_OK),
        OsOp::Spawn => single(OS_SPAWNED),
        OsOp::Wait => single(OS_EXIT),
        OsOp::Pid | OsOp::Ppid => single(OS_PID),
        OsOp::WatchProcess => stream(OS_NONE, OS_EVENT_PROCESS),
        OsOp::WatchSignal => stream(OS_NONE, OS_EVENT_SIGNAL),
        OsOp::Getenv => single(OS_VALUE),
        OsOp::Environ => single(OS_NAMES),
        OsOp::WatchEnv => stream(OS_NONE, OS_EVENT_ENV),
        OsOp::StdinRead => stream(OS_EOF, OS_DATA),
        OsOp::TtySize => single(OS_TTY_SIZE),
        OsOp::TcpConnect | OsOp::UdpBind => single(OS_SOCK),
        OsOp::TcpListen => stream(OS_SOCK, OS_ACCEPTED),
        OsOp::Recvfrom => single(OS_RECVFROM),
        OsOp::DnsResolve => single(OS_ADDRESSES),
        OsOp::WatchPort => stream(OS_NONE, OS_EVENT_PORT),
        OsOp::ClockMonotonic => single(OS_NANOS),
        OsOp::Uid => single(OS_UID),
        OsOp::Gid => single(OS_GID),
        OsOp::Username => single(OS_USERNAME),
        OsOp::GroupOf => single(OS_GROUP),
        OsOp::WatchCron => stream(OS_NONE, OS_EVENT_CRON),
        OsOp::CpuUsage | OsOp::MemoryInfo | OsOp::Uptime | OsOp::LoadAvg | OsOp::Hostname | OsOp::Uname
        | OsOp::DiskUsage | OsOp::Mounts | OsOp::DiskIo | OsOp::NetworkInterfaces | OsOp::NetworkStats
        | OsOp::NetworkConnected | OsOp::DefaultGateway | OsOp::BatteryStatus | OsOp::AcPower
        | OsOp::ThermalZones | OsOp::Notify | OsOp::Speak | OsOp::BrightnessGet | OsOp::BrightnessSet
        | OsOp::VolumeGet | OsOp::VolumeSet | OsOp::VolumeMute | OsOp::Suspend | OsOp::Hibernate => unstated(),
    }
}

/// What `operator` emits on success at a call site.
///
/// `variant` carries what the call site adds to the operator's contract:
/// `persist`'s literal action keyword (the legacy 3-element form has no action
/// keyword; pass [`CallVariant::Plain`] and this returns
/// [`ProducedPayload::Opaque`] rather than guessing "create" -- a wrong guess
/// would report against the wrong contract), or an `http` call's `paginate`.
pub fn success_payload(operator: &str, variant: CallVariant) -> ProducedPayload {
    if let Some(op) = OsOp::from_name(operator) {
        return os_contract(op).success;
    }
    match operator {
        "persist" => match variant {
            CallVariant::PersistAction("create" | "upsert" | "update") => ProducedPayload::EntityRow,
            CallVariant::PersistAction("delete") => ProducedPayload::Fields(PERSIST_DELETE_FIELDS),
            CallVariant::PersistAction("batch") => ProducedPayload::Fields(PERSIST_BATCH_FIELDS),
            _ => ProducedPayload::Opaque,
        },
        // `ref` emits the same wrapper as `fetch` but fires it on `on_change`;
        // see `success_emit_key`.
        "fetch" | "ref" => ProducedPayload::Fields(FETCH_WRAPPER_FIELDS),
        "http" => match variant {
            CallVariant::HttpPaginate => ProducedPayload::Shape(HTTP_PAGED_SHAPE),
            CallVariant::HttpPresign => ProducedPayload::Shape(HTTP_PRESIGN_SHAPE),
            CallVariant::HttpOAuth(op) => {
                use crate::oauth::client::OAuthOp;
                ProducedPayload::Shape(match op {
                    OAuthOp::Discovery => OAUTH_DISCOVERY_SHAPE,
                    OAuthOp::Code => OAUTH_CODE_SHAPE,
                    OAuthOp::Connect => OAUTH_CONNECT_SHAPE,
                    OAuthOp::Refresh => OAUTH_REFRESH_SHAPE,
                    OAuthOp::Revoke => OAUTH_REVOKE_SHAPE,
                    OAuthOp::Userinfo => OAUTH_USERINFO_SHAPE,
                })
            }
            _ => ProducedPayload::Shape(HTTP_REPLY_SHAPE),
        },
        "browser/beacon" => ProducedPayload::Shape(HTTP_BEACON_SHAPE),
        "browser/page-facts" => ProducedPayload::Shape(PAGE_FACTS_SHAPE),
        "random-bytes" => ProducedPayload::Shape(RANDOM_BYTES_SHAPE),
        "credential-status" | "credential-forget" => ProducedPayload::Shape(CREDENTIAL_SHAPE),
        "oauth-authorize" => ProducedPayload::Shape(OAUTH_AUTHORIZE_SHAPE),
        "oauth-connection-status" | "oauth-connection-forget" => ProducedPayload::Shape(OAUTH_CONNECTION_SHAPE),
        // `call-service` and `set` declare `any`; nothing to compare against.
        _ => ProducedPayload::Opaque,
    }
}

/// What `operator` emits on `emit.failure`. `keyed` = the call site carries a
/// `key`: only a keyed `call-service` failure has a known shape (`{ error, key }`);
/// an unkeyed one stays opaque. `http` always echoes its key.
pub fn failure_payload(operator: &str, keyed: bool) -> ProducedPayload {
    if OsOp::from_name(operator).is_some() {
        return ProducedPayload::Shape(OS_FAILURE_SHAPE);
    }
    match operator {
        "http" | "browser/beacon" => ProducedPayload::Shape(HTTP_FAILURE_SHAPE),
        "browser/page-facts" if keyed => ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE),
        "random-bytes" | "credential-status" | "credential-forget" | "browser/page-facts" => ProducedPayload::Shape(HOST_FAILURE_SHAPE),
        "oauth-authorize" => ProducedPayload::Shape(OAUTH_AUTHORIZE_FAILURE_SHAPE),
        "oauth-connection-status" | "oauth-connection-forget" => ProducedPayload::Shape(OAUTH_FAILURE_SHAPE),
        "call-service" | "call_service" if keyed => ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE),
        _ => ProducedPayload::Opaque,
    }
}

/// What `operator` emits on `emit.onMessage`: an `http` `stream`, or each event of a streaming `os/` op.
pub fn message_payload(operator: &str) -> ProducedPayload {
    if let Some(op) = OsOp::from_name(operator) {
        return os_contract(op).event;
    }
    match operator {
        "http" => ProducedPayload::Shape(HTTP_STREAM_EVENT_SHAPE),
        _ => ProducedPayload::Opaque,
    }
}

/// The bus key an operator fires its success payload on.
///
/// `ref` uses `on_change` in both executors, so reading `emit.success` for it
/// inspects a key the runtime never fires.
pub fn success_emit_key(operator: &str) -> &'static str {
    match operator {
        "ref" => "on_change",
        _ => "success",
    }
}

/// The declared field names that the produced payload does NOT carry — i.e.
/// every field whose read would be `undefined` at runtime.
///
/// This is the whole rule, and it is width subtyping on purpose: declaring
/// FEWER fields than are emitted is legal (you only read what you declared),
/// so only the declared side is walked. Equality would reject the many corpus
/// fetch events that legitimately omit `totalCount`.
///
/// `entity_field_names` supplies the row's own fields for
/// [`ProducedPayload::EntityRow`]; pass an empty slice when the entity cannot
/// be resolved, which yields no findings rather than false ones.
pub fn undeclared_reads<'a>(
    declared_field_names: &[&'a str],
    produced: &ProducedPayload,
    entity_field_names: &[&str],
) -> Vec<&'a str> {
    let shape_names: Vec<&str>;
    let produced_names: &[&str] = match produced {
        ProducedPayload::Opaque => return Vec::new(),
        ProducedPayload::Fields(f) => f,
        ProducedPayload::Shape(fields) => {
            shape_names = fields.iter().map(|f| f.name).collect();
            &shape_names
        }
        ProducedPayload::EntityRow => {
            if entity_field_names.is_empty() {
                return Vec::new();
            }
            entity_field_names
        }
    };
    declared_field_names
        .iter()
        .copied()
        .filter(|d| !produced_names.contains(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_effects_emit_their_closed_payloads() {
        assert_eq!(
            success_payload("random-bytes", CallVariant::Plain),
            ProducedPayload::Shape(RANDOM_BYTES_SHAPE)
        );
        for op in ["credential-status", "credential-forget"] {
            assert_eq!(success_payload(op, CallVariant::Plain), ProducedPayload::Shape(CREDENTIAL_SHAPE));
            assert_eq!(failure_payload(op, false), ProducedPayload::Shape(HOST_FAILURE_SHAPE));
        }
        assert_eq!(failure_payload("random-bytes", false), ProducedPayload::Shape(HOST_FAILURE_SHAPE));
        assert_eq!(
            undeclared_reads(&["value", "bytes"], &success_payload("random-bytes", CallVariant::Plain), &[]),
            vec!["bytes"]
        );
        assert_eq!(
            undeclared_reads(&["connected"], &success_payload("credential-status", CallVariant::Plain), &[]),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn persist_create_and_update_emit_the_row_flat() {
        assert_eq!(
            success_payload("persist", CallVariant::PersistAction("create")),
            ProducedPayload::EntityRow
        );
        assert_eq!(
            success_payload("persist", CallVariant::PersistAction("update")),
            ProducedPayload::EntityRow
        );
    }

    #[test]
    fn persist_delete_emits_a_tombstone_not_the_row() {
        assert_eq!(
            success_payload("persist", CallVariant::PersistAction("delete")),
            ProducedPayload::Fields(PERSIST_DELETE_FIELDS)
        );
    }

    /// The legacy 3-element persist form carries no action keyword. Guessing
    /// "create" there would compare against the wrong contract.
    #[test]
    fn an_actionless_persist_is_not_guessed_at() {
        assert_eq!(success_payload("persist", CallVariant::Plain), ProducedPayload::Opaque);
    }

    #[test]
    fn fetch_and_ref_emit_the_data_wrapper_and_ref_uses_on_change() {
        assert_eq!(
            success_payload("fetch", CallVariant::Plain),
            ProducedPayload::Fields(FETCH_WRAPPER_FIELDS)
        );
        assert_eq!(success_emit_key("fetch"), "success");
        assert_eq!(success_emit_key("ref"), "on_change");
    }

    /// The defect that started this: `persist create` emits the row flat, so a
    /// declared `row` wrapper names a field nothing emits.
    #[test]
    fn a_row_wrapper_on_a_created_entity_is_reported() {
        let found = undeclared_reads(
            &["row"],
            &ProducedPayload::EntityRow,
            &["id", "title", "content"],
        );
        assert_eq!(found, vec!["row"]);
    }

    /// Omission is legal — the rule is `declared ⊆ produced`.
    #[test]
    fn declaring_a_subset_is_legal() {
        assert!(undeclared_reads(
            &["data"],
            &ProducedPayload::Fields(FETCH_WRAPPER_FIELDS),
            &[]
        )
        .is_empty());
        assert!(
            undeclared_reads(&["id"], &ProducedPayload::EntityRow, &["id", "title"]).is_empty()
        );
    }

    /// An unresolvable entity must produce NO finding rather than claim every
    /// declared field is missing.
    #[test]
    fn an_unresolvable_entity_yields_no_findings() {
        assert!(undeclared_reads(&["anything"], &ProducedPayload::EntityRow, &[]).is_empty());
    }

    #[test]
    fn http_emits_the_reply_and_a_paginating_call_emits_the_pages() {
        assert_eq!(
            success_payload("http", CallVariant::Plain),
            ProducedPayload::Shape(HTTP_REPLY_SHAPE)
        );
        assert_eq!(
            success_payload("http", CallVariant::HttpPaginate),
            ProducedPayload::Shape(HTTP_PAGED_SHAPE)
        );
        assert_eq!(success_emit_key("http"), "success");
        assert_eq!(
            undeclared_reads(&["status", "body"], &success_payload("http", CallVariant::Plain), &[]),
            Vec::<&str>::new()
        );
        assert_eq!(
            undeclared_reads(&["status"], &success_payload("http", CallVariant::HttpPaginate), &[]),
            vec!["status"],
            "control: a paged payload has no status at its root"
        );
    }

    #[test]
    fn a_paged_payload_types_its_last_reply() {
        let last = HTTP_PAGED_SHAPE.iter().find(|f| f.name == "last").unwrap();
        let names: Vec<&str> = last.fields.iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["status", "headers", "body"]);
    }

    #[test]
    fn http_failure_and_stream_events_echo_the_key() {
        let failure = failure_payload("http", false);
        assert_eq!(failure, ProducedPayload::Shape(HTTP_FAILURE_SHAPE));
        assert!(undeclared_reads(&["error", "kind", "credential", "key"], &failure, &[]).is_empty());
        assert_eq!(undeclared_reads(&["status"], &failure, &[]), vec!["status"]);
        let event = message_payload("http");
        assert!(undeclared_reads(&["data", "event", "id", "key"], &event, &[]).is_empty());
        assert_eq!(undeclared_reads(&["error"], &event, &[]), vec!["error"]);
    }

    #[test]
    fn a_keyed_call_service_failure_is_error_and_key_and_an_unkeyed_one_is_opaque() {
        assert_eq!(
            failure_payload("call-service", true),
            ProducedPayload::Shape(KEYED_CALL_FAILURE_SHAPE)
        );
        assert_eq!(failure_payload("call-service", false), ProducedPayload::Opaque);
        assert_eq!(message_payload("call-service"), ProducedPayload::Opaque);
        assert_eq!(failure_payload("fetch", true), ProducedPayload::Opaque);
    }

    #[test]
    fn opaque_operators_are_never_checked() {
        assert_eq!(
            success_payload("call-service", CallVariant::Plain),
            ProducedPayload::Opaque
        );
        assert!(undeclared_reads(&["whatever"], &ProducedPayload::Opaque, &[]).is_empty());
    }

    #[test]
    fn an_oauth_connect_emit_has_no_token_field_to_read_and_a_code_emit_has() {
        use crate::oauth::client::OAuthOp;
        let connect = success_payload("http", CallVariant::HttpOAuth(OAuthOp::Connect));
        assert_eq!(undeclared_reads(&["accessToken", "connected"], &connect, &[]), vec!["accessToken"]);
        let code = success_payload("http", CallVariant::HttpOAuth(OAuthOp::Code));
        assert!(undeclared_reads(&["accessToken", "refreshToken", "expiresIn"], &code, &[]).is_empty());
        let status = success_payload("oauth-connection-status", CallVariant::Plain);
        assert!(undeclared_reads(&["connected", "scope"], &status, &[]).is_empty());
        assert_eq!(
            undeclared_reads(&["reason"], &failure_payload("oauth-authorize", false), &[]),
            Vec::<&str>::new()
        );
        assert_eq!(undeclared_reads(&["token"], &failure_payload("oauth-authorize", false), &[]), vec!["token"]);
    }
}
