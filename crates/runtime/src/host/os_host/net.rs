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
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

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
use orbital_core_base::sync::{Condvar, Mutex};
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
#[derive(Clone)]
pub struct NetTable {
    pub(super) socks: Arc<HandleSlab<SockEntry>>,
    /// The last write turn taken on each stream socket.
    writes: Arc<Mutex<HashMap<i64, Arc<Turn>>>>,
}

impl NetTable {
    pub(super) fn new(ids: IdCounter) -> Self {
        Self {
            socks: Arc::new(HandleSlab::new(ids)),
            writes: Arc::new(Mutex::new(HashMap::new())),
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
    /// Write in this turn and keep it: the next write waits until the turn is dropped.
    fn write<R>(&mut self, write: impl FnOnce() -> R) -> R {
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
    let mut map = ValueMap::default();
    for (key, value) in pairs {
        map.insert(key.to_string().into(), value);
    }
    Value::object(map)
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

fn send_on(stream: TcpStream, data: &[u8], mut turn: WriteTurn) -> Result<Value, OsError> {
    send_in_turn(stream, data, &mut turn)
}

fn send_in_turn(mut stream: TcpStream, data: &[u8], turn: &mut WriteTurn) -> Result<Value, OsError> {
    turn.write(|| stream.write_all(data)).map_err(|e| io_error(OsOp::Send, &e))?;
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
    let mut turn = host.handles.net.take_turn(id);
    // The turn ends after the completion is posted, so completions follow the write order too.
    Completions::spawn_later(
        &host.handles.completions,
        key,
        site,
        move |_| (send_in_turn(stream, &data, &mut turn), turn),
        |completions, key, site, (outcome, turn)| {
            completions.deliver(key, site, outcome);
            drop(turn);
        },
    )
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
        ("peer", Value::string(peer.to_string())),
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
        ("peer", Value::string(peer.to_string())),
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
        ("address", Value::string(local.ip().to_string())),
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
        .map(|ip| Value::string(ip.to_string()))
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
pub(super) fn close(args: &[Value], table: &NetTable) -> Option<Result<Value, OsError>> {
    let id = match arg_i64(args, 0, OsOp::Close) {
        Ok(id) => id,
        Err(e) => return Some(Err(e)),
    };
    table.writes.lock().remove(&id);
    table.socks.remove(id).map(|entry| {
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
        OsOp::Close => return close(args, &host.handles.net),
        OsOp::TlsUpgrade => Err(err(
            op,
            OsErrorKind::Unsupported,
            "a kernel op: the kernel owns TLS and a host implements none",
        )),
        OsOp::WatchPort => Err(err(
            op,
            OsErrorKind::Unsupported,
            "streaming host not implemented",
        )),
        OsOp::Fetch => Err(err(
            op,
            OsErrorKind::Unsupported,
            "a browser host performs a fetch; this host speaks the protocol over os/tcp-connect and os/tls-upgrade",
        )),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
