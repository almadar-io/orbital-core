//! Metadata effects: access, chown and utimes.

use super::*;

impl<'a> Cx<'a> {
    pub(super) fn access(&self, args: &[Value]) -> Result<Value, OsError> {
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
        let c = c_path(&path).map_err(|e| self.io(&path, &e))?;
        // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
        let rc = unsafe { libc::chown(c.as_ptr(), uid, gid) };
        if rc != 0 {
            return Err(self.io(&path, &io::Error::last_os_error()));
        }
        Ok(obj([]))
    }

    pub(super) fn utimes(&self, args: &[Value]) -> Result<Value, OsError> {
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
