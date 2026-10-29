//! The `process` family: spawn, wait, kill, pids, signals, environment, stdio and tty.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::args::{arg_bytes, arg_i64, arg_opt, arg_str, bytes_value, denied};
use super::fs::FdEntry;
use super::OsHost;
use crate::resumable::CallSite;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::os_grants::{FsMode, StdioKind};
use orbital_core_base::value::{Value, ValueMap};
use orbital_core_base::sync::Mutex;
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
    let mut map = ValueMap::default();
    for (k, v) in fields {
        map.insert(k.to_string().into(), v);
    }
    Value::object(map)
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
        Some(Value::Object(m)) => Some(&**m),
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
                .unwrap_or_else(|| std::path::PathBuf::from(p.as_str()));
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
            vars.insert(name.to_string(), v.clone());
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
                        vars.insert(name.to_string(), v.clone().into_inner());
                    }
                    Value::Null => {
                        if !host.grants.allows(&orbital_core_base::os_grants::OsRequirement::inherited_env(name)) {
                            return Err(denied(op, &format!("environment {name} is not granted")));
                        }
                        match lookup_env(host, name).map_err(|e| io_error(op, &e))? {
                            Some(v) => {
                                vars.insert(name.to_string(), v);
                            }
                            None => {
                                vars.remove(name.as_str());
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
            number(fds.insert(FdEntry::ChildStdout(Arc::new(super::fs::ReadPipe::new(s))))),
        ));
    }
    if let Some(s) = child.stderr.take() {
        fields.push((
            "stderr",
            number(fds.insert(FdEntry::ChildStderr(Arc::new(super::fs::ReadPipe::new(s))))),
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
    let Some(lease) = completions.begin_lease(key) else {
        return Err(invalid(op, format!("key {key} already has a completion pending")));
    };
    let (key, site) = (key.to_string(), site.clone());
    let watcher = {
        let (completions, key) = (completions.clone(), key.clone());
        std::thread::Builder::new()
            .name(format!("os-wait-{pid}"))
            .spawn(move || {
                let awaited = await_exit(pid).map_err(|e| io_error(op, &e));
                if !completions.claim_current(&key, lease) {
                    return;
                }
                let outcome = awaited.and_then(|()| reap(pid, &table));
                completions.deliver(&key, &site, outcome);
            })
    };
    watcher.map(drop).map_err(|e| {
        completions.cancel_current(&key, lease);
        io_error(op, &e)
    })
}

/// The signal names the operator table's `os/kill` declares for its `signal` parameter.
fn table_signal_names() -> Vec<&'static str> {
    orbital_core_base::language::registry::get_operator(OsOp::Kill.name())
        .and_then(|operator| operator.params.get(1))
        .and_then(|param| param.ty.literal_strings())
        .expect("the operator table declares os/kill's signal as a closed set of names")
}

#[cfg(unix)]
fn libc_signal(name: &str) -> Option<i32> {
    Some(match name {
        "TERM" => libc::SIGTERM,
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        "HUP" => libc::SIGHUP,
        "QUIT" => libc::SIGQUIT,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "STOP" => libc::SIGSTOP,
        "CONT" => libc::SIGCONT,
        _ => return None,
    })
}

#[cfg(unix)]
fn signal_number(value: &Value, op: OsOp) -> Result<i32, OsError> {
    let names = table_signal_names();
    match value {
        Value::String(name) if names.contains(&name.as_str()) => libc_signal(name)
            .ok_or_else(|| err(OsErrorKind::Unsupported, op, format!("this host has no signal {name}"))),
        _ => Err(invalid(op, format!("signal must be one of {}", names.join(", ")))),
    }
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
    // An unset variable has no `value` key, as the operator table declares (`{ value? }`), not a null one.
    Ok(object(value.map(|value| ("value", Value::string(value))).into_iter().collect()))
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
        Value::Array(names.into_iter().map(Value::string).collect()),
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

/// Keyed stdin reads defer once; an explicit onMessage route opts into streaming.
pub(super) fn stdin_later(
    args: &[Value],
    key: &str,
    site: &CallSite,
    host: &OsHost<'_>,
) -> Result<(), OsError> {
    let count = stdin_count(args, host)?;
    route_stdin(std::io::stdin(), count, key, site, &host.handles.completions)
}

fn route_stdin<R: Read + Send + 'static>(
    mut reader: R,
    count: usize,
    key: &str,
    site: &CallSite,
    completions: &Arc<super::completion::Completions>,
) -> Result<(), OsError> {
    if site.emit.on_message.is_none() {
        return super::completion::Completions::run_later(completions, key, site, move || read_once(&mut reader, count))
            .map_err(|error| super::net::later_error(OsOp::StdinRead, error, key));
    }
    stream_stdin(reader, count, key, site, completions)
}

fn stream_stdin<R: Read + Send + 'static>(
    mut reader: R,
    count: usize,
    key: &str,
    site: &CallSite,
    completions: &Arc<super::completion::Completions>,
) -> Result<(), OsError> {
    let op = OsOp::StdinRead;
    let Some(lease) = completions.begin_lease(key) else {
        return Err(invalid(op, format!("key {key} already has a stream open")));
    };
    let (owned_key, owned_site) = (key.to_string(), site.clone());
    let started = std::thread::Builder::new()
        .name(format!("os-stdin-{key}"))
        .spawn({
            let completions = completions.clone();
            move || {
                let (key, site) = (owned_key, owned_site);
                let mut buf = vec![0u8; count.min(STDIN_CAP)];
                loop {
                    if !completions.is_current(&key, lease) {
                        return;
                    }
                    let outcome = match reader.read(&mut buf) {
                        Ok(0) => Ok(object(vec![("eof", Value::Bool(true))])),
                        Ok(n) => {
                            let chunk = object(vec![("data", bytes_value(&buf[..n]))]);
                            if completions.message(&key, lease, &site, chunk) {
                                continue;
                            }
                            return;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => Err(io_error(op, &e)),
                    };
                    if completions.claim_current(&key, lease) {
                        completions.deliver(&key, &site, outcome);
                    }
                    return;
                }
            }
        });
    started.map(drop).map_err(|e| {
        completions.cancel_current(key, lease);
        io_error(op, &e)
    })
}

fn current_exe() -> Result<Value, OsError> {
    let op = OsOp::CurrentExe;
    let path = std::env::current_exe().map_err(|e| io_error(op, &e))?;
    match path.to_str() {
        Some(p) => Ok(object(vec![("path", Value::string(p.to_string()))])),
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
mod tests;
