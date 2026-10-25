//! The `fs` family: files, descriptors, directories, stat, path effects and file watching.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::{HandleSlab, IdCounter, OsHost};
use crate::resumable::CallSite;
use orbital_core_base::os::{OsError, OsOp};
use orbital_core_base::value::Value;
use orbital_core_base::sync::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

/// What a descriptor id names: a file, or the end of a pipe to a spawned child (registered by `process`).
#[allow(dead_code)]
pub(super) enum FdEntry {
    File(std::fs::File),
    ChildStdin(Arc<Mutex<std::process::ChildStdin>>),
    ChildStdout(Arc<Mutex<std::process::ChildStdout>>),
    ChildStderr(Arc<Mutex<std::process::ChildStderr>>),
}

/// A pipe end cloned out of the table, so a blocking pipe call never holds the table lock.
enum PipeEnd {
    Stdin(Arc<Mutex<std::process::ChildStdin>>),
    Stdout(Arc<Mutex<std::process::ChildStdout>>),
    Stderr(Arc<Mutex<std::process::ChildStderr>>),
}

/// Open descriptors and the virtual working directory; ids come from the counter shared with the net table,
/// so an id names one handle.
#[derive(Clone)]
pub struct FsTable {
    #[allow(dead_code)]
    pub(super) fds: Arc<HandleSlab<FdEntry>>,
    #[allow(dead_code)]
    cwd: Arc<Mutex<Option<PathBuf>>>,
}

impl FsTable {
    pub(super) fn new(ids: IdCounter) -> Self {
        Self {
            fds: Arc::new(HandleSlab::new(ids)),
            cwd: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn clear(&self) {
        self.fds.clear();
        *self.cwd.lock() = None;
    }
}

pub(super) fn answer(
    op: OsOp,
    args: &[Value],
    _key: Option<&str>,
    _site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Option<Result<Value, OsError>> {
    #[cfg(unix)]
    {
        ops::answer(op, args, host)
    }
    #[cfg(not(unix))]
    {
        let _ = (args, host);
        unsupported(op)
    }
}

#[cfg(not(unix))]
fn unsupported(op: OsOp) -> Option<Result<Value, OsError>> {
    match op {
        OsOp::Close => None,
        _ => Some(Err(OsError::new(
            orbital_core_base::os::OsErrorKind::Unsupported,
            format!("{}: files are not supported on this platform", op.name()),
        ))),
    }
}

pub(super) fn later(op: OsOp, args: &[Value], key: &str, site: &CallSite, host: &OsHost<'_>) -> Result<(), OsError> {
    #[cfg(unix)]
    { ops::later(op, args, key, site, host) }
    #[cfg(not(unix))]
    {
        let (args, net) = (args.to_vec(), host.handles.net.clone());
        super::completion::Completions::run_later(&host.handles.completions, key, site, move || {
            let answered = match (unsupported(op), op) {
                (None, OsOp::Close) => super::net::close(&args, &net),
                (answered, _) => answered,
            };
            super::finish_answer(op, answered)
        }).map_err(|error| super::net::later_error(op, error, key))
    }
}

/// `os/read` under a completion route: the read runs on a thread of its own and the call returns at once, so two
/// reads can be outstanding together (EXEC-QUEUE-10). A regular file's bytes are taken now but still answer through
/// the completion, as a dispatch of their own carrying `{ data, eof, key }`.
pub(super) fn read_later(
    args: &[Value],
    key: &str,
    site: &CallSite,
    host: &OsHost<'_>,
) -> Result<(), OsError> {
    #[cfg(unix)]
    {
        ops::read_later(args, key, site, host)
    }
    #[cfg(not(unix))]
    {
        let _ = (args, key, site, host);
        Err(OsError::new(
            orbital_core_base::os::OsErrorKind::Unsupported,
            "os/read: files are not supported on this platform",
        ))
    }
}

#[cfg(unix)]
mod ops {
    use super::super::args::{arg_bool, arg_bytes, arg_i64, arg_opt, arg_str, bytes_value};
    use super::super::completion::Completions;
    use crate::resumable::CallSite;
    use super::super::{OsHost, FIRST_HANDLE_ID};
    use super::{FdEntry, FsTable, PipeEnd};
    use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
    use orbital_core_base::os_grants::{AnchoredPath, FsMode, GrantAnchor, OsAnchors, OsGrants};
    use orbital_core_base::value::{Value, ValueMap};
    use std::collections::VecDeque;
    use std::ffi::{CString, OsString};
    use std::fs;
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const MAX_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_LINKS: u32 = 40;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    pub(super) fn answer(
        op: OsOp,
        args: &[Value],
        host: &OsHost<'_>,
    ) -> Option<Result<Value, OsError>> {
        answer_context(&Cx::new(op, host), args)
    }

    fn answer_context(cx: &Cx<'_>, args: &[Value]) -> Option<Result<Value, OsError>> {
        let op = cx.op;
        if op == OsOp::Close {
            let fd = match arg_i64(args, 0, op) {
                Ok(fd) => fd,
                Err(e) => return Some(Err(e)),
            };
            if fd < FIRST_HANDLE_ID || !cx.fs.fds.contains(fd) {
                return None;
            }
            cx.fs.fds.remove(fd);
            return Some(Ok(obj([])));
        }
        Some(cx.run(args))
    }

    fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
        Value::Object(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<ValueMap>(),
        )
    }

    fn num(n: u64) -> Value {
        Value::Number(n as f64)
    }

    fn text(path: &Path) -> Value {
        Value::String(path.to_string_lossy().into_owned())
    }

    fn lexical(path: &Path) -> Option<PathBuf> {
        let mut out = PathBuf::from("/");
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        return None;
                    }
                }
                Component::Normal(part) => out.push(part),
                Component::Prefix(_) => return None,
            }
        }
        Some(out)
    }

    fn resolve(path: &Path, follow_last: bool) -> io::Result<PathBuf> {
        let mut queue: VecDeque<OsString> = VecDeque::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => queue.push_back(part.to_os_string()),
                Component::ParentDir => queue.push_back("..".into()),
                _ => {}
            }
        }
        let mut cur = PathBuf::from("/");
        let mut links = 0u32;
        while let Some(part) = queue.pop_front() {
            if part == ".." {
                cur.pop();
                continue;
            }
            let next = cur.join(&part);
            match fs::symlink_metadata(&next) {
                Ok(md) if md.file_type().is_symlink() && (follow_last || !queue.is_empty()) => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(io::Error::from_raw_os_error(libc::ELOOP));
                    }
                    let target = fs::read_link(&next)?;
                    if target.is_absolute() {
                        cur = PathBuf::from("/");
                    }
                    for component in target.components().rev() {
                        match component {
                            Component::Normal(p) => queue.push_front(p.to_os_string()),
                            Component::ParentDir => queue.push_front("..".into()),
                            _ => {}
                        }
                    }
                }
                Ok(_) => cur = next,
                Err(e) if e.kind() == io::ErrorKind::NotFound => cur = next,
                Err(e) => return Err(e),
            }
        }
        Ok(cur)
    }

    fn kind_of(file_type: fs::FileType) -> &'static str {
        if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        }
    }

    fn millis(sec: i64, nsec: i64) -> Value {
        Value::Number(sec as f64 * 1000.0 + nsec as f64 / 1_000_000.0)
    }

    fn stat_value(md: &fs::Metadata) -> Value {
        obj([
            ("kind", Value::String(kind_of(md.file_type()).to_string())),
            ("size", num(md.len())),
            ("mode", num(u64::from(md.mode() & 0o7777))),
            ("uid", num(u64::from(md.uid()))),
            ("gid", num(u64::from(md.gid()))),
            ("atime", millis(md.atime(), md.atime_nsec())),
            ("mtime", millis(md.mtime(), md.mtime_nsec())),
            ("ctime", millis(md.ctime(), md.ctime_nsec())),
        ])
    }

    struct Located {
        norm: PathBuf,
        real: PathBuf,
    }

    struct Flags {
        read: bool,
        write: bool,
        append: bool,
        create: bool,
        truncate: bool,
        exclusive: bool,
    }

    fn io_os_error(op: OsOp, path: &Path, e: &io::Error) -> OsError {
        let kind = match e.kind() {
            io::ErrorKind::NotFound => OsErrorKind::NotFound,
            io::ErrorKind::AlreadyExists => OsErrorKind::Exists,
            io::ErrorKind::PermissionDenied => OsErrorKind::Denied,
            io::ErrorKind::TimedOut => OsErrorKind::Timeout,
            io::ErrorKind::InvalidInput
            | io::ErrorKind::InvalidData
            | io::ErrorKind::IsADirectory
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::InvalidFilename => OsErrorKind::Invalid,
            _ => match e.raw_os_error() {
                Some(code)
                    if code == libc::ELOOP
                        || code == libc::EBADF
                        || code == libc::ENAMETOOLONG =>
                {
                    OsErrorKind::Invalid
                }
                _ => OsErrorKind::Io,
            },
        };
        OsError::new(kind, format!("{}: {}: {e}", op.name(), path.display()))
    }

    /// The blocking half of `os/read`: the pipe read (a file's bytes are already in `buf`), shaped as the reply.
    fn finish_read(op: OsOp, count: u64, pipe: Option<PipeEnd>, mut buf: Vec<u8>) -> Result<Value, OsError> {
        match pipe {
            Some(PipeEnd::Stdout(pipe)) => pipe_read(&mut *pipe.lock(), count, &mut buf),
            Some(PipeEnd::Stderr(pipe)) => pipe_read(&mut *pipe.lock(), count, &mut buf),
            _ => Ok(()),
        }
        .map_err(|e| io_os_error(op, Path::new("fd"), &e))?;
        let eof = count > 0 && buf.is_empty();
        Ok(obj([("data", bytes_value(&buf)), ("eof", Value::Bool(eof))]))
    }

    pub(super) fn read_later(
        args: &[Value],
        key: &str,
        site: &CallSite,
        host: &OsHost<'_>,
    ) -> Result<(), OsError> {
        let cx = Cx::new(OsOp::Read, host);
        let (count, pipe, buf) = cx.read_source(args)?;
        Completions::run_later(&host.handles.completions, key, site, move || {
            finish_read(OsOp::Read, count, pipe, buf)
        })
        .map_err(|e| super::super::net::later_error(OsOp::Read, e, key))
    }

    struct OpenedFile {
        table: Arc<super::super::HandleSlab<FdEntry>>,
        fd: Option<i64>,
    }

    impl OpenedFile {
        fn release(mut self) -> Value {
            let fd = self.fd.take().expect("owned Open allocation");
            obj([("fd", Value::Number(fd as f64))])
        }
    }

    impl Drop for OpenedFile {
        fn drop(&mut self) {
            if let Some(fd) = self.fd.take() { self.table.remove(fd); }
        }
    }

    enum FsCompletion {
        Answer(Result<Value, OsError>),
        Open(Result<OpenedFile, OsError>),
    }

    pub(super) fn later(
        op: OsOp,
        args: &[Value],
        key: &str,
        site: &CallSite,
        host: &OsHost<'_>,
    ) -> Result<(), OsError> {
        let (fs, net) = (host.handles.fs.clone(), host.handles.net.clone());
        let (grants, anchors) = (host.grants.clone(), host.handles.anchors.clone());
        let cwd = fs.cwd.lock().clone();
        let args = args.to_vec();
        Completions::spawn_later(&host.handles.completions, key, site, move |_| {
            let cx = Cx { op, grants: &grants, anchors: &anchors, fs: &fs, cwd };
            if op == OsOp::Open {
                FsCompletion::Open(cx.open(&args))
            } else {
                let answered = answer_context(&cx, &args);
                let answered = match (answered, op) {
                    (None, OsOp::Close) => super::super::net::close(&args, &net),
                    (answered, _) => answered,
                };
                FsCompletion::Answer(super::super::finish_answer(op, answered))
            }
        }, |completions, key, site, outcome| {
            let result = match outcome {
                FsCompletion::Answer(result) => result,
                FsCompletion::Open(result) => result.map(OpenedFile::release),
            };
            completions.deliver(key, site, result);
        }).map_err(|error| super::super::net::later_error(op, error, key))
    }

    struct Cx<'a> {
        op: OsOp,
        grants: &'a OsGrants,
        anchors: &'a OsAnchors,
        fs: &'a FsTable,
        cwd: Option<PathBuf>,
    }

    impl<'a> Cx<'a> {
        fn new(op: OsOp, host: &'a OsHost<'_>) -> Self {
            Self { op, grants: host.grants, anchors: &host.handles.anchors,
                fs: &host.handles.fs, cwd: host.handles.fs.cwd.lock().clone() }
        }

        fn err(&self, kind: OsErrorKind, what: impl std::fmt::Display) -> OsError {
            OsError::new(kind, format!("{}: {what}", self.op.name()))
        }

        fn io(&self, path: &Path, e: &io::Error) -> OsError {
            io_os_error(self.op, path, e)
        }

        fn fd_io(&self, e: &io::Error) -> OsError {
            self.io(Path::new("fd"), e)
        }

        /// The absolute path `raw` names: as written, a `~/` or `./` form under its anchor, or a bare relative path
        /// under the working directory `os/chdir` set.
        fn absolute(&self, raw: &str) -> Result<PathBuf, OsError> {
            if let Some(anchored) = self.anchors.expand(raw).map_err(|m| self.err(OsErrorKind::Invalid, m))? {
                return Ok(anchored);
            }
            let given = Path::new(raw);
            if given.is_absolute() {
                return Ok(given.to_path_buf());
            }
            match self.cwd.as_ref() {
                Some(cwd) => Ok(cwd.join(given)),
                None => Err(self.err(
                    OsErrorKind::Invalid,
                    format!("{raw} is relative and no working directory is set (os/chdir)"),
                )),
            }
        }

        fn allow(&self, path: &Path, mode: FsMode) -> Result<(), OsError> {
            if self.grants.fs_allows_at(path, mode, self.anchors) {
                Ok(())
            } else {
                Err(self.err(
                    OsErrorKind::Denied,
                    format!("{} is not granted for {mode:?}", path.display()),
                ))
            }
        }

        fn locate(&self, raw: &str, mode: FsMode, follow_last: bool) -> Result<Located, OsError> {
            if raw.is_empty() {
                return Err(self.err(OsErrorKind::Invalid, "empty path"));
            }
            let absolute = self.absolute(raw)?;
            let norm = lexical(&absolute).ok_or_else(|| {
                self.err(OsErrorKind::Denied, format!("{raw} climbs above the root"))
            })?;
            self.allow(&norm, mode)?;
            let real = resolve(&norm, follow_last).map_err(|e| self.io(&norm, &e))?;
            if real != norm {
                self.allow(&real, mode).map_err(|_| {
                    self.err(
                        OsErrorKind::Denied,
                        format!(
                            "{} resolves to {}, outside the grant",
                            norm.display(),
                            real.display()
                        ),
                    )
                })?;
            }
            Ok(Located { norm, real })
        }

        fn require(&self, at: &Located, mode: FsMode) -> Result<(), OsError> {
            self.allow(&at.norm, mode)?;
            if at.real != at.norm {
                self.allow(&at.real, mode)?;
            }
            Ok(())
        }

        fn path(&self, raw: &str, mode: FsMode, follow_last: bool) -> Result<PathBuf, OsError> {
            Ok(self.locate(raw, mode, follow_last)?.real)
        }

        fn missing(&self, at: &Located) -> Result<bool, OsError> {
            match fs::symlink_metadata(&at.real) {
                Ok(_) => Ok(false),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(e) => Err(self.io(&at.real, &e)),
            }
        }

        fn write_target(&self, raw: &str) -> Result<PathBuf, OsError> {
            let at = self.locate(raw, FsMode::Write, true)?;
            if self.missing(&at)? {
                self.require(&at, FsMode::Create)?;
            }
            Ok(at.real)
        }

        fn fd_arg(&self, args: &[Value], i: usize) -> Result<i64, OsError> {
            arg_i64(args, i, self.op)
        }

        fn with_fd<R>(
            &self,
            fd: i64,
            f: impl FnOnce(&mut FdEntry) -> Result<R, OsError>,
        ) -> Result<R, OsError> {
            self.fs.fds.with(fd, f).unwrap_or_else(|| {
                Err(self.err(OsErrorKind::Closed, format!("fd {fd} is not open")))
            })
        }

        fn with_file<R>(
            &self,
            fd: i64,
            f: impl FnOnce(&mut fs::File) -> Result<R, OsError>,
        ) -> Result<R, OsError> {
            self.with_fd(fd, |entry| match entry {
                FdEntry::File(file) => f(file),
                _ => Err(self.err(
                    OsErrorKind::Invalid,
                    format!("fd {fd} is a pipe, not a file"),
                )),
            })
        }

        fn mode_arg(&self, args: &[Value], i: usize) -> Result<Option<u32>, OsError> {
            match arg_opt(args, i) {
                None => Ok(None),
                Some(_) => {
                    let mode = arg_i64(args, i, self.op)?;
                    u32::try_from(mode)
                        .ok()
                        .filter(|m| *m <= 0o7777)
                        .map(Some)
                        .ok_or_else(|| {
                            self.err(OsErrorKind::Invalid, "mode must be within 0..=0o7777")
                        })
                }
            }
        }

        fn run(&self, args: &[Value]) -> Result<Value, OsError> {
            match self.op {
                OsOp::ReadFile => self.read_file(args),
                OsOp::WriteFile => self.write_file(args, false),
                OsOp::AppendFile => self.write_file(args, true),
                OsOp::Open => self.open(args).map(OpenedFile::release),
                OsOp::Close => Ok(obj([])),
                OsOp::Read => self.read(args),
                OsOp::Write => self.write(args),
                OsOp::Seek => self.seek(args),
                OsOp::Truncate => self.truncate(args),
                OsOp::Fsync => {
                    let fd = self.fd_arg(args, 0)?;
                    self.with_file(fd, |f| f.sync_all().map_err(|e| self.fd_io(&e)))?;
                    Ok(obj([]))
                }
                OsOp::Copy => self.copy(args),
                OsOp::Rename => self.rename(args),
                OsOp::Remove => self.remove(args),
                OsOp::Link => self.link(args),
                OsOp::Symlink => self.symlink(args),
                OsOp::Readlink => {
                    let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, false)?;
                    let target = fs::read_link(&path).map_err(|e| self.io(&path, &e))?;
                    Ok(obj([("target", text(&target))]))
                }
                OsOp::WatchFiles => {
                    Err(self.err(OsErrorKind::Unsupported, "streaming host not implemented"))
                }
                OsOp::Mkdir => self.mkdir(args),
                OsOp::Rmdir => self.rmdir(args),
                OsOp::ListDir => self.list_dir(args),
                OsOp::Glob => self.glob(args),
                OsOp::Mktemp => self.mktemp(args),
                OsOp::Realpath => {
                    let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
                    fs::symlink_metadata(&path).map_err(|e| self.io(&path, &e))?;
                    Ok(obj([("path", text(&path))]))
                }
                OsOp::Cwd => match self.fs.cwd.lock().as_ref() {
                    Some(cwd) => Ok(obj([("path", text(cwd))])),
                    None => Err(self.err(
                        OsErrorKind::Invalid,
                        "no working directory is set (os/chdir)",
                    )),
                },
                OsOp::Chdir => {
                    let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
                    let md = fs::metadata(&path).map_err(|e| self.io(&path, &e))?;
                    if !md.is_dir() {
                        return Err(self.err(
                            OsErrorKind::Invalid,
                            format!("{} is not a directory", path.display()),
                        ));
                    }
                    *self.fs.cwd.lock() = Some(path);
                    Ok(obj([]))
                }
                OsOp::Stat | OsOp::Lstat => {
                    let follow = self.op == OsOp::Stat;
                    let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, follow)?;
                    let md = if follow {
                        fs::metadata(&path)
                    } else {
                        fs::symlink_metadata(&path)
                    }
                    .map_err(|e| self.io(&path, &e))?;
                    Ok(stat_value(&md))
                }
                OsOp::Access => self.access(args),
                OsOp::Exists => {
                    let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
                    Ok(obj([("ok", Value::Bool(fs::metadata(&path).is_ok()))]))
                }
                OsOp::Chmod => {
                    let path = self.path(arg_str(args, 0, self.op)?, FsMode::Meta, true)?;
                    let mode = self
                        .mode_arg(args, 1)?
                        .ok_or_else(|| self.err(OsErrorKind::Invalid, "missing mode"))?;
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                        .map_err(|e| self.io(&path, &e))?;
                    Ok(obj([]))
                }
                OsOp::Chown => self.chown(args),
                OsOp::Utimes => self.utimes(args),
                _ => Err(self.err(OsErrorKind::Unsupported, "not a file operation")),
            }
        }

        fn read_file(&self, args: &[Value]) -> Result<Value, OsError> {
            let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
            let file = fs::File::open(&path).map_err(|e| self.io(&path, &e))?;
            let mut buf = Vec::new();
            file.take(MAX_BYTES + 1)
                .read_to_end(&mut buf)
                .map_err(|e| self.io(&path, &e))?;
            if buf.len() as u64 > MAX_BYTES {
                return Err(self.err(
                    OsErrorKind::Invalid,
                    format!(
                        "{} exceeds the {MAX_BYTES}-byte (64 MiB) per-call cap; use os/open and os/read",
                        path.display()
                    ),
                ));
            }
            Ok(obj([("data", bytes_value(&buf))]))
        }

        fn write_file(&self, args: &[Value], append: bool) -> Result<Value, OsError> {
            let path = self.write_target(arg_str(args, 0, self.op)?)?;
            let data = arg_bytes(args, 1, self.op)?;
            let mode = self.mode_arg(args, 2)?;
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true);
            if append {
                options.append(true);
            } else {
                options.truncate(true);
            }
            if let Some(mode) = mode {
                options.mode(mode);
            }
            let mut file = options.open(&path).map_err(|e| self.io(&path, &e))?;
            file.write_all(&data).map_err(|e| self.io(&path, &e))?;
            Ok(obj([("bytes", num(data.len() as u64))]))
        }

        fn parse_flags(&self, flags: &str) -> Result<Flags, OsError> {
            let (base, exclusive) = match flags.strip_suffix('x') {
                Some(base) => (base, true),
                None => (flags, false),
            };
            let (read, write, append, create, truncate) = match base {
                "r" => (true, false, false, false, false),
                "r+" => (true, true, false, false, false),
                "w" => (false, true, false, true, true),
                "w+" => (true, true, false, true, true),
                "a" => (false, true, true, true, false),
                "a+" => (true, true, true, true, false),
                _ => {
                    return Err(self.err(
                        OsErrorKind::Invalid,
                        format!(
                        "flags {flags:?} must be one of r r+ w w+ a a+, optionally followed by x"
                    ),
                    ))
                }
            };
            if exclusive && !create {
                return Err(self.err(OsErrorKind::Invalid, "the x flag needs w or a"));
            }
            Ok(Flags {
                read,
                write,
                append,
                create,
                truncate,
                exclusive,
            })
        }

        fn open(&self, args: &[Value]) -> Result<OpenedFile, OsError> {
            let raw = arg_str(args, 0, self.op)?;
            let flags = self.parse_flags(arg_str(args, 1, self.op)?)?;
            let mode = self.mode_arg(args, 2)?;
            let first = if flags.write {
                FsMode::Write
            } else {
                FsMode::Read
            };
            let at = self.locate(raw, first, true)?;
            if flags.read && flags.write {
                self.require(&at, FsMode::Read)?;
            }
            if flags.create && (flags.exclusive || self.missing(&at)?) {
                self.require(&at, FsMode::Create)?;
            }
            let mut options = fs::OpenOptions::new();
            options
                .read(flags.read)
                .write(flags.write && !flags.append)
                .append(flags.append)
                .truncate(flags.truncate);
            if flags.exclusive {
                options.create_new(true);
            } else {
                options.create(flags.create);
            }
            if let Some(mode) = mode {
                options.mode(mode);
            }
            let file = options.open(&at.real).map_err(|e| self.io(&at.real, &e))?;
            let fd = self.fs.fds.insert(FdEntry::File(file));
            Ok(OpenedFile { table: self.fs.fds.clone(), fd: Some(fd) })
        }

        fn read(&self, args: &[Value]) -> Result<Value, OsError> {
            let (count, pipe, buf) = self.read_source(args)?;
            finish_read(self.op, count, pipe, buf)
        }

        /// The count, and what is left to read: a file is read now (it does not block), a pipe end is handed back
        /// for the blocking read.
        fn read_source(&self, args: &[Value]) -> Result<(u64, Option<PipeEnd>, Vec<u8>), OsError> {
            let fd = self.fd_arg(args, 0)?;
            let count = arg_i64(args, 1, self.op)?;
            let count = u64::try_from(count)
                .ok()
                .filter(|c| *c <= MAX_BYTES)
                .ok_or_else(|| {
                    self.err(
                        OsErrorKind::Invalid,
                        format!("count must be within 0..={MAX_BYTES} (64 MiB per-call cap)"),
                    )
                })?;
            let mut buf = Vec::new();
            let pipe = self.with_fd(fd, |entry| match entry {
                FdEntry::File(file) => file
                    .take(count)
                    .read_to_end(&mut buf)
                    .map(|_| None)
                    .map_err(|e| self.fd_io(&e)),
                FdEntry::ChildStdout(pipe) => Ok(Some(PipeEnd::Stdout(Arc::clone(pipe)))),
                FdEntry::ChildStderr(pipe) => Ok(Some(PipeEnd::Stderr(Arc::clone(pipe)))),
                FdEntry::ChildStdin(_) => {
                    Err(self.err(OsErrorKind::Invalid, format!("fd {fd} is write-only")))
                }
            })?;
            Ok((count, pipe, buf))
        }

        fn write(&self, args: &[Value]) -> Result<Value, OsError> {
            let fd = self.fd_arg(args, 0)?;
            let data = arg_bytes(args, 1, self.op)?;
            let pipe = self.with_fd(fd, |entry| match entry {
                FdEntry::File(file) => file
                    .write_all(&data)
                    .map(|_| None)
                    .map_err(|e| self.fd_io(&e)),
                FdEntry::ChildStdin(pipe) => Ok(Some(PipeEnd::Stdin(Arc::clone(pipe)))),
                _ => Err(self.err(OsErrorKind::Invalid, format!("fd {fd} is read-only"))),
            })?;
            if let Some(PipeEnd::Stdin(pipe)) = pipe {
                let mut pipe = pipe.lock();
                pipe.write_all(&data)
                    .and_then(|_| pipe.flush())
                    .map_err(|e| self.fd_io(&e))?;
            }
            Ok(obj([("bytes", num(data.len() as u64))]))
        }

        fn seek(&self, args: &[Value]) -> Result<Value, OsError> {
            let fd = self.fd_arg(args, 0)?;
            let offset = arg_i64(args, 1, self.op)?;
            let whence = arg_str(args, 2, self.op)?;
            let target = match whence {
                "set" => SeekFrom::Start(u64::try_from(offset).map_err(|_| {
                    self.err(OsErrorKind::Invalid, "a set offset cannot be negative")
                })?),
                "cur" => SeekFrom::Current(offset),
                "end" => SeekFrom::End(offset),
                other => {
                    return Err(self.err(
                        OsErrorKind::Invalid,
                        format!("whence {other:?} must be set, cur or end"),
                    ))
                }
            };
            let at = self.with_file(fd, |f| f.seek(target).map_err(|e| self.fd_io(&e)))?;
            Ok(obj([("offset", num(at))]))
        }

        fn truncate(&self, args: &[Value]) -> Result<Value, OsError> {
            let size = arg_i64(args, 1, self.op)?;
            let size = u64::try_from(size)
                .map_err(|_| self.err(OsErrorKind::Invalid, "size cannot be negative"))?;
            match args.first() {
                Some(Value::Number(_)) => {
                    let fd = self.fd_arg(args, 0)?;
                    self.with_file(fd, |f| f.set_len(size).map_err(|e| self.fd_io(&e)))?;
                }
                _ => {
                    let path = self.path(arg_str(args, 0, self.op)?, FsMode::Write, true)?;
                    let file = fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .map_err(|e| self.io(&path, &e))?;
                    file.set_len(size).map_err(|e| self.io(&path, &e))?;
                }
            }
            Ok(obj([]))
        }

        fn copy(&self, args: &[Value]) -> Result<Value, OsError> {
            let from = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
            let to = self.write_target(arg_str(args, 1, self.op)?)?;
            fs::copy(&from, &to).map_err(|e| self.io(&from, &e))?;
            Ok(obj([]))
        }

        fn rename(&self, args: &[Value]) -> Result<Value, OsError> {
            let from = self.path(arg_str(args, 0, self.op)?, FsMode::Remove, false)?;
            let to = self.locate(arg_str(args, 1, self.op)?, FsMode::Create, false)?;
            if !self.missing(&to)? {
                self.require(&to, FsMode::Remove)?;
            }
            fs::rename(&from, &to.real).map_err(|e| self.io(&from, &e))?;
            Ok(obj([]))
        }

        fn remove(&self, args: &[Value]) -> Result<Value, OsError> {
            let path = self.path(arg_str(args, 0, self.op)?, FsMode::Remove, false)?;
            let md = fs::symlink_metadata(&path).map_err(|e| self.io(&path, &e))?;
            if md.is_dir() {
                return Err(self.err(
                    OsErrorKind::Invalid,
                    format!("{} is a directory; use os/rmdir", path.display()),
                ));
            }
            fs::remove_file(&path).map_err(|e| self.io(&path, &e))?;
            Ok(obj([]))
        }

        fn link(&self, args: &[Value]) -> Result<Value, OsError> {
            let target = self.path(arg_str(args, 0, self.op)?, FsMode::Write, true)?;
            let path = self.path(arg_str(args, 1, self.op)?, FsMode::Create, false)?;
            fs::hard_link(&target, &path).map_err(|e| self.io(&path, &e))?;
            Ok(obj([]))
        }

        fn symlink(&self, args: &[Value]) -> Result<Value, OsError> {
            let target = arg_str(args, 0, self.op)?;
            let path = self.path(arg_str(args, 1, self.op)?, FsMode::Create, false)?;
            std::os::unix::fs::symlink(target, &path).map_err(|e| self.io(&path, &e))?;
            Ok(obj([]))
        }

        fn mkdir(&self, args: &[Value]) -> Result<Value, OsError> {
            let at = self.locate(arg_str(args, 0, self.op)?, FsMode::Create, false)?;
            let recursive = match arg_opt(args, 1) {
                Some(_) => arg_bool(args, 1, self.op)?,
                None => false,
            };
            let mode = self.mode_arg(args, 2)?;
            if recursive {
                for ancestor in at.real.ancestors() {
                    if fs::symlink_metadata(ancestor).is_err() {
                        self.allow(ancestor, FsMode::Create)?;
                    }
                }
            }
            let mut builder = fs::DirBuilder::new();
            builder.recursive(recursive);
            if let Some(mode) = mode {
                builder.mode(mode);
            }
            builder
                .create(&at.real)
                .map_err(|e| self.io(&at.real, &e))?;
            Ok(obj([]))
        }

        fn require_tree_removable(&self, dir: &Path) -> Result<(), OsError> {
            for entry in fs::read_dir(dir).map_err(|e| self.io(dir, &e))? {
                let entry = entry.map_err(|e| self.io(dir, &e))?;
                let path = entry.path();
                self.allow(&path, FsMode::Remove)?;
                if entry.file_type().map_err(|e| self.io(&path, &e))?.is_dir() {
                    self.require_tree_removable(&path)?;
                }
            }
            Ok(())
        }

        fn rmdir(&self, args: &[Value]) -> Result<Value, OsError> {
            let path = self.path(arg_str(args, 0, self.op)?, FsMode::Remove, false)?;
            let recursive = match arg_opt(args, 1) {
                Some(_) => arg_bool(args, 1, self.op)?,
                None => false,
            };
            let md = fs::symlink_metadata(&path).map_err(|e| self.io(&path, &e))?;
            if !md.is_dir() {
                return Err(self.err(
                    OsErrorKind::Invalid,
                    format!("{} is not a directory; use os/remove", path.display()),
                ));
            }
            if recursive {
                self.require_tree_removable(&path)?;
                fs::remove_dir_all(&path)
            } else {
                fs::remove_dir(&path)
            }
            .map_err(|e| self.io(&path, &e))?;
            Ok(obj([]))
        }

        fn list_dir(&self, args: &[Value]) -> Result<Value, OsError> {
            let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
            let mut entries: Vec<(String, &'static str)> = Vec::new();
            for entry in fs::read_dir(&path).map_err(|e| self.io(&path, &e))? {
                let entry = entry.map_err(|e| self.io(&path, &e))?;
                let file_type = entry.file_type().map_err(|e| self.io(&path, &e))?;
                entries.push((
                    entry.file_name().to_string_lossy().into_owned(),
                    kind_of(file_type),
                ));
            }
            entries.sort();
            let entries = entries
                .into_iter()
                .map(|(name, kind)| {
                    obj([
                        ("name", Value::String(name)),
                        ("kind", Value::String(kind.to_string())),
                    ])
                })
                .collect();
            Ok(obj([("entries", Value::Array(entries))]))
        }

        fn glob(&self, args: &[Value]) -> Result<Value, OsError> {
            if let Some(anchored) = args.first().and_then(AnchoredPath::from_value) {
                return self.anchored_glob(&anchored);
            }
            let raw = arg_str(args, 0, self.op)?;
            if raw.is_empty() {
                return Err(self.err(OsErrorKind::Invalid, "empty pattern"));
            }
            let absolute = self.absolute(raw)?;
            let pattern = lexical(&absolute).ok_or_else(|| {
                self.err(OsErrorKind::Denied, format!("{raw} climbs above the root"))
            })?;
            let found = self.expand(&pattern, true)?;
            Ok(obj([(
                "paths",
                Value::Array(found.into_iter().map(Value::String).collect()),
            )]))
        }

        /// An anchored pattern matches under the boot-resolved anchor and answers anchor-relative paths. Its prefix
        /// need not be granted: a narrow anchored grant covers it, and every match is checked against the grants.
        fn anchored_glob(&self, anchored: &AnchoredPath) -> Result<Value, OsError> {
            let anchors = self.anchors;
            if anchored.path.as_deref().is_none_or(str::is_empty) {
                return Err(self.err(OsErrorKind::Invalid, "empty pattern"));
            }
            if let GrantAnchor::Env(name) = &anchored.anchor {
                if anchors.env.get(name).is_none_or(str::is_empty) {
                    return Err(self.err(OsErrorKind::NotFound, format!("env {name} is unset")));
                }
            }
            let denied = |why: String| self.err(OsErrorKind::Denied, why);
            let root = anchors
                .resolve_path(&AnchoredPath { anchor: anchored.anchor.clone(), path: None })
                .map_err(denied)?;
            let pattern = anchors.resolve_path(anchored).map_err(denied)?;
            let root = PathBuf::from(root);
            let found = self.expand(Path::new(&pattern), false)?;
            let relative = found
                .into_iter()
                .filter_map(|p| {
                    let rel = Path::new(&p).strip_prefix(&root).ok()?.to_string_lossy().into_owned();
                    Some(Value::String(if rel.is_empty() { ".".to_string() } else { rel }))
                })
                .collect();
            Ok(obj([("paths", Value::Array(relative))]))
        }

        fn expand(&self, pattern: &Path, check_prefix: bool) -> Result<Vec<String>, OsError> {
            let mut prefix = PathBuf::from("/");
            let mut rest = 0usize;
            let mut wild = false;
            let mut deep = false;
            for component in pattern.components() {
                if let Component::Normal(part) = component {
                    let part = part.to_string_lossy();
                    if wild || part.contains(['*', '?']) {
                        wild = true;
                        rest += 1;
                        deep |= part == "**";
                    } else {
                        prefix.push(&*part);
                    }
                }
            }
            if check_prefix {
                self.allow(&prefix, FsMode::Read)?;
            }
            let matcher = OsGrants::read_matcher(&pattern.to_string_lossy());
            let mut found: Vec<String> = Vec::new();
            self.consider(&prefix, &matcher, &mut found);
            if wild {
                self.walk(
                    &prefix,
                    1,
                    if deep { None } else { Some(rest) },
                    &matcher,
                    &mut found,
                );
            }
            found.sort();
            found.dedup();
            Ok(found)
        }

        fn consider(&self, path: &Path, matcher: &OsGrants, out: &mut Vec<String>) {
            if !matcher.fs_allows(path, FsMode::Read)
                || !self.grants.fs_allows_at(path, FsMode::Read, self.anchors)
            {
                return;
            }
            let Ok(real) = resolve(path, true) else {
                return;
            };
            if real != path && !self.grants.fs_allows_at(&real, FsMode::Read, self.anchors) {
                return;
            }
            if fs::symlink_metadata(path).is_ok() {
                out.push(path.to_string_lossy().into_owned());
            }
        }

        fn walk(
            &self,
            dir: &Path,
            level: usize,
            max_level: Option<usize>,
            matcher: &OsGrants,
            out: &mut Vec<String>,
        ) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            let mut children: Vec<(PathBuf, bool)> = entries
                .flatten()
                .map(|e| {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    (e.path(), is_dir)
                })
                .collect();
            children.sort();
            for (path, is_dir) in children {
                self.consider(&path, matcher, out);
                if is_dir && max_level.is_none_or(|max| level < max) {
                    self.walk(&path, level + 1, max_level, matcher, out);
                }
            }
        }

        fn mktemp(&self, args: &[Value]) -> Result<Value, OsError> {
            let prefix = arg_str(args, 0, self.op)?;
            if prefix.contains('/') || prefix.contains('\0') || prefix == ".." || prefix == "." {
                return Err(self.err(
                    OsErrorKind::Invalid,
                    "prefix must be a plain file-name prefix",
                ));
            }
            let base = match arg_opt(args, 1) {
                Some(_) => arg_str(args, 1, self.op)?.to_string(),
                None => std::env::temp_dir().to_string_lossy().into_owned(),
            };
            let at = self.locate(&base, FsMode::Create, true)?;
            for _ in 0..64 {
                let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                let mut x = nanos
                    ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ (u64::from(std::process::id()) << 32);
                x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                x ^= x >> 31;
                let candidate = at.real.join(format!("{prefix}{x:016x}"));
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&candidate)
                {
                    Ok(_) => return Ok(obj([("path", text(&candidate))])),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(self.io(&candidate, &e)),
                }
            }
            Err(self.err(OsErrorKind::Io, "could not find an unused temporary name"))
        }

        fn access(&self, args: &[Value]) -> Result<Value, OsError> {
            let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
            let flags = match arg_opt(args, 1) {
                None => libc::F_OK,
                Some(_) => match arg_str(args, 1, self.op)? {
                    "exists" => libc::F_OK,
                    "read" => libc::R_OK,
                    "write" => libc::W_OK,
                    "exec" | "execute" => libc::X_OK,
                    letters
                        if !letters.is_empty() && letters.chars().all(|c| "rwx".contains(c)) =>
                    {
                        let mut f = 0;
                        for c in letters.chars() {
                            f |= match c {
                                'r' => libc::R_OK,
                                'w' => libc::W_OK,
                                _ => libc::X_OK,
                            };
                        }
                        f
                    }
                    other => {
                        return Err(self.err(
                            OsErrorKind::Invalid,
                            format!(
                            "mode {other:?} must be exists, read, write, exec or letters of rwx"
                        ),
                        ))
                    }
                },
            };
            let c = c_path(&path).map_err(|e| self.io(&path, &e))?;
            // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
            let rc = unsafe { libc::access(c.as_ptr(), flags) };
            if rc == 0 {
                return Ok(obj([("ok", Value::Bool(true))]));
            }
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => {
                    Ok(obj([("ok", Value::Bool(false))]))
                }
                _ => Err(self.io(&path, &e)),
            }
        }

        fn chown(&self, args: &[Value]) -> Result<Value, OsError> {
            let path = self.path(arg_str(args, 0, self.op)?, FsMode::Meta, true)?;
            let id = |i: usize| -> Result<u32, OsError> {
                let v = arg_i64(args, i, self.op)?;
                if v == -1 {
                    return Ok(u32::MAX);
                }
                u32::try_from(v)
                    .ok()
                    .filter(|v| *v != u32::MAX)
                    .ok_or_else(|| {
                        self.err(OsErrorKind::Invalid, "uid and gid must be -1 or a valid id")
                    })
            };
            let (uid, gid) = (id(1)?, id(2)?);
            let c = c_path(&path).map_err(|e| self.io(&path, &e))?;
            // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
            let rc = unsafe { libc::chown(c.as_ptr(), uid, gid) };
            if rc != 0 {
                return Err(self.io(&path, &io::Error::last_os_error()));
            }
            Ok(obj([]))
        }

        fn utimes(&self, args: &[Value]) -> Result<Value, OsError> {
            let path = self.path(arg_str(args, 0, self.op)?, FsMode::Meta, true)?;
            let at = system_time(arg_i64(args, 1, self.op)?)
                .ok_or_else(|| self.err(OsErrorKind::Invalid, "atime is out of range"))?;
            let mt = system_time(arg_i64(args, 2, self.op)?)
                .ok_or_else(|| self.err(OsErrorKind::Invalid, "mtime is out of range"))?;
            let file = fs::File::open(&path).map_err(|e| self.io(&path, &e))?;
            file.set_times(fs::FileTimes::new().set_accessed(at).set_modified(mt))
                .map_err(|e| self.io(&path, &e))?;
            Ok(obj([]))
        }
    }

    fn system_time(ms: i64) -> Option<SystemTime> {
        let delta = Duration::from_millis(ms.unsigned_abs());
        if ms >= 0 {
            UNIX_EPOCH.checked_add(delta)
        } else {
            UNIX_EPOCH.checked_sub(delta)
        }
    }

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
    }

    fn pipe_read(pipe: &mut impl Read, count: u64, out: &mut Vec<u8>) -> io::Result<()> {
        let mut buf = vec![0u8; count as usize];
        let n = pipe.read(&mut buf)?;
        buf.truncate(n);
        *out = buf;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::args::bytes_value;
    use super::super::{OsHandles, OsHost};
    use super::*;
    use orbital_core_base::os::OsErrorKind;
    use orbital_core_base::os_grants::{FsGrant, FsMode, OsGrants};
    use orbital_core_base::value::ValueMap;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    const ALL: [FsMode; 5] = [
        FsMode::Read,
        FsMode::Write,
        FsMode::Create,
        FsMode::Remove,
        FsMode::Meta,
    ];

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
        grants: OsGrants,
        handles: OsHandles,
    }

    fn grant(glob: String, modes: &[FsMode]) -> FsGrant {
        FsGrant {
            glob,
            modes: modes.to_vec(),
        }
    }

    impl Fx {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().canonicalize().expect("canonical");
            let grants = OsGrants {
                fs: vec![grant(format!("{}/**", root.display()), &ALL)],
                ..OsGrants::default()
            };
            Self {
                _dir: dir,
                root,
                grants,
                handles: OsHandles::default(),
            }
        }

        fn with_grants(mut self, make: impl FnOnce(&Path) -> Vec<FsGrant>) -> Self {
            self.grants = OsGrants {
                fs: make(&self.root),
                ..OsGrants::default()
            };
            self
        }

        fn p(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn s(&self, rel: &str) -> Value {
            Value::String(self.p(rel).to_string_lossy().into_owned())
        }

        fn call(&self, op: OsOp, args: Vec<Value>) -> Result<Value, OsError> {
            let on_event = |_: Option<&CallSite>, _: Value| {};
            let host = OsHost {
                grants: &self.grants,
                handles: &self.handles,
                on_event: &on_event,
            };
            answer(op, &args, None, None, &host)
                .unwrap_or_else(|| Err(OsError::new(OsErrorKind::Closed, "not an open handle")))
        }

        fn ok(&self, op: OsOp, args: Vec<Value>) -> Value {
            self.call(op, args)
                .unwrap_or_else(|e| panic!("{} failed: {e:?}", op.name()))
        }

        fn kind(&self, op: OsOp, args: Vec<Value>) -> OsErrorKind {
            match self.call(op, args) {
                Err(e) => e.kind,
                Ok(v) => panic!("{} should fail, got {v:?}", op.name()),
            }
        }

        fn open(&self, rel: &str, flags: &str) -> i64 {
            match get(&self.ok(OsOp::Open, vec![self.s(rel), st(flags)]), "fd") {
                Value::Number(n) => *n as i64,
                other => panic!("{other:?}"),
            }
        }
    }

    fn st(text: &str) -> Value {
        Value::String(text.to_string())
    }

    fn n(v: i64) -> Value {
        Value::Number(v as f64)
    }

    fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
        match value {
            Value::Object(map) => map
                .get(key)
                .unwrap_or_else(|| panic!("no {key} in {value:?}")),
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
            Value::String(s) => s.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn data_of(value: &Value) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(str_of(get(value, "data")))
            .expect("base64")
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write");
    }

    #[test]
    fn write_then_read_round_trips_every_byte_value() {
        let fx = Fx::new();
        let all: Vec<u8> = (0..=255).collect();
        let wrote = fx.ok(OsOp::WriteFile, vec![fx.s("a.bin"), bytes_value(&all)]);
        assert_eq!(num_of(get(&wrote, "bytes")), 256.0);
        assert_eq!(data_of(&fx.ok(OsOp::ReadFile, vec![fx.s("a.bin")])), all);
        assert_eq!(fs::read(fx.p("a.bin")).expect("read"), all);
    }

    #[test]
    fn read_file_controls_missing_directory_and_the_cap() {
        let fx = Fx::new();
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![fx.s("nope")]),
            OsErrorKind::NotFound
        );
        fs::create_dir(fx.p("d")).expect("dir");
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![fx.s("d")]),
            OsErrorKind::Invalid
        );
        assert_eq!(fx.kind(OsOp::ReadFile, vec![n(1)]), OsErrorKind::Invalid);
        let big = fs::File::create(fx.p("big")).expect("big");
        big.set_len(64 * 1024 * 1024).expect("len");
        assert_eq!(
            data_of(&fx.ok(OsOp::ReadFile, vec![fx.s("big")])).len(),
            64 * 1024 * 1024
        );
        big.set_len(64 * 1024 * 1024 + 1).expect("len");
        let e = fx.call(OsOp::ReadFile, vec![fx.s("big")]).expect_err("cap");
        assert_eq!(e.kind, OsErrorKind::Invalid);
        assert!(e.message.contains("64 MiB"));
    }

    #[test]
    fn write_file_truncates_applies_mode_and_rejects_bad_input() {
        let fx = Fx::new();
        fx.ok(OsOp::WriteFile, vec![fx.s("w"), bytes_value(b"long text")]);
        fx.ok(
            OsOp::WriteFile,
            vec![fx.s("w"), bytes_value(b"ab"), n(0o600)],
        );
        assert_eq!(fs::read(fx.p("w")).expect("read"), b"ab");
        fx.ok(OsOp::WriteFile, vec![fx.s("m"), bytes_value(b""), n(0o640)]);
        let stat = fx.ok(OsOp::Stat, vec![fx.s("m")]);
        assert_eq!(num_of(get(&stat, "mode")) as u32 & !0o640, 0);
        assert_eq!(
            fx.kind(OsOp::WriteFile, vec![fx.s("x"), st("!!!")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(
                OsOp::WriteFile,
                vec![fx.s("no/such/dir/x"), bytes_value(b"")]
            ),
            OsErrorKind::NotFound
        );
    }

    #[test]
    fn append_file_appends_and_creates() {
        let fx = Fx::new();
        fx.ok(OsOp::AppendFile, vec![fx.s("l"), bytes_value(b"one")]);
        fx.ok(OsOp::AppendFile, vec![fx.s("l"), bytes_value(b"two")]);
        assert_eq!(fs::read(fx.p("l")).expect("read"), b"onetwo");
        assert_eq!(
            fx.kind(OsOp::AppendFile, vec![fx.s("l"), n(1)]),
            OsErrorKind::Invalid
        );
    }

    #[test]
    fn descriptors_open_read_write_seek_close() {
        let fx = Fx::new();
        write(&fx.p("f"), b"0123456789");
        let fd = fx.open("f", "r+");
        assert!(fd >= 3);
        let r = fx.ok(OsOp::Read, vec![n(fd), n(4)]);
        assert_eq!(data_of(&r), b"0123");
        assert_eq!(get(&r, "eof"), &Value::Bool(false));
        assert_eq!(
            num_of(get(
                &fx.ok(OsOp::Seek, vec![n(fd), n(2), st("cur")]),
                "offset"
            )),
            6.0
        );
        assert_eq!(
            num_of(get(
                &fx.ok(OsOp::Seek, vec![n(fd), n(-3), st("end")]),
                "offset"
            )),
            7.0
        );
        assert_eq!(
            num_of(get(
                &fx.ok(OsOp::Seek, vec![n(fd), n(1), st("set")]),
                "offset"
            )),
            1.0
        );
        fx.ok(OsOp::Write, vec![n(fd), bytes_value(b"XY")]);
        fx.ok(OsOp::Fsync, vec![n(fd)]);
        fx.ok(OsOp::Seek, vec![n(fd), n(0), st("set")]);
        let all = fx.ok(OsOp::Read, vec![n(fd), n(100)]);
        assert_eq!(data_of(&all), b"0XY3456789");
        let eof = fx.ok(OsOp::Read, vec![n(fd), n(10)]);
        assert_eq!(get(&eof, "eof"), &Value::Bool(true));
        assert_eq!(data_of(&eof), b"");
        fx.ok(OsOp::Close, vec![n(fd)]);
        assert_eq!(fx.kind(OsOp::Read, vec![n(fd), n(1)]), OsErrorKind::Closed);
        assert_eq!(
            fx.kind(OsOp::Write, vec![n(fd), bytes_value(b"")]),
            OsErrorKind::Closed
        );
        assert_eq!(
            fx.kind(OsOp::Seek, vec![n(fd), n(0), st("set")]),
            OsErrorKind::Closed
        );
        assert_eq!(fx.kind(OsOp::Fsync, vec![n(fd)]), OsErrorKind::Closed);
        assert_eq!(fx.kind(OsOp::Close, vec![n(fd)]), OsErrorKind::Closed);
    }

    #[test]
    fn seek_out_of_range_and_bad_arguments_are_invalid() {
        let fx = Fx::new();
        write(&fx.p("f"), b"abc");
        let fd = fx.open("f", "r");
        assert_eq!(
            fx.kind(OsOp::Seek, vec![n(fd), n(-1), st("set")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Seek, vec![n(fd), n(-1), st("cur")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Seek, vec![n(fd), n(-4), st("end")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Seek, vec![n(fd), n(0), st("sideways")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            num_of(get(
                &fx.ok(OsOp::Seek, vec![n(fd), n(100), st("set")]),
                "offset"
            )),
            100.0
        );
    }

    #[test]
    fn unknown_standard_and_oversized_descriptors_are_rejected() {
        let fx = Fx::new();
        for fd in [0, 1, 2, 99, -1] {
            assert_eq!(
                fx.kind(OsOp::Read, vec![n(fd), n(1)]),
                OsErrorKind::Closed,
                "{fd}"
            );
            assert_eq!(
                fx.kind(OsOp::Close, vec![n(fd)]),
                OsErrorKind::Closed,
                "{fd}"
            );
        }
        write(&fx.p("f"), b"abc");
        let fd = fx.open("f", "r");
        assert_eq!(
            fx.kind(OsOp::Read, vec![n(fd), n(64 * 1024 * 1024 + 1)]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Read, vec![n(fd), n(-1)]),
            OsErrorKind::Invalid
        );
        let zero = fx.ok(OsOp::Read, vec![n(fd), n(0)]);
        assert_eq!(get(&zero, "eof"), &Value::Bool(false));
    }

    #[test]
    fn open_flags_create_exclusive_and_read_only_writes() {
        let fx = Fx::new();
        let fd = fx.open("new", "w");
        fx.ok(OsOp::Write, vec![n(fd), bytes_value(b"hi")]);
        fx.ok(OsOp::Close, vec![n(fd)]);
        assert_eq!(fs::read(fx.p("new")).expect("read"), b"hi");
        assert_eq!(
            fx.kind(OsOp::Open, vec![fx.s("new"), st("wx")]),
            OsErrorKind::Exists
        );
        assert_eq!(
            fx.kind(OsOp::Open, vec![fx.s("missing"), st("r")]),
            OsErrorKind::NotFound
        );
        assert_eq!(
            fx.kind(OsOp::Open, vec![fx.s("new"), st("zz")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Open, vec![fx.s("new"), st("rx")]),
            OsErrorKind::Invalid
        );
        let ro = fx.open("new", "r");
        assert_ne!(
            fx.kind(OsOp::Write, vec![n(ro), bytes_value(b"x")]),
            OsErrorKind::Denied
        );
        assert_eq!(fs::read(fx.p("new")).expect("read"), b"hi");
        let ap = fx.open("new", "a");
        fx.ok(OsOp::Write, vec![n(ap), bytes_value(b"!")]);
        assert_eq!(fs::read(fx.p("new")).expect("read"), b"hi!");
    }

    #[test]
    fn truncate_grows_and_shrinks_by_path_and_by_fd() {
        let fx = Fx::new();
        write(&fx.p("t"), b"abcdef");
        fx.ok(OsOp::Truncate, vec![fx.s("t"), n(3)]);
        assert_eq!(fs::read(fx.p("t")).expect("read"), b"abc");
        fx.ok(OsOp::Truncate, vec![fx.s("t"), n(5)]);
        assert_eq!(fs::read(fx.p("t")).expect("read"), b"abc\0\0");
        let fd = fx.open("t", "r+");
        fx.ok(OsOp::Truncate, vec![n(fd), n(1)]);
        assert_eq!(fs::read(fx.p("t")).expect("read"), b"a");
        assert_eq!(
            fx.kind(OsOp::Truncate, vec![n(fd), n(-1)]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Truncate, vec![n(77), n(1)]),
            OsErrorKind::Closed
        );
        assert_eq!(
            fx.kind(OsOp::Truncate, vec![fx.s("absent"), n(1)]),
            OsErrorKind::NotFound
        );
        assert!(!fx.p("absent").exists());
    }

    #[test]
    fn copy_and_rename_including_onto_an_existing_file() {
        let fx = Fx::new();
        write(&fx.p("a"), b"A");
        write(&fx.p("b"), b"B");
        fx.ok(OsOp::Copy, vec![fx.s("a"), fx.s("c")]);
        assert_eq!(fs::read(fx.p("c")).expect("read"), b"A");
        fx.ok(OsOp::Copy, vec![fx.s("a"), fx.s("b")]);
        assert_eq!(fs::read(fx.p("b")).expect("read"), b"A");
        write(&fx.p("d"), b"D");
        fx.ok(OsOp::Rename, vec![fx.s("d"), fx.s("b")]);
        assert_eq!(fs::read(fx.p("b")).expect("read"), b"D");
        assert!(!fx.p("d").exists());
        assert_eq!(
            fx.kind(OsOp::Rename, vec![fx.s("d"), fx.s("z")]),
            OsErrorKind::NotFound
        );
        assert_eq!(
            fx.kind(OsOp::Copy, vec![fx.s("gone"), fx.s("z")]),
            OsErrorKind::NotFound
        );
    }

    #[test]
    fn recursive_rmdir_needs_the_remove_grant_on_every_entry_and_never_follows_links() {
        let fx = Fx::new().with_grants(|root| {
            vec![
                grant(format!("{}/d", root.display()), &[FsMode::Remove]),
                grant(format!("{}/ok/**", root.display()), &[FsMode::Remove]),
                grant(format!("{}/ok", root.display()), &[FsMode::Remove]),
            ]
        });
        fs::create_dir(fx.p("d")).expect("dir");
        write(&fx.p("d/x"), b"keep");
        assert_eq!(
            fx.kind(OsOp::Rmdir, vec![fx.s("d"), Value::Bool(true)]),
            OsErrorKind::Denied
        );
        assert_eq!(fs::read(fx.p("d/x")).expect("kept"), b"keep");

        let outside = tempfile::tempdir().expect("outside");
        write(&outside.path().join("secret"), b"s");
        fs::create_dir(fx.p("ok")).expect("dir");
        symlink(outside.path(), fx.p("ok/link")).expect("link");
        fx.ok(OsOp::Rmdir, vec![fx.s("ok"), Value::Bool(true)]);
        assert!(!fx.p("ok").exists());
        assert!(outside.path().join("secret").exists());
    }

    #[test]
    fn a_blocking_pipe_read_does_not_hold_the_descriptor_table() {
        let fx = Fx::new();
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 2; printf hi"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        let out = child.stdout.take().expect("stdout");
        let fd = fx
            .handles
            .fs
            .fds
            .insert(FdEntry::ChildStdout(Arc::new(Mutex::new(out))));
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| fx.ok(OsOp::Read, vec![n(fd), n(16)]));
            std::thread::sleep(std::time::Duration::from_millis(300));
            let started = std::time::Instant::now();
            assert!(fx.handles.fs.fds.contains(fd));
            assert!(started.elapsed() < std::time::Duration::from_millis(1000));
            let read = reader.join().expect("reader");
            assert_eq!(data_of(&read), b"hi");
        });
        child.wait().expect("wait");
    }

    #[test]
    fn remove_takes_files_and_rmdir_takes_directories() {
        let fx = Fx::new();
        write(&fx.p("f"), b"");
        fs::create_dir(fx.p("d")).expect("dir");
        write(&fx.p("d/x"), b"");
        fx.ok(OsOp::Remove, vec![fx.s("f")]);
        assert!(!fx.p("f").exists());
        assert_eq!(
            fx.kind(OsOp::Remove, vec![fx.s("f")]),
            OsErrorKind::NotFound
        );
        assert_eq!(fx.kind(OsOp::Remove, vec![fx.s("d")]), OsErrorKind::Invalid);
        assert_eq!(
            fx.kind(OsOp::Rmdir, vec![fx.s("d/x")]),
            OsErrorKind::Invalid
        );
        assert_eq!(fx.kind(OsOp::Rmdir, vec![fx.s("d")]), OsErrorKind::Io);
        fx.ok(OsOp::Rmdir, vec![fx.s("d"), Value::Bool(true)]);
        assert!(!fx.p("d").exists());
        fs::create_dir(fx.p("e")).expect("dir");
        fx.ok(OsOp::Rmdir, vec![fx.s("e")]);
    }

    #[test]
    fn mkdir_plain_recursive_and_existing() {
        let fx = Fx::new();
        fx.ok(OsOp::Mkdir, vec![fx.s("one")]);
        assert!(fx.p("one").is_dir());
        assert_eq!(fx.kind(OsOp::Mkdir, vec![fx.s("one")]), OsErrorKind::Exists);
        assert_eq!(
            fx.kind(OsOp::Mkdir, vec![fx.s("p/q/r")]),
            OsErrorKind::NotFound
        );
        fx.ok(OsOp::Mkdir, vec![fx.s("p/q/r"), Value::Bool(true)]);
        assert!(fx.p("p/q/r").is_dir());
        fx.ok(OsOp::Mkdir, vec![fx.s("p/q/r"), Value::Bool(true)]);
        fx.ok(OsOp::Mkdir, vec![fx.s("m"), Value::Bool(false), n(0o700)]);
        let stat = fx.ok(OsOp::Stat, vec![fx.s("m")]);
        assert_eq!(num_of(get(&stat, "mode")) as u32, 0o700);
    }

    #[test]
    fn links_symlinks_readlink_and_lstat_kinds() {
        let fx = Fx::new();
        write(&fx.p("t"), b"data");
        fs::create_dir(fx.p("d")).expect("dir");
        fx.ok(OsOp::Link, vec![fx.s("t"), fx.s("hard")]);
        assert_eq!(fs::read(fx.p("hard")).expect("read"), b"data");
        fx.ok(OsOp::Symlink, vec![st("t"), fx.s("soft")]);
        assert_eq!(
            str_of(get(&fx.ok(OsOp::Readlink, vec![fx.s("soft")]), "target")),
            "t"
        );
        assert_eq!(data_of(&fx.ok(OsOp::ReadFile, vec![fx.s("soft")])), b"data");
        let kind = |op, rel: &str| str_of(get(&fx.ok(op, vec![fx.s(rel)]), "kind"));
        assert_eq!(kind(OsOp::Lstat, "soft"), "symlink");
        assert_eq!(kind(OsOp::Stat, "soft"), "file");
        assert_eq!(kind(OsOp::Lstat, "d"), "dir");
        assert_eq!(kind(OsOp::Stat, "t"), "file");
        assert_eq!(
            fx.kind(OsOp::Link, vec![fx.s("t"), fx.s("hard")]),
            OsErrorKind::Exists
        );
        assert_eq!(
            fx.kind(OsOp::Symlink, vec![st("t"), fx.s("soft")]),
            OsErrorKind::Exists
        );
        assert_eq!(
            fx.kind(OsOp::Link, vec![fx.s("none"), fx.s("h2")]),
            OsErrorKind::NotFound
        );
        assert_eq!(
            fx.kind(OsOp::Readlink, vec![fx.s("t")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Lstat, vec![fx.s("none")]),
            OsErrorKind::NotFound
        );
        fx.ok(OsOp::Remove, vec![fx.s("soft")]);
        assert!(fx.p("t").exists());
    }

    #[test]
    fn stat_reports_the_contract_fields() {
        let fx = Fx::new();
        write(&fx.p("f"), b"12345");
        let stat = fx.ok(OsOp::Stat, vec![fx.s("f")]);
        let Value::Object(map) = &stat else { panic!() };
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["kind", "size", "mode", "uid", "gid", "atime", "mtime", "ctime"]
        );
        assert_eq!(num_of(get(&stat, "size")), 5.0);
        assert!(num_of(get(&stat, "mtime")) > 1.0e12);
    }

    #[test]
    fn list_dir_is_sorted_with_kinds() {
        let fx = Fx::new();
        write(&fx.p("b"), b"");
        write(&fx.p("a"), b"");
        fs::create_dir(fx.p("c")).expect("dir");
        symlink("a", fx.p("d")).expect("link");
        let listing = fx.ok(
            OsOp::ListDir,
            vec![Value::String(fx.root.to_string_lossy().into_owned())],
        );
        let Value::Array(entries) = get(&listing, "entries") else {
            panic!()
        };
        let pairs: Vec<(String, String)> = entries
            .iter()
            .map(|e| (str_of(get(e, "name")), str_of(get(e, "kind"))))
            .collect();
        let want = [("a", "file"), ("b", "file"), ("c", "dir"), ("d", "symlink")];
        assert_eq!(pairs, want.map(|(a, b)| (a.to_string(), b.to_string())));
        assert_eq!(
            fx.kind(OsOp::ListDir, vec![fx.s("a")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::ListDir, vec![fx.s("zzz")]),
            OsErrorKind::NotFound
        );
    }

    fn paths_of(value: &Value) -> Vec<String> {
        let Value::Array(items) = get(value, "paths") else {
            panic!()
        };
        items.iter().map(str_of).collect()
    }

    #[test]
    fn glob_matches_sorted_and_filters_to_granted_results() {
        let fx = Fx::new();
        fs::create_dir_all(fx.p("a/deep")).expect("dirs");
        fs::create_dir(fx.p("b")).expect("dir");
        write(&fx.p("a/one.txt"), b"");
        write(&fx.p("a/deep/two.txt"), b"");
        write(&fx.p("a/skip.md"), b"");
        write(&fx.p("b/three.txt"), b"");
        let all = paths_of(&fx.ok(
            OsOp::Glob,
            vec![Value::String(format!("{}/**/*.txt", fx.root.display()))],
        ));
        let want: Vec<String> = ["a/deep/two.txt", "a/one.txt", "b/three.txt"]
            .iter()
            .map(|r| fx.p(r).to_string_lossy().into_owned())
            .collect();
        assert_eq!(all, want);
        let shallow = paths_of(&fx.ok(
            OsOp::Glob,
            vec![Value::String(format!("{}/*/*.txt", fx.root.display()))],
        ));
        assert_eq!(shallow, [want[1].clone(), want[2].clone()]);
        assert!(paths_of(&fx.ok(
            OsOp::Glob,
            vec![Value::String(format!("{}/nomatch/*", fx.root.display()))]
        ))
        .is_empty());
    }

    #[test]
    fn glob_never_returns_ungranted_or_escaping_paths() {
        let outside = tempfile::tempdir().expect("outside");
        let out_root = outside.path().canonicalize().expect("canon");
        write(&out_root.join("secret.txt"), b"");
        let fx = Fx::new();
        fs::create_dir_all(fx.p("a")).expect("dir");
        write(&fx.p("a/ok.txt"), b"");
        symlink(out_root.join("secret.txt"), fx.p("a/leak.txt")).expect("link");
        let fx =
            fx.with_grants(|root| vec![grant(format!("{}/a/**", root.display()), &[FsMode::Read])]);
        let pattern = |p: String| vec![Value::String(p)];
        let got = paths_of(&fx.ok(
            OsOp::Glob,
            pattern(format!("{}/a/*.txt", fx.root.display())),
        ));
        assert_eq!(got, [fx.p("a/ok.txt").to_string_lossy().into_owned()]);
        assert_eq!(
            fx.kind(
                OsOp::Glob,
                pattern(format!("{}/**/*.txt", fx.root.display()))
            ),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Glob, pattern("/*".to_string())),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(
                OsOp::Glob,
                pattern(format!("{}/a/../*.txt", fx.root.display()))
            ),
            OsErrorKind::Denied
        );
    }

    #[test]
    fn mktemp_makes_unique_files_inside_the_directory() {
        let fx = Fx::new();
        let dir = Value::String(fx.root.to_string_lossy().into_owned());
        let mut seen = std::collections::HashSet::new();
        for _ in 0..20 {
            let made = str_of(get(
                &fx.ok(OsOp::Mktemp, vec![st("job-"), dir.clone()]),
                "path",
            ));
            let path = PathBuf::from(&made);
            assert!(path.is_file(), "{made}");
            assert!(path.starts_with(&fx.root));
            assert!(path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .starts_with("job-"));
            assert!(seen.insert(made));
        }
        assert_eq!(
            fx.kind(OsOp::Mktemp, vec![st("a/b"), dir.clone()]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Mktemp, vec![st(".."), dir]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Mktemp, vec![st("x"), fx.s("missing")]),
            OsErrorKind::NotFound
        );
    }

    #[test]
    fn realpath_resolves_symlinks_and_needs_the_target_to_exist() {
        let fx = Fx::new();
        write(&fx.p("t"), b"");
        symlink("t", fx.p("l")).expect("link");
        let got = str_of(get(&fx.ok(OsOp::Realpath, vec![fx.s("l")]), "path"));
        assert_eq!(got, fx.p("t").to_string_lossy());
        assert_eq!(
            fx.kind(OsOp::Realpath, vec![fx.s("nope")]),
            OsErrorKind::NotFound
        );
    }

    #[test]
    fn cwd_is_virtual_and_relative_paths_need_it() {
        let fx = Fx::new();
        let before = std::env::current_dir().expect("cwd");
        fs::create_dir(fx.p("sub")).expect("dir");
        write(&fx.p("sub/f"), b"hi");
        assert_eq!(fx.kind(OsOp::Cwd, vec![]), OsErrorKind::Invalid);
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![st("sub/f")]),
            OsErrorKind::Invalid
        );
        fx.ok(OsOp::Chdir, vec![fx.s("sub")]);
        assert_eq!(
            str_of(get(&fx.ok(OsOp::Cwd, vec![]), "path")),
            fx.p("sub").to_string_lossy()
        );
        assert_eq!(data_of(&fx.ok(OsOp::ReadFile, vec![st("f")])), b"hi");
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![st("../../../../etc/passwd")]),
            OsErrorKind::Denied
        );
        fx.ok(OsOp::Chdir, vec![st("..")]);
        assert_eq!(
            str_of(get(&fx.ok(OsOp::Cwd, vec![]), "path")),
            fx.root.to_string_lossy()
        );
        assert_eq!(
            fx.kind(OsOp::Chdir, vec![fx.s("sub/f")]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Chdir, vec![fx.s("absent")]),
            OsErrorKind::NotFound
        );
        assert_eq!(fx.kind(OsOp::Chdir, vec![st("/")]), OsErrorKind::Denied);
        assert_eq!(std::env::current_dir().expect("cwd"), before);
        fx.handles.close_all();
        assert_eq!(fx.kind(OsOp::Cwd, vec![]), OsErrorKind::Invalid);
    }

    #[test]
    fn access_and_exists_answer_ok_flags() {
        let fx = Fx::new();
        write(&fx.p("f"), b"");
        let ok = |op, args| get(&fx.ok(op, args), "ok").clone();
        assert_eq!(ok(OsOp::Exists, vec![fx.s("f")]), Value::Bool(true));
        assert_eq!(ok(OsOp::Exists, vec![fx.s("nope")]), Value::Bool(false));
        assert_eq!(ok(OsOp::Access, vec![fx.s("f")]), Value::Bool(true));
        assert_eq!(
            ok(OsOp::Access, vec![fx.s("f"), st("rw")]),
            Value::Bool(true)
        );
        assert_eq!(
            ok(OsOp::Access, vec![fx.s("f"), st("exec")]),
            Value::Bool(false)
        );
        assert_eq!(
            ok(OsOp::Access, vec![fx.s("nope"), st("read")]),
            Value::Bool(false)
        );
        assert_eq!(
            fx.kind(OsOp::Access, vec![fx.s("f"), st("sing")]),
            OsErrorKind::Invalid
        );
    }

    #[test]
    fn chmod_round_trips_and_rejects_bad_modes() {
        let fx = Fx::new();
        write(&fx.p("f"), b"");
        for mode in [0o600, 0o755, 0o4755 & 0o777, 0o444] {
            fx.ok(OsOp::Chmod, vec![fx.s("f"), n(mode)]);
            let stat = fx.ok(OsOp::Stat, vec![fx.s("f")]);
            assert_eq!(num_of(get(&stat, "mode")) as i64, mode);
        }
        assert_eq!(
            fx.kind(OsOp::Chmod, vec![fx.s("f"), n(-1)]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Chmod, vec![fx.s("f"), n(0o10000)]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Chmod, vec![fx.s("nope"), n(0o600)]),
            OsErrorKind::NotFound
        );
    }

    #[test]
    fn chown_to_self_and_utimes_round_trip() {
        let fx = Fx::new();
        write(&fx.p("f"), b"");
        let stat = fx.ok(OsOp::Stat, vec![fx.s("f")]);
        let (uid, gid) = (
            num_of(get(&stat, "uid")) as i64,
            num_of(get(&stat, "gid")) as i64,
        );
        fx.ok(OsOp::Chown, vec![fx.s("f"), n(uid), n(gid)]);
        fx.ok(OsOp::Chown, vec![fx.s("f"), n(-1), n(-1)]);
        assert_eq!(
            fx.kind(OsOp::Chown, vec![fx.s("f"), n(-5), n(gid)]),
            OsErrorKind::Invalid
        );
        assert_eq!(
            fx.kind(OsOp::Chown, vec![fx.s("nope"), n(uid), n(gid)]),
            OsErrorKind::NotFound
        );
        fx.ok(
            OsOp::Utimes,
            vec![fx.s("f"), n(1_500_000_000_000), n(1_600_000_000_000)],
        );
        let stat = fx.ok(OsOp::Stat, vec![fx.s("f")]);
        assert_eq!(num_of(get(&stat, "atime")), 1_500_000_000_000.0);
        assert_eq!(num_of(get(&stat, "mtime")), 1_600_000_000_000.0);
        assert_eq!(
            fx.kind(OsOp::Utimes, vec![fx.s("nope"), n(0), n(0)]),
            OsErrorKind::NotFound
        );
    }

    #[test]
    fn empty_grants_deny_every_path_op_before_touching_the_target() {
        let fx = Fx::new().with_grants(|_| Vec::new());
        write(&fx.p("f"), b"keep");
        let f = || fx.s("f");
        let g = || fx.s("g");
        let cases: Vec<(OsOp, Vec<Value>)> = vec![
            (OsOp::ReadFile, vec![f()]),
            (OsOp::WriteFile, vec![g(), bytes_value(b"x")]),
            (OsOp::AppendFile, vec![f(), bytes_value(b"x")]),
            (OsOp::Open, vec![f(), st("r")]),
            (OsOp::Open, vec![g(), st("w")]),
            (OsOp::Truncate, vec![f(), n(0)]),
            (OsOp::Copy, vec![f(), g()]),
            (OsOp::Rename, vec![f(), g()]),
            (OsOp::Remove, vec![f()]),
            (OsOp::Link, vec![f(), g()]),
            (OsOp::Symlink, vec![st("x"), g()]),
            (OsOp::Readlink, vec![f()]),
            (OsOp::Mkdir, vec![g()]),
            (OsOp::Rmdir, vec![f()]),
            (OsOp::ListDir, vec![fx.s("")]),
            (OsOp::Glob, vec![fx.s("*")]),
            (
                OsOp::Mktemp,
                vec![
                    st("p"),
                    Value::String(fx.root.to_string_lossy().into_owned()),
                ],
            ),
            (OsOp::Realpath, vec![f()]),
            (
                OsOp::Chdir,
                vec![Value::String(fx.root.to_string_lossy().into_owned())],
            ),
            (OsOp::Stat, vec![f()]),
            (OsOp::Lstat, vec![f()]),
            (OsOp::Access, vec![f()]),
            (OsOp::Exists, vec![f()]),
            (OsOp::Chmod, vec![f(), n(0)]),
            (OsOp::Chown, vec![f(), n(0), n(0)]),
            (OsOp::Utimes, vec![f(), n(0), n(0)]),
        ];
        for (op, args) in cases {
            if op == OsOp::ListDir {
                continue;
            }
            assert_eq!(fx.kind(op, args), OsErrorKind::Denied, "{}", op.name());
        }
        assert_eq!(
            fx.kind(
                OsOp::ListDir,
                vec![Value::String(fx.root.to_string_lossy().into_owned())]
            ),
            OsErrorKind::Denied
        );
        assert_eq!(fs::read(fx.p("f")).expect("read"), b"keep");
        assert!(!fx.p("g").exists());
        assert!(fx.p("f").metadata().expect("meta").permissions().mode() & 0o777 != 0);
    }

    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_grant_that_misses_the_path_or_the_mode_denies() {
        let fx = Fx::new();
        fs::create_dir_all(fx.p("in")).expect("dir");
        write(&fx.p("in/f"), b"x");
        write(&fx.p("out"), b"y");
        let fx = fx
            .with_grants(|root| vec![grant(format!("{}/in/**", root.display()), &[FsMode::Read])]);
        assert_eq!(data_of(&fx.ok(OsOp::ReadFile, vec![fx.s("in/f")])), b"x");
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![fx.s("out")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![fx.s("in-evil/f")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::WriteFile, vec![fx.s("in/f"), bytes_value(b"z")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Open, vec![fx.s("in/f"), st("r+")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Remove, vec![fx.s("in/f")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Chmod, vec![fx.s("in/f"), n(0o600)]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Copy, vec![fx.s("in/f"), fx.s("in/g")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Rename, vec![fx.s("in/f"), fx.s("in/g")]),
            OsErrorKind::Denied
        );
        assert_eq!(fs::read(fx.p("in/f")).expect("read"), b"x");
    }

    #[test]
    fn create_is_a_separate_mode_from_write() {
        let fx = Fx::new();
        write(&fx.p("f"), b"old");
        let fx = fx.with_grants(|root| {
            vec![grant(
                format!("{}/**", root.display()),
                &[FsMode::Read, FsMode::Write],
            )]
        });
        fx.ok(OsOp::WriteFile, vec![fx.s("f"), bytes_value(b"new")]);
        assert_eq!(fs::read(fx.p("f")).expect("read"), b"new");
        assert_eq!(
            fx.kind(OsOp::WriteFile, vec![fx.s("fresh"), bytes_value(b"")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Open, vec![fx.s("fresh"), st("w")]),
            OsErrorKind::Denied
        );
        assert_eq!(fx.kind(OsOp::Mkdir, vec![fx.s("d")]), OsErrorKind::Denied);
        assert!(!fx.p("fresh").exists() && !fx.p("d").exists());
    }

    #[test]
    fn dot_dot_traversal_out_of_the_grant_is_denied() {
        let fx = Fx::new();
        fs::create_dir(fx.p("in")).expect("dir");
        write(&fx.p("secret"), b"s");
        let fx = fx.with_grants(|root| vec![grant(format!("{}/in/**", root.display()), &ALL)]);
        let sneaky = Value::String(format!("{}/in/../secret", fx.root.display()));
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![sneaky.clone()]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Open, vec![sneaky, st("r")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![st("/../../etc/passwd")]),
            OsErrorKind::Denied
        );
    }

    #[test]
    fn a_path_built_under_a_covered_root_cannot_climb_out_of_it() {
        let fx = Fx::new();
        fs::create_dir(fx.p("in")).expect("dir");
        write(&fx.p("in/a"), b"a");
        write(&fx.p("secret"), b"s");
        let fx = fx.with_grants(|root| vec![grant(format!("{}/in/**", root.display()), &ALL)]);
        let root = format!("{}/in/", fx.root.display());
        let built = |tail: &str| Value::String(format!("{root}{tail}"));
        assert_eq!(data_of(&fx.ok(OsOp::ReadFile, vec![built("a")])), b"a");
        assert_eq!(fx.kind(OsOp::ReadFile, vec![built("../secret")]), OsErrorKind::Denied);
        assert_eq!(fx.kind(OsOp::ReadFile, vec![built("../../../../etc/passwd")]), OsErrorKind::Denied);
        assert_eq!(
            fx.kind(OsOp::WriteFile, vec![built("../new"), bytes_value(b"x")]),
            OsErrorKind::Denied
        );
        assert!(!fx.p("new").exists());
    }

    #[test]
    fn a_symlink_inside_the_grant_cannot_reach_outside_it() {
        let outside = tempfile::tempdir().expect("outside");
        let out_root = outside.path().canonicalize().expect("canon");
        write(&out_root.join("secret"), b"s");
        fs::create_dir(out_root.join("dir")).expect("dir");
        let fx = Fx::new();
        symlink(out_root.join("secret"), fx.p("file-link")).expect("link");
        symlink(&out_root, fx.p("dir-link")).expect("link");
        symlink(out_root.join("fresh"), fx.p("dangling")).expect("link");
        symlink("../..", fx.p("up")).expect("link");
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![fx.s("file-link")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![fx.s("dir-link/secret")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::ListDir, vec![fx.s("dir-link")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Stat, vec![fx.s("file-link")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Open, vec![fx.s("file-link"), st("r")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::WriteFile, vec![fx.s("dangling"), bytes_value(b"x")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(
                OsOp::WriteFile,
                vec![fx.s("dir-link/new"), bytes_value(b"x")]
            ),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Mkdir, vec![fx.s("dir-link/m"), Value::Bool(true)]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Realpath, vec![fx.s("file-link")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Chdir, vec![fx.s("dir-link")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Chmod, vec![fx.s("file-link"), n(0o777)]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Truncate, vec![fx.s("file-link"), n(0)]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::Copy, vec![fx.s("file-link"), fx.s("stolen")]),
            OsErrorKind::Denied
        );
        assert_eq!(
            fx.kind(OsOp::ReadFile, vec![fx.s("up/etc/hosts")]),
            OsErrorKind::Denied
        );
        assert_eq!(fs::read(out_root.join("secret")).expect("read"), b"s");
        assert!(!out_root.join("fresh").exists() && !out_root.join("new").exists());
        assert!(!fx.p("stolen").exists());
        let lstat = fx.ok(OsOp::Lstat, vec![fx.s("file-link")]);
        assert_eq!(str_of(get(&lstat, "kind")), "symlink");
        fx.ok(OsOp::Remove, vec![fx.s("file-link")]);
        assert!(out_root.join("secret").exists());
    }

    #[test]
    fn a_symlink_that_stays_inside_the_grant_is_followed() {
        let fx = Fx::new();
        fs::create_dir(fx.p("real")).expect("dir");
        write(&fx.p("real/f"), b"in");
        symlink("real", fx.p("alias")).expect("link");
        assert_eq!(
            data_of(&fx.ok(OsOp::ReadFile, vec![fx.s("alias/f")])),
            b"in"
        );
    }

    #[test]
    fn watching_files_is_unsupported_until_the_streaming_host_exists() {
        let fx = Fx::new();
        let e = fx
            .call(
                OsOp::WatchFiles,
                vec![st("/x/**"), Value::Object(ValueMap::default())],
            )
            .expect_err("unsupported");
        assert_eq!(e.kind, OsErrorKind::Unsupported);
        assert!(e.message.contains("streaming host not implemented"));
    }
    mod finite_fs_completion {
        use super::*;
        use crate::entity_source::InMemoryEntityCache;
        use crate::native_host::NativeHost;
        use crate::os_host::completion::{CompletionTestPhase, OsCompletionSink};
        use crate::resumable::{CallRoutes, CallSite, IoAnswer, IoRequest};
        use std::sync::{mpsc, Arc};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        struct Sink(mpsc::Sender<(String, Result<Value, OsError>)>);
        impl OsCompletionSink for Sink {
            fn complete(&self, _: &CallSite, key: &str, outcome: Result<Value, OsError>) {
                self.0.send((key.to_string(), outcome)).expect("completion receiver");
            }
        }

        struct Paused {
            entered: mpsc::Receiver<u64>,
            release: mpsc::Sender<()>,
            terminal: mpsc::Receiver<u64>,
        }

        fn pause_first(handles: &OsHandles, phase: CompletionTestPhase) -> Paused {
            let (entered_tx, entered) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let (terminal_tx, terminal) = mpsc::channel();
            let release_rx = orbital_core_base::sync::Mutex::new(release_rx);
            let first = AtomicBool::new(true);
            handles.completions.set_test_observer(Arc::new(move |key, lease, observed| {
                assert_eq!(key, "open");
                if observed == phase && first.swap(false, Ordering::SeqCst) {
                    entered_tx.send(lease).expect("phase receiver");
                    release_rx.lock().recv_timeout(Duration::from_secs(5)).expect("release paused worker");
                }
                if observed == CompletionTestPhase::Terminal {
                    terminal_tx.send(lease).expect("terminal receiver");
                }
            }));
            Paused { entered, release, terminal }
        }

        fn native(fx: &Fx, handles: Arc<OsHandles>) -> (NativeHost, mpsc::Receiver<(String, Result<Value, OsError>)>) {
            let (tx, rx) = mpsc::channel();
            handles.completions.set_sink(Arc::new(Sink(tx)));
            (native_context(fx, handles), rx)
        }

        fn native_context(fx: &Fx, handles: Arc<OsHandles>) -> NativeHost {
            NativeHost::new(
                Arc::new(InMemoryEntityCache::new()),
                Arc::new(crate::native_host::tests::Offline),
                Arc::new(crate::credentials::Credentials::from_env(Default::default())),
                Vec::new(), fx.grants.clone(), handles,
                Arc::new(|_: Option<&CallSite>, _: Value| {}),
            )
        }

        fn request(op: OsOp, args: Vec<Value>, key: Option<&str>) -> IoRequest {
            IoRequest::Os {
                op, args, key: key.map(str::to_string),
                site: Some(CallSite {
                    orbital: None, trait_name: "Boundary".into(), follow_on: false,
                    entity_id: "singleton".into(),
                    emit: CallRoutes { success: Some("DONE".into()), failure: Some("FAILED".into()), ..Default::default() },
                }),
            }
        }

        fn open(host: &NativeHost, fx: &Fx) {
            let answer = host.answer(&request(OsOp::Open, vec![fx.s("file"), st("r")], Some("open")));
            assert!(matches!(answer, IoAnswer::ServiceDeferred), "keyed Open must defer before waiting for a worker: {answer:?}");
        }

        fn terminal(paused: &Paused, lease: u64) {
            loop {
                if paused.terminal.recv_timeout(Duration::from_secs(5)).expect("worker reached terminal after discard cleanup") == lease { return; }
            }
        }

        #[test]
        fn finite_fs_completion_close_preserves_file_socket_and_unknown_handle_results() {
            let fx = Fx::new();
            fs::write(fx.p("file"), b"payload").unwrap();
            for keyed in [false, true] {
                for kind in ["file", "socket", "unknown"] {
                    let handles = Arc::new(OsHandles::default());
                    let (host, delivered) = native(&fx, handles.clone());
                    let mut peer = None;
                    let fd = match kind {
                        "file" => {
                            let IoAnswer::Os(Ok(opened)) = host.answer(&request(OsOp::Open, vec![fx.s("file"), st("r")], None)) else { panic!("inline Open") };
                            let Value::Number(fd) = get(&opened, "fd") else { panic!("Open descriptor") };
                            *fd as i64
                        }
                        "socket" => {
                            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
                            let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                            peer = Some(listener.accept().unwrap().0);
                            handles.net.socks.insert(super::super::super::net::SockEntry::Stream(stream))
                        }
                        _ => 999_999,
                    };
                    let result = if keyed {
                        let paused = pause_first(&handles, CompletionTestPhase::BeforeClaim);
                        let answer = host.answer(&request(OsOp::Close, vec![n(fd)], Some("open")));
                        assert!(matches!(answer, IoAnswer::ServiceDeferred), "{kind}: {answer:?}");
                        let lease = paused.entered.recv_timeout(Duration::from_secs(5)).expect("Close before claim");
                        paused.release.send(()).unwrap();
                        let (key, result) = delivered.recv_timeout(Duration::from_secs(5)).expect("Close completion");
                        assert_eq!(key, "open");
                        terminal(&paused, lease);
                        result
                    } else {
                        let IoAnswer::Os(result) = host.answer(&request(OsOp::Close, vec![n(fd)], None)) else { panic!("inline Close") };
                        result
                    };
                    if kind == "unknown" {
                        assert_eq!(result, Err(OsError::new(OsErrorKind::Closed, "os/close: not an open handle")));
                    } else {
                        assert_eq!(result, Ok(Value::Object(ValueMap::default())));
                    }
                    assert!(!handles.fs.fds.contains(fd));
                    assert!(!handles.net.socks.contains(fd));
                    assert!(handles.completions.live_keys().is_empty());
                    drop(peer);
                }
            }
        }

        #[test]
        fn finite_fs_completion_captures_virtual_cwd_before_later_chdir() {
            let fx = Fx::new();
            fs::create_dir(fx.p("first")).unwrap();
            fs::create_dir(fx.p("second")).unwrap();
            fs::write(fx.p("first/file"), b"first").unwrap();
            fs::write(fx.p("second/file"), b"second").unwrap();
            let handles = Arc::new(OsHandles::default());
            let (host, delivered) = native(&fx, handles.clone());
            assert!(matches!(host.answer(&request(OsOp::Chdir, vec![fx.s("first")], None)), IoAnswer::Os(Ok(_))));
            let IoAnswer::Os(Ok(expected)) = host.answer(&request(OsOp::ReadFile, vec![st("file")], None)) else { panic!("inline read") };
            let paused = pause_first(&handles, CompletionTestPhase::BeforeWork);
            let answer = host.answer(&request(OsOp::ReadFile, vec![st("file")], Some("open")));
            assert!(matches!(answer, IoAnswer::ServiceDeferred), "{answer:?}");
            let lease = paused.entered.recv_timeout(Duration::from_secs(5)).expect("read paused before work");
            assert!(matches!(host.answer(&request(OsOp::Chdir, vec![fx.s("second")], None)), IoAnswer::Os(Ok(_))));
            paused.release.send(()).unwrap();
            let (_, result) = delivered.recv_timeout(Duration::from_secs(5)).expect("read completion");
            assert_eq!(result, Ok(expected));
            terminal(&paused, lease);
            assert_eq!(handles.fs.cwd.lock().as_ref(), Some(&fx.p("second")));
        }

        #[test]
        fn finite_fs_completion_captures_original_grants_and_boot_anchors() {
            let fx = Fx::new();
            fs::create_dir(fx.p("first")).unwrap();
            fs::create_dir(fx.p("second")).unwrap();
            fs::write(fx.p("first/file"), b"first").unwrap();
            fs::write(fx.p("second/file"), b"second").unwrap();
            let mut handles = Arc::new(OsHandles { anchors: super::super::super::OsAnchors {
                start: Some(fx.p("first")), ..Default::default()
            }, ..Default::default() });
            let (host, delivered) = native(&fx, handles.clone());
            let IoAnswer::Os(Ok(expected)) = host.answer(&request(OsOp::ReadFile, vec![st("./file")], None)) else { panic!("inline anchored read") };
            let paused = pause_first(&handles, CompletionTestPhase::BeforeWork);
            let answer = host.answer(&request(OsOp::ReadFile, vec![st("./file")], Some("open")));
            assert!(matches!(answer, IoAnswer::ServiceDeferred), "{answer:?}");
            let lease = paused.entered.recv_timeout(Duration::from_secs(5)).expect("read paused before work");
            drop(host);
            Arc::get_mut(&mut handles).expect("worker owns only canonical shared tables and captured context")
                .anchors.start = Some(fx.p("second"));
            let denied = Fx::new().with_grants(|_| Vec::new());
            let later_host = native_context(&denied, handles.clone());
            assert!(matches!(later_host.answer(&request(OsOp::ReadFile, vec![st("./file")], None)), IoAnswer::Os(Err(OsError { kind: OsErrorKind::Denied, .. }))));
            paused.release.send(()).unwrap();
            let (_, result) = delivered.recv_timeout(Duration::from_secs(5)).expect("original-context completion");
            assert_eq!(result, Ok(expected));
            terminal(&paused, lease);
        }

        #[test]
        fn finite_fs_completion_denied_stat_preserves_the_original_kind_and_message() {
            let fx = Fx::new().with_grants(|root| vec![grant(format!("{}/file", root.display()), &ALL)]);
            let expected = fx.call(OsOp::Stat, vec![fx.s("denied")]).expect_err("outside the grant");
            assert_eq!(expected.kind, OsErrorKind::Denied);
            let handles = Arc::new(OsHandles::default());
            let paused = pause_first(&handles, CompletionTestPhase::BeforeClaim);
            let (host, delivered) = native(&fx, handles.clone());
            let request = |key| IoRequest::Os {
                op: OsOp::Stat, args: vec![fx.s("denied")], key,
                site: Some(CallSite {
                    orbital: None, trait_name: "Boundary".into(), follow_on: false,
                    entity_id: "singleton".into(),
                    emit: CallRoutes { failure: Some("FAILED".into()), ..Default::default() },
                }),
            };
            match host.answer(&request(None)) {
                IoAnswer::Os(Err(error)) => assert_eq!(error, expected),
                other => panic!("unkeyed refusal must remain inline: {other:?}"),
            }
            let answer = host.answer(&request(Some("open".into())));
            assert!(matches!(answer, IoAnswer::ServiceDeferred), "keyed stat refusal must defer: {answer:?}");
            let lease = paused.entered.recv_timeout(Duration::from_secs(5)).expect("refusal before claim");
            paused.release.send(()).unwrap();
            let (key, outcome) = delivered.recv_timeout(Duration::from_secs(5)).expect("deferred failure");
            assert_eq!(key, "open");
            assert_eq!(outcome.expect_err("grant refusal"), expected);
            terminal(&paused, lease);
            assert!(handles.completions.live_keys().is_empty());
            assert!(handles.fs.fds.entries.lock().is_empty());
        }

        #[test]
        fn finite_fs_completion_cancelled_open_closes_its_exact_allocation() {
            let fx = Fx::new();
            fs::write(fx.p("file"), b"payload").unwrap();
            let handles = Arc::new(OsHandles::default());
            let paused = pause_first(&handles, CompletionTestPhase::BeforeClaim);
            let (host, delivered) = native(&fx, handles.clone());
            open(&host, &fx);
            let lease = paused.entered.recv_timeout(Duration::from_secs(5)).expect("open allocated before claim");
            let allocated: Vec<_> = handles.fs.fds.entries.lock().keys().copied().collect();
            assert_eq!(allocated.len(), 1);
            assert!(handles.cancel_completion("open"));
            paused.release.send(()).unwrap();
            terminal(&paused, lease);
            assert!(!handles.fs.fds.contains(allocated[0]), "discarded Open leaked its allocated descriptor");
            assert!(handles.completions.live_keys().is_empty());
            assert!(matches!(delivered.try_recv(), Err(mpsc::TryRecvError::Empty)));
        }

        #[test]
        fn finite_fs_completion_close_all_prevents_a_late_open_handle_leak() {
            let fx = Fx::new();
            fs::write(fx.p("file"), b"payload").unwrap();
            let handles = Arc::new(OsHandles::default());
            let paused = pause_first(&handles, CompletionTestPhase::BeforeWork);
            let (host, delivered) = native(&fx, handles.clone());
            open(&host, &fx);
            let lease = paused.entered.recv_timeout(Duration::from_secs(5)).expect("worker paused before Open");
            assert!(handles.fs.fds.entries.lock().is_empty());
            handles.close_all();
            paused.release.send(()).unwrap();
            terminal(&paused, lease);
            assert!(handles.fs.fds.entries.lock().is_empty(), "Open after close_all left an unreachable descriptor");
            assert!(handles.completions.live_keys().is_empty());
            assert!(matches!(delivered.try_recv(), Err(mpsc::TryRecvError::Empty)));
        }

        #[test]
        fn finite_fs_completion_reused_key_keeps_only_the_new_open_allocation() {
            let fx = Fx::new();
            fs::write(fx.p("file"), b"payload").unwrap();
            let handles = Arc::new(OsHandles::default());
            let paused = pause_first(&handles, CompletionTestPhase::BeforeClaim);
            let (host, delivered) = native(&fx, handles.clone());
            open(&host, &fx);
            let old_lease = paused.entered.recv_timeout(Duration::from_secs(5)).expect("old Open allocated");
            let allocated: Vec<_> = handles.fs.fds.entries.lock().keys().copied().collect();
            assert_eq!(allocated.len(), 1);
            let old_fd = allocated[0];
            assert!(handles.cancel_completion("open"));
            open(&host, &fx);
            let (key, outcome) = delivered.recv_timeout(Duration::from_secs(5)).expect("new lease completes while old lease is paused");
            assert_eq!(key, "open");
            let outcome = outcome.expect("new Open succeeds");
            let Value::Number(new_fd) = get(&outcome, "fd") else { panic!("Open returned no numeric descriptor: {outcome:?}") };
            let new_fd = *new_fd as i64;
            assert_ne!(old_fd, new_fd);
            let new_lease = paused.terminal.recv_timeout(Duration::from_secs(5)).expect("new lease reaches terminal");
            assert_ne!(new_lease, old_lease);
            paused.release.send(()).unwrap();
            terminal(&paused, old_lease);
            assert!(!handles.fs.fds.contains(old_fd));
            assert!(handles.fs.fds.contains(new_fd), "old lease cleanup closed the new allocation");
            assert!(handles.completions.live_keys().is_empty());
            assert!(matches!(delivered.try_recv(), Err(mpsc::TryRecvError::Empty)));
            handles.close_all();
        }
    }

}
