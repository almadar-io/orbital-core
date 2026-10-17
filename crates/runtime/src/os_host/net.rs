//! The `net` family: TCP, UDP and DNS. Listening and port watching are unbounded streams a synchronous
//! answerer cannot serve, so they answer `unsupported`.
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

use super::args::{arg_bytes, arg_i64, arg_opt, arg_str, bytes_value, denied};
use super::{HandleSlab, IdCounter, OsHost};
use crate::resumable::CallSite;
use orbital_core_base::http_wire::is_public_address;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::os_grants::NetKind;
use orbital_core_base::value::{Value, ValueMap};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RECV: usize = 64 * 1024 * 1024;
const MAX_DATAGRAM: usize = 65_535;

#[allow(dead_code)]
pub(super) enum SockEntry {
    Stream(TcpStream),
    Listener(std::net::TcpListener),
    Datagram(UdpSocket),
}

/// Open sockets; ids come from the counter shared with the fs table, so an id names one handle.
pub struct NetTable {
    pub(super) socks: HandleSlab<SockEntry>,
}

impl NetTable {
    pub(super) fn new(ids: IdCounter) -> Self {
        Self {
            socks: HandleSlab::new(ids),
        }
    }

    pub(super) fn clear(&self) {
        self.socks.clear();
    }
}

fn err(op: OsOp, kind: OsErrorKind, what: impl std::fmt::Display) -> OsError {
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

fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
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

fn send(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Send;
    let id = arg_i64(args, 0, op)?;
    let data = arg_bytes(args, 1, op)?;
    let mut stream = stream_of(&host.handles.net.socks, id, op)?;
    stream.write_all(&data).map_err(|e| io_error(op, &e))?;
    Ok(object([("bytes", count(data.len()))]))
}

fn recv(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Recv;
    let id = arg_i64(args, 0, op)?;
    let want = count_arg(args, 1, op)?.min(MAX_RECV);
    let mut stream = stream_of(&host.handles.net.socks, id, op)?;
    let mut buf = vec![0u8; want];
    let read = stream.read(&mut buf).map_err(|e| io_error(op, &e))?;
    Ok(object([
        ("data", bytes_value(&buf[..read])),
        ("eof", Value::Bool(read == 0 && want > 0)),
    ]))
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

fn recvfrom(args: &[Value], host: &OsHost<'_>) -> Result<Value, OsError> {
    let op = OsOp::Recvfrom;
    let id = arg_i64(args, 0, op)?;
    let want = count_arg(args, 1, op)?.min(MAX_RECV).min(MAX_DATAGRAM);
    let socket = datagram_of(&host.handles.net.socks, id, op)?;
    let mut buf = vec![0u8; want];
    let (read, peer) = socket.recv_from(&mut buf).map_err(|e| io_error(op, &e))?;
    Ok(object([
        ("data", bytes_value(&buf[..read])),
        ("peer", Value::String(peer.to_string())),
    ]))
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
    host.handles.net.socks.remove(id).map(|_| Ok(object([])))
}

pub(super) fn answer(
    op: OsOp,
    args: &[Value],
    _key: Option<&str>,
    _site: Option<&CallSite>,
    host: &OsHost<'_>,
) -> Option<Result<Value, OsError>> {
    Some(match op {
        OsOp::TcpConnect => tcp_connect(args, host),
        OsOp::Send => send(args, host),
        OsOp::Recv => recv(args, host),
        OsOp::UdpBind => udp_bind(args, host),
        OsOp::Sendto => sendto(args, host),
        OsOp::Recvfrom => recvfrom(args, host),
        OsOp::DnsResolve => dns_resolve(args, host),
        OsOp::Shutdown => shutdown(args, host),
        OsOp::Close => return close(args, host),
        OsOp::TcpListen | OsOp::WatchPort => Err(err(
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
    fn unbounded_streams_are_unsupported_and_bad_arguments_are_invalid() {
        with_host(OsGrants::default(), |host| {
            for (op, args) in [
                (OsOp::TcpListen, vec![s("127.0.0.1"), n(0)]),
                (OsOp::WatchPort, vec![n(80), s("tcp")]),
            ] {
                let error = ask(host, op, &args).expect_err("unsupported");
                assert_eq!(error.kind, OsErrorKind::Unsupported);
                assert!(error.message.contains("streaming host not implemented"));
            }
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
