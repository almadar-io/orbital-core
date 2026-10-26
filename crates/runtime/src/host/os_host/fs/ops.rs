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

mod files;
mod paths;
mod dirs;
mod metadata;
