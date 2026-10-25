//! The native answerer of [`IoRequest::Os`]: one place every native host reaches through `answer_http`.
//! Wasm hosts answer `Os` themselves, as they do `Random`. The grant check and handle ownership live in the
//! family modules; this file routes an op to its family and owns the handle tables.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

pub mod args;
pub mod completion;
pub mod fs;
pub mod net;
pub mod process;
pub mod sys;

pub use orbital_core_base::os_grants::*;

use orbital_core_base::value::Value;
use orbital_core_base::os::{family_of, OsError, OsErrorKind, OsFamily, OsOp};
use crate::resumable::{CallSite, IoAnswer, IoRequest};
use orbital_core_base::sync::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Descriptors 0, 1 and 2 are the standard streams; allocated ids start here.
pub const FIRST_HANDLE_ID: i64 = 3;

/// One id sequence shared by every table that hands out handles, so `os/close` can tell an fd from a socket.
pub type IdCounter = Arc<AtomicI64>;

/// A `Mutex`-guarded id-to-entry table.
pub struct HandleSlab<T> {
    ids: IdCounter,
    entries: Mutex<HashMap<i64, T>>,
}

impl<T> HandleSlab<T> {
    pub fn new(ids: IdCounter) -> Self {
        Self {
            ids,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn insert(&self, entry: T) -> i64 {
        let id = self.ids.fetch_add(1, Ordering::SeqCst);
        self.entries.lock().insert(id, entry);
        id
    }

    /// Run `f` on the entry `id` names; `None` when no such handle is open.
    pub fn with<R>(&self, id: i64, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.entries.lock().get_mut(&id).map(f)
    }

    pub fn remove(&self, id: i64) -> Option<T> {
        self.entries.lock().remove(&id)
    }

    pub fn contains(&self, id: i64) -> bool {
        self.entries.lock().contains_key(&id)
    }

    pub fn clear(&self) {
        self.entries.lock().clear();
    }
}

/// The handles one program instance owns; closed by [`OsHandles::close_all`] at teardown.
pub struct OsHandles {
    pub fs: fs::FsTable,
    pub process: process::ProcTable,
    pub net: net::NetTable,
    pub completions: Arc<completion::Completions>,
    /// Where `~/` and `./` paths and grants are anchored: the user's home and the directory the program started in.
    pub anchors: OsAnchors,
    /// The `kv/` stores of this program instance.
    pub kv: crate::kv_host::KvStores,
}

impl Default for OsHandles {
    fn default() -> Self {
        let ids: IdCounter = Arc::new(AtomicI64::new(FIRST_HANDLE_ID));
        Self {
            fs: fs::FsTable::new(ids.clone()),
            process: process::ProcTable::default(),
            net: net::NetTable::new(ids),
            completions: Arc::default(),
            anchors: OsAnchors::of_process(),
            kv: crate::kv_host::KvStores::default(),
        }
    }
}

impl OsHandles {
    /// Drop the pending completion of a deferred keyed call, as `cancel-call` of its key does: the operation is
    /// not stopped, its outcome is never delivered. `false` when the key has none pending.
    pub fn cancel_completion(&self, key: &str) -> bool {
        self.completions.cancel(key)
    }

    pub fn close_all(&self) {
        self.completions.clear();
        self.process.clear();
        self.fs.clear();
        self.net.clear();
    }
}

/// What a native host hands the answerer: the program's grants, its handles, and where stream events go.
pub struct OsHost<'a> {
    pub grants: &'a OsGrants,
    pub handles: &'a OsHandles,
    pub on_event: &'a dyn Fn(Option<&CallSite>, Value),
}


fn dispatch(
    op: OsOp,
    args: &[Value],
    key: Option<&str>,
    site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Result<Value, OsError> {
    let answered = match family_of(op) {
        OsFamily::Fs => fs::answer(op, args, key, site, host),
        OsFamily::Process => process::answer(op, args, key, site, host),
        OsFamily::Net => net::answer(op, args, key, site, host),
        OsFamily::Sys => sys::answer(op, args, key, site, host),
        OsFamily::Kv => Some(crate::kv_host::answer(op, args, host)),
        OsFamily::Dev => Some(Err(OsError::new(OsErrorKind::Unsupported, format!("{}: this host has no devices", op.name())))),
    };
    // `os/close` takes an fd or a socket: the fs table is asked first, and a handle it does not own
    // (`None`) is offered to the net table.
    let answered = match (answered, op) {
        (None, OsOp::Close) => net::answer(op, args, key, site, host),
        (answered, _) => answered,
    };
    finish_answer(op, answered)
}

fn finish_answer(op: OsOp, answered: Option<Result<Value, OsError>>) -> Result<Value, OsError> {
    match (answered, op) {
        (Some(result), _) => result,
        (None, OsOp::Close) => Err(OsError::new(
            OsErrorKind::Closed,
            "os/close: not an open handle",
        )),
        (None, _) => Err(OsError::new(
            OsErrorKind::Unsupported,
            format!(
                "{}: this host does not implement the operation yet",
                op.name()
            ),
        )),
    }
}

/// A keyed call of a completion-deferring op under a declared route runs in the background when the host has a
/// completion sink; `None` means the call is answered in place. EXEC-QUEUE-10.
fn defer(
    op: OsOp,
    args: &[Value],
    key: Option<&str>,
    site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Option<Result<(), OsError>> {
    let (key, site) = (key?, site?);
    if !op.defers_completion() || host.handles.completions.sink().is_none() {
        return None;
    }
    Some(match op {
        OsOp::Wait => process::wait_later(args, key, site, host),
        OsOp::Read => fs::read_later(args, key, site, host),
        OsOp::Send => net::send_later(args, key, site, host),
        OsOp::Recv => net::recv_later(args, key, site, host),
        OsOp::Recvfrom => net::recvfrom_later(args, key, site, host),
        OsOp::StdinRead => process::stdin_later(args, key, site, host),
        _ if family_of(op) == OsFamily::Fs => fs::later(op, args, key, site, host),
        _ => return None,
    })
}

/// Answer an [`IoRequest::Os`]. With no host every op is `denied`: the `os/` family is closed by default.
pub fn answer_os(host: Option<&OsHost<'_>>, request: &IoRequest) -> IoAnswer {
    let IoRequest::Os {
        op,
        args,
        key,
        site,
    } = request
    else {
        return IoAnswer::Service(Err("not an os request".to_string()));
    };
    let Some(host) = host else {
        return IoAnswer::Os(Err(args::denied(*op, "no os host configured")));
    };
    let _span = tracing::trace_span!(target: orbital_core_base::logging::ns::RUNTIME_OS, "os ask", op = ?op, keyed = key.is_some()).entered();
    let resolved = match resolve_anchored_args(*op, args, &host.handles.anchors) {
        Ok(resolved) => resolved,
        Err(error) => return IoAnswer::Os(Err(error)),
    };
    let args = &resolved;
    if let Some(started) = defer(*op, args, key.as_deref(), site.as_ref(), host) {
        return match started {
            Ok(()) => IoAnswer::ServiceDeferred,
            Err(error) => IoAnswer::Os(Err(error)),
        };
    }
    IoAnswer::Os(dispatch(*op, args, key.as_deref(), site.as_ref(), host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(op: OsOp) -> IoRequest {
        IoRequest::Os {
            op,
            args: Vec::new(),
            key: None,
            site: None,
        }
    }

    fn with_host<R>(check: impl FnOnce(&OsHost<'_>) -> R) -> R {
        let grants = OsGrants::default();
        let handles = OsHandles::default();
        let on_event = |_: Option<&CallSite>, _: Value| {};
        check(&OsHost {
            grants: &grants,
            handles: &handles,
            on_event: &on_event,
        })
    }

    fn anchored_value(json: serde_json::Value) -> Value {
        serde_json::from_value(json).expect("value")
    }

    fn os_answer(op: OsOp, args: Vec<Value>, grants: &OsGrants, handles: &OsHandles) -> Result<Value, OsError> {
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let request = IoRequest::Os { op, args, key: None, site: None };
        match answer_os(Some(&OsHost { grants, handles, on_event: &on_event }), &request) {
            IoAnswer::Os(answer) => answer,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_anchored_path_is_resolved_at_the_boot_anchor_and_checked_against_the_resolved_grant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/a.md"), "ledger").unwrap();
        std::fs::write(root.join("secret.txt"), "no").unwrap();
        let handles = OsHandles {
            anchors: OsAnchors { start: Some(root.clone()), ..OsAnchors::default() },
            ..OsHandles::default()
        };
        let declared = OsGrants {
            anchored: AnchoredGrants {
                fs: vec![AnchoredFsGrant { anchor: GrantAnchor::Cwd, glob: "docs/**".into(), modes: vec![FsMode::Read] }],
                ..AnchoredGrants::default()
            },
            ..OsGrants::default()
        };
        let (grants, problems) = declared.resolve_anchors(&handles.anchors);
        assert!(problems.is_empty());
        let read = |path: &str| os_answer(OsOp::ReadFile, vec![anchored_value(serde_json::json!({"anchor": "cwd", "path": path}))], &grants, &handles);
        assert!(read("docs/a.md").is_ok());
        assert_eq!(read("secret.txt").expect_err("outside").kind, OsErrorKind::Denied, "control: outside the anchored glob");
        assert_eq!(read("../x").expect_err("climbs").kind, OsErrorKind::Denied, "edge: climbing above the anchor");
        assert_eq!(
            os_answer(OsOp::ReadFile, vec![anchored_value(serde_json::json!({"anchor": "cwd", "path": "docs/a.md"}))], &declared, &handles)
                .expect_err("unresolved")
                .kind,
            OsErrorKind::Denied,
            "control: grants never resolved at boot match nothing"
        );
        let unplaced = OsHandles::default();
        let env_read = os_answer(OsOp::ReadFile, vec![anchored_value(serde_json::json!({"anchor": {"env": "ORB_L2_NEVER_SET"}, "path": "a"}))], &grants, &unplaced);
        assert_eq!(env_read.expect_err("unset").kind, OsErrorKind::NotFound, "edge: an unset env anchor names nothing");
    }

    #[test]
    fn a_computed_anchored_path_is_checked_against_the_narrow_glob_after_resolution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/app.lolo"), "app").unwrap();
        std::fs::write(root.join("notes.txt"), "no").unwrap();
        let handles = OsHandles {
            anchors: OsAnchors { start: Some(root.join("src")), ..OsAnchors::default() },
            ..OsHandles::default()
        };
        let declared = OsGrants {
            anchored: AnchoredGrants {
                fs: vec![AnchoredFsGrant { anchor: GrantAnchor::Cwd, glob: "**/*.lolo".into(), modes: vec![FsMode::Read] }],
                ..AnchoredGrants::default()
            },
            ..OsGrants::default()
        };
        let (grants, problems) = declared.resolve_anchors(&handles.anchors);
        assert!(problems.is_empty());
        let read = |path: &str| os_answer(OsOp::ReadFile, vec![anchored_value(serde_json::json!({"anchor": "cwd", "path": path}))], &grants, &handles);
        assert!(read("app.lolo").is_ok());
        assert!(read("./sub/../app.lolo").is_ok(), "edge: `..` that stays under the anchor collapses");
        std::fs::write(root.join("x.lolo"), "outside").unwrap();
        assert_eq!(read("../x.lolo").expect_err("climbs").kind, OsErrorKind::Denied, "edge: a `..` escape is refused before the glob");
        let wide = OsHandles { anchors: OsAnchors { start: Some(root.clone()), ..OsAnchors::default() }, ..OsHandles::default() };
        let (wide_grants, _) = declared.resolve_anchors(&wide.anchors);
        let wide_read = |path: &str| os_answer(OsOp::ReadFile, vec![anchored_value(serde_json::json!({"anchor": "cwd", "path": path}))], &wide_grants, &wide);
        assert!(wide_read("src/app.lolo").is_ok());
        assert_eq!(wide_read("notes.txt").expect_err("not lolo").kind, OsErrorKind::Denied, "control: outside the glob");
    }

    #[test]
    fn an_anchored_glob_answers_anchor_relative_paths_the_narrow_grant_covers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real");
        let work = root.join("work");
        for d in ["a", "b/c", "node_modules/x"] {
            std::fs::create_dir_all(work.join(d)).unwrap();
        }
        for f in ["package.json", "a/package.json", "b/c/package.json", "a/notes.txt"] {
            std::fs::write(work.join(f), "{}").unwrap();
        }
        std::fs::write(root.join("package.json"), "{}").unwrap();
        let handles = OsHandles { anchors: OsAnchors { start: Some(work.clone()), ..OsAnchors::default() }, ..OsHandles::default() };
        let declared = OsGrants {
            anchored: AnchoredGrants {
                fs: vec![AnchoredFsGrant { anchor: GrantAnchor::Cwd, glob: "**/package.json".into(), modes: vec![FsMode::Read] }],
                ..AnchoredGrants::default()
            },
            ..OsGrants::default()
        };
        let (grants, problems) = declared.resolve_anchors(&handles.anchors);
        assert!(problems.is_empty());
        let glob = |pattern: &str| os_answer(OsOp::Glob, vec![anchored_value(serde_json::json!({"anchor": "cwd", "path": pattern}))], &grants, &handles);
        let paths = |answer: Value| -> Vec<String> {
            let Value::Object(map) = answer else { panic!("{answer:?}") };
            let Some(Value::Array(items)) = map.get("paths") else { panic!("{map:?}") };
            items.iter().map(|v| match v { Value::String(s) => s.clone(), other => panic!("{other:?}") }).collect()
        };
        assert_eq!(paths(glob("*/package.json").expect("depth 1")), ["a/package.json"]);
        assert_eq!(paths(glob("**/*").expect("deep")), ["a/package.json", "b/c/package.json", "package.json"], "control: matches the grant does not cover are dropped");
        assert_eq!(paths(glob("*/*.txt").expect("ungranted")), Vec::<String>::new(), "control: nothing granted matches");
        assert_eq!(glob("../*").expect_err("climbs").kind, OsErrorKind::Denied, "edge: a glob climbing above the anchor");
        assert_eq!(glob("a/../../*").expect_err("climbs").kind, OsErrorKind::Denied, "edge: a `..` escape inside the pattern");
        let plain = os_answer(OsOp::Glob, vec![Value::String(format!("{}/*/package.json", work.display()))], &grants, &handles);
        assert_eq!(plain.expect_err("plain").kind, OsErrorKind::Denied, "control: a plain pattern still needs read on its prefix");
        let unset = os_answer(OsOp::Glob, vec![anchored_value(serde_json::json!({"anchor": {"env": "ORB_L2_NEVER_SET"}, "path": "*"}))], &grants, &OsHandles::default());
        assert_eq!(unset.expect_err("unset").kind, OsErrorKind::NotFound, "edge: an unset env anchor names nothing");
    }

    #[test]
    fn an_unset_or_empty_env_anchored_spawn_command_is_not_found() {
        let anchors = OsAnchors {
            exe_dir: Some("/opt/orb".into()),
            env: EnvSnapshot::from_pairs([("ORB_L19_EMPTY".to_string(), String::new()), ("ORB_L19_RELATIVE".to_string(), "bin/tool".to_string())]),
            ..OsAnchors::default()
        };
        let unset = anchored_value(serde_json::json!({"anchor": {"env": "ORB_L18_NEVER_SET"}}));
        let e = resolve_anchored_args(OsOp::Spawn, &[unset, Value::Array(Vec::new())], &anchors).expect_err("unset");
        assert_eq!(e.kind, OsErrorKind::NotFound);
        let empty = anchored_value(serde_json::json!({"anchor": {"env": "ORB_L19_EMPTY"}}));
        let e = resolve_anchored_args(OsOp::Spawn, &[empty, Value::Array(Vec::new())], &anchors).expect_err("empty");
        assert_eq!(e.kind, OsErrorKind::NotFound, "an empty value names nothing either");
        let relative = anchored_value(serde_json::json!({"anchor": {"env": "ORB_L19_RELATIVE"}}));
        let e = resolve_anchored_args(OsOp::Spawn, &[relative, Value::Array(Vec::new())], &anchors).expect_err("relative");
        assert_eq!(e.kind, OsErrorKind::Denied, "control: a relative value is refused");
        let beside = anchored_value(serde_json::json!({"anchor": "exe-dir", "path": "bun"}));
        let args = resolve_anchored_args(OsOp::Spawn, &[beside, Value::Array(Vec::new())], &anchors).expect("control: a set anchor resolves");
        assert_eq!(args[0], Value::String("/opt/orb/bun".into()));
    }

    #[test]
    fn an_env_anchored_port_is_the_boot_value_as_a_number() {
        let anchors = OsAnchors { env: EnvSnapshot::from_pairs([("PORT".to_string(), "8080".to_string()), ("BAD".to_string(), "x".to_string())]), ..OsAnchors::default() };
        let port = anchored_value(serde_json::json!({"anchor": {"env": "PORT"}}));
        let args = resolve_anchored_args(OsOp::TcpListen, &[Value::String("127.0.0.1".into()), port.clone()], &anchors).unwrap();
        assert_eq!(args[1], Value::Number(8080.0));
        assert_eq!(resolve_anchored_args(OsOp::Getenv, &[port.clone()], &anchors).unwrap()[0], port, "control: not a net or path position");
        let bad = anchored_value(serde_json::json!({"anchor": {"env": "BAD"}}));
        assert!(resolve_anchored_args(OsOp::TcpListen, &[Value::String("h".into()), bad], &anchors).is_err());
    }

    #[test]
    fn with_no_host_every_op_is_denied() {
        for op in OsOp::ALL {
            match answer_os(None, &ask(*op)) {
                IoAnswer::Os(Err(error)) => {
                    assert_eq!(error.kind, OsErrorKind::Denied, "{}", op.name());
                    assert!(error.message.contains("no os host configured"));
                    assert!(error.message.starts_with(op.name()));
                }
                other => panic!("{} answered {other:?}", op.name()),
            }
        }
    }

    #[test]
    fn a_request_that_is_not_an_os_ask_is_not_answered_as_one() {
        let request = IoRequest::Random { bytes: 1 };
        assert!(!matches!(answer_os(None, &request), IoAnswer::Os(_)));
        with_host(|host| assert!(!matches!(answer_os(Some(host), &request), IoAnswer::Os(_))));
    }

    #[test]
    fn every_op_belongs_to_exactly_one_family_in_contract_order() {
        let mut counts = [0usize; 6];
        for op in OsOp::ALL {
            let slot = match family_of(*op) {
                OsFamily::Fs => 0,
                OsFamily::Process => 1,
                OsFamily::Net => 2,
                OsFamily::Sys => 3,
                OsFamily::Kv => 4,
                OsFamily::Dev => 5,
            };
            counts[slot] += 1;
        }
        assert_eq!(counts.iter().sum::<usize>(), OsOp::ALL.len());
        assert_eq!(counts, [32, 18, 10, 31, 5, 4]);
    }

    #[test]
    fn family_membership_follows_the_contract() {
        for op in [
            OsOp::ReadFile,
            OsOp::Close,
            OsOp::WatchFiles,
            OsOp::Utimes,
            OsOp::Cwd,
        ] {
            assert_eq!(family_of(op), OsFamily::Fs, "{}", op.name());
        }
        for op in [
            OsOp::Spawn,
            OsOp::CurrentExe,
            OsOp::Getenv,
            OsOp::WatchEnv,
            OsOp::StdinRead,
            OsOp::TtySize,
        ] {
            assert_eq!(family_of(op), OsFamily::Process, "{}", op.name());
        }
        for op in [
            OsOp::TcpListen,
            OsOp::DnsResolve,
            OsOp::Shutdown,
            OsOp::WatchPort,
        ] {
            assert_eq!(family_of(op), OsFamily::Net, "{}", op.name());
        }
        for op in [
            OsOp::ClockMonotonic,
            OsOp::Username,
            OsOp::WatchCron,
            OsOp::Hostname,
            OsOp::Hibernate,
        ] {
            assert_eq!(family_of(op), OsFamily::Sys, "{}", op.name());
        }
    }

    #[test]
    fn a_hosted_process_answers_every_device_call_unsupported() {
        with_host(|host| {
            for (op, args) in [
                (OsOp::MmioRead, vec![Value::Number(150994944.0), Value::Number(24.0), Value::Number(4.0)]),
                (OsOp::MmioWrite, vec![Value::Number(150994944.0), Value::Number(0.0), Value::Number(1.0), Value::Number(65.0)]),
                (OsOp::IrqWait, vec![Value::Number(33.0)]),
                (OsOp::IrqAck, vec![Value::Number(33.0)]),
            ] {
                let request = IoRequest::Os { op, args, key: Some("k".into()), site: None };
                let IoAnswer::Os(Err(error)) = answer_os(Some(host), &request) else { panic!("{} must fail", op.name()) };
                assert_eq!(error.kind, OsErrorKind::Unsupported, "{}", op.name());
                assert!(error.message.contains("no devices"), "{}", error.message);
            }
        });
        let request = IoRequest::Os { op: OsOp::IrqWait, args: vec![Value::Number(33.0)], key: None, site: None };
        let IoAnswer::Os(Err(error)) = answer_os(None, &request) else { panic!("no host must deny") };
        assert_eq!(error.kind, OsErrorKind::Denied, "control: no host denies before any device question");
    }

    #[test]
    fn closing_an_unknown_handle_is_closed_not_unsupported() {
        with_host(|host| {
            let request = IoRequest::Os {
                op: OsOp::Close,
                args: vec![Value::Number(99.0)],
                key: None,
                site: None,
            };
            match answer_os(Some(host), &request) {
                IoAnswer::Os(Err(error)) => assert_eq!(error.kind, OsErrorKind::Closed),
                other => panic!("{other:?}"),
            }
        });
    }

    #[test]
    fn fds_and_sockets_draw_from_one_sequence_starting_after_stdio() {
        let handles = OsHandles::default();
        let a = handles.fs.fds.insert(fs::FdEntry::File(
            std::fs::File::open(concat!(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."), "/Cargo.toml")).expect("open"),
        ));
        let b = handles.net.socks.insert(net::SockEntry::Datagram(
            std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"),
        ));
        assert_eq!(a, FIRST_HANDLE_ID);
        assert_eq!(b, FIRST_HANDLE_ID + 1);
        assert!(handles.fs.fds.contains(a) && !handles.fs.fds.contains(b));
        assert!(handles.net.socks.contains(b) && !handles.net.socks.contains(a));
        handles.close_all();
        assert!(!handles.fs.fds.contains(a) && !handles.net.socks.contains(b));
    }

    #[test]
    fn a_slab_removes_and_reaches_entries_by_id() {
        let slab: HandleSlab<u8> = HandleSlab::new(Arc::new(AtomicI64::new(FIRST_HANDLE_ID)));
        let id = slab.insert(7);
        assert_eq!(slab.with(id, |v| *v), Some(7));
        assert_eq!(slab.with(id + 1, |v| *v), None);
        assert_eq!(slab.remove(id), Some(7));
        assert_eq!(slab.remove(id), None);
        assert_eq!(slab.insert(8), id + 1);
    }
}
