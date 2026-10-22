//! The `net` family: TCP, UDP and DNS. `os/tcp-listen` is a keyed stream: it answers the bound listener at once and
//! hands each accepted connection to the completion sink as an event of the call's `onMessage` route, until the
//! key is cancelled or the listener closed. Port watching answers `unsupported`.
//!
//! Grant rules. Every target is checked with `net_allows(host, port, kind)` before any syscall, spelled exactly
//! as the program wrote it. `os/dns-resolve` checks `net_allows(name, 0, tcp)` or `(name, 0, udp)`: a grant
//! entry `name:0` lets that name be resolved. An outbound destination named by a host name (connect, sendto) is
//! resolved, every resolved address must be public (`is_public_address`, the helper the http transport uses),
//! and the connection goes to those checked addresses; a destination written as an IP literal is exactly what
//! the grant names, so it is not re-checked. Binding is local and is not re-checked, and `dns-resolve` returns
//! addresses without connecting, so it reports private ones too.
//!
//! Bounded I/O. Every socket carries a read and write timeout of [`IO_TIMEOUT`] (5 s); a read that waits longer
//! answers `timeout`. `recv` returns at most `count` bytes (at most [`MAX_RECV`]) from one read.
//!
//! Ordered writes. Every `send` on a socket takes a turn in call order and writes only after the turn before it has
//! ended, so a keyed `send` (written on a thread of its own) and an unkeyed one never interleave their bytes.

use super::completion::{Completions, RunLaterError};
use super::args::{arg_bytes, arg_i64, arg_opt, arg_str, bytes_value, denied};
use super::{HandleSlab, IdCounter, OsHost};
use crate::resumable::CallSite;
use orbital_core_base::http_wire::is_public_address;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::os_grants::NetKind;
use orbital_core_base::value::{Value, ValueMap};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const MAX_RECV: usize = 64 * 1024 * 1024;
const MAX_DATAGRAM: usize = 65_535;
/// How long an accept loop sleeps when no connection is waiting before it looks again for a cancel or a close.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

pub(super) enum SockEntry {
    Stream(TcpStream),
    Listener(TcpListener),
    Datagram(UdpSocket),
}

/// Open sockets; ids come from the counter shared with the fs table, so an id names one handle. Shared with the
/// accept loops, which add each accepted connection.
pub struct NetTable {
    pub(super) socks: Arc<HandleSlab<SockEntry>>,
    /// The last write turn taken on each stream socket.
    writes: Mutex<HashMap<i64, Arc<Turn>>>,
}

impl NetTable {
    pub(super) fn new(ids: IdCounter) -> Self {
        Self {
            socks: Arc::new(HandleSlab::new(ids)),
            writes: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn clear(&self) {
        self.socks.clear();
        self.writes.lock().clear();
    }

    /// The next write turn on socket `id`, after every turn taken on it before.
    fn take_turn(&self, id: i64) -> WriteTurn {
        let mine = Arc::new(Turn::default());
        let prior = self.writes.lock().insert(id, mine.clone());
        WriteTurn { prior, mine, ran: false }
    }
}

#[derive(Default)]
enum TurnState {
    #[default]
    Pending,
    Done,
    /// The write never ran; whoever waits on it waits on the turn before it instead.
    Skipped(Option<Arc<Turn>>),
}

#[derive(Default)]
struct Turn {
    state: Mutex<TurnState>,
    ended: Condvar,
}

impl Turn {
    fn wait(&self) {
        let mut state = self.state.lock();
        loop {
            match &*state {
                TurnState::Done => return,
                TurnState::Skipped(prior) => {
                    let prior = prior.clone();
                    drop(state);
                    if let Some(prior) = prior {
                        prior.wait();
                    }
                    return;
                }
                TurnState::Pending => self.ended.wait(&mut state),
            }
        }
    }

    fn end(&self, state: TurnState) {
        *self.state.lock() = state;
        self.ended.notify_all();
    }
}

/// One `send`'s place in its socket's write order; dropped without running, it hands its place to the turn before.
struct WriteTurn {
    prior: Option<Arc<Turn>>,
    mine: Arc<Turn>,
    ran: bool,
}

impl WriteTurn {
    fn run<R>(mut self, write: impl FnOnce() -> R) -> R {
        if let Some(prior) = &self.prior {
            prior.wait();
        }
        let outcome = write();
        self.ran = true;
        outcome
    }
}

impl Drop for WriteTurn {
    fn drop(&mut self) {
        self.mine.end(if self.ran { TurnState::Done } else { TurnState::Skipped(self.prior.take()) });
    }
}

pub(super) fn err(op: OsOp, kind: OsErrorKind, what: impl std::fmt::Display) -> OsError {
    OsError::new(kind, format!("{}: {what}", op.name()))
}

pub(super) fn io_error(op: OsOp, e: &io::Error) -> OsError {
    use io::ErrorKind as K;
    let kind = match e.kind() {
        K::NotFound => OsErrorKind::NotFound,
        K::PermissionDenied => OsErrorKind::Denied,
        K::TimedOut | K::WouldBlock => OsErrorKind::Timeout,
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe | K::NotConnected => {
            OsErrorKind::Closed
        }
        K::AddrInUse | K::AlreadyExists => OsErrorKind::Exists,
        K::InvalidInput | K::InvalidData => OsErrorKind::Invalid,
        _ => OsErrorKind::Io,
    };
    err(op, kind, e)
}

pub(super) fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = ValueMap::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

fn count(n: usize) -> Value {
    Value::Number(n as f64)
}

fn host_arg(args: &[Value], i: usize, op: OsOp) -> Result<String, OsError> {
    let text = arg_str(args, i, op)?
        .trim_start_matches('[')
        .trim_end_matches(']');
    if text.is_empty() {
        return Err(err(op, OsErrorKind::Invalid, "the host is empty"));
    }
    Ok(text.to_string())
}

fn port_arg(args: &[Value], i: usize, op: OsOp) -> Result<u16, OsError> {
    let n = arg_i64(args, i, op)?;
    u16::try_from(n).map_err(|_| err(op, OsErrorKind::Invalid, "the port must be 0 to 65535"))
}

fn count_arg(args: &[Value], i: usize, op: OsOp) -> Result<usize, OsError> {
    let n = arg_i64(args, i, op)?;
    usize::try_from(n).map_err(|_| err(op, OsErrorKind::Invalid, "the count must not be negative"))
}

fn check_grant(
    host: &OsHost<'_>,
    op: OsOp,
    name: &str,
    port: u16,
    kind: NetKind,
) -> Result<(), OsError> {
    if host.grants.net_allows(name, port, kind) {
        Ok(())
    } else {
        Err(denied(op, &format!("{name}:{port} is not granted")))
    }
}

fn resolve(op: OsOp, name: &str, port: u16) -> Result<Vec<SocketAddr>, OsError> {
    let addrs: Vec<SocketAddr> = (name, port)
        .to_socket_addrs()
        .map_err(|e| {
            err(
                op,
                OsErrorKind::NotFound,
                format!("{name} did not resolve: {e}"),
            )
        })?
        .collect();
    if addrs.is_empty() {
        return Err(err(
            op,
            OsErrorKind::NotFound,
            format!("{name} has no addresses"),
        ));
    }
    Ok(addrs)
}

/// The addresses an outbound destination may be reached at: an IP literal as written, a name only when every
/// address it resolves to is public.
fn outbound(op: OsOp, name: &str, port: u16) -> Result<Vec<SocketAddr>, OsError> {
    if let Ok(ip) = name.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs = resolve(op, name, port)?;
    match addrs.iter().find(|a| !is_public_address(a.ip())) {
        Some(a) => Err(denied(
            op,
            &format!("{name} resolves to {}, not a public address", a.ip()),
        )),
        None => Ok(addrs),
    }
}

fn set_timeouts(
    op: OsOp,
    read: impl Fn(Option<Duration>) -> io::Result<()>,
    write: impl Fn(Option<Duration>) -> io::Result<()>,
) -> Result<(), OsError> {
    read(Some(IO_TIMEOUT)).map_err(|e| io_error(op, &e))?;
    write(Some(IO_TIMEOUT)).map_err(|e| io_error(op, &e))
}

fn stream_of(table: &HandleSlab<SockEntry>, id: i64, op: OsOp) -> Result<TcpStream, OsError> {
    match table.with(id, |entry| match entry {
        SockEntry::Stream(s) => s.try_clone().map_err(|e| io_error(op, &e)),
        _ => Err(err(
            op,
            OsErrorKind::Invalid,
            "not a connected stream socket",
        )),
    }) {
        Some(result) => result,
        None => Err(err(op, OsErrorKind::Closed, "not an open socket")),
    }
}

fn datagram_of(table: &HandleSlab<SockEntry>, id: i64, op: OsOp) -> Result<UdpSocket, OsError> {
    match table.with(id, |entry| match entry {
        SockEntry::Datagram(s) => s.try_clone().map_err(|e| io_error(op, &e)),
        _ => Err(err(op, OsErrorKind::Invalid, "not a datagram socket")),
    }) {
        Some(result) => result,
        None => Err(err(op, OsErrorKind::Closed, "not an open socket")),
    }
}

fn tcp_connect(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::TcpConnect;
    let name = host_arg(args, 0, op)?;
    let port = port_arg(args, 1, op)?;
    check_grant(host, op, &name, port, NetKind::Tcp)?;
    let mut last = None;
    for addr in outbound(op, &name, port)? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                set_timeouts(
                    op,
                    |t| stream.set_read_timeout(t),
                    |t| stream.set_write_timeout(t),
                )?;
                let id = host.handles.net.socks.insert(SockEntry::Stream(stream));
                return Ok(object([("sock", Value::Number(id as f64))]));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(
        || err(op, OsErrorKind::Io, "no address to connect to"),
        |e| io_error(op, &e),
    ))
}

fn send_on(mut stream: TcpStream, data: &[u8], turn: WriteTurn) -> Result<Value, OsError> {
    turn.run(|| stream.write_all(data)).map_err(|e| io_error(OsOp::Send, &e))?;
    Ok(object([("bytes", count(data.len()))]))
}

fn send(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Send;
    let id = arg_i64(args, 0, op)?;
    let data = arg_bytes(args, 1, op)?;
    let stream = stream_of(&host.handles.net.socks, id, op)?;
    send_on(stream, &data, host.handles.net.take_turn(id))
}

/// `os/send` under a completion route: the write runs on a thread of its own, after every earlier send on the socket,
/// and the call returns at once, so a peer that stops reading holds the write, not the dispatch.
pub(super) fn send_later(args: &[Value], key: &str, site: &CallSite, host: &OsHost<'_>) -> Result<(), OsError> {
    let op = OsOp::Send;
    let id = arg_i64(args, 0, op)?;
    let data = arg_bytes(args, 1, op)?;
    let stream = stream_of(&host.handles.net.socks, id, op)?;
    let turn = host.handles.net.take_turn(id);
    Completions::run_later(&host.handles.completions, key, site, move || send_on(stream, &data, turn))
        .map_err(|e| later_error(op, e, key))
}

fn recv_on(mut stream: TcpStream, want: usize) -> Result<Value, OsError> {
    let mut buf = vec![0u8; want];
    let read = stream.read(&mut buf).map_err(|e| io_error(OsOp::Recv, &e))?;
    Ok(object([
        ("data", bytes_value(&buf[..read])),
        ("eof", Value::Bool(read == 0 && want > 0)),
    ]))
}

fn recv(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Recv;
    let id = arg_i64(args, 0, op)?;
    let want = count_arg(args, 1, op)?.min(MAX_RECV);
    recv_on(stream_of(&host.handles.net.socks, id, op)?, want)
}

pub(super) fn later_error(op: OsOp, error: RunLaterError, key: &str) -> OsError {
    match error {
        RunLaterError::KeyPending => err(op, OsErrorKind::Invalid, format!("key {key} already has a completion pending")),
        RunLaterError::Spawn(e) => io_error(op, &e),
    }
}

/// `os/recv` under a completion route: the read runs on a thread of its own and the call returns at once.
pub(super) fn recv_later(args: &[Value], key: &str, site: &CallSite, host: &OsHost<'_>) -> Result<(), OsError> {
    let op = OsOp::Recv;
    let id = arg_i64(args, 0, op)?;
    let want = count_arg(args, 1, op)?.min(MAX_RECV);
    let stream = stream_of(&host.handles.net.socks, id, op)?;
    Completions::run_later(&host.handles.completions, key, site, move || recv_on(stream, want))
        .map_err(|e| later_error(op, e, key))
}

fn udp_bind(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::UdpBind;
    let name = host_arg(args, 0, op)?;
    let port = port_arg(args, 1, op)?;
    check_grant(host, op, &name, port, NetKind::Udp)?;
    let addrs = resolve(op, &name, port)?;
    let socket = UdpSocket::bind(addrs.as_slice()).map_err(|e| io_error(op, &e))?;
    set_timeouts(
        op,
        |t| socket.set_read_timeout(t),
        |t| socket.set_write_timeout(t),
    )?;
    let id = host.handles.net.socks.insert(SockEntry::Datagram(socket));
    Ok(object([("sock", Value::Number(id as f64))]))
}

fn sendto(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Sendto;
    let id = arg_i64(args, 0, op)?;
    let data = arg_bytes(args, 1, op)?;
    let name = host_arg(args, 2, op)?;
    let port = port_arg(args, 3, op)?;
    check_grant(host, op, &name, port, NetKind::Udp)?;
    let socket = datagram_of(&host.handles.net.socks, id, op)?;
    let mut last = None;
    for addr in outbound(op, &name, port)? {
        match socket.send_to(&data, addr) {
            Ok(sent) => return Ok(object([("bytes", count(sent))])),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(
        || err(op, OsErrorKind::Io, "no address to send to"),
        |e| io_error(op, &e),
    ))
}

fn recvfrom_on(socket: UdpSocket, want: usize) -> Result<Value, OsError> {
    let mut buf = vec![0u8; want];
    let (read, peer) = socket.recv_from(&mut buf).map_err(|e| io_error(OsOp::Recvfrom, &e))?;
    Ok(object([
        ("data", bytes_value(&buf[..read])),
        ("peer", Value::String(peer.to_string())),
    ]))
}

fn recvfrom(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Recvfrom;
    let id = arg_i64(args, 0, op)?;
    let want = count_arg(args, 1, op)?.min(MAX_RECV).min(MAX_DATAGRAM);
    recvfrom_on(datagram_of(&host.handles.net.socks, id, op)?, want)
}

/// `os/recvfrom` under a completion route, as [`recv_later`].
pub(super) fn recvfrom_later(args: &[Value], key: &str, site: &CallSite, host: &OsHost<'_>) -> Result<(), OsError> {
    let op = OsOp::Recvfrom;
    let id = arg_i64(args, 0, op)?;
    let want = count_arg(args, 1, op)?.min(MAX_RECV).min(MAX_DATAGRAM);
    let socket = datagram_of(&host.handles.net.socks, id, op)?;
    Completions::run_later(&host.handles.completions, key, site, move || recvfrom_on(socket, want))
        .map_err(|e| later_error(op, e, key))
}

/// What an accept loop does with each ready connection; `false` ends the stream.
pub(super) type OnAccept =
    fn(TcpStream, SocketAddr, &Arc<HandleSlab<SockEntry>>, &Arc<Completions>, &str, &CallSite, u64) -> bool;

/// `os/tcp-listen`: bind, answer `{ sock, address, port }`, and accept on a thread of its own. Each connection is
/// added to the socket table and delivered as `{ sock, peer }` on the `onMessage` route. Cancelling the key or
/// closing the listener ends the stream; an accept error ends it on the failure route.
fn tcp_listen(
    args: &[Value],
    key: Option<&str>,
    site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Result<Value, OsError> {
    listen(OsOp::TcpListen, NetKind::Listen, deliver_connection, args, key, site, host)
}

fn deliver_connection(
    stream: TcpStream,
    peer: SocketAddr,
    socks: &Arc<HandleSlab<SockEntry>>,
    completions: &Arc<Completions>,
    key: &str,
    site: &CallSite,
    lease: u64,
) -> bool {
    let conn = socks.insert(SockEntry::Stream(stream));
    let event = object([
        ("sock", Value::Number(conn as f64)),
        ("peer", Value::String(peer.to_string())),
    ]);
    if completions.message(key, lease, site, event) {
        return true;
    }
    socks.remove(conn);
    false
}

/// The keyed listening stream of `os/tcp-listen`; `on_accept` takes each connection.
pub(super) fn listen(
    op: OsOp,
    kind: NetKind,
    on_accept: OnAccept,
    args: &[Value],
    key: Option<&str>,
    site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Result<Value, OsError> {
    let name = host_arg(args, 0, op)?;
    let port = port_arg(args, 1, op)?;
    let (Some(key), Some(site)) = (key, site) else {
        return Err(err(op, OsErrorKind::Invalid, "a stream needs a key and a call site"));
    };
    let completions = host.handles.completions.clone();
    if completions.sink().is_none() {
        return Err(err(op, OsErrorKind::Unsupported, "this host has no completion sink to stream to"));
    }
    check_grant(host, op, &name, port, kind)?;
    let addrs = resolve(op, &name, port)?;
    let listener = TcpListener::bind(addrs.as_slice()).map_err(|e| io_error(op, &e))?;
    let local = listener.local_addr().map_err(|e| io_error(op, &e))?;
    listener.set_nonblocking(true).map_err(|e| io_error(op, &e))?;
    let Some(lease) = completions.begin_lease(key) else {
        return Err(err(op, OsErrorKind::Invalid, format!("key {key} already has a stream open")));
    };
    let socks = host.handles.net.socks.clone();
    let id = socks.insert(SockEntry::Listener(listener));
    let (owned_key, owned_site) = (key.to_string(), site.clone());
    let started = std::thread::Builder::new()
        .name(format!("os-listen-{key}"))
        .spawn({
            let (completions, socks) = (completions.clone(), socks.clone());
            move || accept_loop(op, id, &socks, &completions, &owned_key, &owned_site, on_accept, lease)
        });
    if let Err(e) = started {
        completions.cancel_current(key, lease);
        socks.remove(id);
        return Err(io_error(op, &e));
    }
    Ok(object([
        ("sock", Value::Number(id as f64)),
        ("address", Value::String(local.ip().to_string())),
        ("port", count(usize::from(local.port()))),
    ]))
}

/// Accepts on the listener the table holds, so closing the handle stops listening at once.
fn accept_loop(
    op: OsOp,
    id: i64,
    socks: &Arc<HandleSlab<SockEntry>>,
    completions: &Arc<Completions>,
    key: &str,
    site: &CallSite,
    on_accept: OnAccept,
    lease: u64,
) {
    loop {
        if !completions.is_current(key, lease) {
            return;
        }
        let accepted = socks.with(id, |entry| match entry {
            SockEntry::Listener(listener) => Some(listener.accept()),
            _ => None,
        });
        let Some(Some(accepted)) = accepted else {
            completions.cancel_current(key, lease);
            return;
        };
        match accepted {
            Ok((stream, peer)) => {
                let ready = stream
                    .set_nonblocking(false)
                    .map_err(|e| io_error(op, &e))
                    .and_then(|()| {
                        set_timeouts(op, |t| stream.set_read_timeout(t), |t| stream.set_write_timeout(t))
                    });
                if let Err(error) = ready {
                    if completions.claim_current(key, lease) {
                        completions.deliver(key, site, Err(error));
                    }
                    return;
                }
                if !on_accept(stream, peer, socks, completions, key, site, lease) {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(e) => {
                if completions.claim_current(key, lease) {
                    completions.deliver(key, site, Err(io_error(op, &e)));
                }
                return;
            }
        }
    }
}

fn dns_resolve(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::DnsResolve;
    let name = host_arg(args, 0, op)?;
    let family = match arg_opt(args, 1) {
        None => None,
        Some(_) => match arg_i64(args, 1, op)? {
            f @ (4 | 6) => Some(f),
            _ => return Err(err(op, OsErrorKind::Invalid, "the family must be 4 or 6")),
        },
    };
    if !(host.grants.net_allows(&name, 0, NetKind::Tcp)
        || host.grants.net_allows(&name, 0, NetKind::Udp))
    {
        return Err(denied(op, &format!("{name}:0 is not granted")));
    }
    let mut seen: Vec<IpAddr> = Vec::new();
    for addr in resolve(op, &name, 0)? {
        let ip = addr.ip();
        let wanted = match family {
            Some(4) => ip.is_ipv4(),
            Some(_) => ip.is_ipv6(),
            None => true,
        };
        if wanted && !seen.contains(&ip) {
            seen.push(ip);
        }
    }
    if seen.is_empty() {
        return Err(err(
            op,
            OsErrorKind::NotFound,
            format!("{name} has no address of that family"),
        ));
    }
    let addresses = seen
        .iter()
        .map(|ip| Value::String(ip.to_string()))
        .collect();
    Ok(object([("addresses", Value::Array(addresses))]))
}

fn shutdown(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Shutdown;
    let id = arg_i64(args, 0, op)?;
    let stream = stream_of(&host.handles.net.socks, id, op)?;
    stream
        .shutdown(Shutdown::Both)
        .map_err(|e| io_error(op, &e))?;
    Ok(object([]))
}

/// `None` when `id` is not a socket, so the caller answers `closed`.
fn close(args: &[Value], host: &OsHost<'_>) -> Option<Result<Value, OsError>> {
    let id = match arg_i64(args, 0, OsOp::Close) {
        Ok(id) => id,
        Err(e) => return Some(Err(e)),
    };
    host.handles.net.writes.lock().remove(&id);
    host.handles.net.socks.remove(id).map(|entry| {
        // A keyed read pending on this socket holds a duplicate of it; shutting down ends the connection for both.
        if let SockEntry::Stream(stream) = entry {
            let _ = stream.shutdown(Shutdown::Both);
        }
        Ok(object([]))
    })
}

pub(super) fn answer(
    op: OsOp,
    args: &[Value],
    key: Option<&str>,
    site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Option<Result<Value, OsError>> {
    Some(match op {
        OsOp::TcpConnect => tcp_connect(args, host),
        OsOp::TcpListen => tcp_listen(args, key, site, host),
        OsOp::Send => send(args, host),
        OsOp::Recv => recv(args, host),
        OsOp::UdpBind => udp_bind(args, host),
        OsOp::Sendto => sendto(args, host),
        OsOp::Recvfrom => recvfrom(args, host),
        OsOp::DnsResolve => dns_resolve(args, host),
        OsOp::Shutdown => shutdown(args, host),
        OsOp::Close => return close(args, host),
        OsOp::WatchPort => Err(err(
            op,
            OsErrorKind::Unsupported,
            "streaming host not implemented",
        )),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Instant;
    use {
        crate::os_host::OsHandles,
        orbital_core_base::os_grants::{NetGrant, OsGrants},
    };

    fn s(text: &str) -> Value {
        Value::String(text.to_string())
    }

    fn n(v: i64) -> Value {
        Value::Number(v as f64)
    }

    fn grants(entries: &[(String, &[NetKind])]) -> OsGrants {
        OsGrants {
            net: entries
                .iter()
                .map(|(target, kinds)| NetGrant {
                    target: target.clone(),
                    kinds: kinds.to_vec(),
                })
                .collect(),
            ..OsGrants::default()
        }
    }

    fn with_host<R>(grants: OsGrants, check: impl FnOnce(&OsHost<'_>) -> R) -> R {
        let handles = OsHandles::default();
        let on_event = |_: Option<&CallSite>, _: Value| {};
        check(&OsHost {
            grants: &grants,
            handles: &handles,
            on_event: &on_event,
        })
    }

    fn ask(host: &OsHost<'_>, op: OsOp, args: &[Value]) -> Result<Value, OsError> {
        answer(op, args, None, None, host).expect("net answers this op")
    }

    fn field(value: &Value, name: &str) -> Value {
        value
            .as_object()
            .and_then(|m| m.get(name))
            .cloned()
            .expect("field")
    }

    fn kind_of(result: Result<Value, OsError>) -> OsErrorKind {
        result.expect_err("an error").kind
    }

    fn tcp_grant(port: u16) -> OsGrants {
        grants(&[(format!("127.0.0.1:{port}"), &[NetKind::Tcp])])
    }

    #[test]
    fn a_keyed_recv_under_a_route_defers_and_delivers_the_bytes_when_they_arrive() {
        use crate::os_host::completion::OsCompletionSink;
        use crate::resumable::CallRoutes;
        use std::sync::{Arc, Mutex};
        struct Collect(Mutex<mpsc::Sender<(String, Result<Value, OsError>)>>);
        impl OsCompletionSink for Collect {
            fn complete(&self, _: &CallSite, key: &str, outcome: Result<Value, OsError>) {
                let _ = self.0.lock().expect("lock").send((key.to_string(), outcome));
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            std::thread::sleep(Duration::from_millis(150));
            stream.write_all(b"hi").expect("write");
            stream
        });
        with_host(tcp_grant(port), |host| {
            let (tx, rx) = mpsc::channel();
            host.handles.completions.set_sink(Arc::new(Collect(Mutex::new(tx))));
            let sock = field(&ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(i64::from(port))]).expect("connected"), "sock");
            let site = CallSite {
                orbital: None,
                trait_name: "T".to_string(),
                follow_on: false,
                entity_id: "singleton".to_string(),
                emit: CallRoutes { success: Some("GOT".to_string()), ..CallRoutes::default() },
            };
            let started = Instant::now();
            assert!(recv_later(&[sock.clone(), n(16)], "r1", &site, host).is_ok());
            assert!(started.elapsed() < Duration::from_millis(100), "the call returned before the bytes arrived");
            let (key, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("the completion arrives");
            assert_eq!(key, "r1");
            assert_eq!(field(&outcome.expect("bytes"), "data"), bytes_value(b"hi"));
            assert!(
                recv_later(&[sock.clone(), n(16)], "r3", &site, host).is_ok()
                    && recv_later(&[sock, n(16)], "r3", &site, host).is_err(),
                "control: a key with a completion pending is refused"
            );
            host.handles.completions.cancel("r3");
        });
        drop(server.join());
    }

    type Fired = (CallSite, String, Result<Value, OsError>);

    fn collecting_sink(handles: &OsHandles) -> mpsc::Receiver<Fired> {
        use crate::os_host::completion::OsCompletionSink;
        use std::sync::{Arc, Mutex};
        struct Collect(Mutex<mpsc::Sender<Fired>>);
        impl OsCompletionSink for Collect {
            fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>) {
                let _ = self.0.lock().expect("lock").send((site.clone(), key.to_string(), outcome));
            }
        }
        let (tx, rx) = mpsc::channel();
        handles.completions.set_sink(Arc::new(Collect(Mutex::new(tx))));
        rx
    }

    fn listen_site() -> CallSite {
        CallSite {
            orbital: None,
            trait_name: "T".to_string(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: crate::resumable::CallRoutes {
                success: Some("LISTENING".to_string()),
                failure: Some("FAILED".to_string()),
                on_message: Some("ACCEPTED".to_string()),
                ..crate::resumable::CallRoutes::default()
            },
        }
    }

    fn id_of(value: &Value) -> i64 {
        match value {
            Value::Number(v) => *v as i64,
            other => panic!("not a handle: {other:?}"),
        }
    }

    #[test]
    fn a_keyed_tcp_listen_streams_each_accepted_connection_to_the_message_route_until_closed() {
        use crate::resumable::completion_event;
        let handles = OsHandles::default();
        let rx = collecting_sink(&handles);
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let listen_grants = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
        let listening = OsHost { grants: &listen_grants, handles: &handles, on_event: &on_event };
        let site = listen_site();
        let bound = answer(OsOp::TcpListen, &[s("127.0.0.1"), n(0)], Some("l1"), Some(&site), &listening)
            .expect("net answers tcp-listen")
            .expect("listening");
        let listener = id_of(&field(&bound, "sock"));
        let port = id_of(&field(&bound, "port"));
        assert!(port > 0, "an ephemeral port is reported");
        assert_eq!(field(&bound, "address"), s("127.0.0.1"));
        let Value::Object(fields) = &bound else { panic!("listener reply must be an object") };
        let names: Vec<&str> = fields.keys().map(String::as_str).collect();
        let contract = orbital_core_base::emit_contract::success_payload("os/tcp-listen", orbital_core_base::emit_contract::CallVariant::Plain);
        assert!(orbital_core_base::emit_contract::undeclared_reads(&names, &contract, &[]).is_empty(), "the published contract must expose every field the listener returns");
        let connect_grants = tcp_grant(port as u16);
        let client = OsHost { grants: &connect_grants, handles: &handles, on_event: &on_event };
        let mut accepted = Vec::new();
        for _ in 0..2 {
            let sock = field(&ask(&client, OsOp::TcpConnect, &[s("127.0.0.1"), n(port)]).expect("connected"), "sock");
            let (at, key, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("an accepted connection arrives");
            assert_eq!(key, "l1");
            let (event, payload) = completion_event(&at, &key, outcome).expect("routed");
            assert_eq!(event, "ACCEPTED", "each connection fires the message route, never success");
            assert_eq!(payload.get_path("key"), Some(&s("l1")));
            assert!(matches!(payload.get_path("peer"), Some(Value::String(p)) if p.starts_with("127.0.0.1:")));
            let conn = payload.get_path("sock").cloned().expect("the accepted socket");
            assert_ne!(id_of(&conn), listener);
            accepted.push((sock, conn));
        }
        let (client_sock, server_sock) = accepted.pop().expect("one");
        ask(&client, OsOp::Send, &[client_sock, bytes_value(b"ping")]).expect("send");
        let got = ask(&client, OsOp::Recv, &[server_sock, n(16)]).expect("the accepted socket reads");
        assert_eq!(field(&got, "data"), bytes_value(b"ping"));
        assert!(handles.completions.is_live("l1"), "the stream stays open across connections");
        answer(OsOp::Close, &[n(listener)], None, None, &client).expect("a socket").expect("closed");
        let deadline = Instant::now() + Duration::from_secs(5);
        while handles.completions.is_live("l1") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!handles.completions.is_live("l1"), "closing the listener ends the stream and frees its key");
        assert!(
            ask(&client, OsOp::TcpConnect, &[s("127.0.0.1"), n(port)]).is_err(),
            "control: nothing listens after close"
        );
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "no event after close");
        handles.close_all();
    }

    fn accept_one(handles: &OsHandles, rx: &mpsc::Receiver<Fired>) -> (i64, TcpStream) {
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let listen_grants = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
        let host = OsHost { grants: &listen_grants, handles, on_event: &on_event };
        let bound = answer(OsOp::TcpListen, &[s("127.0.0.1"), n(0)], Some("lc"), Some(&listen_site()), &host)
            .expect("answered")
            .expect("listening");
        let client = TcpStream::connect(("127.0.0.1", id_of(&field(&bound, "port")) as u16)).expect("connect");
        client.set_read_timeout(Some(Duration::from_secs(2))).expect("timeout");
        let (_, _, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("accepted");
        (id_of(&field(&outcome.expect("a connection"), "sock")), client)
    }

    fn outcome_of(rx: &mpsc::Receiver<Fired>, key: &str) -> Result<Value, OsError> {
        loop {
            let (_, got, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("a completion");
            if got == key {
                return outcome;
            }
        }
    }

    #[test]
    fn an_accepted_connection_takes_sends_over_time_reads_a_second_request_and_reports_a_client_abort() {
        let handles = OsHandles::default();
        let rx = collecting_sink(&handles);
        let (conn, mut client) = accept_one(&handles, &rx);
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let none = OsGrants::default();
        let host = OsHost { grants: &none, handles: &handles, on_event: &on_event };
        client.write_all(b"req1").expect("write");
        assert_eq!(field(&ask(&host, OsOp::Recv, &[n(conn), n(16)]).expect("read"), "data"), bytes_value(b"req1"));
        for chunk in [b"a", b"b"] {
            ask(&host, OsOp::Send, &[n(conn), bytes_value(chunk)]).expect("a chunk is written");
            let mut got = [0u8; 1];
            client.read_exact(&mut got).expect("the chunk arrives before the stream ends");
            assert_eq!(&got, chunk);
            std::thread::sleep(Duration::from_millis(50));
        }
        client.write_all(b"req2").expect("write");
        assert_eq!(
            field(&ask(&host, OsOp::Recv, &[n(conn), n(16)]).expect("read"), "data"),
            bytes_value(b"req2"),
            "the read side stays open after a reply"
        );
        assert!(recv_later(&[n(conn), n(16)], "watch", &listen_site(), &host).is_ok());
        drop(client);
        let ended = outcome_of(&rx, "watch");
        assert!(
            matches!(&ended, Ok(v) if field(v, "eof") == Value::Bool(true))
                || matches!(&ended, Err(e) if e.kind == OsErrorKind::Closed),
            "a client abort reaches the program on the watching read: {ended:?}"
        );
        handles.close_all();
    }

    fn recv_fired(rx: &mpsc::Receiver<Fired>, within: Duration) -> (String, Result<Value, OsError>) {
        let (_, key, outcome) = rx.recv_timeout(within).expect("a completion");
        (key, outcome)
    }

    #[test]
    fn a_keyed_send_to_a_peer_that_never_reads_returns_at_once_and_completes_later() {
        let handles = OsHandles::default();
        let rx = collecting_sink(&handles);
        let (conn, client) = accept_one(&handles, &rx);
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let none = OsGrants::default();
        let host = OsHost { grants: &none, handles: &handles, on_event: &on_event };
        let big = vec![b'a'; 16 * 1024 * 1024];
        let started = Instant::now();
        assert!(send_later(&[n(conn), bytes_value(&big)], "s1", &listen_site(), &host).is_ok());
        assert!(started.elapsed() < Duration::from_millis(500), "the dispatch is not held by the write: {:?}", started.elapsed());
        // The 5 s bound is per write call: a stalled peer that took part of the payload ends it after a few bounds.
        let (key, outcome) = recv_fired(&rx, Duration::from_secs(30));
        assert_eq!(key, "s1");
        assert!(
            matches!(&outcome, Err(e) if e.kind == OsErrorKind::Timeout)
                || matches!(&outcome, Ok(v) if field(v, "bytes") == Value::Number(big.len() as f64)),
            "the write ends in its own completion: {outcome:?}"
        );
        drop(client);
        handles.close_all();
    }

    #[test]
    fn keyed_sends_on_one_socket_are_written_in_call_order_and_never_interleave() {
        let handles = OsHandles::default();
        let rx = collecting_sink(&handles);
        let (conn, mut client) = accept_one(&handles, &rx);
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let none = OsGrants::default();
        let host = OsHost { grants: &none, handles: &handles, on_event: &on_event };
        let first = vec![b'a'; 2 * 1024 * 1024];
        let total = first.len() + 6;
        assert!(send_later(&[n(conn), bytes_value(&first)], "s1", &listen_site(), &host).is_ok());
        assert!(send_later(&[n(conn), bytes_value(b"bbbb")], "s2", &listen_site(), &host).is_ok());
        let reader = std::thread::spawn(move || {
            let mut got = vec![0u8; total];
            client.read_exact(&mut got).expect("every byte arrives");
            got
        });
        let sent = ask(&host, OsOp::Send, &[n(conn), bytes_value(b"cc")]).expect("control: an unkeyed send answers in place");
        assert_eq!(field(&sent, "bytes"), Value::Number(2.0), "after the keyed sends before it");
        let got = reader.join().expect("reader");
        assert!(got[..first.len()].iter().all(|b| *b == b'a'), "the first send is written whole before the second");
        assert_eq!(&got[first.len()..], b"bbbbcc");
        let order: Vec<String> = (0..2).map(|_| recv_fired(&rx, Duration::from_secs(5)).0).collect();
        assert_eq!(order, vec!["s1", "s2"], "completions follow the write order");
        handles.close_all();
    }

    #[test]
    fn closing_a_connection_with_a_pending_keyed_recv_ends_it_at_once() {
        let handles = OsHandles::default();
        let rx = collecting_sink(&handles);
        let (conn, mut client) = accept_one(&handles, &rx);
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let none = OsGrants::default();
        let host = OsHost { grants: &none, handles: &handles, on_event: &on_event };
        assert!(recv_later(&[n(conn), n(16)], "watch", &listen_site(), &host).is_ok());
        std::thread::sleep(Duration::from_millis(50));
        answer(OsOp::Close, &[n(conn)], None, None, &host).expect("a socket").expect("closed");
        let started = Instant::now();
        let mut buf = [0u8; 4];
        let read = client.read(&mut buf);
        assert!(
            matches!(read, Ok(0)) && started.elapsed() < Duration::from_secs(1),
            "the client sees the end at once, not when the pending read gives up: {read:?} after {:?}",
            started.elapsed()
        );
        let woke = outcome_of(&rx, "watch");
        assert!(woke.is_ok_and(|v| field(&v, "eof") == Value::Bool(true)), "the pending read ends with eof");
        handles.close_all();
    }

    #[test]
    fn cancelling_a_listen_key_ends_its_stream_and_a_listen_needs_a_key_a_sink_and_a_grant() {
        let handles = OsHandles::default();
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let listen_grants = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
        let host = OsHost { grants: &listen_grants, handles: &handles, on_event: &on_event };
        let site = listen_site();
        let args = [s("127.0.0.1"), n(0)];
        let no_sink = answer(OsOp::TcpListen, &args, Some("l0"), Some(&site), &host).expect("answered");
        assert_eq!(kind_of(no_sink), OsErrorKind::Unsupported, "no sink: nowhere to stream to");
        let rx = collecting_sink(&handles);
        let unkeyed = answer(OsOp::TcpListen, &args, None, None, &host).expect("answered");
        assert_eq!(kind_of(unkeyed), OsErrorKind::Invalid, "a stream needs a key");
        with_host(OsGrants::default(), |ungranted| {
            ungranted.handles.completions.set_sink(handles.completions.sink().expect("sink"));
            let denied = answer(OsOp::TcpListen, &args, Some("l9"), Some(&site), ungranted).expect("answered");
            assert_eq!(kind_of(denied), OsErrorKind::Denied);
        });
        let bound = answer(OsOp::TcpListen, &args, Some("l2"), Some(&site), &host)
            .expect("answered")
            .expect("listening");
        let again = answer(OsOp::TcpListen, &args, Some("l2"), Some(&site), &host).expect("answered");
        assert_eq!(kind_of(again), OsErrorKind::Invalid, "control: a live key is refused");
        let port = id_of(&field(&bound, "port"));
        assert!(handles.cancel_completion("l2"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut quiet = false;
        while Instant::now() < deadline && !quiet {
            let _ = TcpStream::connect(("127.0.0.1", port as u16));
            quiet = rx.recv_timeout(Duration::from_millis(200)).is_err();
        }
        assert!(quiet, "a cancelled stream delivers nothing");
        handles.close_all();
    }

    #[test]
    fn listening_needs_the_listen_grant_the_program_check_requires_and_a_tcp_grant_alone_is_denied() {
        use orbital_core_base::os_grants::{os_requirements, OsRequirement};
        use orbital_core_base::schema::sexpr::SExpression;
        let program_needs = os_requirements(
            OsOp::TcpListen,
            &[SExpression::String("127.0.0.1".into()), SExpression::Number(0.0)],
        );
        assert_eq!(
            program_needs,
            Ok(vec![OsRequirement::Net { host: "127.0.0.1".into(), port: 0, kinds: vec![NetKind::Listen] }])
        );
        let site = listen_site();
        let args = [s("127.0.0.1"), n(0)];
        for (op, key) in [(OsOp::TcpListen, "g1")] {
            let handles = OsHandles::default();
            let _rx = collecting_sink(&handles);
            let on_event = |_: Option<&CallSite>, _: Value| {};
            let listen_only = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
            let host = OsHost { grants: &listen_only, handles: &handles, on_event: &on_event };
            assert!(
                answer(op, &args, Some(key), Some(&site), &host).expect("answered").is_ok(),
                "{}: a listen grant alone listens",
                op.name()
            );
            let tcp_only = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Tcp])]);
            let host = OsHost { grants: &tcp_only, handles: &handles, on_event: &on_event };
            let denied = answer(op, &args, Some("other"), Some(&site), &host).expect("answered");
            assert_eq!(kind_of(denied), OsErrorKind::Denied, "control {}: a tcp grant does not listen", op.name());
            handles.close_all();
        }
    }

    #[test]
    fn tcp_connect_send_recv_round_trip_then_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4];
            conn.read_exact(&mut buf).expect("read");
            assert_eq!(&buf, b"ping");
            conn.write_all(b"pong").expect("write");
        });
        with_host(tcp_grant(port), |host| {
            let opened =
                ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
            let sock = field(&opened, "sock");
            let sent = ask(host, OsOp::Send, &[sock.clone(), bytes_value(b"ping")]).expect("send");
            assert_eq!(field(&sent, "bytes"), Value::Number(4.0));
            let got = ask(host, OsOp::Recv, &[sock.clone(), n(16)]).expect("recv");
            assert_eq!(field(&got, "data"), bytes_value(b"pong"));
            assert_eq!(field(&got, "eof"), Value::Bool(false));
            server.join().expect("server");
            let end = ask(host, OsOp::Recv, &[sock.clone(), n(16)]).expect("eof");
            assert_eq!(field(&end, "data"), s(""));
            assert_eq!(field(&end, "eof"), Value::Bool(true));
            let zero = ask(host, OsOp::Recv, &[sock, n(0)]).expect("zero");
            assert_eq!(field(&zero, "eof"), Value::Bool(false));
        });
    }

    #[test]
    fn close_removes_the_socket_and_later_use_is_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        with_host(tcp_grant(port), |host| {
            let opened =
                ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
            let sock = field(&opened, "sock");
            let closed = answer(OsOp::Close, std::slice::from_ref(&sock), None, None, host)
                .expect("a socket")
                .expect("closed");
            assert_eq!(closed, Value::Object(ValueMap::new()));
            assert_eq!(
                kind_of(ask(host, OsOp::Send, &[sock.clone(), s("aGk=")])),
                OsErrorKind::Closed
            );
            assert_eq!(
                kind_of(ask(host, OsOp::Recv, &[sock.clone(), n(1)])),
                OsErrorKind::Closed
            );
            assert!(answer(OsOp::Close, &[sock], None, None, host).is_none());
            assert!(answer(OsOp::Close, &[n(9999)], None, None, host).is_none());
        });
    }

    #[test]
    fn connect_is_denied_without_a_matching_grant() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let args = [s("127.0.0.1"), n(port as i64)];
        with_host(OsGrants::default(), |host| {
            assert_eq!(
                kind_of(ask(host, OsOp::TcpConnect, &args)),
                OsErrorKind::Denied
            );
        });
        with_host(tcp_grant(port.wrapping_add(1)), |host| {
            assert_eq!(
                kind_of(ask(host, OsOp::TcpConnect, &args)),
                OsErrorKind::Denied
            );
        });
        with_host(
            grants(&[(format!("127.0.0.1:{port}"), &[NetKind::Udp])]),
            |host| {
                assert_eq!(
                    kind_of(ask(host, OsOp::TcpConnect, &args)),
                    OsErrorKind::Denied
                );
            },
        );
    }

    #[test]
    fn a_granted_port_on_another_host_is_denied() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        with_host(tcp_grant(port), |host| {
            assert_eq!(
                kind_of(ask(
                    host,
                    OsOp::TcpConnect,
                    &[s("127.0.0.2"), n(port as i64)]
                )),
                OsErrorKind::Denied
            );
        });
    }

    #[test]
    fn a_host_name_resolving_to_loopback_is_refused_unless_the_grant_names_the_ip() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        with_host(
            grants(&[(format!("localhost:{port}"), &[NetKind::Tcp])]),
            |host| {
                let refused = ask(host, OsOp::TcpConnect, &[s("localhost"), n(port as i64)])
                    .expect_err("refused");
                assert_eq!(refused.kind, OsErrorKind::Denied);
                assert!(
                    refused.message.contains("not a public address"),
                    "{}",
                    refused.message
                );
            },
        );
        with_host(tcp_grant(port), |host| {
            assert!(ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).is_ok());
        });
    }

    #[test]
    fn udp_bind_sendto_recvfrom_round_trip() {
        let peer = UdpSocket::bind("127.0.0.1:0").expect("peer");
        peer.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let peer_addr = peer.local_addr().expect("addr");
        let free = UdpSocket::bind("127.0.0.1:0").expect("free");
        let port = free.local_addr().expect("addr").port();
        drop(free);
        let g = grants(&[
            (format!("127.0.0.1:{port}"), &[NetKind::Udp]),
            (format!("127.0.0.1:{}", peer_addr.port()), &[NetKind::Udp]),
        ]);
        with_host(g, |host| {
            let bound = ask(host, OsOp::UdpBind, &[s("127.0.0.1"), n(port as i64)]).expect("bind");
            let sock = field(&bound, "sock");
            let sent = ask(
                host,
                OsOp::Sendto,
                &[
                    sock.clone(),
                    bytes_value(b"hi"),
                    s("127.0.0.1"),
                    n(peer_addr.port() as i64),
                ],
            )
            .expect("sendto");
            assert_eq!(field(&sent, "bytes"), Value::Number(2.0));
            let mut buf = [0u8; 16];
            let (len, from) = peer.recv_from(&mut buf).expect("peer recv");
            assert_eq!(&buf[..len], b"hi");
            assert_eq!(from.port(), port);
            peer.send_to(b"yo", from).expect("reply");
            let got = ask(host, OsOp::Recvfrom, &[sock.clone(), n(64)]).expect("recvfrom");
            assert_eq!(field(&got, "data"), bytes_value(b"yo"));
            assert_eq!(field(&got, "peer"), Value::String(peer_addr.to_string()));
            let other = ask(
                host,
                OsOp::Sendto,
                &[sock.clone(), s("aGk="), s("127.0.0.1"), n(1)],
            );
            assert_eq!(kind_of(other), OsErrorKind::Denied);
            assert_eq!(
                kind_of(ask(host, OsOp::Recv, &[sock, n(1)])),
                OsErrorKind::Invalid
            );
        });
        with_host(OsGrants::default(), |host| {
            assert_eq!(
                kind_of(ask(host, OsOp::UdpBind, &[s("127.0.0.1"), n(port as i64)])),
                OsErrorKind::Denied
            );
        });
    }

    #[test]
    fn dns_resolve_needs_a_name_grant_and_filters_by_family() {
        let args = [s("localhost")];
        with_host(OsGrants::default(), |host| {
            assert_eq!(
                kind_of(ask(host, OsOp::DnsResolve, &args)),
                OsErrorKind::Denied
            );
        });
        with_host(
            grants(&[("localhost:0".to_string(), &[NetKind::Tcp])]),
            |host| {
                let all = ask(host, OsOp::DnsResolve, &args).expect("resolve");
                let Value::Array(list) = field(&all, "addresses") else {
                    panic!("array")
                };
                assert!(!list.is_empty());
                assert!(list.iter().all(|v| matches!(v, Value::String(_))));
                let four = ask(host, OsOp::DnsResolve, &[s("localhost"), n(4)]).expect("v4");
                let Value::Array(list) = field(&four, "addresses") else {
                    panic!("array")
                };
                assert!(list
                    .iter()
                    .all(|v| matches!(v, Value::String(t) if !t.contains(':'))));
                assert_eq!(
                    kind_of(ask(host, OsOp::DnsResolve, &[s("localhost"), n(5)])),
                    OsErrorKind::Invalid
                );
                assert_eq!(
                    kind_of(ask(host, OsOp::DnsResolve, &[s("other.example")])),
                    OsErrorKind::Denied
                );
            },
        );
        with_host(
            grants(&[("127.0.0.1:0".to_string(), &[NetKind::Udp])]),
            |host| {
                let lit = ask(host, OsOp::DnsResolve, &[s("127.0.0.1")]).expect("literal");
                assert_eq!(field(&lit, "addresses"), Value::Array(vec![s("127.0.0.1")]));
            },
        );
    }

    #[test]
    fn recv_times_out_instead_of_hanging() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (release, held) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (_conn, _) = listener.accept().expect("accept");
            let _ = held.recv();
        });
        with_host(tcp_grant(port), |host| {
            let opened =
                ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
            let sock = field(&opened, "sock");
            let id = match sock {
                Value::Number(v) => v as i64,
                _ => panic!("sock"),
            };
            host.handles.net.socks.with(id, |entry| {
                if let SockEntry::Stream(stream) = entry {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .expect("timeout");
                }
            });
            let started = Instant::now();
            let result = ask(host, OsOp::Recv, &[n(id), n(8)]);
            assert_eq!(kind_of(result), OsErrorKind::Timeout);
            assert!(started.elapsed() < Duration::from_secs(3));
        });
        release.send(()).expect("release");
        server.join().expect("server");
    }

    #[test]
    fn shutdown_ends_the_stream_and_a_datagram_socket_is_not_a_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        with_host(tcp_grant(port), |host| {
            let opened =
                ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
            let sock = field(&opened, "sock");
            assert_eq!(
                ask(host, OsOp::Shutdown, std::slice::from_ref(&sock)).expect("shutdown"),
                Value::Object(ValueMap::new())
            );
            let after = kind_of(ask(host, OsOp::Send, &[sock, s("aGk=")]));
            assert!(
                matches!(after, OsErrorKind::Closed | OsErrorKind::Io),
                "{after:?}"
            );
            assert_eq!(
                kind_of(ask(host, OsOp::Shutdown, &[n(4242)])),
                OsErrorKind::Closed
            );
        });
    }

    #[test]
    fn watching_a_port_is_unsupported_and_bad_arguments_are_invalid() {
        with_host(OsGrants::default(), |host| {
            let error = ask(host, OsOp::WatchPort, &[n(80), s("tcp")]).expect_err("unsupported");
            assert_eq!(error.kind, OsErrorKind::Unsupported);
            assert!(error.message.contains("streaming host not implemented"));
            for args in [
                vec![s("127.0.0.1"), n(70_000)],
                vec![s("127.0.0.1"), n(-1)],
                vec![s(""), n(80)],
                vec![n(1), n(80)],
            ] {
                assert_eq!(
                    kind_of(ask(host, OsOp::TcpConnect, &args)),
                    OsErrorKind::Invalid
                );
            }
            assert_eq!(
                kind_of(ask(host, OsOp::Recv, &[n(3), n(-1)])),
                OsErrorKind::Invalid
            );
            assert_eq!(
                kind_of(ask(host, OsOp::Send, &[n(3), s("!!")])),
                OsErrorKind::Invalid
            );
            assert!(answer(OsOp::Cwd, &[], None, None, host).is_none());
        });
    }
}
