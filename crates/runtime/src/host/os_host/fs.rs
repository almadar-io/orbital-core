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
    ChildStdout(Arc<ReadPipe<std::process::ChildStdout>>),
    ChildStderr(Arc<ReadPipe<std::process::ChildStderr>>),
}

impl FdEntry {
    pub(super) fn cancel_reads(&self) {
        match self {
            Self::ChildStdout(pipe) => pipe.cancel(),
            Self::ChildStderr(pipe) => pipe.cancel(),
            Self::File(_) | Self::ChildStdin(_) => {}
        }
    }
}

/// The read end of a child pipe. A reader waits on the descriptor in bounded polls and gives up once `os/close`
/// has cancelled the pipe, so closing interrupts a read blocked on it.
pub(super) struct ReadPipe<T> {
    inner: Mutex<T>,
    #[cfg(unix)]
    raw: std::os::fd::RawFd,
    cancelled: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd> ReadPipe<T> {
    pub(super) fn new(pipe: T) -> Self {
        Self { raw: pipe.as_raw_fd(), inner: Mutex::new(pipe), cancelled: std::sync::atomic::AtomicBool::new(false) }
    }
}

impl<T> ReadPipe<T> {
    pub(super) fn lock(&self) -> orbital_core_base::sync::MutexGuard<'_, T> {
        self.inner.lock()
    }

    #[cfg(unix)]
    pub(super) fn raw(&self) -> std::os::fd::RawFd {
        self.raw
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A pipe end cloned out of the table, so a blocking pipe call never holds the table lock.
enum PipeEnd {
    Stdin(Arc<Mutex<std::process::ChildStdin>>),
    Stdout(Arc<ReadPipe<std::process::ChildStdout>>),
    Stderr(Arc<ReadPipe<std::process::ChildStderr>>),
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
        for entry in self.fds.drain() {
            entry.cancel_reads();
        }
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
mod ops;

#[cfg(all(test, unix))]
mod tests;
