//! Path effects: copy, rename, remove, link and symlink.

use super::*;

impl<'a> Cx<'a> {
    pub(super) fn copy(&self, args: &[Value]) -> Result<Value, OsError> {
        let from = self.path(arg_str(args, 0, self.op)?, FsMode::Read, true)?;
        let to = self.write_target(arg_str(args, 1, self.op)?)?;
        fs::copy(&from, &to).map_err(|e| self.io(&from, &e))?;
        Ok(obj([]))
    }

    pub(super) fn rename(&self, args: &[Value]) -> Result<Value, OsError> {
        let from = self.path(arg_str(args, 0, self.op)?, FsMode::Remove, false)?;
        let to = self.locate(arg_str(args, 1, self.op)?, FsMode::Create, false)?;
        if !self.missing(&to)? {
            self.require(&to, FsMode::Remove)?;
        }
        fs::rename(&from, &to.real).map_err(|e| self.io(&from, &e))?;
        Ok(obj([]))
    }

    pub(super) fn remove(&self, args: &[Value]) -> Result<Value, OsError> {
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

    pub(super) fn link(&self, args: &[Value]) -> Result<Value, OsError> {
        let target = self.path(arg_str(args, 0, self.op)?, FsMode::Write, true)?;
        let path = self.path(arg_str(args, 1, self.op)?, FsMode::Create, false)?;
        fs::hard_link(&target, &path).map_err(|e| self.io(&path, &e))?;
        Ok(obj([]))
    }

    pub(super) fn symlink(&self, args: &[Value]) -> Result<Value, OsError> {
        let target = arg_str(args, 0, self.op)?;
        let path = self.path(arg_str(args, 1, self.op)?, FsMode::Create, false)?;
        std::os::unix::fs::symlink(target, &path).map_err(|e| self.io(&path, &e))?;
        Ok(obj([]))
    }
}
