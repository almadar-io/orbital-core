//! The `process` family: spawn, wait, kill, pids, signals, environment, stdio and tty.

use super::args::{arg_bytes, arg_i64, arg_opt, arg_str, bytes_value, denied};
use super::fs::FdEntry;
use super::OsHost;
use crate::resumable::CallSite;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::os_grants::{FsMode, StdioKind};
use orbital_core_base::value::{Value, ValueMap};
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

const STDIN_CAP: usize = 64 * 1024;
const STDIN_CHUNK: usize = 8 * 1024;

/// Children this program spawned, by pid, and the program's environment overlay (`None` = unset).
#[derive(Default)]
pub struct ProcTable {
    pub(super) children: Arc<Mutex<HashMap<i64, std::process::Child>>>,
    env: Mutex<HashMap<String, Option<String>>>,
}

impl ProcTable {
    pub(super) fn clear(&self) {
        let mut children = self.children.lock();
        for child in children.values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        children.clear();
        self.env.lock().clear();
    }
}

fn err(kind: OsErrorKind, op: OsOp, what: impl std::fmt::Display) -> OsError {
    OsError::new(kind, format!("{}: {what}", op.name()))
}

fn invalid(op: OsOp, what: impl std::fmt::Display) -> OsError {
    err(OsErrorKind::Invalid, op, what)
}

fn io_error(op: OsOp, e: &std::io::Error) -> OsError {
    let kind = match e.kind() {
        std::io::ErrorKind::NotFound => OsErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied => OsErrorKind::Denied,
        std::io::ErrorKind::InvalidInput => OsErrorKind::Invalid,
        _ => OsErrorKind::Io,
    };
    err(kind, op, e)
}

fn object(fields: Vec<(&str, Value)>) -> Value {
    let mut map = ValueMap::new();
    for (k, v) in fields {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

fn number(n: i64) -> Value {
    Value::Number(n as f64)
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('=') && !name.contains('\0')
}

fn lookup_env(host: &OsHost<'_>, name: &str) -> Result<Option<String>, std::io::Error> {
    if let Some(entry) = host.handles.process.env.lock().get(name) {
        return Ok(entry.clone());
    }
    match std::env::var(name) {
        Ok(v) => Ok(Some(v)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(std::io::Error::other(e.to_string())),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Inherit,
    Pipe,
    Null,
}

fn stdio_mode(
    opts: Option<&ValueMap>,
    key: &str,
    kind: StdioKind,
    host: &OsHost<'_>,
) -> Result<Mode, OsError> {
    let op = OsOp::Spawn;
    let mode = match opts.and_then(|m| m.get(key)) {
        None | Some(Value::Null) => Mode::Null,
        Some(Value::String(s)) => match s.as_str() {
            "inherit" => Mode::Inherit,
            "pipe" => Mode::Pipe,
            "null" => Mode::Null,
            other => {
                return Err(invalid(
                    op,
                    format!("{key} must be inherit, pipe or null, got {other}"),
                ))
            }
        },
        Some(_) => return Err(invalid(op, format!("{key} must be a string"))),
    };
    if mode == Mode::Inherit && !host.grants.stdio_allows(kind) {
        return Err(denied(op, &format!("inheriting {key} is not granted")));
    }
    Ok(mode)
}

fn as_stdio(mode: Mode) -> Stdio {
    match mode {
        Mode::Inherit => Stdio::inherit(),
        Mode::Pipe => Stdio::piped(),
        Mode::Null => Stdio::null(),
    }
}

/// The child leads a new session (Unix) or process group without a console (Windows), so it outlives this process
/// and a terminal's interrupt to our group does not reach it.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

fn spawn(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Spawn;
    let command = arg_str(args, 0, op)?;
    let argv: Vec<&str> = match args.get(1) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.as_str()),
                _ => Err(invalid(op, "argument 2 must be a list of strings")),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(invalid(op, "argument 2 must be a list of strings")),
    };
    let opts = match arg_opt(args, 2) {
        None => None,
        Some(Value::Object(m)) => Some(m),
        Some(_) => return Err(invalid(op, "argument 3 must be an object")),
    };
    if let Some(m) = opts {
        for key in m.keys() {
            if !["cwd", "env", "stdin", "stdout", "stderr", "detached"].contains(&key.as_str()) {
                return Err(invalid(op, format!("unknown option {key}")));
            }
        }
    }
    if !host.grants.exec_allows(command) {
        return Err(denied(op, &format!("command {command} is not granted")));
    }
    let cwd = match opts.and_then(|m| m.get("cwd")) {
        None | Some(Value::Null) => None,
        Some(Value::String(p)) => {
            let dir = host
                .handles
                .anchors
                .expand(p)
                .map_err(|m| invalid(op, m))?
                .unwrap_or_else(|| std::path::PathBuf::from(p));
            if !host.grants.fs_allows_at(&dir, FsMode::Read, &host.handles.anchors) {
                return Err(denied(op, &format!("cwd {p} is not granted for read")));
            }
            Some(dir)
        }
        Some(_) => return Err(invalid(op, "cwd must be a string")),
    };
    let stdin_mode = stdio_mode(opts, "stdin", StdioKind::In, host)?;
    let stdout_mode = stdio_mode(opts, "stdout", StdioKind::Out, host)?;
    let stderr_mode = stdio_mode(opts, "stderr", StdioKind::Err, host)?;
    let detached = match opts.and_then(|m| m.get("detached")) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(invalid(op, "detached must be a boolean")),
    };
    if detached && [stdin_mode, stdout_mode, stderr_mode].contains(&Mode::Pipe) {
        return Err(invalid(op, "a detached child takes no piped stream"));
    }

    let mut vars: HashMap<String, String> = HashMap::new();
    for (name, value) in host.handles.process.env.lock().iter() {
        if let Some(v) = value {
            vars.insert(name.clone(), v.clone());
        }
    }
    match opts.and_then(|m| m.get("env")) {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (name, value) in map {
                if !valid_env_name(name) {
                    return Err(invalid(op, format!("bad environment name {name}")));
                }
                match value {
                    Value::String(v) if !v.contains('\0') => {
                        vars.insert(name.clone(), v.clone());
                    }
                    Value::Null => {
                        if !host.grants.allows(&orbital_core_base::os_grants::OsRequirement::inherited_env(name)) {
                            return Err(denied(op, &format!("environment {name} is not granted")));
                        }
                        match lookup_env(host, name).map_err(|e| io_error(op, &e))? {
                            Some(v) => {
                                vars.insert(name.clone(), v);
                            }
                            None => {
                                vars.remove(name);
                            }
                        }
                    }
                    _ => {
                        return Err(invalid(
                            op,
                            format!("environment {name} must be a string or null"),
                        ))
                    }
                }
            }
        }
        Some(_) => return Err(invalid(op, "env must be an object")),
    }

    let mut cmd = Command::new(command);
    cmd.args(&argv)
        .env_clear()
        .envs(&vars)
        .stdin(as_stdio(stdin_mode))
        .stdout(as_stdio(stdout_mode))
        .stderr(as_stdio(stderr_mode));
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if detached {
        detach(&mut cmd);
    }
    let mut child = cmd.spawn().map_err(|e| io_error(op, &e))?;
    let pid = i64::from(child.id());
    let fds = &host.handles.fs.fds;
    let mut fields = vec![("pid", number(pid))];
    if let Some(s) = child.stdin.take() {
        fields.push((
            "stdin",
            number(fds.insert(FdEntry::ChildStdin(Arc::new(Mutex::new(s))))),
        ));
    }
    if let Some(s) = child.stdout.take() {
        fields.push((
            "stdout",
            number(fds.insert(FdEntry::ChildStdout(Arc::new(Mutex::new(s))))),
        ));
    }
    if let Some(s) = child.stderr.take() {
        fields.push((
            "stderr",
            number(fds.insert(FdEntry::ChildStderr(Arc::new(Mutex::new(s))))),
        ));
    }
    host.handles.process.children.lock().insert(pid, child);
    Ok(object(fields))
}

#[cfg(unix)]
fn await_exit(pid: i64) -> std::io::Result<()> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(not(unix))]
fn await_exit(_pid: i64) -> std::io::Result<()> {
    Ok(())
}

fn wait(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Wait;
    let pid = arg_i64(args, 0, op)?;
    let table = &host.handles.process.children;
    if !table.lock().contains_key(&pid) {
        return Err(err(
            OsErrorKind::Closed,
            op,
            format!("pid {pid} is not a spawned process"),
        ));
    }
    await_exit(pid).map_err(|e| io_error(op, &e))?;
    reap(pid, table)
}

fn reap(pid: i64, table: &Mutex<HashMap<i64, std::process::Child>>) -> Result<Value, OsError> {
    let op = OsOp::Wait;
    let Some(mut child) = table.lock().remove(&pid) else {
        return Err(err(
            OsErrorKind::Closed,
            op,
            format!("pid {pid} was already reaped"),
        ));
    };
    let status = child.wait().map_err(|e| io_error(op, &e))?;
    let mut fields = Vec::new();
    if let Some(code) = status.code() {
        fields.push(("code", number(i64::from(code))));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            fields.push(("signal", number(i64::from(sig))));
        }
    }
    Ok(object(fields))
}

/// `os/wait` under a completion route: the child is watched on a thread of its own and the call returns at once.
/// When it ends, the outcome (`{ code?, signal? }`, or the failure) goes to the host's completion sink unless the
/// key was cancelled first, in which case the child is left unreaped for a later `os/wait`.
pub(super) fn wait_later(
    args: &[Value],
    key: &str,
    site: &CallSite,
    host: &OsHost<'_>,
) -> Result<(), OsError> {
    let op = OsOp::Wait;
    let pid = arg_i64(args, 0, op)?;
    let table = host.handles.process.children.clone();
    if !table.lock().contains_key(&pid) {
        return Err(err(
            OsErrorKind::Closed,
            op,
            format!("pid {pid} is not a spawned process"),
        ));
    }
    let completions = host.handles.completions.clone();
    if !completions.begin(key) {
        return Err(invalid(
            op,
            format!("key {key} already has a completion pending"),
        ));
    }
    let (key, site) = (key.to_string(), site.clone());
    let watcher = {
        let (completions, key) = (completions.clone(), key.clone());
        std::thread::Builder::new()
            .name(format!("os-wait-{pid}"))
            .spawn(move || {
                let awaited = await_exit(pid).map_err(|e| io_error(op, &e));
                if !completions.claim(&key) {
                    return;
                }
                let outcome = awaited.and_then(|()| reap(pid, &table));
                completions.deliver(&key, &site, outcome);
            })
    };
    watcher.map(drop).map_err(|e| {
        completions.cancel(&key);
        io_error(op, &e)
    })
}

#[cfg(unix)]
fn signal_number(value: &Value, op: OsOp) -> Result<i32, OsError> {
    const SET: [(&str, i32); 9] = [
        ("TERM", libc::SIGTERM),
        ("KILL", libc::SIGKILL),
        ("INT", libc::SIGINT),
        ("HUP", libc::SIGHUP),
        ("QUIT", libc::SIGQUIT),
        ("USR1", libc::SIGUSR1),
        ("USR2", libc::SIGUSR2),
        ("STOP", libc::SIGSTOP),
        ("CONT", libc::SIGCONT),
    ];
    let found = match value {
        Value::String(name) => SET.iter().find(|(n, _)| n == name).map(|(_, s)| *s),
        Value::Number(n) if n.fract() == 0.0 => SET
            .iter()
            .find(|(_, s)| f64::from(*s) == *n)
            .map(|(_, s)| *s),
        _ => None,
    };
    found.ok_or_else(|| {
        invalid(op, "signal must be one of TERM, KILL, INT, HUP, QUIT, USR1, USR2, STOP, CONT (or its number)")
    })
}

fn kill(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Kill;
    let pid = arg_i64(args, 0, op)?;
    #[cfg(unix)]
    {
        let signal = signal_number(args.get(1).unwrap_or(&Value::Null), op)?;
        let children = host.handles.process.children.lock();
        if !children.contains_key(&pid) {
            return Err(err(
                OsErrorKind::Closed,
                op,
                format!("pid {pid} is not a spawned process"),
            ));
        }
        let target = libc::pid_t::try_from(pid).map_err(|_| invalid(op, "pid out of range"))?;
        if unsafe { libc::kill(target, signal) } != 0 {
            return Err(io_error(op, &std::io::Error::last_os_error()));
        }
        Ok(object(vec![]))
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, host);
        Err(err(
            OsErrorKind::Unsupported,
            op,
            "signals are not available on this platform",
        ))
    }
}

fn getenv(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Getenv;
    let name = arg_str(args, 0, op)?;
    if !host.grants.env_allows(name) {
        return Err(denied(op, &format!("environment {name} is not granted")));
    }
    let value = lookup_env(host, name).map_err(|e| io_error(op, &e))?;
    Ok(object(vec![(
        "value",
        value.map_or(Value::Null, Value::String),
    )]))
}

fn setenv(args: &[Value], host: &OsHost<'_>, set: bool) -> Result<Value, OsError> {
    let op = if set { OsOp::Setenv } else { OsOp::Unsetenv };
    let name = arg_str(args, 0, op)?;
    if !host.grants.env_allows(name) {
        return Err(denied(op, &format!("environment {name} is not granted")));
    }
    if !valid_env_name(name) {
        return Err(invalid(op, format!("bad environment name {name}")));
    }
    let entry = if set {
        let value = arg_str(args, 1, op)?;
        if value.contains('\0') {
            return Err(invalid(op, "value contains a NUL byte"));
        }
        Some(value.to_string())
    } else {
        None
    };
    host.handles
        .process
        .env
        .lock()
        .insert(name.to_string(), entry);
    Ok(object(vec![]))
}

fn environ(host: &OsHost<'_>) -> Value {
    let mut names: BTreeSet<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| host.grants.env_allows(k))
        .collect();
    for (name, value) in host.handles.process.env.lock().iter() {
        if value.is_some() && host.grants.env_allows(name) {
            names.insert(name.clone());
        } else if value.is_none() {
            names.remove(name);
        }
    }
    object(vec![(
        "names",
        Value::Array(names.into_iter().map(Value::String).collect()),
    )])
}

fn write_stdio(args: &[Value], host: &OsHost<'_>, err_stream: bool) -> Result<Value, OsError> {
    let op = if err_stream {
        OsOp::StderrWrite
    } else {
        OsOp::StdoutWrite
    };
    let kind = if err_stream {
        StdioKind::Err
    } else {
        StdioKind::Out
    };
    let data = arg_bytes(args, 0, op)?;
    if !host.grants.stdio_allows(kind) {
        return Err(denied(op, "stdio is not granted"));
    }
    let written = if err_stream {
        let mut s = std::io::stderr().lock();
        s.write_all(&data).and_then(|()| s.flush())
    } else {
        let mut s = std::io::stdout().lock();
        s.write_all(&data).and_then(|()| s.flush())
    };
    written.map_err(|e| io_error(op, &e))?;
    Ok(object(vec![("bytes", number(data.len() as i64))]))
}

fn std_fd(args: &[Value], host: &OsHost<'_>, op: OsOp) -> Result<i32, OsError> {
    let fd = arg_i64(args, 0, op)?;
    let kind = match fd {
        0 => StdioKind::In,
        1 => StdioKind::Out,
        2 => StdioKind::Err,
        _ => {
            return Err(err(
                OsErrorKind::Unsupported,
                op,
                "only fds 0, 1 and 2 are supported",
            ))
        }
    };
    if !host.grants.stdio_allows(kind) {
        return Err(denied(op, "stdio is not granted"));
    }
    Ok(fd as i32)
}

#[cfg(unix)]
fn is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

#[cfg(not(unix))]
fn is_tty(_fd: i32) -> bool {
    false
}

fn tty_size(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::TtySize;
    let fd = std_fd(args, host, op)?;
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
            return Err(err(
                OsErrorKind::Io,
                op,
                format!("fd {fd} is not a terminal"),
            ));
        }
        Ok(object(vec![
            ("rows", number(i64::from(ws.ws_row))),
            ("cols", number(i64::from(ws.ws_col))),
        ]))
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(err(
            OsErrorKind::Unsupported,
            op,
            "tty size is not available on this platform",
        ))
    }
}

/// The per-chunk `count` of `os/stdin-read` (default one chunk), checked with the grant and the terminal refusal.
fn stdin_count(args: &[Value], host: &OsHost<'_>) -> Result<usize, OsError> {
    let op = OsOp::StdinRead;
    let count = match arg_opt(args, 0) {
        None => STDIN_CHUNK,
        Some(_) => {
            let n = arg_i64(args, 0, op)?;
            usize::try_from(n)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid(op, "count must be a positive integer"))?
        }
    };
    if !host.grants.stdio_allows(StdioKind::In) {
        return Err(denied(op, "stdio in is not granted"));
    }
    if is_tty(0) {
        return Err(err(
            OsErrorKind::Unsupported,
            op,
            "stdin is an interactive terminal; a bounded read would block indefinitely",
        ));
    }
    Ok(count)
}

/// `os/stdin-read` answered in place: one read of at most `count` bytes, `{ data, eof }`.
fn stdin_read(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let count = stdin_count(args, host)?;
    read_once(&mut std::io::stdin().lock(), count)
}

fn read_once(reader: &mut impl Read, count: usize) -> Result<Value, OsError> {
    let mut buf = vec![0u8; count.min(STDIN_CAP)];
    loop {
        match reader.read(&mut buf) {
            Ok(n) => {
                return Ok(object(vec![
                    ("data", bytes_value(&buf[..n])),
                    ("eof", Value::Bool(n == 0)),
                ]))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error(OsOp::StdinRead, &e)),
        }
    }
}

/// `os/stdin-read` keyed under a route when the host has a completion sink: the call is answered deferred and stdin
/// is read on a thread of its own. Each chunk (at most `count` bytes) goes to the `onMessage` route as `{ data }`, one
/// dispatch at a time, and the end of stdin to the success route as `{ eof: true }`. Cancelling the key ends the
/// stream; a read already blocked drops the chunk it returns.
pub(super) fn stdin_later(
    args: &[Value],
    key: &str,
    site: &CallSite,
    host: &OsHost<'_>,
) -> Result<(), OsError> {
    let count = stdin_count(args, host)?;
    stream_stdin(
        std::io::stdin(),
        count,
        key,
        site,
        &host.handles.completions,
    )
}

fn stream_stdin<R: Read + Send + 'static>(
    mut reader: R,
    count: usize,
    key: &str,
    site: &CallSite,
    completions: &Arc<super::completion::Completions>,
) -> Result<(), OsError> {
    let op = OsOp::StdinRead;
    if !completions.begin(key) {
        return Err(invalid(op, format!("key {key} already has a stream open")));
    }
    let (owned_key, owned_site) = (key.to_string(), site.clone());
    let started = std::thread::Builder::new()
        .name(format!("os-stdin-{key}"))
        .spawn({
            let completions = completions.clone();
            move || {
                let (key, site) = (owned_key, owned_site);
                let mut buf = vec![0u8; count.min(STDIN_CAP)];
                loop {
                    if !completions.is_live(&key) {
                        return;
                    }
                    let outcome = match reader.read(&mut buf) {
                        Ok(0) => Ok(object(vec![("eof", Value::Bool(true))])),
                        Ok(n) => {
                            let chunk = object(vec![("data", bytes_value(&buf[..n]))]);
                            if completions.message(&key, &site, chunk) {
                                continue;
                            }
                            return;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => Err(io_error(op, &e)),
                    };
                    if completions.claim(&key) {
                        completions.deliver(&key, &site, outcome);
                    }
                    return;
                }
            }
        });
    started.map(drop).map_err(|e| {
        completions.cancel(key);
        io_error(op, &e)
    })
}

fn current_exe() -> Result<Value, OsError> {
    let op = OsOp::CurrentExe;
    let path = std::env::current_exe().map_err(|e| io_error(op, &e))?;
    match path.to_str() {
        Some(p) => Ok(object(vec![("path", Value::String(p.to_string()))])),
        None => Err(invalid(op, format!("{} is not UTF-8", path.display()))),
    }
}

pub(super) fn answer(
    op: OsOp,
    args: &[Value],
    _key: Option<&str>,
    _site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Option<Result<Value, OsError>> {
    Some(match op {
        OsOp::Spawn => spawn(args, host),
        OsOp::Wait => wait(args, host),
        OsOp::Kill => kill(args, host),
        OsOp::Pid => Ok(object(vec![("pid", number(i64::from(std::process::id())))])),
        OsOp::Ppid => {
            #[cfg(unix)]
            {
                Ok(object(vec![(
                    "pid",
                    number(i64::from(unsafe { libc::getppid() })),
                )]))
            }
            #[cfg(not(unix))]
            {
                Err(err(
                    OsErrorKind::Unsupported,
                    op,
                    "not available on this platform",
                ))
            }
        }
        OsOp::CurrentExe => current_exe(),
        OsOp::Getenv => getenv(args, host),
        OsOp::Setenv => setenv(args, host, true),
        OsOp::Unsetenv => setenv(args, host, false),
        OsOp::Environ => Ok(environ(host)),
        OsOp::StdoutWrite => write_stdio(args, host, false),
        OsOp::StderrWrite => write_stdio(args, host, true),
        OsOp::Isatty => {
            std_fd(args, host, op).map(|fd| object(vec![("ok", Value::Bool(is_tty(fd)))]))
        }
        OsOp::TtySize => tty_size(args, host),
        OsOp::StdinRead => stdin_read(args, host),
        OsOp::WatchProcess | OsOp::WatchSignal | OsOp::WatchEnv => Err(err(
            OsErrorKind::Unsupported,
            op,
            "streaming host not implemented",
        )),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::completion::OsCompletionSink;
    use super::super::{answer_os, OsHandles};
    use super::*;
    use crate::resumable::{CallRoutes, IoAnswer, IoRequest};
    use orbital_core_base::os_grants::{FsGrant, OsGrants};

    fn s(t: &str) -> Value {
        Value::String(t.to_string())
    }

    fn list(items: &[&str]) -> Value {
        Value::Array(items.iter().map(|i| s(i)).collect())
    }

    fn grants() -> OsGrants {
        OsGrants {
            exec: vec!["/bin/sh".to_string()],
            ..OsGrants::default()
        }
    }

    fn call(g: &OsGrants, h: &OsHandles, op: OsOp, args: Vec<Value>) -> Result<Value, OsError> {
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let host = OsHost {
            grants: g,
            handles: h,
            on_event: &on_event,
        };
        answer(op, &args, None, None, &host).expect("process op")
    }

    fn field(v: &Value, k: &str) -> Value {
        match v {
            Value::Object(m) => m.get(k).cloned().unwrap_or(Value::Null),
            other => panic!("not an object: {other:?}"),
        }
    }

    fn int(v: Value) -> i64 {
        match v {
            Value::Number(n) => n as i64,
            other => panic!("not a number: {other:?}"),
        }
    }

    fn sh(script: &str, rest: &[&str]) -> Vec<Value> {
        let mut argv = vec!["-c", script];
        argv.extend_from_slice(rest);
        vec![s("/bin/sh"), list(&argv)]
    }

    fn opts(fields: Vec<(&str, Value)>) -> Value {
        object(fields)
    }

    fn read_all(h: &OsHandles, fd: i64) -> Vec<u8> {
        let mut out = Vec::new();
        h.fs.fds
            .with(fd, |e| match e {
                FdEntry::ChildStdout(p) => p.lock().read_to_end(&mut out).map(|_| ()),
                FdEntry::ChildStderr(p) => p.lock().read_to_end(&mut out).map(|_| ()),
                _ => panic!("not a read pipe"),
            })
            .expect("fd")
            .expect("read");
        out
    }

    fn spawn_out(
        g: &OsGrants,
        h: &OsHandles,
        script: &str,
        rest: &[&str],
        extra: Vec<(&str, Value)>,
    ) -> Value {
        let mut o = vec![("stdout", s("pipe"))];
        o.extend(extra);
        let mut args = sh(script, rest);
        args.push(opts(o));
        call(g, h, OsOp::Spawn, args).expect("spawn")
    }

    #[test]
    fn spawn_pipes_stdout_into_the_fd_table() {
        let (g, h) = (grants(), OsHandles::default());
        let r = spawn_out(&g, &h, "printf hello", &[], vec![]);
        assert_eq!(read_all(&h, int(field(&r, "stdout"))), b"hello");
        let done = call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
        assert_eq!(field(&done, "code"), Value::Number(0.0));
    }

    #[cfg(unix)]
    #[test]
    fn a_detached_child_runs_in_its_own_session_and_an_attached_one_in_ours() {
        let (g, h) = (grants(), OsHandles::default());
        let start = |detached: bool| {
            let mut args = sh("exec sleep 30", &[]);
            args.push(opts(vec![("detached", Value::Bool(detached))]));
            int(field(&call(&g, &h, OsOp::Spawn, args).expect("spawn"), "pid"))
        };
        let (detached, attached) = (start(true), start(false));
        let ours = unsafe { libc::getsid(0) };
        assert_eq!(unsafe { libc::getsid(detached as libc::pid_t) }, detached as libc::pid_t, "a detached child leads its own session");
        assert_eq!(unsafe { libc::getsid(attached as libc::pid_t) }, ours, "control: an attached child stays in ours");
        for pid in [detached, attached] {
            call(&g, &h, OsOp::Kill, vec![Value::Number(pid as f64), s("KILL")]).expect("kill");
        }
    }

    #[test]
    fn edge_a_detached_child_takes_no_pipe_and_detached_must_be_a_boolean() {
        let (g, h) = (grants(), OsHandles::default());
        let mut piped = sh("true", &[]);
        piped.push(opts(vec![("stdout", s("pipe")), ("detached", Value::Bool(true))]));
        let refused = call(&g, &h, OsOp::Spawn, piped).expect_err("a pipe would tie it to us");
        assert_eq!(refused.kind, OsErrorKind::Invalid);
        assert!(refused.message.contains("takes no piped stream"), "{}", refused.message);
        let mut stringly = sh("true", &[]);
        stringly.push(opts(vec![("detached", s("yes"))]));
        let refused = call(&g, &h, OsOp::Spawn, stringly).expect_err("not a boolean");
        assert_eq!(refused.kind, OsErrorKind::Invalid);
        assert!(refused.message.contains("detached must be a boolean"), "{}", refused.message);
    }

    #[test]
    fn exit_code_propagates_and_wait_reaps_once() {
        let (g, h) = (grants(), OsHandles::default());
        let r = call(&g, &h, OsOp::Spawn, sh("exit 7", &[])).expect("spawn");
        let pid = field(&r, "pid");
        let done = call(&g, &h, OsOp::Wait, vec![pid.clone()]).expect("wait");
        assert_eq!(field(&done, "code"), Value::Number(7.0));
        let again = call(&g, &h, OsOp::Wait, vec![pid]).expect_err("reaped");
        assert_eq!(again.kind, OsErrorKind::Closed);
    }

    #[test]
    fn kill_term_ends_a_sleeping_child_with_a_signal() {
        let (g, h) = (grants(), OsHandles::default());
        let r = call(&g, &h, OsOp::Spawn, sh("exec sleep 30", &[])).expect("spawn");
        let pid = field(&r, "pid");
        call(&g, &h, OsOp::Kill, vec![pid.clone(), s("TERM")]).expect("kill");
        let done = call(&g, &h, OsOp::Wait, vec![pid]).expect("wait");
        assert_eq!(field(&done, "signal"), Value::Number(15.0));
        assert_eq!(field(&done, "code"), Value::Null);
    }

    #[test]
    fn a_signal_outside_the_closed_set_is_invalid() {
        let (g, h) = (grants(), OsHandles::default());
        let r = call(&g, &h, OsOp::Spawn, sh("exec sleep 30", &[])).expect("spawn");
        let pid = field(&r, "pid");
        for bad in [s("SEGV"), Value::Number(11.0), s("term")] {
            let e = call(&g, &h, OsOp::Kill, vec![pid.clone(), bad]).expect_err("bad signal");
            assert_eq!(e.kind, OsErrorKind::Invalid);
        }
        call(&g, &h, OsOp::Kill, vec![pid.clone(), Value::Number(9.0)]).expect("numeric KILL");
        call(&g, &h, OsOp::Wait, vec![pid]).expect("wait");
    }

    #[test]
    fn a_command_outside_the_grant_is_denied_and_creates_no_child() {
        let (g, h) = (grants(), OsHandles::default());
        let e = call(&g, &h, OsOp::Spawn, vec![s("/bin/echo"), list(&["x"])]).expect_err("denied");
        assert_eq!(e.kind, OsErrorKind::Denied);
        assert!(h.process.children.lock().is_empty());
        let e = call(&OsGrants::default(), &h, OsOp::Spawn, sh("true", &[])).expect_err("denied");
        assert_eq!(e.kind, OsErrorKind::Denied);
        assert!(h.process.children.lock().is_empty());
    }

    #[test]
    fn a_granted_command_that_does_not_exist_is_not_found() {
        let g = OsGrants {
            exec: vec!["/no/such/binary".to_string()],
            ..OsGrants::default()
        };
        let h = OsHandles::default();
        let e = call(&g, &h, OsOp::Spawn, vec![s("/no/such/binary"), list(&[])]).expect_err("nf");
        assert_eq!(e.kind, OsErrorKind::NotFound);
    }

    #[test]
    fn shell_metacharacters_in_args_are_passed_literally() {
        let (g, h) = (grants(), OsHandles::default());
        let marker = std::env::temp_dir().join(format!("orb-os-meta-{}", std::process::id()));
        let payload = format!("; touch {}", marker.display());
        let r = spawn_out(&g, &h, "printf %s \"$1\"", &["sh", &payload], vec![]);
        assert_eq!(read_all(&h, int(field(&r, "stdout"))), payload.as_bytes());
        call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
        assert!(!marker.exists());
    }

    #[test]
    fn command_substitution_in_args_is_never_expanded() {
        let (g, h) = (grants(), OsHandles::default());
        let r = spawn_out(
            &g,
            &h,
            "printf %s \"$1\"",
            &["sh", "$(echo hacked)`echo hacked`"],
            vec![],
        );
        assert_eq!(
            read_all(&h, int(field(&r, "stdout"))),
            b"$(echo hacked)`echo hacked`"
        );
        call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
    }

    #[test]
    fn the_child_environment_is_cleared_to_the_given_vars() {
        let (g, h) = (grants(), OsHandles::default());
        let r = spawn_out(
            &g,
            &h,
            "printf %s \"${FOO}|${HOME-unset}|${USER-unset}\"",
            &[],
            vec![("env", object(vec![("FOO", s("bar"))]))],
        );
        assert_eq!(read_all(&h, int(field(&r, "stdout"))), b"bar|unset|unset");
        call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
    }

    #[test]
    fn a_null_env_entry_passes_the_host_value_only_when_granted() {
        let h = OsHandles::default();
        let denied_run = call(&grants(), &h, OsOp::Spawn, {
            let mut a = sh("true", &[]);
            a.push(opts(vec![("env", object(vec![("PATH", Value::Null)]))]));
            a
        })
        .expect_err("denied");
        assert_eq!(denied_run.kind, OsErrorKind::Denied);
        let g = OsGrants {
            env: vec!["PATH".to_string()],
            ..grants()
        };
        let r = spawn_out(
            &g,
            &h,
            "test -n \"$PATH\" && printf yes",
            &[],
            vec![("env", object(vec![("PATH", Value::Null)]))],
        );
        assert_eq!(read_all(&h, int(field(&r, "stdout"))), b"yes");
        call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
    }

    #[test]
    fn unknown_pids_are_closed_and_the_host_pid_is_never_killable() {
        let (g, h) = (grants(), OsHandles::default());
        for op_args in [
            (OsOp::Wait, vec![Value::Number(999_999.0)]),
            (OsOp::Kill, vec![Value::Number(999_999.0), s("TERM")]),
            (
                OsOp::Kill,
                vec![Value::Number(f64::from(std::process::id())), s("TERM")],
            ),
        ] {
            let e = call(&g, &h, op_args.0, op_args.1).expect_err("closed");
            assert_eq!(e.kind, OsErrorKind::Closed);
        }
    }

    #[test]
    fn getenv_needs_a_grant_and_reports_null_when_unset() {
        let h = OsHandles::default();
        let e = call(&OsGrants::default(), &h, OsOp::Getenv, vec![s("PATH")]).expect_err("denied");
        assert_eq!(e.kind, OsErrorKind::Denied);
        let g = OsGrants {
            env: vec!["PATH".to_string(), "ZZ_NEVER_SET_X".to_string()],
            ..OsGrants::default()
        };
        let set = call(&g, &h, OsOp::Getenv, vec![s("PATH")]).expect("get");
        assert!(matches!(field(&set, "value"), Value::String(_)));
        let unset = call(&g, &h, OsOp::Getenv, vec![s("ZZ_NEVER_SET_X")]).expect("get");
        assert_eq!(field(&unset, "value"), Value::Null);
    }

    #[test]
    fn environ_lists_only_granted_names() {
        let h = OsHandles::default();
        let g = OsGrants {
            env: vec!["PATH".to_string(), "ZZ_NEVER_SET_X".to_string()],
            ..OsGrants::default()
        };
        let r = call(&g, &h, OsOp::Environ, vec![]).expect("environ");
        assert_eq!(field(&r, "names"), Value::Array(vec![s("PATH")]));
        let none = call(&OsGrants::default(), &h, OsOp::Environ, vec![]).expect("environ");
        assert_eq!(field(&none, "names"), Value::Array(vec![]));
    }

    #[test]
    fn the_setenv_overlay_reaches_spawn_but_not_the_process_environment() {
        let h = OsHandles::default();
        let g = OsGrants {
            env: vec!["ZZ_OVERLAY_T".to_string()],
            ..grants()
        };
        let denied_set =
            call(&grants(), &h, OsOp::Setenv, vec![s("ZZ_OVERLAY_T"), s("v")]).expect_err("denied");
        assert_eq!(denied_set.kind, OsErrorKind::Denied);
        call(&g, &h, OsOp::Setenv, vec![s("ZZ_OVERLAY_T"), s("v1")]).expect("set");
        let got = call(&g, &h, OsOp::Getenv, vec![s("ZZ_OVERLAY_T")]).expect("get");
        assert_eq!(field(&got, "value"), s("v1"));
        let names = call(&g, &h, OsOp::Environ, vec![]).expect("environ");
        assert_eq!(
            field(&names, "names"),
            Value::Array(vec![s("ZZ_OVERLAY_T")])
        );
        let r = spawn_out(&g, &h, "printf %s \"$ZZ_OVERLAY_T\"", &[], vec![]);
        assert_eq!(read_all(&h, int(field(&r, "stdout"))), b"v1");
        call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
        assert!(std::env::var("ZZ_OVERLAY_T").is_err());
        call(&g, &h, OsOp::Unsetenv, vec![s("ZZ_OVERLAY_T")]).expect("unset");
        let gone = call(&g, &h, OsOp::Getenv, vec![s("ZZ_OVERLAY_T")]).expect("get");
        assert_eq!(field(&gone, "value"), Value::Null);
    }

    #[test]
    fn a_stdin_pipe_feeds_the_child() {
        let (g, h) = (grants(), OsHandles::default());
        let mut args = sh("read x; printf %s \"got:$x\"", &[]);
        args.push(opts(vec![("stdin", s("pipe")), ("stdout", s("pipe"))]));
        let r = call(&g, &h, OsOp::Spawn, args).expect("spawn");
        let stdin = int(field(&r, "stdin"));
        h.fs.fds
            .with(stdin, |e| match e {
                FdEntry::ChildStdin(p) => p.lock().write_all(b"ping\n"),
                _ => panic!("not stdin"),
            })
            .expect("fd")
            .expect("write");
        assert!(h.fs.fds.remove(stdin).is_some());
        assert_eq!(read_all(&h, int(field(&r, "stdout"))), b"got:ping");
        call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
    }

    #[test]
    fn inheriting_stdio_and_a_cwd_each_need_their_grant() {
        let (g, h) = (grants(), OsHandles::default());
        for key in ["stdin", "stdout", "stderr"] {
            let mut a = sh("true", &[]);
            a.push(opts(vec![(key, s("inherit"))]));
            let e = call(&g, &h, OsOp::Spawn, a).expect_err("denied");
            assert_eq!(e.kind, OsErrorKind::Denied, "{key}");
        }
        let mut a = sh("true", &[]);
        a.push(opts(vec![("cwd", s("/tmp"))]));
        assert_eq!(
            call(&g, &h, OsOp::Spawn, a.clone())
                .expect_err("denied")
                .kind,
            OsErrorKind::Denied
        );
        let with_fs = OsGrants {
            fs: vec![FsGrant {
                glob: "/tmp".to_string(),
                modes: vec![FsMode::Read],
            }],
            ..grants()
        };
        let r = call(&with_fs, &h, OsOp::Spawn, a).expect("spawn");
        call(&with_fs, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
        assert!(h.process.children.lock().is_empty());
    }

    #[test]
    fn a_granted_inherited_stream_hands_the_terminal_to_the_child_and_captures_nothing() {
        let (g, h) = (
            OsGrants {
                stdio: vec![StdioKind::In, StdioKind::Out, StdioKind::Err],
                ..grants()
            },
            OsHandles::default(),
        );
        let mut a = sh("exit 3", &[]);
        a.push(opts(vec![("stdin", s("inherit")), ("stdout", s("inherit")), ("stderr", s("inherit"))]));
        let r = call(&g, &h, OsOp::Spawn, a).expect("spawn");
        for stream in ["stdin", "stdout", "stderr"] {
            assert_eq!(field(&r, stream), Value::Null, "{stream} is the host's, not a captured fd");
        }
        let done = call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
        assert_eq!(field(&done, "code"), Value::Number(3.0), "the exit code still comes back");
        let mut piped = sh("exit 0", &[]);
        piped.push(opts(vec![("stdout", s("pipe")), ("stderr", s("inherit"))]));
        let r = call(&g, &h, OsOp::Spawn, piped).expect("spawn");
        assert!(matches!(field(&r, "stdout"), Value::Number(_)), "control: a piped stream is captured");
        assert_eq!(field(&r, "stderr"), Value::Null, "control: the inherited one beside it is not");
        call(&g, &h, OsOp::Wait, vec![field(&r, "pid")]).expect("wait");
    }

    #[test]
    fn stdout_and_stderr_writes_need_stdio_grants() {
        let h = OsHandles::default();
        for op in [OsOp::StdoutWrite, OsOp::StderrWrite] {
            let e = call(&OsGrants::default(), &h, op, vec![s("aGk=")]).expect_err("denied");
            assert_eq!(e.kind, OsErrorKind::Denied);
        }
        let g = OsGrants {
            stdio: vec![StdioKind::Out, StdioKind::Err],
            ..OsGrants::default()
        };
        for op in [OsOp::StdoutWrite, OsOp::StderrWrite] {
            let r = call(&g, &h, op, vec![s("")]).expect("write");
            assert_eq!(field(&r, "bytes"), Value::Number(0.0));
        }
    }

    #[test]
    fn isatty_and_tty_size_cover_only_the_standard_fds() {
        let h = OsHandles::default();
        let g = OsGrants {
            stdio: vec![StdioKind::In, StdioKind::Out, StdioKind::Err],
            ..OsGrants::default()
        };
        for op in [OsOp::Isatty, OsOp::TtySize] {
            let e = call(&g, &h, op, vec![Value::Number(7.0)]).expect_err("unsupported");
            assert_eq!(e.kind, OsErrorKind::Unsupported);
            let e =
                call(&OsGrants::default(), &h, op, vec![Value::Number(1.0)]).expect_err("denied");
            assert_eq!(e.kind, OsErrorKind::Denied);
        }
        let r = call(&g, &h, OsOp::Isatty, vec![Value::Number(1.0)]).expect("isatty");
        assert!(matches!(field(&r, "ok"), Value::Bool(_)));
    }

    #[test]
    fn current_exe_answers_the_running_binary_as_an_absolute_path_under_no_grant() {
        let h = OsHandles::default();
        let r = call(&OsGrants::default(), &h, OsOp::CurrentExe, vec![]).expect("current-exe");
        let Value::String(path) = field(&r, "path") else { panic!("a path string") };
        assert!(std::path::Path::new(&path).is_absolute(), "{path}");
        assert_eq!(std::path::PathBuf::from(&path), std::env::current_exe().expect("exe"));
        assert!(std::path::Path::new(&path).is_file(), "control: it names an existing binary");
    }

    #[test]
    fn pid_and_ppid_report_numbers_and_streams_are_unsupported() {
        let (g, h) = (grants(), OsHandles::default());
        let me = call(&g, &h, OsOp::Pid, vec![]).expect("pid");
        assert_eq!(int(field(&me, "pid")), i64::from(std::process::id()));
        assert!(
            int(field(
                &call(&g, &h, OsOp::Ppid, vec![]).expect("ppid"),
                "pid"
            )) > 0
        );
        for op in [OsOp::WatchProcess, OsOp::WatchSignal, OsOp::WatchEnv] {
            let e = call(&g, &h, op, vec![s("x")]).expect_err("unsupported");
            assert_eq!(e.kind, OsErrorKind::Unsupported);
        }
    }

    #[test]
    fn stdin_read_needs_the_in_grant() {
        let h = OsHandles::default();
        let e = call(&OsGrants::default(), &h, OsOp::StdinRead, vec![]).expect_err("denied");
        assert_eq!(e.kind, OsErrorKind::Denied);
        let g = OsGrants {
            stdio: vec![StdioKind::In],
            ..OsGrants::default()
        };
        let e = call(&g, &h, OsOp::StdinRead, vec![Value::Number(0.0)]).expect_err("invalid");
        assert_eq!(e.kind, OsErrorKind::Invalid);
    }

    #[test]
    fn close_all_kills_and_reaps_running_children() {
        let (g, h) = (grants(), OsHandles::default());
        call(&g, &h, OsOp::Spawn, sh("exec sleep 30", &[])).expect("spawn");
        assert_eq!(h.process.children.lock().len(), 1);
        h.close_all();
        assert!(h.process.children.lock().is_empty());
    }

    type Completed = (String, String, Result<Value, OsError>);

    struct Recorder(Mutex<std::sync::mpsc::Sender<Completed>>);

    impl OsCompletionSink for Recorder {
        fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>) {
            let _ = self
                .0
                .lock()
                .send((site.trait_name.clone(), key.to_string(), outcome));
        }
    }

    fn with_sink(h: &OsHandles) -> std::sync::mpsc::Receiver<Completed> {
        let (tx, rx) = std::sync::mpsc::channel();
        h.completions.set_sink(Arc::new(Recorder(Mutex::new(tx))));
        rx
    }

    fn routed() -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "Sup".to_string(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: CallRoutes {
                success: Some("CHILD_EXITED".to_string()),
                failure: Some("CHILD_WAIT_FAILED".to_string()),
                ..CallRoutes::default()
            },
        }
    }

    fn ask_wait(
        g: &OsGrants,
        h: &OsHandles,
        pid: Value,
        key: Option<&str>,
        site: Option<CallSite>,
    ) -> IoAnswer {
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let host = OsHost {
            grants: g,
            handles: h,
            on_event: &on_event,
        };
        answer_os(
            Some(&host),
            &IoRequest::Os {
                op: OsOp::Wait,
                args: vec![pid],
                key: key.map(str::to_string),
                site,
            },
        )
    }

    fn spawn_sleeper(g: &OsGrants, h: &OsHandles) -> Value {
        field(
            &call(g, h, OsOp::Spawn, sh("exec sleep 30", &[])).expect("spawn"),
            "pid",
        )
    }

    #[test]
    fn a_keyed_wait_under_a_route_returns_deferred_at_once_and_completes_with_the_signal() {
        let (g, h) = (grants(), OsHandles::default());
        let completed = with_sink(&h);
        let pid = spawn_sleeper(&g, &h);
        let started = std::time::Instant::now();
        let answer = ask_wait(&g, &h, pid.clone(), Some("child-1"), Some(routed()));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "the wait blocked"
        );
        assert_eq!(answer, IoAnswer::ServiceDeferred);
        assert!(completed.try_recv().is_err(), "the child is still running");
        call(&g, &h, OsOp::Kill, vec![pid, s("TERM")]).expect("kill");
        let (trait_name, key, outcome) = completed
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("one completion");
        assert_eq!((trait_name.as_str(), key.as_str()), ("Sup", "child-1"));
        let done = outcome.expect("the child ended");
        assert_eq!(field(&done, "signal"), Value::Number(15.0));
        assert_eq!(field(&done, "code"), Value::Null);
        assert!(
            completed
                .recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "exactly one"
        );
        assert!(h.process.children.lock().is_empty());
    }

    #[test]
    fn control_an_unkeyed_wait_still_blocks_until_the_child_exits() {
        let (g, h) = (grants(), OsHandles::default());
        let completed = with_sink(&h);
        let r = call(&g, &h, OsOp::Spawn, sh("sleep 0.3; exit 4", &[])).expect("spawn");
        let started = std::time::Instant::now();
        let answer = ask_wait(&g, &h, field(&r, "pid"), None, None);
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(250),
            "returned before the child ended"
        );
        let done = match answer {
            IoAnswer::Os(Ok(done)) => done,
            other => panic!("a plain answer, got {other:?}"),
        };
        assert_eq!(field(&done, "code"), Value::Number(4.0));
        assert!(completed
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err());
    }

    #[test]
    fn control_a_keyed_wait_with_no_sink_or_no_route_is_answered_in_place() {
        let (g, h) = (grants(), OsHandles::default());
        let r = call(&g, &h, OsOp::Spawn, sh("exit 5", &[])).expect("spawn");
        let IoAnswer::Os(Ok(done)) = ask_wait(&g, &h, field(&r, "pid"), Some("k"), Some(routed()))
        else {
            panic!("no sink answers in place")
        };
        assert_eq!(field(&done, "code"), Value::Number(5.0));
        let completed = with_sink(&h);
        let r = call(&g, &h, OsOp::Spawn, sh("exit 6", &[])).expect("spawn");
        let IoAnswer::Os(Ok(done)) = ask_wait(&g, &h, field(&r, "pid"), Some("k"), None) else {
            panic!("no site answers in place")
        };
        assert_eq!(field(&done, "code"), Value::Number(6.0));
        assert!(completed
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err());
    }

    #[test]
    fn a_deferred_wait_of_an_unknown_pid_or_a_reused_key_fails_in_place_and_completes_nothing() {
        let (g, h) = (grants(), OsHandles::default());
        let completed = with_sink(&h);
        let IoAnswer::Os(Err(unknown)) =
            ask_wait(&g, &h, Value::Number(999_999.0), Some("k"), Some(routed()))
        else {
            panic!("an unknown pid fails in place")
        };
        assert_eq!(unknown.kind, OsErrorKind::Closed);
        let pid = spawn_sleeper(&g, &h);
        assert_eq!(
            ask_wait(&g, &h, pid.clone(), Some("k"), Some(routed())),
            IoAnswer::ServiceDeferred
        );
        let IoAnswer::Os(Err(reused)) = ask_wait(&g, &h, pid, Some("k"), Some(routed())) else {
            panic!("a key with a pending completion is refused")
        };
        assert_eq!(reused.kind, OsErrorKind::Invalid);
        assert!(completed.try_recv().is_err());
        h.close_all();
    }

    #[test]
    fn cancelling_the_key_before_the_child_ends_drops_the_completion_and_leaves_the_child_waitable()
    {
        let (g, h) = (grants(), OsHandles::default());
        let completed = with_sink(&h);
        let pid = spawn_sleeper(&g, &h);
        assert_eq!(
            ask_wait(&g, &h, pid.clone(), Some("k"), Some(routed())),
            IoAnswer::ServiceDeferred
        );
        assert!(h.cancel_completion("k"));
        assert!(!h.cancel_completion("k"));
        call(&g, &h, OsOp::Kill, vec![pid.clone(), s("TERM")]).expect("kill");
        assert!(
            completed
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err(),
            "a cancelled key completed"
        );
        let done =
            call(&g, &h, OsOp::Wait, vec![pid]).expect("the child was not reaped by the watcher");
        assert_eq!(field(&done, "signal"), Value::Number(15.0));
    }

    struct Feed(std::sync::mpsc::Receiver<Vec<u8>>);

    impl Read for Feed {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(chunk) => {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                Err(_) => Ok(0),
            }
        }
    }

    /// Records each routed event as (route, payload); a chunk's dispatch also writes a reply before it returns.
    struct Replier(Mutex<std::sync::mpsc::Sender<(String, Value)>>);

    impl OsCompletionSink for Replier {
        fn complete(&self, site: &CallSite, _key: &str, outcome: Result<Value, OsError>) {
            let route = site.emit.success.clone().unwrap_or_default();
            let payload = outcome.expect("no failure");
            let tx = self.0.lock().clone();
            let _ = tx.send((route.clone(), payload.clone()));
            if route == "CHUNK" {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let _ = tx.send(("REPLY".to_string(), field(&payload, "data")));
            }
        }
    }

    fn stdin_site() -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "Mcp".to_string(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: CallRoutes {
                success: Some("EOF".to_string()),
                failure: Some("STDIN_FAILED".to_string()),
                on_message: Some("CHUNK".to_string()),
                ..CallRoutes::default()
            },
        }
    }

    type Fed = (
        std::sync::mpsc::Sender<Vec<u8>>,
        std::sync::mpsc::Receiver<(String, Value)>,
        OsHandles,
    );

    fn fed_stream(key: &str) -> Fed {
        let h = OsHandles::default();
        let (events_tx, events) = std::sync::mpsc::channel();
        h.completions
            .set_sink(Arc::new(Replier(Mutex::new(events_tx))));
        let (feed, rx) = std::sync::mpsc::channel();
        stream_stdin(Feed(rx), STDIN_CHUNK, key, &stdin_site(), &h.completions)
            .expect("stream opens");
        (feed, events, h)
    }

    fn next(events: &std::sync::mpsc::Receiver<(String, Value)>) -> (String, Value) {
        events
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("an event")
    }

    #[test]
    fn stdin_chunks_arrive_as_separate_events_before_eof() {
        let (feed, events, h) = fed_stream("stdin");
        feed.send(b"one\n".to_vec()).unwrap();
        assert_eq!(
            next(&events),
            (
                "CHUNK".to_string(),
                object(vec![("data", bytes_value(b"one\n"))])
            )
        );
        assert_eq!(next(&events).0, "REPLY");
        feed.send(b"two\n".to_vec()).unwrap();
        assert_eq!(
            next(&events).1,
            object(vec![("data", bytes_value(b"two\n"))])
        );
        assert_eq!(next(&events).0, "REPLY");
        drop(feed);
        assert_eq!(
            next(&events),
            ("EOF".to_string(), object(vec![("eof", Value::Bool(true))]))
        );
        assert!(
            events
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "eof is the last event"
        );
        assert!(h.completions.live_keys().is_empty());
    }

    #[test]
    fn stdin_reply_written_during_a_chunk_is_ordered_before_the_next_chunk() {
        let (feed, events, _h) = fed_stream("stdin");
        for chunk in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            feed.send(chunk).unwrap();
        }
        drop(feed);
        let order: Vec<(String, Value)> = (0..7).map(|_| next(&events)).collect();
        let shape: Vec<String> = order
            .iter()
            .map(|(route, payload)| match route.as_str() {
                "CHUNK" => format!("chunk:{:?}", field(payload, "data")),
                "REPLY" => format!("reply:{payload:?}"),
                other => other.to_string(),
            })
            .collect();
        let want: Vec<String> = [b"a", b"b", b"c"]
            .iter()
            .flat_map(|c| {
                let d = bytes_value(*c);
                [format!("chunk:{d:?}"), format!("reply:{d:?}")]
            })
            .chain(["EOF".to_string()])
            .collect();
        assert_eq!(shape, want);
    }

    #[test]
    fn stdin_cancel_call_on_the_key_stops_the_stream() {
        let (feed, events, h) = fed_stream("stdin");
        feed.send(b"first".to_vec()).unwrap();
        assert_eq!(next(&events).0, "CHUNK");
        assert_eq!(next(&events).0, "REPLY");
        assert!(h.cancel_completion("stdin"));
        let _ = feed.send(b"late".to_vec());
        drop(feed);
        assert!(
            events
                .recv_timeout(std::time::Duration::from_millis(400))
                .is_err(),
            "a cancelled stream delivered more"
        );
        assert!(h.completions.live_keys().is_empty());
    }

    #[test]
    fn stdin_control_empty_gives_eof_only() {
        let (feed, events, _h) = fed_stream("stdin");
        drop(feed);
        assert_eq!(
            next(&events),
            ("EOF".to_string(), object(vec![("eof", Value::Bool(true))]))
        );
        assert!(events
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err());
    }

    #[test]
    fn stdin_second_stream_on_an_open_key_is_refused() {
        let (_feed, _events, h) = fed_stream("stdin");
        let (_f2, rx2) = std::sync::mpsc::channel::<Vec<u8>>();
        let e = stream_stdin(
            Feed(rx2),
            STDIN_CHUNK,
            "stdin",
            &stdin_site(),
            &h.completions,
        )
        .expect_err("refused");
        assert_eq!(e.kind, OsErrorKind::Invalid);
    }

    #[test]
    fn stdin_unkeyed_read_returns_at_most_count_bytes() {
        let mut long = std::io::Cursor::new(b"abcdefgh".to_vec());
        let r = read_once(&mut long, 4).expect("read");
        assert_eq!(r, object(vec![("data", bytes_value(b"abcd")), ("eof", Value::Bool(false))]));
        let rest = read_once(&mut long, 4).expect("read");
        assert_eq!(field(&rest, "data"), bytes_value(b"efgh"), "the next read takes the next chunk");
    }

    #[test]
    fn stdin_unkeyed_control_a_short_stdin_returns_everything() {
        let mut short = std::io::Cursor::new(b"hi".to_vec());
        let r = read_once(&mut short, 64).expect("read");
        assert_eq!(r, object(vec![("data", bytes_value(b"hi")), ("eof", Value::Bool(false))]));
        let end = read_once(&mut short, 64).expect("read");
        assert_eq!(end, object(vec![("data", bytes_value(b"")), ("eof", Value::Bool(true))]), "edge: an empty read is eof");
    }
}
