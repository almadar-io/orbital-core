//! Metadata effects: access, chown and utimes.

use super::*;

impl<'a> Cx<'a> {
    pub(super) fn access(&self, args: &[Value]) -> Result<Value, OsError> {
        let path = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
        let (read, write, exec) = match arg_opt(args, 1) {
            None => (false, false, false),
            Some(_) => match arg_str(args, 1, self.op)? {
                "exists" => (false, false, false),
                "read" => (true, false, false),
                "write" => (false, true, false),
                "exec" | "execute" => (false, false, true),
                letters
                    if !letters.is_empty() && letters.chars().all(|c| "rwx".contains(c)) =>
                {
                    (letters.contains('r'), letters.contains('w'), letters.contains('x'))
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
        self.access_at(&path, read, write, exec)
    }

    /// Windows checks what libuv checks: the path exists, and `write` fails on a read-only file. Its ACLs are not
    /// consulted, so `read` and `exec` answer as `exists`.
    #[cfg(windows)]
    fn access_at(&self, path: &Path, _read: bool, write: bool, _exec: bool) -> Result<Value, OsError> {
        let ok = match fs::metadata(path) {
            Ok(md) => !(write && !md.is_dir() && md.permissions().readonly()),
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied) => false,
            Err(e) => return Err(self.io(path, &e)),
        };
        Ok(obj([("ok", Value::Bool(ok))]))
    }

    #[cfg(unix)]
    fn access_at(&self, path: &Path, read: bool, write: bool, exec: bool) -> Result<Value, OsError> {
        let mut flags = libc::F_OK;
        if read {
            flags |= libc::R_OK;
        }
        if write {
            flags |= libc::W_OK;
        }
        if exec {
            flags |= libc::X_OK;
        }
        let path = path.to_path_buf();
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

    pub(super) fn chown(&self, args: &[Value]) -> Result<Value, OsError> {
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
        self.chown_at(&path, uid, gid)
    }

    #[cfg(windows)]
    fn chown_at(&self, _path: &Path, _uid: u32, _gid: u32) -> Result<Value, OsError> {
        Err(self.err(OsErrorKind::Unsupported, "Windows has no file owner ids"))
    }

    #[cfg(unix)]
    fn chown_at(&self, path: &Path, uid: u32, gid: u32) -> Result<Value, OsError> {
        let c = c_path(path).map_err(|e| self.io(path, &e))?;
        // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
        let rc = unsafe { libc::chown(c.as_ptr(), uid, gid) };
        if rc != 0 {
            return Err(self.io(path, &io::Error::last_os_error()));
        }
        Ok(obj([]))
    }

    pub(super) fn utimes(&self, args: &[Value]) -> Result<Value, OsError> {
        let path = self.path(arg_str(args, 0, self.op)?, FsMode::Meta, true)?;
        let at = system_time(arg_i64(args, 1, self.op)?)
            .ok_or_else(|| self.err(OsErrorKind::Invalid, "atime is out of range"))?;
        let mt = system_time(arg_i64(args, 2, self.op)?)
            .ok_or_else(|| self.err(OsErrorKind::Invalid, "mtime is out of range"))?;
        let file = times_handle(&path).map_err(|e| self.io(&path, &e))?;
        file.set_times(fs::FileTimes::new().set_accessed(at).set_modified(mt))
            .map_err(|e| self.io(&path, &e))?;
        Ok(obj([]))
    }
}

#[cfg(unix)]
fn times_handle(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

/// Setting times needs `FILE_WRITE_ATTRIBUTES` on Windows, which a read-only open does not carry.
#[cfg(windows)]
fn times_handle(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).open(path)
}
