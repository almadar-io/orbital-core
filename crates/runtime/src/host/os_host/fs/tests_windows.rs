use super::super::args::bytes_value;
use super::super::{OsHandles, OsHost};
use super::*;
use orbital_core_base::os::OsErrorKind;
use orbital_core_base::os_grants::{FsGrant, FsMode, OsGrants};
use std::fs;
use std::path::{Path, PathBuf};

const ALL: [FsMode; 5] = [FsMode::Read, FsMode::Write, FsMode::Create, FsMode::Remove, FsMode::Meta];

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    grants: OsGrants,
    handles: OsHandles,
}

/// The grant glob naming everything under the drive-rooted `dir`: `/C:/x/y/**`.
fn glob_under(dir: &Path) -> String {
    let text = dir.to_string_lossy().replace('\\', "/");
    let mut chars = text.chars();
    let drive = chars.next().expect("drive letter").to_ascii_uppercase();
    format!("/{drive}{}/**", chars.as_str())
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("granted");
        fs::create_dir(&root).expect("granted dir");
        let grants = OsGrants { fs: vec![FsGrant { glob: glob_under(&root), modes: ALL.to_vec() }], ..OsGrants::default() };
        Self { _dir: dir, root, grants, handles: OsHandles::default() }
    }

    fn s(&self, rel: &str) -> Value {
        Value::string(self.root.join(rel).to_string_lossy().into_owned())
    }

    fn call(&self, op: OsOp, args: Vec<Value>) -> Result<Value, OsError> {
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let host = OsHost { grants: &self.grants, handles: &self.handles, on_event: &on_event, secrets: None };
        answer(op, &args, None, None, &host).unwrap_or_else(|| Err(OsError::new(OsErrorKind::Closed, "not an open handle")))
    }

    fn ok(&self, op: OsOp, args: Vec<Value>) -> Value {
        self.call(op, args).unwrap_or_else(|e| panic!("{} failed: {e:?}", op.name()))
    }

    fn kind(&self, op: OsOp, args: Vec<Value>) -> OsErrorKind {
        match self.call(op, args) {
            Err(e) => e.kind,
            Ok(v) => panic!("{} should fail, got {v:?}", op.name()),
        }
    }
}

fn st(text: &str) -> Value {
    Value::string(text.to_string())
}

fn b(text: &str) -> Value {
    bytes_value(text.as_bytes())
}

fn n(v: i64) -> Value {
    Value::Number(v as f64)
}

fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
    match value {
        Value::Object(map) => map.get(key).unwrap_or_else(|| panic!("no {key} in {value:?}")),
        other => panic!("{other:?}"),
    }
}

fn num_of(value: &Value) -> f64 {
    match value {
        Value::Number(n) => *n,
        other => panic!("{other:?}"),
    }
}

fn str_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone().into_inner(),
        other => panic!("{other:?}"),
    }
}

fn bool_of(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        other => panic!("{other:?}"),
    }
}

fn str_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone().into_inner(),
                other => panic!("{other:?}"),
            })
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_under_a_drive_rooted_grant_round_trips_and_its_sibling_is_denied() {
    let fx = Fx::new();
    fx.ok(OsOp::WriteFile, vec![fx.s("a.txt"), b("hello")]);
    assert_eq!(fs::read(fx.root.join("a.txt")).expect("written"), b"hello");
    fx.ok(OsOp::AppendFile, vec![fx.s("a.txt"), b(" world")]);
    assert_eq!(fs::read(fx.root.join("a.txt")).expect("appended"), b"hello world");
    fx.ok(OsOp::ReadFile, vec![fx.s("a.txt")]);
    let outside = fx.root.parent().expect("parent").join("outside.txt");
    let outside = Value::string(outside.to_string_lossy().into_owned());
    assert_eq!(fx.kind(OsOp::WriteFile, vec![outside, b("x")]), OsErrorKind::Denied);
    assert_eq!(fx.kind(OsOp::ReadFile, vec![fx.s(r"..\outside.txt")]), OsErrorKind::Denied, "`..` leaves the grant");
    assert_eq!(fx.kind(OsOp::ReadFile, vec![st(r"C:\..\..\x")]), OsErrorKind::Denied, "never above the drive");
    assert_eq!(fx.kind(OsOp::ReadFile, vec![st(r"\\server\share\x")]), OsErrorKind::Denied, "UNC");
}

#[test]
fn directories_are_made_listed_globbed_and_removed_under_the_grant() {
    let fx = Fx::new();
    fx.ok(OsOp::Mkdir, vec![fx.s(r"d\e"), Value::Bool(true)]);
    fx.ok(OsOp::WriteFile, vec![fx.s(r"d\one.txt"), b("1")]);
    fx.ok(OsOp::WriteFile, vec![fx.s(r"d\two.md"), b("2")]);
    let listed = fx.ok(OsOp::ListDir, vec![fx.s("d")]);
    let Value::Array(entries) = get(&listed, "entries") else { panic!("{listed:?}") };
    assert_eq!(entries.len(), 3);
    let globbed = str_list(get(&fx.ok(OsOp::Glob, vec![fx.s(r"d\*.txt")]), "paths"));
    assert_eq!(globbed.len(), 1, "{globbed:?}");
    assert!(globbed[0].ends_with("one.txt"), "{globbed:?}");
    assert_eq!(fx.kind(OsOp::Rmdir, vec![fx.s("d")]), OsErrorKind::Io, "not empty");
    fx.ok(OsOp::Rmdir, vec![fx.s("d"), Value::Bool(true)]);
    assert!(!fx.root.join("d").exists());
}

#[test]
fn stat_mode_and_access_follow_the_read_only_attribute() {
    let fx = Fx::new();
    fx.ok(OsOp::WriteFile, vec![fx.s("f.txt"), b("abc")]);
    let stat = fx.ok(OsOp::Stat, vec![fx.s("f.txt")]);
    assert_eq!(str_of(get(&stat, "kind")), "file");
    assert_eq!(num_of(get(&stat, "size")), 3.0);
    assert_eq!(num_of(get(&stat, "mode")), f64::from(0o666));
    assert_eq!(num_of(get(&stat, "uid")), 0.0);
    assert!(num_of(get(&stat, "ctime")) > 1.0e12, "a real change time: {stat:?}");
    assert!(bool_of(get(&fx.ok(OsOp::Access, vec![fx.s("f.txt"), st("write")]), "ok")));
    fx.ok(OsOp::Chmod, vec![fx.s("f.txt"), n(0o444)]);
    assert_eq!(num_of(get(&fx.ok(OsOp::Stat, vec![fx.s("f.txt")]), "mode")), f64::from(0o444));
    assert!(!bool_of(get(&fx.ok(OsOp::Access, vec![fx.s("f.txt"), st("write")]), "ok")));
    assert!(bool_of(get(&fx.ok(OsOp::Access, vec![fx.s("f.txt"), st("read")]), "ok")));
    assert!(!bool_of(get(&fx.ok(OsOp::Access, vec![fx.s("missing.txt")]), "ok")));
    fx.ok(OsOp::Chmod, vec![fx.s("f.txt"), n(0o666)]);
    assert!(bool_of(get(&fx.ok(OsOp::Access, vec![fx.s("f.txt"), st("write")]), "ok")));
    let dir = fx.ok(OsOp::Stat, vec![fx.s(".")]);
    assert_eq!((str_of(get(&dir, "kind")), num_of(get(&dir, "mode"))), ("dir".to_string(), f64::from(0o777)));
}

#[test]
fn chown_is_refused_and_utimes_sets_the_times() {
    let fx = Fx::new();
    fx.ok(OsOp::WriteFile, vec![fx.s("t.txt"), b("x")]);
    assert_eq!(fx.kind(OsOp::Chown, vec![fx.s("t.txt"), n(0), n(0)]), OsErrorKind::Unsupported);
    let at = 1_600_000_000_000i64;
    fx.ok(OsOp::Utimes, vec![fx.s("t.txt"), n(at), n(at + 5_000)]);
    let stat = fx.ok(OsOp::Stat, vec![fx.s("t.txt")]);
    assert_eq!(num_of(get(&stat, "mtime")), (at + 5_000) as f64);
}

#[test]
fn copy_rename_and_remove_move_files_within_the_grant() {
    let fx = Fx::new();
    fx.ok(OsOp::WriteFile, vec![fx.s("a.txt"), b("a")]);
    fx.ok(OsOp::Copy, vec![fx.s("a.txt"), fx.s("b.txt")]);
    fx.ok(OsOp::Rename, vec![fx.s("b.txt"), fx.s("c.txt")]);
    assert_eq!(fs::read(fx.root.join("c.txt")).expect("renamed"), b"a");
    assert!(!fx.root.join("b.txt").exists());
    fx.ok(OsOp::Remove, vec![fx.s("c.txt")]);
    assert!(!fx.root.join("c.txt").exists());
    assert_eq!(fx.kind(OsOp::Remove, vec![fx.s("c.txt")]), OsErrorKind::NotFound);
}

#[test]
fn a_descriptor_reads_and_writes_and_mkdir_with_a_read_only_mode_marks_only_what_it_made() {
    let fx = Fx::new();
    let fd = match get(&fx.ok(OsOp::Open, vec![fx.s("o.txt"), st("w+")]), "fd") {
        Value::Number(n) => *n as i64,
        other => panic!("{other:?}"),
    };
    fx.ok(OsOp::Write, vec![n(fd), b("xyz")]);
    fx.ok(OsOp::Seek, vec![n(fd), n(0), st("set")]);
    fx.ok(OsOp::Read, vec![n(fd), n(3)]);
    fx.ok(OsOp::Close, vec![n(fd)]);
    fx.ok(OsOp::WriteFile, vec![fx.s("ro.txt"), b("r"), n(0o444)]);
    assert!(fs::metadata(fx.root.join("ro.txt")).expect("made").permissions().readonly());
    fx.ok(OsOp::WriteFile, vec![fx.s("o.txt"), b("again"), n(0o444)]);
    assert!(!fs::metadata(fx.root.join("o.txt")).expect("kept").permissions().readonly(), "an existing file keeps its mode");
    fx.ok(OsOp::Chmod, vec![fx.s("ro.txt"), n(0o666)]);
}
