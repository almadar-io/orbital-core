//! Files and descriptors: whole-file reads and writes, open, read, write, seek and truncate.

use super::*;

impl<'a> Cx<'a> {
    pub(super) fn read_file(&self, args: &[Value]) -> Result<Value, OsError> {
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

    pub(super) fn write_file(&self, args: &[Value], append: bool) -> Result<Value, OsError> {
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

    pub(super) fn parse_flags(&self, flags: &str) -> Result<Flags, OsError> {
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

    pub(super) fn open(&self, args: &[Value]) -> Result<OpenedFile, OsError> {
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

    pub(super) fn read(&self, args: &[Value]) -> Result<Value, OsError> {
        let (count, pipe, buf) = self.read_source(args)?;
        finish_read(self.op, count, pipe, buf)
    }

    /// The count, and what is left to read: a file is read now (it does not block), a pipe end is handed back
    /// for the blocking read.
    pub(super) fn read_source(&self, args: &[Value]) -> Result<(u64, Option<PipeEnd>, Vec<u8>), OsError> {
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

    pub(super) fn write(&self, args: &[Value]) -> Result<Value, OsError> {
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

    pub(super) fn seek(&self, args: &[Value]) -> Result<Value, OsError> {
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

    pub(super) fn truncate(&self, args: &[Value]) -> Result<Value, OsError> {
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
}
