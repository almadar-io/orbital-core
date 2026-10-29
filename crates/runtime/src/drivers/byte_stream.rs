//! The byte stream a TCP protocol driver (redis, postgres) speaks over: a sans-I/O step machine whose every call
//! returns a [`StreamEvent`], an `Ask` being answered through [`ByteStream::resume`]. [`PlainTcp`] drives the host's
//! raw `os/tcp-connect`, `os/send`, `os/recv` and `os/close`; the TLS stream wraps the same asks (`tls::TlsStream`).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::codec;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::value::Value;

use crate::driver::{DriverAnswer, DriverAsk};
use crate::http_request::tcp_target_allowed;

/// The bytes one `os/recv` ask requests.
pub const RECV_BYTES: u32 = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Refused before any byte moved: egress, or the host's grants.
    Refused,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFailure {
    pub kind: FailureKind,
    pub message: String,
}

impl StreamFailure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    fn of_os(error: &OsError) -> Self {
        let kind = match error.kind {
            OsErrorKind::Denied => FailureKind::Refused,
            OsErrorKind::Timeout => FailureKind::Timeout,
            _ => FailureKind::Failed,
        };
        Self::new(kind, error.message.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Ask(DriverAsk),
    Connected,
    /// Every byte handed to [`ByteStream::send`] reached the peer.
    Sent,
    /// Bytes from the peer.
    Data(Vec<u8>),
    /// The peer closed and sent nothing more.
    Eof,
    Closed,
    Failed(StreamFailure),
}

pub trait ByteStream {
    fn send(&mut self, bytes: &[u8]) -> StreamEvent;
    /// The next bytes from the peer; ends in `Data`, `Eof` or `Failed`.
    fn recv(&mut self) -> StreamEvent;
    fn close(&mut self) -> StreamEvent;
    /// Answer the outstanding ask.
    fn resume(&mut self, answer: DriverAnswer) -> StreamEvent;
    /// The connected socket a protocol that negotiates TLS in-band (Postgres) hands to `TlsStream::upgrade`.
    fn socket(&self) -> Option<Value> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Awaiting {
    Connect,
    Send,
    Recv,
    Close,
}

pub struct PlainTcp {
    sock: Option<Value>,
    awaiting: Option<Awaiting>,
}

fn os_ask(op: OsOp, args: Vec<Value>) -> StreamEvent {
    StreamEvent::Ask(DriverAsk::Os { op, args, key: None })
}

impl PlainTcp {
    /// Connect to `origin` (`redis://host[:port]`, ...), refused unless `egress` lists it.
    pub fn connect(origin: &str, egress: &[String]) -> (Self, StreamEvent) {
        let mut stream = Self { sock: None, awaiting: None };
        let target = match tcp_target_allowed(origin, egress) {
            Ok(target) => target,
            Err(refusal) => {
                return (stream, StreamEvent::Failed(StreamFailure::new(FailureKind::Refused, refusal.message())))
            }
        };
        stream.awaiting = Some(Awaiting::Connect);
        let ask = os_ask(
            OsOp::TcpConnect,
            vec![Value::string(target.host().to_string()), Value::Number(f64::from(target.port()))],
        );
        (stream, ask)
    }

    fn busy(&self, what: &str) -> Option<StreamEvent> {
        if self.awaiting.is_some() {
            return Some(StreamEvent::Failed(StreamFailure::new(
                FailureKind::Failed,
                format!("tcp: a {what} began while another ask is outstanding"),
            )));
        }
        None
    }

    fn connected(&self, what: &str) -> Result<Value, StreamEvent> {
        self.sock.clone().ok_or_else(|| {
            StreamEvent::Failed(StreamFailure::new(FailureKind::Failed, format!("tcp: {what} on a stream that is not connected")))
        })
    }
}

impl ByteStream for PlainTcp {
    fn send(&mut self, bytes: &[u8]) -> StreamEvent {
        if let Some(refused) = self.busy("send") {
            return refused;
        }
        let sock = match self.connected("send") {
            Ok(sock) => sock,
            Err(event) => return event,
        };
        self.awaiting = Some(Awaiting::Send);
        os_ask(OsOp::Send, vec![sock, Value::string(codec::base64_encode(bytes))])
    }

    fn recv(&mut self) -> StreamEvent {
        if let Some(refused) = self.busy("recv") {
            return refused;
        }
        let sock = match self.connected("recv") {
            Ok(sock) => sock,
            Err(event) => return event,
        };
        self.awaiting = Some(Awaiting::Recv);
        os_ask(OsOp::Recv, vec![sock, Value::Number(f64::from(RECV_BYTES))])
    }

    fn close(&mut self) -> StreamEvent {
        if let Some(refused) = self.busy("close") {
            return refused;
        }
        let sock = match self.connected("close") {
            Ok(sock) => sock,
            Err(event) => return event,
        };
        self.awaiting = Some(Awaiting::Close);
        os_ask(OsOp::Close, vec![sock])
    }

    fn socket(&self) -> Option<Value> {
        self.sock.clone()
    }

    fn resume(&mut self, answer: DriverAnswer) -> StreamEvent {
        let Some(awaiting) = self.awaiting.take() else {
            return StreamEvent::Failed(StreamFailure::new(FailureKind::Failed, "tcp: an answer arrived with no ask outstanding"));
        };
        let DriverAnswer::Os(answer) = answer else {
            return StreamEvent::Failed(StreamFailure::new(FailureKind::Failed, format!("tcp: unexpected answer {answer:?}")));
        };
        match (awaiting, answer) {
            (Awaiting::Close, _) => {
                self.sock = None;
                StreamEvent::Closed
            }
            (_, Err(error)) => StreamEvent::Failed(StreamFailure::of_os(&error)),
            (Awaiting::Connect, Ok(reply)) => match reply.get_path("sock") {
                Some(sock @ Value::Number(_)) => {
                    self.sock = Some(sock.clone());
                    StreamEvent::Connected
                }
                _ => StreamEvent::Failed(StreamFailure::new(FailureKind::Failed, "tcp: os/tcp-connect did not answer a socket")),
            },
            (Awaiting::Send, Ok(_)) => StreamEvent::Sent,
            (Awaiting::Recv, Ok(reply)) => {
                let Some(Value::String(data)) = reply.get_path("data") else {
                    return StreamEvent::Failed(StreamFailure::new(FailureKind::Failed, "tcp: os/recv did not answer data"));
                };
                match codec::base64_decode(data) {
                    Ok(bytes) if bytes.is_empty() => StreamEvent::Eof,
                    Ok(bytes) => StreamEvent::Data(bytes),
                    Err(message) => StreamEvent::Failed(StreamFailure::new(FailureKind::Failed, format!("tcp: os/recv data is not base64: {message}"))),
                }
            }
        }
    }
}

impl ByteStream for crate::tls::TlsStream {
    fn send(&mut self, bytes: &[u8]) -> StreamEvent {
        of_tls(crate::tls::TlsStream::send(self, bytes))
    }

    fn recv(&mut self) -> StreamEvent {
        of_tls(crate::tls::TlsStream::recv(self))
    }

    fn close(&mut self) -> StreamEvent {
        of_tls(crate::tls::TlsStream::close(self))
    }

    fn resume(&mut self, answer: DriverAnswer) -> StreamEvent {
        of_tls(crate::tls::TlsStream::resume(self, answer))
    }
}

/// A TLS stream's event as a [`StreamEvent`]; a TLS failure is a `Failed` one, whatever caused it.
pub fn of_tls(event: crate::tls::TlsEvent) -> StreamEvent {
    use crate::tls::TlsEvent;
    match event {
        TlsEvent::Ask(ask) => StreamEvent::Ask(ask),
        TlsEvent::Connected => StreamEvent::Connected,
        TlsEvent::Sent => StreamEvent::Sent,
        TlsEvent::Data(bytes) => StreamEvent::Data(bytes),
        TlsEvent::Eof => StreamEvent::Eof,
        TlsEvent::Closed => StreamEvent::Closed,
        TlsEvent::Failed(message) => StreamEvent::Failed(StreamFailure::new(FailureKind::Failed, message)),
    }
}

/// The stream a TCP origin is spoken over, with its first event: plain TCP for `redis://`, `postgres://` and
/// `postgresql://`, TLS for `rediss://`. The origin is refused unless `egress` lists it.
pub fn open(origin: &str, egress: &[String]) -> Result<(Box<dyn ByteStream>, StreamEvent), StreamFailure> {
    let target = tcp_target_allowed(origin, egress)
        .map_err(|refusal| StreamFailure::new(FailureKind::Refused, refusal.message()))?;
    if target.scheme() == "rediss" {
        let (stream, event) = crate::tls::TlsStream::connect(origin, egress);
        Ok((Box::new(stream), of_tls(event)))
    } else {
        let (stream, event) = PlainTcp::connect(origin, egress);
        Ok((Box::new(stream), event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::value::ValueMap;

    fn egress() -> Vec<String> {
        vec!["redis://cache.internal:6379".to_string()]
    }

    fn os(op: OsOp, args: Vec<Value>) -> StreamEvent {
        StreamEvent::Ask(DriverAsk::Os { op, args, key: None })
    }

    fn sock_reply() -> DriverAnswer {
        DriverAnswer::Os(Ok(Value::object(ValueMap::from_iter([("sock".into(), Value::Number(7.0))]))))
    }

    #[test]
    fn plain_tcp_connects_sends_receives_and_closes_through_the_os_asks() {
        let (mut stream, first) = PlainTcp::connect("redis://cache.internal", &egress());
        assert_eq!(
            first,
            os(OsOp::TcpConnect, vec![Value::string("cache.internal"), Value::Number(6379.0)])
        );
        assert_eq!(stream.resume(sock_reply()), StreamEvent::Connected);
        assert_eq!(
            stream.send(b"PING"),
            os(OsOp::Send, vec![Value::Number(7.0), Value::string("UElORw==")])
        );
        assert_eq!(stream.resume(DriverAnswer::Os(Ok(Value::Null))), StreamEvent::Sent);
        assert_eq!(stream.recv(), os(OsOp::Recv, vec![Value::Number(7.0), Value::Number(16384.0)]));
        let reply = Value::object(ValueMap::from_iter([
            ("data".into(), Value::String(("K1BPTkcNCg==".to_string()).into())),
            ("eof".into(), Value::Bool(false)),
        ]));
        assert_eq!(stream.resume(DriverAnswer::Os(Ok(reply))), StreamEvent::Data(b"+PONG\r\n".to_vec()));
        assert_eq!(stream.close(), os(OsOp::Close, vec![Value::Number(7.0)]));
        assert_eq!(stream.resume(DriverAnswer::Os(Ok(Value::Null))), StreamEvent::Closed);
    }

    #[test]
    fn an_empty_read_is_the_peer_closing() {
        let (mut stream, _) = PlainTcp::connect("redis://cache.internal", &egress());
        stream.resume(sock_reply());
        stream.recv();
        let reply = Value::object(ValueMap::from_iter([
            ("data".into(), Value::String(String::new().into())),
            ("eof".into(), Value::Bool(true)),
        ]));
        assert_eq!(stream.resume(DriverAnswer::Os(Ok(reply))), StreamEvent::Eof);
    }

    #[test]
    fn os_errors_keep_their_kind() {
        let (mut stream, _) = PlainTcp::connect("redis://cache.internal", &egress());
        let denied = OsError::new(OsErrorKind::Denied, "os/tcp-connect: cache.internal:6379 is not granted");
        assert_eq!(
            stream.resume(DriverAnswer::Os(Err(denied.clone()))),
            StreamEvent::Failed(StreamFailure::new(FailureKind::Refused, denied.message))
        );
        let (mut stream, _) = PlainTcp::connect("redis://cache.internal", &egress());
        stream.resume(sock_reply());
        stream.recv();
        let slow = OsError::new(OsErrorKind::Timeout, "os/recv: timed out");
        assert_eq!(
            stream.resume(DriverAnswer::Os(Err(slow.clone()))),
            StreamEvent::Failed(StreamFailure::new(FailureKind::Timeout, slow.message))
        );
    }

    #[test]
    fn control_a_target_outside_egress_never_asks() {
        let (_, first) = PlainTcp::connect("redis://other.internal:6379", &egress());
        assert_eq!(
            first,
            StreamEvent::Failed(StreamFailure::new(FailureKind::Refused, "egress denied: redis://other.internal:6379"))
        );
        let (_, first) = PlainTcp::connect("https://cache.internal", &egress());
        assert!(matches!(first, StreamEvent::Failed(_)));
    }

    #[test]
    fn control_a_second_ask_while_one_is_outstanding_fails() {
        let (mut stream, _) = PlainTcp::connect("redis://cache.internal", &egress());
        assert!(matches!(stream.send(b"x"), StreamEvent::Failed(_)));
    }

    #[test]
    fn a_stream_not_yet_connected_refuses_a_send() {
        let (mut stream, _) = PlainTcp::connect("redis://cache.internal", &egress());
        stream.resume(DriverAnswer::Os(Err(OsError::new(OsErrorKind::Io, "refused"))));
        assert!(matches!(stream.send(b"x"), StreamEvent::Failed(_)));
    }
}
