//! EXEC-HTTP-7: TLS on `os/*` sockets belongs to the kernel. `os/tls-upgrade` (and the TLS argument of
//! `os/tcp-connect`) runs the client handshake through [`TlsStream`] as a journaled sequence of raw `os/send` and
//! `os/recv` asks; from then on the program's `os/send`, `os/recv` and `os/close` on that socket carry plaintext and
//! each becomes its own raw sequence. The host keeps only the raw socket.
//!
//! A socket's session is its recorded transcript (seed, clock, every inbound ciphertext, write and close). Each
//! operation rebuilds the client from the transcript as it stands, drives the operation, and appends only the events
//! the operation itself recorded, when it ends: appends are ordered by completion, so one receive and one send may be
//! outstanding on a socket together (full duplex) and a replayed dispatch rebuilds the same session. A keyed, routed
//! `os/send` or `os/recv` that the host defers (EXEC-QUEUE-10) parks here; the host's raw answer reaches
//! [`RuntimeKernel::tls_complete`], which rebuilds the session, decrypts or encrypts the answer and either asks for the
//! next raw operation or ends with the program's outcome.
//!
//! A dispatch step is replayed from its journal each time the host answers an ask, so what a step's TLS operations
//! change is held in an overlay that lives for one pass: a pass that ends the step commits it, a pass that parks on
//! the host drops it, and the replay builds it again identically.

use super::*;
use super::resumable::AWAITING_HOST;
use crate::driver::{drive, DriveContext, DriveEnd, Driver, DriverAnswer, DriverAsk, DriverStep};
use crate::tls::session::{roots_from_pem, web_roots, Recorded};
use crate::tls::{TlsEvent, TlsStream};
use orbital_core_base::os::OsOp;
use orbital_core_base::secret::Secret;
use orbital_core_base::os_grants::NetKind;
use orbital_core_base::sync::Mutex;
use rustls::RootCertStore;
use std::collections::HashSet;

#[derive(Default)]
pub(crate) struct TlsSockets {
    open: Mutex<HashMap<i64, TlsSocket>>,
    parks: Mutex<HashMap<i64, Parks>>,
    pending: Mutex<HashMap<String, PendingTls>>,
    overlay: Mutex<Option<Overlay>>,
    live: Mutex<HashMap<i64, Live>>,
    rebuilds: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    keep_none: std::sync::atomic::AtomicBool,
}

/// A socket's session kept from the operation that last advanced it, valid while the transcript is still the
/// `len` events and `consumed` bytes it reflects; any other state rebuilds the session from the transcript.
struct Live {
    stream: TlsStream,
    len: usize,
    consumed: usize,
}

#[derive(Clone)]
struct TlsSocket {
    host: String,
    roots: RootCertStore,
    alpn: Vec<Vec<u8>>,
    seed: [u8; 32],
    now: u64,
    transcript: Vec<Recorded>,
    consumed: usize,
}

/// The keys of the send and the receive parked on a socket.
#[derive(Default)]
struct Parks {
    send: Option<String>,
    recv: Option<String>,
}

#[derive(Default)]
struct Delta {
    events: Vec<Recorded>,
    consumed: usize,
}

/// What one pass of one dispatch step changed.
struct Overlay {
    thread: std::thread::ThreadId,
    opened: HashMap<i64, TlsSocket>,
    deltas: HashMap<i64, Delta>,
    closed: HashSet<i64>,
    live: HashMap<i64, Live>,
}

impl TlsSockets {
    pub(crate) fn begin_pass(&self) {
        *self.overlay.lock() = Some(Overlay {
            thread: std::thread::current().id(),
            opened: HashMap::new(),
            deltas: HashMap::new(),
            closed: HashSet::new(),
            live: HashMap::new(),
        });
    }

    /// End the pass; a pass that ended its step commits what it changed.
    pub(crate) fn end_pass(&self, complete: bool) {
        let Some(overlay) = self.overlay.lock().take() else {
            return;
        };
        if !complete {
            return;
        }
        let mut open = self.open.lock();
        for (sock, socket) in overlay.opened {
            open.insert(sock, socket);
        }
        for (sock, delta) in overlay.deltas {
            if let Some(socket) = open.get_mut(&sock) {
                socket.transcript.extend(delta.events);
                socket.consumed += delta.consumed;
            }
        }
        let mut parks = self.parks.lock();
        let mut live = self.live.lock();
        for sock in overlay.closed {
            open.remove(&sock);
            parks.remove(&sock);
            live.remove(&sock);
        }
        live.extend(overlay.live);
    }

    /// The transcript length and delivered byte count of `sock`'s session as this pass sees it.
    fn state(&self, sock: i64) -> Option<(usize, usize)> {
        let overlay = self.overlay.lock();
        let mine = overlay
            .as_ref()
            .filter(|o| o.thread == std::thread::current().id());
        if mine.is_some_and(|o| o.closed.contains(&sock)) {
            return None;
        }
        let (mut len, mut consumed) = match mine.and_then(|o| o.opened.get(&sock)) {
            Some(socket) => (socket.transcript.len(), socket.consumed),
            None => {
                let open = self.open.lock();
                let socket = open.get(&sock)?;
                (socket.transcript.len(), socket.consumed)
            }
        };
        if let Some(delta) = mine.and_then(|o| o.deltas.get(&sock)) {
            len += delta.events.len();
            consumed += delta.consumed;
        }
        Some((len, consumed))
    }

    /// The kept session of `sock` when it reflects exactly `len` events and `consumed` bytes.
    fn take_live(&self, sock: i64, len: usize, consumed: usize) -> Option<TlsStream> {
        let mut overlay = self.overlay.lock();
        let kept = match overlay
            .as_mut()
            .filter(|o| o.thread == std::thread::current().id())
        {
            Some(mine) => mine
                .live
                .remove(&sock)
                .or_else(|| self.live.lock().remove(&sock)),
            None => self.live.lock().remove(&sock),
        };
        let in_pass = overlay.is_some();
        match kept {
            Some(live) if live.len == len && live.consumed == consumed => Some(live.stream),
            other => {
                tracing::debug!(
                    sock,
                    len,
                    consumed,
                    in_pass,
                    kept = ?other.as_ref().map(|live| (live.len, live.consumed)),
                    "tls session rebuilt from the transcript"
                );
                None
            }
        }
    }

    /// Keep `stream` as `sock`'s session when it is idle and its transcript is the socket's whole transcript.
    fn keep(&self, sock: i64, stream: TlsStream) {
        #[cfg(test)]
        if self.keep_none.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if stream.is_waiting() {
            tracing::debug!(sock, "tls session not kept: an ask is outstanding");
            return;
        }
        let Some((len, consumed)) = self.state(sock) else {
            tracing::debug!(sock, "tls session not kept: the socket is closed");
            return;
        };
        let own = stream.transcript().map(<[Recorded]>::len);
        if own != Some(len) {
            tracing::debug!(sock, len, ?own, "tls session not kept: the transcript moved");
            return;
        }
        tracing::debug!(sock, len, consumed, "tls session kept");
        let live = Live { stream, len, consumed };
        let mut overlay = self.overlay.lock();
        match overlay
            .as_mut()
            .filter(|o| o.thread == std::thread::current().id())
        {
            Some(mine) => {
                mine.live.insert(sock, live);
            }
            None => {
                self.live.lock().insert(sock, live);
            }
        }
    }

    /// Keep a receive's session that a pass abandoned at its raw ask, when the receive changed nothing yet and the
    /// committed transcript is the one it began from: the replayed pass takes it up again.
    fn keep_withdrawn(&self, sock: i64, mut stream: TlsStream, start_len: usize) {
        #[cfg(test)]
        if self.keep_none.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if stream.transcript().map(<[Recorded]>::len) != Some(start_len) || !stream.withdraw_recv() {
            return;
        }
        let consumed = match self.open.lock().get(&sock) {
            Some(socket) if socket.transcript.len() == start_len => socket.consumed,
            _ => return,
        };
        self.live.lock().insert(
            sock,
            Live {
                stream,
                len: start_len,
                consumed,
            },
        );
    }

    fn snapshot(&self, sock: i64) -> Option<TlsSocket> {
        let overlay = self.overlay.lock();
        let mine = overlay
            .as_ref()
            .filter(|o| o.thread == std::thread::current().id());
        if mine.is_some_and(|o| o.closed.contains(&sock)) {
            return None;
        }
        let mut socket = match mine.and_then(|o| o.opened.get(&sock)) {
            Some(socket) => socket.clone(),
            None => self.open.lock().get(&sock)?.clone(),
        };
        if let Some(delta) = mine.and_then(|o| o.deltas.get(&sock)) {
            socket.transcript.extend(delta.events.iter().cloned());
            socket.consumed += delta.consumed;
        }
        Some(socket)
    }

    fn contains(&self, sock: i64) -> bool {
        self.state(sock).is_some()
    }

    fn register(&self, sock: i64, socket: TlsSocket) {
        let mut overlay = self.overlay.lock();
        match overlay
            .as_mut()
            .filter(|o| o.thread == std::thread::current().id())
        {
            Some(mine) => {
                mine.closed.remove(&sock);
                mine.opened.insert(sock, socket);
            }
            None => {
                self.open.lock().insert(sock, socket);
            }
        }
    }

    fn record(&self, sock: i64, events: &[Recorded], consumed: usize) {
        let mut overlay = self.overlay.lock();
        match overlay
            .as_mut()
            .filter(|o| o.thread == std::thread::current().id())
        {
            Some(mine) => {
                let delta = mine.deltas.entry(sock).or_default();
                delta.events.extend_from_slice(events);
                delta.consumed += consumed;
            }
            None => {
                if let Some(socket) = self.open.lock().get_mut(&sock) {
                    socket.transcript.extend_from_slice(events);
                    socket.consumed += consumed;
                }
            }
        }
    }

    fn forget(&self, sock: i64) {
        let mut overlay = self.overlay.lock();
        match overlay
            .as_mut()
            .filter(|o| o.thread == std::thread::current().id())
        {
            Some(mine) => {
                mine.opened.remove(&sock);
                mine.deltas.remove(&sock);
                mine.live.remove(&sock);
                mine.closed.insert(sock);
            }
            None => {
                self.open.lock().remove(&sock);
                self.parks.lock().remove(&sock);
            }
        }
        self.live.lock().remove(&sock);
        let parked = self.parks.lock().remove(&sock);
        if let Some(parked) = parked {
            let mut pending = self.pending.lock();
            for key in parked.send.iter().chain(parked.recv.iter()) {
                pending.remove(key);
            }
        }
    }
}

struct PendingTls {
    sock: i64,
    op: TrafficOp,
    plain: Vec<u8>,
    resumed: Option<(TlsStream, usize)>,
    /// For a session parked at its first raw ask: the delivered byte count it began from, so the host's answer resumes
    /// it only while the transcript is still the one it began from.
    parked_from: Option<usize>,
}

#[derive(Clone, Copy)]
enum TrafficOp {
    Send { plain: usize },
    Recv { count: usize },
    Close,
}

/// What the host is asked next to finish a parked TLS operation, or how the operation ended.
#[derive(Debug, Clone, PartialEq)]
pub enum TlsCompletion {
    Ask { op: OsOp, args: Vec<Value> },
    Done(Result<Value, OsError>),
}

struct TlsSpec {
    server_name: String,
    roots: RootCertStore,
    alpn: Vec<Vec<u8>>,
}

fn fail(op: OsOp, kind: OsErrorKind, what: impl std::fmt::Display) -> OsError {
    OsError::new(kind, format!("{}: {what}", op.name()))
}

fn sock_id(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_int)
}

fn record(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::object(ValueMap::from_iter(
        entries.into_iter().map(|(k, v)| (k.into(), v)),
    ))
}

fn parse_spec(op: OsOp, value: &Value, grants: &OsGrants) -> Result<TlsSpec, OsError> {
    let Value::Object(map) = value else {
        return Err(fail(
            op,
            OsErrorKind::Invalid,
            "the TLS argument must be { serverName, ca?, alpn? }",
        ));
    };
    if let Some(extra) = map
        .keys()
        .find(|k| !matches!(k.as_str(), "serverName" | "ca" | "alpn"))
    {
        return Err(fail(
            op,
            OsErrorKind::Invalid,
            format!("the TLS argument has no key `{extra}`"),
        ));
    }
    let Some(Value::String(name)) = map.get("serverName") else {
        return Err(fail(
            op,
            OsErrorKind::Invalid,
            "serverName must be a non-empty string",
        ));
    };
    let server_name = name.as_str().to_string();
    if server_name.is_empty() {
        return Err(fail(
            op,
            OsErrorKind::Invalid,
            "serverName must be a non-empty string",
        ));
    }
    if !grants.net_names_host(&server_name, NetKind::Tcp) {
        return Err(fail(
            op,
            OsErrorKind::Denied,
            format!("serverName {server_name} is not a host the net grant names"),
        ));
    }
    let roots = match map.get("ca") {
        None | Some(Value::Null) => web_roots(),
        Some(Value::String(pem)) => {
            roots_from_pem(pem.as_str()).map_err(|e| fail(op, OsErrorKind::Invalid, e))?
        }
        Some(_) => return Err(fail(op, OsErrorKind::Invalid, "ca must be PEM text")),
    };
    let alpn = match map.get("alpn") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| match name {
                Value::String(text) if !text.as_str().is_empty() => {
                    Ok(text.as_str().as_bytes().to_vec())
                }
                _ => Err(fail(
                    op,
                    OsErrorKind::Invalid,
                    "alpn must list non-empty protocol names",
                )),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => {
            return Err(fail(
                op,
                OsErrorKind::Invalid,
                "alpn must list protocol names",
            ))
        }
    };
    Ok(TlsSpec {
        server_name,
        roots,
        alpn,
    })
}

struct Handshaken {
    sock: Value,
    stream: TlsStream,
    spec: TlsSpec,
}

/// The handshake as a [`Driver`]: an optional raw connect, then the TLS stream's own asks.
struct HandshakeRun {
    op: OsOp,
    connect: Option<Vec<Value>>,
    spec: Option<TlsSpec>,
    stream: Option<TlsStream>,
    outcome: Option<Result<Handshaken, OsError>>,
}

impl HandshakeRun {
    fn first(&mut self, sock: Option<Value>) -> DriverStep {
        match (&self.connect, sock) {
            (Some(args), None) => DriverStep::Ask(DriverAsk::Os {
                op: OsOp::TcpConnect,
                args: args.clone(),
                key: None,
            }),
            (_, Some(sock)) => self.begin(sock),
            (None, None) => self.end(Err(fail(
                self.op,
                OsErrorKind::Invalid,
                "no socket to upgrade",
            ))),
        }
    }

    fn begin(&mut self, sock: Value) -> DriverStep {
        let Some(spec) = self.spec.take() else {
            return self.end(Err(fail(
                self.op,
                OsErrorKind::Invalid,
                "the handshake began twice",
            )));
        };
        let (stream, event) = TlsStream::upgrade_with(
            &spec.server_name,
            sock,
            spec.roots.clone(),
            spec.alpn.clone(),
        );
        self.spec = Some(spec);
        self.stream = Some(stream.recorded());
        self.event(event)
    }

    fn event(&mut self, event: TlsEvent) -> DriverStep {
        match event {
            TlsEvent::Ask(ask) => DriverStep::Ask(ask),
            TlsEvent::Connected => {
                let (Some(stream), Some(spec)) = (self.stream.take(), self.spec.take()) else {
                    return self.end(Err(fail(
                        self.op,
                        OsErrorKind::Tls,
                        "the handshake ended with no session",
                    )));
                };
                let sock = stream.sock().clone();
                self.end(Ok(Handshaken { sock, stream, spec }))
            }
            TlsEvent::Failed(message) => {
                let error = self
                    .stream
                    .as_ref()
                    .and_then(|stream| stream.host_error().cloned())
                    .unwrap_or_else(|| OsError::new(OsErrorKind::Tls, message));
                self.end(Err(error))
            }
            other => self.end(Err(fail(
                self.op,
                OsErrorKind::Tls,
                format!("unexpected {other:?} during the handshake"),
            ))),
        }
    }

    fn end(&mut self, outcome: Result<Handshaken, OsError>) -> DriverStep {
        self.outcome = Some(outcome);
        DriverStep::Done(Ok(Value::Null))
    }
}

impl Driver for HandshakeRun {
    fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
        if self.stream.is_none() {
            return match answer {
                DriverAnswer::Os(Ok(Value::Object(fields))) => match fields.get("sock") {
                    Some(sock) => self.begin(sock.clone()),
                    None => self.end(Err(fail(
                        self.op,
                        OsErrorKind::Io,
                        "os/tcp-connect did not answer a socket",
                    ))),
                },
                DriverAnswer::Os(Ok(_)) => self.end(Err(fail(
                    self.op,
                    OsErrorKind::Io,
                    "os/tcp-connect did not answer a socket",
                ))),
                DriverAnswer::Os(Err(error)) => self.end(Err(error)),
                _ => self.end(Err(fail(
                    self.op,
                    OsErrorKind::Io,
                    "unexpected answer to os/tcp-connect",
                ))),
            };
        }
        let Some(stream) = self.stream.as_mut() else {
            return self.end(Err(fail(
                self.op,
                OsErrorKind::Tls,
                "the handshake has no stream",
            )));
        };
        let event = stream.resume(answer);
        self.event(event)
    }
}

/// A traffic operation as a [`Driver`] over a stream.
struct TrafficRun {
    sockets: Arc<TlsSockets>,
    sock: i64,
    stream: TlsStream,
    op: TrafficOp,
    start_len: usize,
    done: Option<Result<Value, OsError>>,
}

impl TrafficRun {
    fn step(&mut self, event: TlsEvent) -> DriverStep {
        match advance(
            &self.sockets,
            self.sock,
            &mut self.stream,
            self.op,
            self.start_len,
            event,
        ) {
            TlsCompletion::Ask { op, args } => DriverStep::Ask(DriverAsk::Os {
                op,
                args,
                key: None,
            }),
            TlsCompletion::Done(outcome) => {
                self.done = Some(outcome);
                DriverStep::Done(Ok(Value::Null))
            }
        }
    }
}

impl Driver for TrafficRun {
    fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
        let event = self.stream.resume(answer);
        self.step(event)
    }
}

fn op_of(op: TrafficOp) -> OsOp {
    match op {
        TrafficOp::Send { .. } => OsOp::Send,
        TrafficOp::Recv { .. } => OsOp::Recv,
        TrafficOp::Close => OsOp::Close,
    }
}

/// One stream event of a traffic operation: the next raw ask, or the end, appending the operation's own events to the
/// socket's transcript (or dropping the session on a failure).
fn advance(
    sockets: &TlsSockets,
    sock: i64,
    stream: &mut TlsStream,
    op: TrafficOp,
    start_len: usize,
    event: TlsEvent,
) -> TlsCompletion {
    let program_op = op_of(op);
    let commit = |delivered: usize| {
        let events = stream
            .transcript()
            .and_then(|t| t.get(start_len..))
            .unwrap_or_default();
        sockets.record(sock, events, delivered);
    };
    match (event, op) {
        (TlsEvent::Ask(DriverAsk::Os { op, args, .. }), _) => TlsCompletion::Ask { op, args },
        (TlsEvent::Sent, TrafficOp::Send { plain }) => {
            commit(0);
            TlsCompletion::Done(Ok(record([("bytes", Value::Number(plain as f64))])))
        }
        (TlsEvent::Data(bytes), TrafficOp::Recv { count }) => {
            let taken = bytes.len().min(count);
            commit(taken);
            stream.unread(&bytes[taken..]);
            TlsCompletion::Done(Ok(record([
                ("data", Value::bytes(bytes[..taken].to_vec())),
                ("eof", Value::Bool(false)),
            ])))
        }
        (TlsEvent::Eof, TrafficOp::Recv { .. }) => {
            commit(0);
            TlsCompletion::Done(Ok(record([
                ("data", Value::string("")),
                ("eof", Value::Bool(true)),
            ])))
        }
        (TlsEvent::Closed, TrafficOp::Close) => {
            sockets.forget(sock);
            TlsCompletion::Done(Ok(record([])))
        }
        (TlsEvent::TimedOut(error), _) => TlsCompletion::Done(Err(error)),
        (TlsEvent::Failed(message), _) => {
            sockets.forget(sock);
            let error = stream.host_error().cloned().unwrap_or_else(|| {
                OsError::new(
                    OsErrorKind::Tls,
                    format!("{}: {message}", program_op.name()),
                )
            });
            TlsCompletion::Done(Err(error))
        }
        (other, _) => {
            sockets.forget(sock);
            TlsCompletion::Done(Err(fail(
                program_op,
                OsErrorKind::Tls,
                format!("unexpected {other:?}"),
            )))
        }
    }
}

fn traffic_of(op: OsOp, args: &[Value]) -> Result<(TrafficOp, Vec<u8>), OsError> {
    Ok(match op {
        OsOp::Send => match args.get(1) {
            Some(data @ (Value::String(_) | Value::Bytes(_))) => {
                let plain = data
                    .bytes_view()
                    .map_err(|e| fail(op, OsErrorKind::Invalid, format!("argument 2 is not bytes: {e}")))?
                    .into_owned();
                (TrafficOp::Send { plain: plain.len() }, plain)
            }
            _ => {
                return Err(fail(
                    op,
                    OsErrorKind::Invalid,
                    "argument 2 must be bytes",
                ))
            }
        },
        OsOp::Recv => match args.get(1).and_then(Value::as_int).and_then(|n| usize::try_from(n).ok()) {
            Some(count) => (TrafficOp::Recv { count }, Vec::new()),
            _ => {
                return Err(fail(
                    op,
                    OsErrorKind::Invalid,
                    "the count must be a non-negative integer",
                ))
            }
        },
        _ => (TrafficOp::Close, Vec::new()),
    })
}

impl RuntimeKernel {
    /// The kernel's answer to an `os/` ask it owns or that rides a TLS socket; `None` when the host answers it.
    pub(super) fn tls_os(
        &self,
        op: OsOp,
        args: &[Value],
        key: Option<&str>,
        principal: Option<&str>,
        orbital: Option<&str>,
    ) -> Option<Result<Value, OsError>> {
        match op {
            OsOp::Fetch => self.fetch_refusal(op, args, orbital).map(Err),
            OsOp::TlsUpgrade => Some(self.tls_handshake(op, args, orbital)),
            OsOp::TcpConnect if args.len() > 2 => Some(self.tls_handshake(op, args, orbital)),
            OsOp::Send | OsOp::Recv | OsOp::Close => {
                let sock = sock_id(args.first())?;
                if !self.tls.contains(sock) {
                    return None;
                }
                Some(self.tls_traffic(op, sock, args, key, principal))
            }
            _ => None,
        }
    }

    /// The kernel's policy for an `os/fetch`: the url must be text and its origin one the asking orbital's own program
    /// declares in `egress`. An ask no orbital makes is held to the union the deployment unit declares.
    /// `None` lets the host perform the fetch.
    fn fetch_refusal(&self, op: OsOp, args: &[Value], orbital: Option<&str>) -> Option<OsError> {
        let Some(Value::String(url)) = args.get(1) else {
            return Some(fail(op, OsErrorKind::Invalid, "the url must be text"));
        };
        let own = orbital.and_then(|name| {
            let schemas = self.registry.schemas.read();
            schemas.values().find(|program| program.orbitals.iter().any(|o| o.name == name)).cloned()
        });
        let allowed = match own {
            Some(program) => orbital_core_base::http_wire::origin_allowed(url, program.egress.as_deref().unwrap_or(&[])),
            None => orbital_core_base::http_wire::origin_allowed(url, &self.grants.egress.read()),
        };
        if allowed {
            return None;
        }
        Some(fail(op, OsErrorKind::Denied, &format!("{url} is not an origin the program declares in egress")))
    }

    fn tls_handshake(&self, op: OsOp, args: &[Value], orbital: Option<&str>) -> Result<Value, OsError> {
        let (connect, sock, spec) = match op {
            OsOp::TlsUpgrade => {
                let Some(sock) = sock_id(args.first()) else {
                    return Err(fail(
                        op,
                        OsErrorKind::Invalid,
                        "the socket must be an open handle",
                    ));
                };
                if self.tls.contains(sock) {
                    return Err(fail(op, OsErrorKind::Invalid, "the socket is already TLS"));
                }
                let spec = args
                    .get(1)
                    .ok_or_else(|| fail(op, OsErrorKind::Invalid, "the TLS argument is missing"))?;
                (None, Some(Value::Number(sock as f64)), spec)
            }
            _ => (Some(args[..2].to_vec()), None, &args[2]),
        };
        let spec = parse_spec(op, spec, self.grants.os_grants.read().for_orbital(orbital))?;
        let mut run = HandshakeRun {
            op,
            connect,
            spec: Some(spec),
            stream: None,
            outcome: None,
        };
        let first = run.first(sock);
        match drive(&self.io, &mut run, first, DriveContext { orbital, ..DriveContext::default() }) {
            DriveEnd::Done(_) => {}
            DriveEnd::AwaitingHost => return Err(OsError::new(OsErrorKind::Io, AWAITING_HOST)),
        }
        let Handshaken { sock, stream, spec } = run
            .outcome
            .ok_or_else(|| fail(op, OsErrorKind::Tls, "the handshake ended with no outcome"))??;
        let Some(id) = sock_id(Some(&sock)) else {
            return Err(fail(
                op,
                OsErrorKind::Io,
                "the host answered a socket that is not a handle",
            ));
        };
        let negotiated = stream
            .alpn_protocol()
            .map(|name| String::from_utf8_lossy(&name).into_owned());
        let transcript = stream
            .transcript()
            .map(<[Recorded]>::to_vec)
            .unwrap_or_default();
        self.tls.register(
            id,
            TlsSocket {
                host: spec.server_name,
                roots: spec.roots,
                alpn: spec.alpn,
                seed: stream.seed(),
                now: stream.now_secs(),
                transcript,
                consumed: 0,
            },
        );
        self.tls.keep(id, stream.surviving_timeouts());
        let mut answer = vec![("sock", sock)];
        if let Some(name) = negotiated {
            answer.push(("alpn", Value::string(name)));
        }
        Ok(record(answer))
    }

    fn tls_traffic(
        &self,
        op: OsOp,
        sock: i64,
        args: &[Value],
        key: Option<&str>,
        principal: Option<&str>,
    ) -> Result<Value, OsError> {
        let resolved = self.resolve_segments(op, args, principal)?;
        let (traffic, plain) = traffic_of(op, &resolved)?;
        self.tls_claim(op, sock, traffic, key)?;
        let (mut stream, start_len) = self.tls_stream(op, sock)?;
        let first = match traffic {
            TrafficOp::Send { .. } => stream.send(&plain),
            TrafficOp::Recv { .. } => stream.recv(),
            TrafficOp::Close => stream.close(),
        };
        let sockets = self.tls.clone();
        let first_step = match advance(&sockets, sock, &mut stream, traffic, start_len, first) {
            TlsCompletion::Ask { op, args } => DriverStep::Ask(DriverAsk::Os {
                op,
                args,
                key: None,
            }),
            TlsCompletion::Done(outcome) => {
                if outcome.is_ok() {
                    self.tls.keep(sock, stream);
                }
                return outcome;
            }
        };
        if let (
            Some(key),
            DriverStep::Ask(DriverAsk::Os {
                op: raw_op,
                args: raw_args,
                ..
            }),
        ) = (key, &first_step)
        {
            if matches!(raw_op, OsOp::Send | OsOp::Recv) && self.io.os_site(op, Some(key)).is_some()
            {
                let answer = self.io.os(*raw_op, raw_args, Some(key), None, None);
                if self.io.peek_call_verdict() == Some(CallVerdict::Deferred) {
                    self.tls_park(sock, traffic, key);
                    let parked_from = self.tls.state(sock).map(|(_, consumed)| consumed);
                    self.tls.pending.lock().insert(
                        key.to_string(),
                        PendingTls {
                            sock,
                            op: traffic,
                            plain,
                            resumed: Some((stream, start_len)),
                            parked_from,
                        },
                    );
                    return Ok(Value::Null);
                }
                if self.io.suspended() {
                    if matches!(traffic, TrafficOp::Recv { .. }) {
                        self.tls.keep_withdrawn(sock, stream, start_len);
                    }
                    return Err(OsError::new(OsErrorKind::Io, AWAITING_HOST));
                }
                let mut run = TrafficRun {
                    sockets,
                    sock,
                    stream,
                    op: traffic,
                    start_len,
                    done: None,
                };
                let step = run.resume(DriverAnswer::Os(answer));
                return self.finish_traffic(run, step);
            }
        }
        let run = TrafficRun {
            sockets,
            sock,
            stream,
            op: traffic,
            start_len,
            done: None,
        };
        self.finish_traffic(run, first_step)
    }

    /// The credential segments of a TLS `os/send` are filled in here, before the plaintext is framed into records: the
    /// kernel asks each credential by name, so the journal holds the names and a replay asks again, and the host sees
    /// only ciphertext.
    fn resolve_segments(&self, op: OsOp, args: &[Value], principal: Option<&str>) -> Result<Vec<Value>, OsError> {
        let awaiting = std::cell::Cell::new(false);
        let held = |name: &str, owner: Option<&str>| {
            self.io.secret(name, owner).map(|held| held.map(Secret::into_inner)).inspect_err(|message| awaiting.set(message == AWAITING_HOST))
        };
        let resolved = orbital_core_base::os::resolve_credential_segments(op, args, principal, Some(&held));
        if awaiting.get() {
            return Err(OsError::new(OsErrorKind::Io, AWAITING_HOST));
        }
        resolved
    }

    fn finish_traffic(&self, mut run: TrafficRun, step: DriverStep) -> Result<Value, OsError> {
        match drive(&self.io, &mut run, step, DriveContext::default()) {
            DriveEnd::Done(_) => {
                let outcome = run.done.take().unwrap_or_else(|| {
                    Err(OsError::new(
                        OsErrorKind::Tls,
                        "the TLS operation ended with no outcome",
                    ))
                });
                if outcome.is_ok() {
                    self.tls.keep(run.sock, run.stream);
                }
                outcome
            }
            DriveEnd::AwaitingHost => {
                if matches!(run.op, TrafficOp::Recv { .. }) {
                    self.tls.keep_withdrawn(run.sock, run.stream, run.start_len);
                }
                Err(OsError::new(OsErrorKind::Io, AWAITING_HOST))
            }
        }
    }

    /// One send and one receive may be outstanding on a socket; a second of the same direction is refused. The
    /// operation that holds the park (a replay of it) is not a second.
    fn tls_claim(
        &self,
        op: OsOp,
        sock: i64,
        traffic: TrafficOp,
        key: Option<&str>,
    ) -> Result<(), OsError> {
        if !self.tls.contains(sock) {
            return Err(fail(op, OsErrorKind::Closed, "not an open socket"));
        }
        let parks = self.tls.parks.lock();
        let held = parks.get(&sock).and_then(|p| match traffic {
            TrafficOp::Send { .. } => p.send.as_deref(),
            TrafficOp::Recv { .. } => p.recv.as_deref(),
            TrafficOp::Close => None,
        });
        match held {
            Some(held) if Some(held) != key => Err(fail(
                op,
                OsErrorKind::Invalid,
                "a TLS operation of this direction is pending on the socket",
            )),
            _ => Ok(()),
        }
    }

    fn tls_park(&self, sock: i64, traffic: TrafficOp, key: &str) {
        let mut parks = self.tls.parks.lock();
        let parked = parks.entry(sock).or_default();
        match traffic {
            TrafficOp::Send { .. } => parked.send = Some(key.to_string()),
            TrafficOp::Recv { .. } => parked.recv = Some(key.to_string()),
            TrafficOp::Close => {}
        }
    }

    fn tls_unpark(&self, sock: i64, traffic: TrafficOp) {
        if let Some(parked) = self.tls.parks.lock().get_mut(&sock) {
            match traffic {
                TrafficOp::Send { .. } => parked.send = None,
                TrafficOp::Recv { .. } => parked.recv = None,
                TrafficOp::Close => {}
            }
        }
    }

    /// The stream of `sock`'s session rebuilt from its transcript as it stands, and the transcript length it began at.
    fn tls_stream(&self, op: OsOp, sock: i64) -> Result<(TlsStream, usize), OsError> {
        let (len, consumed) = self
            .tls
            .state(sock)
            .ok_or_else(|| fail(op, OsErrorKind::Closed, "not an open socket"))?;
        if let Some(stream) = self.tls.take_live(sock, len, consumed) {
            return Ok((stream, len));
        }
        self.tls
            .rebuilds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let socket = self
            .tls
            .snapshot(sock)
            .ok_or_else(|| fail(op, OsErrorKind::Closed, "not an open socket"))?;
        let replayed = crate::tls::TlsClient::replay_with_alpn(
            &socket.host,
            socket.seed,
            socket.now,
            socket.roots.clone(),
            &socket.alpn,
            &socket.transcript,
        )
        .map_err(|e| fail(op, OsErrorKind::Tls, e))?;
        let undelivered = replayed.plaintext.len().saturating_sub(socket.consumed);
        let eof_seen = socket
            .transcript
            .iter()
            .any(|event| matches!(event, Recorded::Eof));
        let stream = TlsStream::established(
            &socket.host,
            Value::Number(sock as f64),
            socket.seed,
            socket.now,
            replayed,
            undelivered,
            eof_seen,
        );
        Ok((stream, socket.transcript.len()))
    }

    #[cfg(test)]
    pub(crate) fn tls_open_op_error(&self, sock: i64, send: bool) -> Option<OsErrorKind> {
        let traffic = if send {
            TrafficOp::Send { plain: 0 }
        } else {
            TrafficOp::Recv { count: 1 }
        };
        self.tls_claim(OsOp::Recv, sock, traffic, None)
            .err()
            .map(|e| e.kind)
    }

    #[cfg(test)]
    pub(crate) fn tls_forget_live_for_test(&self) {
        self.tls
            .keep_none
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn tls_rebuilds(&self) -> usize {
        self.tls.rebuilds.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn tls_begin_pass(&self) {
        self.tls.begin_pass();
    }

    pub(super) fn tls_end_pass(&self, complete: bool) {
        self.tls.end_pass(complete);
    }

    /// Whether `key` names a keyed TLS operation parked on a deferred raw ask.
    pub fn tls_pending(&self, key: &str) -> bool {
        self.tls.pending.lock().contains_key(key)
    }

    /// Park `key` as `tls_traffic` does once the host deferred its raw read, on a socket that is not open.
    #[cfg(test)]
    pub(crate) fn tls_park_for_test(&self, key: &str) {
        self.tls.pending.lock().insert(
            key.to_string(),
            PendingTls {
                sock: -1,
                op: TrafficOp::Recv { count: 16 },
                plain: Vec::new(),
                resumed: None,
                parked_from: None,
            },
        );
    }

    /// Feed the host's raw outcome of the ask that parked `key` into its TLS operation. `None` when `key` parks no
    /// TLS operation (the outcome is the program's as it stands); otherwise the next raw ask the host must answer
    /// (pass its outcome to this method again) or the program's outcome. The session is rebuilt from the transcript as
    /// it stands now, so operations that completed meanwhile are part of it.
    pub fn tls_complete(&self, key: &str, raw: Result<Value, OsError>) -> Option<TlsCompletion> {
        let PendingTls {
            sock,
            op,
            plain,
            resumed,
            parked_from,
        } = self.tls.pending.lock().remove(key)?;
        let program_op = op_of(op);
        let resumed = match (resumed, parked_from) {
            (Some((stream, start_len)), Some(consumed))
                if self.tls.state(sock) != Some((start_len, consumed)) =>
            {
                drop(stream);
                None
            }
            (resumed, _) => resumed,
        };
        let (mut stream, start_len) = match resumed {
            Some(resumed) => resumed,
            None => match self.tls_stream(program_op, sock) {
                Ok((mut stream, start_len)) => {
                    let first = match op {
                        TrafficOp::Send { .. } => stream.send(&plain),
                        _ => stream.recv(),
                    };
                    if !matches!(first, TlsEvent::Ask(DriverAsk::Os { .. })) {
                        self.tls_unpark(sock, op);
                        return Some(TlsCompletion::Done(Err(fail(
                            program_op,
                            OsErrorKind::Tls,
                            "the session moved while the operation was parked",
                        ))));
                    }
                    (stream, start_len)
                }
                Err(error) => {
                    self.tls_unpark(sock, op);
                    return Some(TlsCompletion::Done(Err(error)));
                }
            },
        };
        let event = stream.resume(DriverAnswer::Os(raw));
        let completion = advance(&self.tls, sock, &mut stream, op, start_len, event);
        match &completion {
            TlsCompletion::Ask { .. } => {
                self.tls.pending.lock().insert(
                    key.to_string(),
                    PendingTls {
                        sock,
                        op,
                        plain,
                        resumed: Some((stream, start_len)),
                        parked_from: None,
                    },
                );
            }
            TlsCompletion::Done(outcome) => {
                self.tls_unpark(sock, op);
                if outcome.is_ok() {
                    self.tls.keep(sock, stream);
                }
            }
        }
        Some(completion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::response::{EventRequest, EventResponse};
    use crate::embed::{EmbeddedServer, ServerProgress};
    use crate::resumable::{IoAnswer, IoReply, IoRequest};
    use crate::tls::stream::tests::{FakeServer, CERT, NOW};
    use serde_json::{json, Value as Json};

    fn pem() -> String {
        let body = codec::base64_encode(CERT);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).expect("ascii"))
            .collect();
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            lines.join("\n")
        )
    }

    fn routes(ok: &str) -> Json {
        json!({ "emit": { "success": ok, "failure": "BAD" } })
    }

    fn schema(go: Vec<Json>, grants: Json) -> orbital_core_base::schema::types::OrbitalSchema {
        schema_recv(go, grants, json!(["os/recv", 5, 4096, routes("GOT")]))
    }

    fn schema_recv(
        go: Vec<Json>,
        grants: Json,
        recv: Json,
    ) -> orbital_core_base::schema::types::OrbitalSchema {
        schema_chain(
            go,
            grants,
            vec![
                (
                    "CONNECTED",
                    vec![json!(["os/send", 5, "cGluZw==", routes("SENT")])],
                ),
                ("SENT", vec![recv]),
                ("GOT", vec![json!(["os/close", 5, routes("CLOSED")])]),
            ],
        )
    }

    /// The host key the `Host` trait's keyed ask carries from the singleton frame of the program session.
    fn host_key(key: &str) -> String {
        crate::resumable::host_call_key(None, "Host", "", key)
    }

    fn schema_chain(
        go: Vec<Json>,
        grants: Json,
        chain: Vec<(&str, Vec<Json>)>,
    ) -> orbital_core_base::schema::types::OrbitalSchema {
        let mut transitions =
            vec![json!({ "from": "idle", "to": "idle", "event": "GO", "effects": go })];
        for (event, effects) in chain {
            transitions
                .push(json!({ "from": "idle", "to": "idle", "event": event, "effects": effects }));
        }
        for event in ["CLOSED", "BAD"] {
            transitions.push(json!({ "from": "idle", "to": "idle", "event": event }));
        }
        serde_json::from_value(json!({
            "name": "App",
            "typeContractVersion": 1,
            "osGrants": grants,
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Job", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{ "name": "Host", "linkedEntity": "Job",
                    "stateMachine": { "states": [{ "name": "idle", "isInitial": true }], "transitions": transitions } }]
            }]
        }))
        .expect("schema")
    }

    fn grants() -> Json {
        json!({ "net": [{ "target": "localhost:443", "kinds": ["tcp"] }] })
    }

    struct Wire {
        server: FakeServer,
        seed: u8,
        sent: Vec<Vec<u8>>,
        ops: Vec<OsOp>,
        defer_recv: bool,
        deferred: Vec<String>,
        secrets: Vec<(String, String)>,
        asked: Vec<String>,
        asked_for: Vec<Option<String>>,
        viewer: Option<Json>,
    }

    impl Wire {
        fn new(seed: u8) -> Self {
            Self {
                server: FakeServer::new(9),
                seed,
                sent: Vec::new(),
                ops: Vec::new(),
                defer_recv: false,
                deferred: Vec::new(),
                secrets: Vec::new(),
                asked: Vec::new(),
                asked_for: Vec::new(),
                viewer: None,
            }
        }

        fn answer(&mut self, request: &IoRequest) -> IoAnswer {
            match request {
                IoRequest::Os {
                    op: OsOp::Recv,
                    key: Some(key),
                    site: Some(_),
                    ..
                } if self.defer_recv => {
                    self.deferred.push(key.clone());
                    IoAnswer::ServiceDeferred
                }
                IoRequest::Random { bytes } => {
                    IoAnswer::Random(Ok(vec![self.seed; *bytes as usize].into()))
                }
                IoRequest::Secret { name, principal } => {
                    self.asked.push(name.clone());
                    self.asked_for.push(principal.clone());
                    let held = |name: &String| match principal {
                        Some(who) => format!("{name}@{who}"),
                        None => name.clone(),
                    };
                    IoAnswer::Secret(Ok(self
                        .secrets
                        .iter()
                        .find(|(stored, _)| *stored == held(name))
                        .map(|(_, value)| Secret::new(value.clone()))))
                }
                IoRequest::Clock => IoAnswer::Clock(Ok((NOW * 1000) as i64)),
                IoRequest::Os {
                    op: OsOp::Recv,
                    key: Some(key),
                    site: Some(_),
                    ..
                } if self.defer_recv => {
                    self.deferred.push(key.clone());
                    IoAnswer::ServiceDeferred
                }
                IoRequest::Os { op, args, .. } => {
                    self.ops.push(*op);
                    IoAnswer::Os(self.os(*op, args).map(Into::into))
                }
                other => IoAnswer::Service(Err(format!("unexpected {other:?}"))),
            }
        }

        fn os(&mut self, op: OsOp, args: &[Value]) -> Result<Value, OsError> {
            match op {
                OsOp::TcpConnect => Ok(record([("sock", Value::Number(5.0))])),
                OsOp::Send => {
                    let bytes = args.get(1).expect("send data").bytes_view().expect("bytes").into_owned();
                    self.server.receive(&bytes);
                    self.sent.push(bytes.clone());
                    Ok(record([("bytes", Value::Number(bytes.len() as f64))]))
                }
                OsOp::Recv => {
                    let bytes = self.server.take(16384);
                    Ok(record([
                        ("data", Value::bytes(bytes.clone())),
                        ("eof", Value::Bool(bytes.is_empty())),
                    ]))
                }
                OsOp::Close => Ok(record([])),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn run(server: &EmbeddedServer, wire: &mut Wire) -> EventResponse {
        run_event(server, wire, "GO")
    }

    fn run_event(server: &EmbeddedServer, wire: &mut Wire, event: &str) -> EventResponse {
        let request = EventRequest {
            event: event.to_string(),
            target_trait: Some("Host".to_string()),
            client_id: wire.viewer.as_ref().and_then(|user| user["id"].as_str().map(str::to_string)),
            ..Default::default()
        };
        let mut progress = server.start("Main", request).expect("start");
        loop {
            match progress {
                ServerProgress::Done(response) => return response,
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|pending| IoReply {
                            id: pending.id,
                            answer: match &pending.request {
                                IoRequest::ReadRow { .. } => {
                                    IoAnswer::Row(Err("no row".to_string()))
                                }
                                other => wire.answer(other),
                            },
                        })
                        .collect();
                    progress = server.resume(flight, replies).expect("resume");
                }
                ServerProgress::Queued { ticket } => panic!("queued {ticket}"),
            }
        }
    }

    fn payload(response: &EventResponse, event: &str) -> Option<Json> {
        response
            .emitted_events
            .iter()
            .find(|e| e.event == event)
            .map(|e| serde_json::to_value(e.payload.as_ref()).expect("payload"))
    }

    fn connect_tls(ca: bool, server_name: &str) -> Json {
        let mut tls = json!({ "serverName": server_name });
        if ca {
            tls["ca"] = json!(pem());
        }
        json!(["os/tcp-connect", "localhost", 443, tls, routes("CONNECTED")])
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn a_tls_connect_send_recv_and_close_carry_plaintext_to_the_program_and_only_ciphertext_to_the_host(
    ) {
        let server = EmbeddedServer::new(schema(vec![connect_tls(true, "localhost")], grants()))
            .expect("server");
        let mut wire = Wire::new(7);
        let response = run(&server, &mut wire);
        assert_eq!(
            payload(&response, "SENT"),
            Some(json!({ "bytes": 4.0 })),
            "the program sees its plaintext length"
        );
        let got = payload(&response, "GOT").expect("received");
        assert_eq!(got["data"], json!(codec::base64_encode(b"pong")));
        assert!(payload(&response, "CLOSED").is_some());
        assert!(payload(&response, "BAD").is_none());
        assert!(
            wire.sent.iter().all(|raw| !contains(raw, b"ping")),
            "control: the host never sees the plaintext"
        );
        assert_eq!(
            wire.sent[0][0], 0x16,
            "the first raw bytes are a TLS handshake record"
        );
        assert_eq!(wire.ops.last(), Some(&OsOp::Close));
        assert!(!server.kernel().tls_pending("none"));
    }

    fn segment_schema(segments: Json) -> orbital_core_base::schema::types::OrbitalSchema {
        schema_chain(
            vec![connect_tls(true, "localhost")],
            grants(),
            vec![
                ("CONNECTED", vec![json!(["os/send", 5, segments, routes("SENT")])]),
                ("SENT", vec![json!(["os/close", 5, routes("CLOSED")])]),
            ],
        )
    }

    #[test]
    fn a_credential_segment_of_a_tls_send_reaches_the_peer_and_neither_the_host_nor_the_journal_sees_it() {
        let segments = json!([
            { "bytes": codec::base64_encode(b"AUTH "), "credential": "REDIS_PASSWORD" },
            { "bytes": codec::base64_encode(b"\r\n"), "credential": "" }
        ]);
        let server = EmbeddedServer::new(segment_schema(segments)).expect("server");
        let mut wire = Wire::new(7);
        wire.secrets.push(("REDIS_PASSWORD".to_string(), "hunter2-pass".to_string()));
        let response = run(&server, &mut wire);
        assert_eq!(payload(&response, "SENT"), Some(json!({ "bytes": 19.0 })), "the program sees the plaintext length, credential included");
        assert_eq!(wire.server.app_received(), b"AUTH hunter2-pass\r\n", "the peer decrypted the credential");
        assert_eq!(wire.asked, vec!["REDIS_PASSWORD".to_string()], "the kernel asked the credential by name");
        assert!(wire.sent.iter().all(|raw| !contains(raw, b"hunter2-pass")), "the host sees only ciphertext");
        let shown = format!("{response:?} {:?}", wire.asked);
        assert!(!shown.contains("hunter2-pass"), "the response never shows the credential: {shown}");
        assert!(payload(&response, "BAD").is_none());
    }

    #[test]
    fn a_user_segment_of_a_tls_send_asks_the_dispatchs_viewer_and_without_one_sends_nothing() {
        let segments = json!([
            { "bytes": codec::base64_encode(b"AUTH "), "credential": "TOKEN", "scope": "user" }
        ]);
        let server = EmbeddedServer::new(segment_schema(segments)).expect("server");
        for viewer in ["alice", "bob"] {
            let mut wire = Wire::new(7);
            wire.secrets.push((format!("TOKEN@{viewer}"), format!("tok-{viewer}")));
            wire.viewer = Some(json!({ "id": viewer }));
            {
                let _session = server.kernel().enter_session(viewer);
                server.kernel().bind_test_identity(serde_json::from_value(json!({ "id": viewer })).expect("viewer"));
            }
            let response = run(&server, &mut wire);
            assert!(payload(&response, "BAD").is_none(), "{viewer}");
            assert_eq!(wire.server.app_received(), format!("AUTH tok-{viewer}").as_bytes(), "{viewer}");
            assert_eq!(wire.asked_for, vec![Some(viewer.to_string())], "the viewer reached the resolver");
        }
        let mut anonymous = Wire::new(7);
        anonymous.secrets.push(("TOKEN".to_string(), "app-token".to_string()));
        let response = run(&server, &mut anonymous);
        let bad = payload(&response, "BAD").expect("the send failed");
        assert_eq!(bad["kind"], json!("denied"), "{bad}");
        assert!(anonymous.asked.is_empty(), "no credential was asked without a viewer");
        assert!(anonymous.server.app_received().is_empty());
    }

    #[test]
    fn control_a_tls_send_of_a_credential_nobody_holds_fails_before_any_application_byte() {
        let segments = json!([{ "bytes": codec::base64_encode(b"AUTH "), "credential": "MISSING" }]);
        let server = EmbeddedServer::new(segment_schema(segments)).expect("server");
        let mut wire = Wire::new(7);
        let response = run(&server, &mut wire);
        let bad = payload(&response, "BAD").expect("the send failed");
        assert_eq!(bad["kind"], json!("denied"), "{bad}");
        assert!(payload(&response, "SENT").is_none());
        assert!(wire.server.app_received().is_empty(), "no application byte reached the peer");
        assert_eq!(wire.asked, vec!["MISSING".to_string()]);
    }

    #[test]
    fn control_a_tls_send_of_plain_bytes_asks_no_credential() {
        let server = EmbeddedServer::new(schema(vec![connect_tls(true, "localhost")], grants())).expect("server");
        let mut wire = Wire::new(7);
        let _ = run(&server, &mut wire);
        assert!(wire.asked.is_empty());
    }

    #[test]
    fn a_plain_socket_is_untouched_and_the_host_sees_the_plaintext() {
        let plain = json!(["os/tcp-connect", "localhost", 443, routes("CONNECTED")]);
        let server = EmbeddedServer::new(schema(vec![plain], grants())).expect("server");
        let mut wire = Wire::new(7);
        let _ = run(&server, &mut wire);
        assert!(
            wire.sent.iter().any(|raw| raw == b"ping"),
            "control: with no TLS argument the raw bytes are the program's"
        );
    }

    #[test]
    fn the_same_seed_and_clock_replay_byte_identical_wire_bytes_and_another_seed_does_not() {
        let sent = |seed| {
            let server =
                EmbeddedServer::new(schema(vec![connect_tls(true, "localhost")], grants()))
                    .expect("server");
            let mut wire = Wire::new(seed);
            let _ = run(&server, &mut wire);
            wire.sent
        };
        assert_eq!(sent(7), sent(7));
        assert_ne!(
            sent(7),
            sent(8),
            "control: another seed is another handshake"
        );
    }

    #[test]
    fn a_server_name_no_net_grant_names_is_denied_before_any_raw_ask() {
        let server = EmbeddedServer::new(schema(
            vec![connect_tls(true, "elsewhere.example")],
            grants(),
        ))
        .expect("server");
        let mut wire = Wire::new(7);
        let response = run(&server, &mut wire);
        assert_eq!(
            payload(&response, "BAD").expect("failure")["kind"],
            json!("denied")
        );
        assert!(wire.ops.is_empty(), "nothing connected");
    }

    #[test]
    fn a_certificate_the_roots_do_not_vouch_for_answers_kind_tls_and_closes_the_socket() {
        let server = EmbeddedServer::new(schema(vec![connect_tls(false, "localhost")], grants()))
            .expect("server");
        let mut wire = Wire::new(7);
        let response = run(&server, &mut wire);
        assert_eq!(
            payload(&response, "BAD").expect("failure")["kind"],
            json!("tls")
        );
        assert!(payload(&response, "CONNECTED").is_none());
        assert_eq!(
            wire.ops.last(),
            Some(&OsOp::Close),
            "the failed handshake closes the raw socket"
        );
    }

    #[test]
    fn os_tls_upgrade_turns_an_already_connected_socket_into_a_tls_stream() {
        let go = vec![
            json!(["os/tcp-connect", "localhost", 443]),
            json!(["os/tls-upgrade", 5, { "serverName": "localhost", "ca": pem() }, routes("CONNECTED")]),
        ];
        let server = EmbeddedServer::new(schema(go, grants())).expect("server");
        let mut wire = Wire::new(7);
        let response = run(&server, &mut wire);
        assert_eq!(
            payload(&response, "GOT").expect("received")["data"],
            json!(codec::base64_encode(b"pong"))
        );
        assert!(wire.sent.iter().all(|raw| !contains(raw, b"ping")));
    }

    #[test]
    fn a_bad_tls_argument_is_refused_as_invalid() {
        for tls in [
            json!(null),
            json!({}),
            json!({ "serverName": "localhost", "extra": 1 }),
            json!({ "serverName": "localhost", "alpn": [""] }),
        ] {
            let go = vec![json!([
                "os/tcp-connect",
                "localhost",
                443,
                tls,
                routes("CONNECTED")
            ])];
            let server = EmbeddedServer::new(schema(go, grants())).expect("server");
            let mut wire = Wire::new(7);
            let response = run(&server, &mut wire);
            assert_eq!(
                payload(&response, "BAD").expect("failure")["kind"],
                json!("invalid"),
                "{tls}"
            );
        }
    }

    #[test]
    fn a_keyed_routed_recv_the_host_defers_completes_in_the_kernel_with_decrypted_plaintext() {
        let keyed = json!(["os/recv", 5, 4096, { "key": "r1", "emit": { "success": "GOT", "failure": "BAD" } }]);
        let server = EmbeddedServer::new(schema_recv(
            vec![connect_tls(true, "localhost")],
            grants(),
            keyed,
        ))
        .expect("server");
        let mut wire = Wire::new(7);
        let mut deferred_ask = None;
        let request = EventRequest {
            event: "GO".to_string(),
            target_trait: Some("Host".to_string()),
            ..Default::default()
        };
        let mut progress = server.start("Main", request).expect("start");
        let response = loop {
            match progress {
                ServerProgress::Done(response) => break response,
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|pending| IoReply {
                            id: pending.id,
                            answer: match &pending.request {
                                IoRequest::ReadRow { .. } => {
                                    IoAnswer::Row(Err("no row".to_string()))
                                }
                                IoRequest::Os {
                                    op: OsOp::Recv,
                                    key: Some(key),
                                    site: Some(_),
                                    args, principal: None, ..
                                } => {
                                    deferred_ask = Some((key.clone(), args.clone()));
                                    IoAnswer::ServiceDeferred
                                }
                                other => wire.answer(other),
                            },
                        })
                        .collect();
                    progress = server.resume(flight, replies).expect("resume");
                }
                ServerProgress::Queued { ticket } => panic!("queued {ticket}"),
            }
        };
        let (key, _) =
            deferred_ask.expect("the host was asked to defer the raw recv under the program's key");
        assert_eq!(key, host_key("r1"));
        assert!(
            payload(&response, "GOT").is_none(),
            "a deferring ask fires no route in its own dispatch"
        );
        assert!(server.kernel().tls_pending(&host_key("r1")));
        assert_eq!(
            server.kernel().tls_open_op_error(5, false),
            Some(OsErrorKind::Invalid),
            "a second recv on the parked socket is refused"
        );
        assert_eq!(
            server.kernel().tls_open_op_error(5, true),
            None,
            "control: a send may run beside the parked recv"
        );

        let raw = wire.os(OsOp::Recv, &[Value::Number(5.0), Value::Number(16384.0)]);
        let done = server.kernel().tls_complete(&host_key("r1"), raw).expect("parked");
        let TlsCompletion::Done(Ok(answer)) = done else {
            panic!("the raw bytes hold the whole reply: {done:?}")
        };
        assert_eq!(
            answer.get_path("data"),
            Some(&Value::bytes(b"pong".to_vec()))
        );
        assert!(!server.kernel().tls_pending(&host_key("r1")));
        assert!(
            server
                .kernel()
                .tls_complete(&host_key("r1"), Ok(Value::Null))
                .is_none(),
            "control: a key parks one operation"
        );
    }

    #[test]
    fn a_partial_record_asks_the_host_for_more_raw_bytes_before_the_program_sees_anything() {
        let keyed = json!(["os/recv", 5, 4096, { "key": "r2", "emit": { "success": "GOT", "failure": "BAD" } }]);
        let server = EmbeddedServer::new(schema_recv(
            vec![connect_tls(true, "localhost")],
            grants(),
            keyed,
        ))
        .expect("server");
        let mut wire = Wire::new(7);
        let request = EventRequest {
            event: "GO".to_string(),
            target_trait: Some("Host".to_string()),
            ..Default::default()
        };
        let mut progress = server.start("Main", request).expect("start");
        loop {
            match progress {
                ServerProgress::Done(_) => break,
                ServerProgress::Await { flight, requests } => {
                    let replies = requests
                        .into_iter()
                        .map(|pending| IoReply {
                            id: pending.id,
                            answer: match &pending.request {
                                IoRequest::ReadRow { .. } => {
                                    IoAnswer::Row(Err("no row".to_string()))
                                }
                                IoRequest::Os {
                                    op: OsOp::Recv,
                                    key: Some(_),
                                    site: Some(_),
                                    ..
                                } => IoAnswer::ServiceDeferred,
                                other => wire.answer(other),
                            },
                        })
                        .collect();
                    progress = server.resume(flight, replies).expect("resume");
                }
                ServerProgress::Queued { ticket } => panic!("queued {ticket}"),
            }
        }
        let Ok(Value::Object(full)) =
            wire.os(OsOp::Recv, &[Value::Number(5.0), Value::Number(16384.0)])
        else {
            panic!("raw")
        };
        let Some(Value::Bytes(bytes)) = full.get("data") else {
            panic!("data")
        };
        let bytes = bytes.to_vec();
        let (head, tail) = bytes.split_at(bytes.len() - 3);
        let piece = |part: &[u8]| {
            Ok(record([
                ("data", Value::bytes(part.to_vec())),
                ("eof", Value::Bool(false)),
            ]))
        };
        let first = server
            .kernel()
            .tls_complete(&host_key("r2"), piece(head))
            .expect("parked");
        assert!(
            matches!(first, TlsCompletion::Ask { op: OsOp::Recv, .. }),
            "{first:?}: the record is incomplete, so the kernel asks for more"
        );
        assert!(server.kernel().tls_pending(&host_key("r2")));
        let second = server
            .kernel()
            .tls_complete(&host_key("r2"), piece(tail))
            .expect("parked");
        let TlsCompletion::Done(Ok(answer)) = second else {
            panic!("{second:?}")
        };
        assert_eq!(
            answer.get_path("data"),
            Some(&Value::bytes(b"pong".to_vec()))
        );
    }

    fn split_pong(wire: &mut Wire) -> (Vec<u8>, Vec<u8>) {
        let Ok(Value::Object(full)) =
            wire.os(OsOp::Recv, &[Value::Number(5.0), Value::Number(16384.0)])
        else {
            panic!("raw")
        };
        let Some(Value::Bytes(bytes)) = full.get("data") else {
            panic!("data")
        };
        let bytes = bytes.to_vec();
        let (head, tail) = bytes.split_at(bytes.len() - 3);
        (head.to_vec(), tail.to_vec())
    }

    fn piece(part: &[u8]) -> Result<Value, OsError> {
        Ok(record([
            ("data", Value::bytes(part.to_vec())),
            ("eof", Value::Bool(false)),
        ]))
    }

    #[test]
    fn a_parked_recv_and_a_send_run_together_and_the_send_is_part_of_the_session_the_recv_completes_into(
    ) {
        let recv = json!(["os/recv", 5, 4096, { "key": "r1", "emit": { "success": "GOT", "failure": "BAD" } }]);
        let chain = vec![
            (
                "CONNECTED",
                vec![recv, json!(["os/send", 5, "cGluZw==", routes("SENT")])],
            ),
            ("SENT", vec![]),
            ("GOT", vec![]),
        ];
        let server = EmbeddedServer::new(schema_chain(
            vec![connect_tls(true, "localhost")],
            grants(),
            chain,
        ))
        .expect("server");
        let mut wire = Wire::new(7);
        wire.defer_recv = true;
        let response = run(&server, &mut wire);
        assert_eq!(
            payload(&response, "SENT"),
            Some(json!({ "bytes": 4.0 })),
            "the send completed while the recv was parked"
        );
        assert!(server.kernel().tls_pending(&host_key("r1")));
        assert_eq!(
            server.kernel().tls_open_op_error(5, true),
            None,
            "a send may follow beside the parked recv"
        );
        assert_eq!(
            server.kernel().tls_open_op_error(5, false),
            Some(OsErrorKind::Invalid),
            "control: a second recv may not"
        );
        let (head, tail) = split_pong(&mut wire);
        let first = server
            .kernel()
            .tls_complete(&host_key("r1"), piece(&head))
            .expect("parked");
        assert!(matches!(first, TlsCompletion::Ask { op: OsOp::Recv, .. }));
        let TlsCompletion::Done(Ok(answer)) = server
            .kernel()
            .tls_complete(&host_key("r1"), piece(&tail))
            .expect("parked")
        else {
            panic!("done")
        };
        assert_eq!(
            answer.get_path("data"),
            Some(&Value::bytes(b"pong".to_vec()))
        );
        assert_eq!(
            server.kernel().tls_open_op_error(5, false),
            None,
            "once answered, the next recv may park"
        );
    }

    #[test]
    fn an_event_stream_style_long_recv_stays_parked_while_more_sends_go_out() {
        let recv = json!(["os/recv", 5, 4096, { "key": "sse", "emit": { "success": "GOT", "failure": "BAD" } }]);
        let chain = vec![
            (
                "CONNECTED",
                vec![recv, json!(["os/send", 5, "cGluZw==", routes("SENT")])],
            ),
            (
                "MORE",
                vec![json!(["os/send", 5, "cGluZw==", routes("SENT")])],
            ),
            ("SENT", vec![]),
            ("GOT", vec![]),
        ];
        let server = EmbeddedServer::new(schema_chain(
            vec![connect_tls(true, "localhost")],
            grants(),
            chain,
        ))
        .expect("server");
        let mut wire = Wire::new(7);
        wire.defer_recv = true;
        run(&server, &mut wire);
        let before = wire.sent.len();
        for _ in 0..3 {
            let response = run_event(&server, &mut wire, "MORE");
            assert_eq!(payload(&response, "SENT"), Some(json!({ "bytes": 4.0 })));
            let parked = wire.deferred.first().expect("the long recv was deferred");
            assert_eq!(crate::resumable::program_call_key(parked), "sse");
            assert!(
                server.kernel().tls_pending(parked),
                "the long recv is still parked"
            );
        }
        assert_eq!(
            wire.sent.len(),
            before + 3,
            "each later send reached the host as its own record"
        );
        assert!(
            wire.sent[before..]
                .windows(2)
                .all(|pair| pair[0] != pair[1]),
            "control: each record has its own sequence number, so none repeats"
        );
        let (head, tail) = split_pong(&mut wire);
        let parked = wire.deferred.first().expect("the long recv was deferred").clone();
        let first = server
            .kernel()
            .tls_complete(&parked, piece(&head))
            .expect("parked");
        assert!(matches!(first, TlsCompletion::Ask { .. }));
        let TlsCompletion::Done(Ok(answer)) = server
            .kernel()
            .tls_complete(&parked, piece(&tail))
            .expect("parked")
        else {
            panic!("done")
        };
        assert_eq!(
            answer.get_path("data"),
            Some(&Value::bytes(b"pong".to_vec()))
        );
    }

    #[test]
    fn the_negotiated_alpn_protocol_is_part_of_the_connect_and_upgrade_answers() {
        let offer = |names: Json| {
            let tls = json!({ "serverName": "localhost", "ca": pem(), "alpn": names });
            let go = vec![json!([
                "os/tcp-connect",
                "localhost",
                443,
                tls,
                routes("CONNECTED")
            ])];
            let server =
                EmbeddedServer::new(schema_chain(go, grants(), vec![("CONNECTED", vec![])]))
                    .expect("server");
            let mut wire = Wire::new(7);
            let response = run(&server, &mut wire);
            payload(&response, "CONNECTED")
                .or_else(|| payload(&response, "BAD"))
                .expect("answered")
        };
        assert_eq!(
            offer(json!(["h2", "http/1.1"])),
            json!({ "sock": 5.0, "alpn": "http/1.1" }),
            "the server chose from the offer"
        );
        assert_eq!(
            offer(json!(["h2"]))["kind"],
            json!("tls"),
            "control: nothing in common, the server refuses the handshake"
        );
        assert_eq!(
            offer(json!([])),
            json!({ "sock": 5.0 }),
            "control: nothing offered, nothing negotiated"
        );
        let upgrade = vec![
            json!(["os/tcp-connect", "localhost", 443]),
            json!(["os/tls-upgrade", 5, { "serverName": "localhost", "ca": pem(), "alpn": ["http/1.1"] }, routes("CONNECTED")]),
        ];
        let server =
            EmbeddedServer::new(schema_chain(upgrade, grants(), vec![("CONNECTED", vec![])]))
                .expect("server");
        let response = run(&server, &mut Wire::new(7));
        assert_eq!(
            payload(&response, "CONNECTED"),
            Some(json!({ "sock": 5.0, "alpn": "http/1.1" }))
        );
    }

    #[test]
    fn several_tls_operations_in_one_step_each_run_once_though_the_step_replays() {
        let chain = vec![
            (
                "CONNECTED",
                vec![
                    json!(["os/send", 5, "cGluZw==", routes("SENT")]),
                    json!(["os/send", 5, "cGluZw==", routes("SENT")]),
                    json!(["os/recv", 5, 4096, routes("GOT")]),
                ],
            ),
            ("SENT", vec![]),
            ("GOT", vec![]),
        ];
        let server = EmbeddedServer::new(schema_chain(
            vec![connect_tls(true, "localhost")],
            grants(),
            chain,
        ))
        .expect("server");
        let mut wire = Wire::new(7);
        let response = run(&server, &mut wire);
        assert_eq!(payload(&response, "SENT"), Some(json!({ "bytes": 4.0 })));
        assert!(
            payload(&response, "BAD").is_none(),
            "a replay of the step does not write twice: {:?}",
            payload(&response, "BAD")
        );
        let records: Vec<&Vec<u8>> = wire.sent.iter().filter(|raw| raw[0] == 0x17).collect();
        assert_eq!(
            records.len(),
            2,
            "control: two sends, two application records, no third from a replay"
        );
        assert_ne!(
            records[0], records[1],
            "each record carries its own sequence number"
        );
    }

    fn streaming_schema(recvs: usize) -> orbital_core_base::schema::types::OrbitalSchema {
        let names: Vec<String> = (0..recvs).map(|i| format!("R{i}")).collect();
        let mut chain: Vec<(&str, Vec<Json>)> = vec![
            ("CONNECTED", vec![json!(["os/send", 5, "cGluZw==", routes("SENT")])]),
            ("SENT", vec![json!(["os/recv", 5, 4096, routes(&names[0])])]),
        ];
        for (i, name) in names.iter().enumerate() {
            let next = match names.get(i + 1) {
                Some(next) => json!(["os/recv", 5, 4096, routes(next)]),
                None => json!(["os/close", 5, routes("CLOSED")]),
            };
            chain.push((name.as_str(), vec![next]));
        }
        schema_chain(vec![connect_tls(true, "localhost")], grants(), chain)
    }

    fn received(response: &EventResponse, recvs: usize) -> Vec<u8> {
        (0..recvs)
            .flat_map(|i| {
                let got = payload(response, &format!("R{i}")).expect("each receive answered");
                codec::base64_decode(got["data"].as_str().expect("data")).expect("base64")
            })
            .collect()
    }

    #[test]
    fn a_long_download_keeps_one_session_and_delivers_every_byte_in_order() {
        let body: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        let recvs = 16;
        let server = EmbeddedServer::new(streaming_schema(recvs)).expect("server");
        let mut wire = Wire::new(7);
        wire.server = FakeServer::new(9).answering(body.clone());
        let response = run(&server, &mut wire);
        assert!(payload(&response, "BAD").is_none(), "{:?}", payload(&response, "BAD"));
        assert_eq!(received(&response, recvs), body, "every byte, in order, exactly once");
        assert!(payload(&response, "CLOSED").is_some());
        assert!(
            server.kernel().tls_rebuilds() <= body.len() / 16384,
            "a host that suspends rebuilds only where a record spans a replayed raw read, never per receive: {}",
            server.kernel().tls_rebuilds()
        );
    }

    #[test]
    fn a_deferred_recv_resumes_the_session_it_parked_and_rebuilds_nothing() {
        let body: Vec<u8> = (0..20000u32).map(|i| (i % 239) as u8).collect();
        let keyed = json!(["os/recv", 5, 65536, { "key": "r1", "emit": { "success": "GOT", "failure": "BAD" } }]);
        let server = EmbeddedServer::new(schema_recv(
            vec![connect_tls(true, "localhost")],
            grants(),
            keyed,
        ))
        .expect("server");
        let mut wire = Wire::new(7);
        wire.server = FakeServer::new(9).answering(body.clone());
        wire.defer_recv = true;
        let response = run(&server, &mut wire);
        assert!(payload(&response, "BAD").is_none());
        assert!(server.kernel().tls_pending(&host_key("r1")));
        let rebuilt_before = server.kernel().tls_rebuilds();
        let mut got = Vec::new();
        let mut asks = 0;
        loop {
            let raw = wire.os(OsOp::Recv, &[Value::Number(5.0), Value::Number(16384.0)]);
            match server.kernel().tls_complete(&host_key("r1"), raw).expect("parked") {
                TlsCompletion::Ask { op, .. } => {
                    assert_eq!(op, OsOp::Recv, "a record split over raw reads asks for the rest");
                    asks += 1;
                }
                TlsCompletion::Done(Ok(answer)) => {
                    got.extend(answer.get_path("data").expect("data").bytes_view().expect("bytes").into_owned());
                    break;
                }
                TlsCompletion::Done(Err(error)) => panic!("{error:?}"),
            }
        }
        assert!(asks >= 1, "control: the first record spans more than one raw read");
        assert!(!got.is_empty() && body.starts_with(&got), "the decrypted head of the body");
        assert_eq!(rebuilt_before, 1, "only the send's replayed pass rebuilds; the recv's takes up the withdrawn session");
        assert_eq!(
            server.kernel().tls_rebuilds(),
            rebuilt_before,
            "the parked session resumes; nothing is rebuilt from the transcript"
        );
    }

    #[test]
    fn control_a_rebuilt_session_delivers_the_same_bytes_as_the_kept_one() {
        let body: Vec<u8> = (0..65536u32).map(|i| (i % 241) as u8).collect();
        let recvs = 16;
        let kept = {
            let server = EmbeddedServer::new(streaming_schema(recvs)).expect("server");
            let mut wire = Wire::new(7);
            wire.server = FakeServer::new(9).answering(body.clone());
            let response = run(&server, &mut wire);
            (received(&response, recvs), wire.sent)
        };
        let rebuilt = {
            let server = EmbeddedServer::new(streaming_schema(recvs)).expect("server");
            server.kernel().tls_forget_live_for_test();
            let mut wire = Wire::new(7);
            wire.server = FakeServer::new(9).answering(body.clone());
            let response = run(&server, &mut wire);
            assert!(server.kernel().tls_rebuilds() > 0, "control: this run rebuilds every operation");
            (received(&response, recvs), wire.sent)
        };
        assert_eq!(kept.0, body);
        assert_eq!(kept, rebuilt, "the same plaintext and the same wire bytes either way");
    }

    #[test]
    fn a_hosts_os_tls_upgrade_is_unsupported() {
        assert!(OsOp::TlsUpgrade.is_kernel_owned());
        assert!(
            !OsOp::Send.is_kernel_owned(),
            "control: the raw ops are the host's"
        );
    }
}
