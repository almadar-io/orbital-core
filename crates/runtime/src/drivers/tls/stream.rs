//! The TLS byte stream a TCP protocol driver (redis, postgres) wraps by composition. It is a step machine on the
//! shared [`Driver`] contract: every call returns a [`TlsEvent`], and an `Ask` is answered through
//! [`TlsStream::resume`]. Connecting asks `os/tcp-connect`, the journaled `Random` seed and the journaled clock;
//! the handshake then moves ciphertext through `os/send` and `os/recv`. A wrapper forwards the answer to the
//! stream while [`TlsStream::is_waiting`] and handles it itself otherwise.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use rustls::RootCertStore;

use orbital_core_base::codec;
use orbital_core_base::os::{OsError, OsErrorKind, OsOp};
use orbital_core_base::value::Value;

use super::session::{web_roots, Poll, Recorded, Replayed, TlsClient};
use crate::driver::{Driver, DriverAnswer, DriverAsk, DriverStep};
use orbital_core_base::os_grants::tcp_target_allowed;

const RECV_BYTES: f64 = 16384.0;

#[derive(Debug, Clone, PartialEq)]
pub enum TlsEvent {
    Ask(DriverAsk),
    /// The handshake finished and the peer is authenticated.
    Connected,
    /// Every queued plaintext byte was encrypted and sent.
    Sent,
    /// Decrypted bytes from the peer.
    Data(Vec<u8>),
    /// The peer closed with a close_notify and sent nothing more.
    Eof,
    /// The close_notify was sent and the socket closed.
    Closed,
    /// A receive timed out on a stream that survives timeouts: nothing was consumed and the session is intact.
    TimedOut(OsError),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Awaiting {
    Connect,
    Seed,
    Clock,
    Alert,
    Send,
    Recv,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Goal {
    Idle,
    Handshake,
    Send,
    Recv,
    Close,
}

pub struct TlsStream {
    host: String,
    roots: Option<RootCertStore>,
    alpn: Vec<Vec<u8>>,
    sock: Value,
    seed: [u8; 32],
    client: Option<TlsClient>,
    awaiting: Option<Awaiting>,
    goal: Goal,
    inbox: Vec<u8>,
    eof_seen: bool,
    after_close: Option<TlsEvent>,
    now: u64,
    record: bool,
    survive_timeout: bool,
    host_error: Option<OsError>,
}

fn os_ask(op: OsOp, args: Vec<Value>) -> TlsEvent {
    TlsEvent::Ask(DriverAsk::Os {
        op,
        args,
        key: None,
    })
}

impl TlsStream {
    /// Connect to `origin` (`rediss://host[:port]`, `postgres://host[:port]`, ...), refused unless `egress` lists it.
    pub fn connect(origin: &str, egress: &[String]) -> (Self, TlsEvent) {
        Self::connect_with_roots(origin, egress, web_roots())
    }

    pub fn connect_with_roots(
        origin: &str,
        egress: &[String],
        roots: RootCertStore,
    ) -> (Self, TlsEvent) {
        let mut stream = Self {
            host: String::new(),
            roots: Some(roots),
            alpn: Vec::new(),
            sock: Value::Null,
            seed: [0u8; 32],
            client: None,
            awaiting: None,
            goal: Goal::Idle,
            inbox: Vec::new(),
            eof_seen: false,
            after_close: None,
            now: 0,
            record: false,
            survive_timeout: false,
            host_error: None,
        };
        let target = match tcp_target_allowed(origin, egress) {
            Ok(target) => target,
            Err(refusal) => return (stream, TlsEvent::Failed(refusal.message())),
        };
        stream.host = target.host().to_string();
        stream.awaiting = Some(Awaiting::Connect);
        let ask = os_ask(
            OsOp::TcpConnect,
            vec![
                Value::string(stream.host.clone()),
                Value::Number(f64::from(target.port())),
            ],
        );
        (stream, ask)
    }

    /// Start TLS over `sock`, already connected to `host` and past the protocol's in-band TLS request (Postgres'
    /// SSLRequest): the handshake asks the journaled seed and clock, then moves ciphertext. The caller has checked
    /// `host` against `egress`.
    pub fn upgrade(host: &str, sock: Value) -> (Self, TlsEvent) {
        let stream = Self {
            host: host.to_string(),
            roots: Some(web_roots()),
            alpn: Vec::new(),
            sock,
            seed: [0u8; 32],
            client: None,
            awaiting: Some(Awaiting::Seed),
            goal: Goal::Idle,
            inbox: Vec::new(),
            eof_seen: false,
            after_close: None,
            now: 0,
            record: false,
            survive_timeout: false,
            host_error: None,
        };
        (stream, TlsEvent::Ask(DriverAsk::Random { bytes: 32 }))
    }

    /// [`TlsStream::upgrade`] under `roots` (a program's own CA) offering `alpn` protocol names in order.
    pub fn upgrade_with(
        host: &str,
        sock: Value,
        roots: RootCertStore,
        alpn: Vec<Vec<u8>>,
    ) -> (Self, TlsEvent) {
        let (mut stream, event) = Self::upgrade(host, sock);
        stream.roots = Some(roots);
        stream.alpn = alpn;
        (stream, event)
    }

    /// Record the session's events from the handshake on, so the kernel can rebuild it ([`TlsStream::transcript`]).
    pub fn recorded(mut self) -> Self {
        self.record = true;
        self
    }

    /// A receive that times out leaves the session intact ([`TlsEvent::TimedOut`]) instead of ending the stream.
    pub fn surviving_timeouts(mut self) -> Self {
        self.survive_timeout = true;
        self
    }

    /// An established session rebuilt from its transcript (see [`TlsClient::replay_with_alpn`]) over `sock`, with
    /// the plaintext it decrypted but the caller has not yet taken, and whether the peer's end of stream was seen.
    pub fn established(
        host: &str,
        sock: Value,
        seed: [u8; 32],
        now: u64,
        replayed: Replayed,
        undelivered: usize,
        eof_seen: bool,
    ) -> Self {
        let skip = replayed.plaintext.len().saturating_sub(undelivered);
        Self {
            host: host.to_string(),
            roots: None,
            alpn: Vec::new(),
            sock,
            seed,
            client: Some(replayed.client),
            awaiting: None,
            goal: Goal::Idle,
            inbox: replayed.plaintext[skip..].to_vec(),
            eof_seen,
            after_close: None,
            now,
            record: true,
            survive_timeout: true,
            host_error: None,
        }
    }

    pub fn sock(&self) -> &Value {
        &self.sock
    }

    pub fn seed(&self) -> [u8; 32] {
        self.seed
    }

    /// The unix second the session's clock read at the handshake.
    pub fn now_secs(&self) -> u64 {
        self.now
    }

    pub fn transcript(&self) -> Option<&[Recorded]> {
        self.client.as_ref().and_then(TlsClient::transcript)
    }

    /// The ALPN protocol the peer chose.
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.client.as_ref().and_then(TlsClient::alpn_protocol)
    }

    pub fn eof_seen(&self) -> bool {
        self.eof_seen
    }

    /// The plaintext decrypted but not yet handed on.
    pub fn undelivered(&self) -> usize {
        self.inbox.len()
    }

    /// The host's own failure when the stream ended because an `os/send` or `os/recv` it asked failed.
    pub fn host_error(&self) -> Option<&OsError> {
        self.host_error.as_ref()
    }

    /// The server name the session verifies.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether an ask of this stream is outstanding, so the next answer belongs to it.
    pub fn is_waiting(&self) -> bool {
        self.awaiting.is_some()
    }

    /// Encrypt and send `plaintext`; ends in `Sent`.
    pub fn send(&mut self, plaintext: &[u8]) -> TlsEvent {
        if self.awaiting.is_some() {
            return TlsEvent::Failed(
                "tls: a send began while another ask is outstanding".to_string(),
            );
        }
        let Some(client) = &mut self.client else {
            return TlsEvent::Failed("tls: the stream is not connected".to_string());
        };
        client.write(plaintext);
        self.goal = Goal::Send;
        self.pump()
    }

    /// The next plaintext from the peer; ends in `Data`, `Eof` or `Failed`.
    pub fn recv(&mut self) -> TlsEvent {
        if self.awaiting.is_some() {
            return TlsEvent::Failed(
                "tls: a recv began while another ask is outstanding".to_string(),
            );
        }
        if self.client.is_none() {
            return TlsEvent::Failed("tls: the stream is not connected".to_string());
        }
        if !self.inbox.is_empty() {
            return TlsEvent::Data(std::mem::take(&mut self.inbox));
        }
        self.goal = Goal::Recv;
        self.pump()
    }

    /// Send the close_notify and close the socket; ends in `Closed`.
    pub fn close(&mut self) -> TlsEvent {
        if self.awaiting.is_some() {
            return TlsEvent::Failed(
                "tls: a close began while another ask is outstanding".to_string(),
            );
        }
        let Some(client) = &mut self.client else {
            return TlsEvent::Failed("tls: the stream is not connected".to_string());
        };
        client.close();
        self.goal = Goal::Close;
        self.pump()
    }

    /// Answer the outstanding ask.
    pub fn resume(&mut self, answer: DriverAnswer) -> TlsEvent {
        let Some(awaiting) = self.awaiting.take() else {
            return TlsEvent::Failed("tls: an answer arrived with no ask outstanding".to_string());
        };
        match (awaiting, answer) {
            (Awaiting::Close, DriverAnswer::Os(_)) => {
                self.after_close.take().unwrap_or_else(|| {
                    TlsEvent::Failed("tls: the stream closed with no outcome".to_string())
                })
            }
            (Awaiting::Connect, DriverAnswer::Os(Ok(reply))) => {
                let Value::Object(fields) = reply else {
                    return TlsEvent::Failed(
                        "tls: os/tcp-connect did not answer a socket".to_string(),
                    );
                };
                let Some(sock) = fields.get("sock").cloned() else {
                    return TlsEvent::Failed(
                        "tls: os/tcp-connect did not answer a socket".to_string(),
                    );
                };
                self.sock = sock;
                self.awaiting = Some(Awaiting::Seed);
                TlsEvent::Ask(DriverAsk::Random { bytes: 32 })
            }
            (Awaiting::Connect, DriverAnswer::Os(Err(error))) => TlsEvent::Failed(error.message),
            (Awaiting::Seed, DriverAnswer::Random(Ok(bytes))) => {
                match <[u8; 32]>::try_from(bytes.as_slice()) {
                    Ok(seed) => {
                        self.seed = seed;
                        self.awaiting = Some(Awaiting::Clock);
                        TlsEvent::Ask(DriverAsk::Clock)
                    }
                    Err(_) => self.fail("tls: the seed ask did not answer 32 bytes".to_string()),
                }
            }
            (Awaiting::Seed, DriverAnswer::Random(Err(message))) => self.fail(message),
            (Awaiting::Clock, DriverAnswer::Clock(Ok(value))) => {
                let Some(roots) = self.roots.take() else {
                    return self.fail("tls: the stream began a second handshake".to_string());
                };
                let now = u64::try_from(value / 1000).unwrap_or(0);
                self.now = now;
                match TlsClient::new_with_alpn(&self.host, self.seed, now, roots, &self.alpn) {
                    Ok(client) => {
                        self.client = Some(if self.record { client.recording() } else { client });
                        self.goal = Goal::Handshake;
                        self.pump()
                    }
                    Err(error) => self.fail(error.to_string()),
                }
            }
            (Awaiting::Clock, DriverAnswer::Clock(Err(message))) => self.fail(message),
            (Awaiting::Alert, DriverAnswer::Os(_)) => {
                let outcome = self.after_close.take().unwrap_or_else(|| {
                    TlsEvent::Failed("tls: the stream failed with no outcome".to_string())
                });
                self.close_socket(outcome)
            }
            (Awaiting::Send, DriverAnswer::Os(Ok(_))) => self.pump(),
            (Awaiting::Recv, DriverAnswer::Os(Ok(reply))) => self.received(reply),
            (Awaiting::Recv, DriverAnswer::Os(Err(error)))
                if self.survive_timeout && error.kind == OsErrorKind::Timeout =>
            {
                self.goal = Goal::Idle;
                TlsEvent::TimedOut(error)
            }
            (Awaiting::Send | Awaiting::Recv, DriverAnswer::Os(Err(error))) => {
                let message = error.message.clone();
                self.host_error = Some(error);
                self.fail(message)
            }
            (_, other) => self.fail(format!("tls: unexpected answer {other:?}")),
        }
    }

    fn received(&mut self, reply: Value) -> TlsEvent {
        let Value::Object(fields) = reply else {
            return self.fail("tls: os/recv did not answer an object".to_string());
        };
        let Some(client) = &mut self.client else {
            return self.fail("tls: bytes arrived with no session".to_string());
        };
        match fields.get("data") {
            Some(Value::String(text)) => match codec::base64_decode(text) {
                Ok(bytes) => client.feed(&bytes),
                Err(message) => {
                    return self.fail(format!("tls: os/recv data is not base64: {message}"))
                }
            },
            _ => return self.fail("tls: os/recv answered no data".to_string()),
        }
        if matches!(fields.get("eof"), Some(Value::Bool(true))) {
            client.feed_eof();
            self.eof_seen = true;
        }
        self.pump()
    }

    fn pump(&mut self) -> TlsEvent {
        loop {
            let Some(client) = &mut self.client else {
                return self.fail("tls: the stream is not connected".to_string());
            };
            let poll = match client.poll() {
                Ok(poll) => poll,
                Err(error) => return self.fail(error.to_string()),
            };
            match (poll, self.goal) {
                (Poll::Transmit(bytes), _) => {
                    self.awaiting = Some(Awaiting::Send);
                    return os_ask(
                        OsOp::Send,
                        vec![
                            self.sock.clone(),
                            Value::string(codec::base64_encode(&bytes)),
                        ],
                    );
                }
                (Poll::Plaintext(bytes), Goal::Recv) => {
                    self.inbox.extend_from_slice(&bytes);
                    return TlsEvent::Data(std::mem::take(&mut self.inbox));
                }
                (Poll::Plaintext(bytes), _) => self.inbox.extend_from_slice(&bytes),
                (Poll::Idle, Goal::Handshake) => {
                    self.goal = Goal::Idle;
                    return TlsEvent::Connected;
                }
                (Poll::Idle, Goal::Send) => {
                    self.goal = Goal::Idle;
                    return TlsEvent::Sent;
                }
                (Poll::Idle, Goal::Close) => return self.close_socket(TlsEvent::Closed),
                (Poll::Idle, Goal::Recv) if self.eof_seen => {
                    return self.fail(
                        "tls: the peer closed the connection without a close_notify".to_string(),
                    );
                }
                (Poll::Idle | Poll::NeedInbound, _) => {
                    self.awaiting = Some(Awaiting::Recv);
                    return os_ask(
                        OsOp::Recv,
                        vec![self.sock.clone(), Value::Number(RECV_BYTES)],
                    );
                }
                (Poll::Closed, Goal::Recv) => {
                    self.goal = Goal::Idle;
                    return TlsEvent::Eof;
                }
                (Poll::Closed, Goal::Close) => return self.close_socket(TlsEvent::Closed),
                (Poll::Closed, _) => {
                    return self.fail("tls: the peer closed the connection".to_string());
                }
            }
        }
    }

    fn close_socket(&mut self, outcome: TlsEvent) -> TlsEvent {
        self.goal = Goal::Idle;
        self.after_close = Some(outcome);
        self.awaiting = Some(Awaiting::Close);
        os_ask(OsOp::Close, vec![self.sock.clone()])
    }

    fn fail(&mut self, message: String) -> TlsEvent {
        self.goal = Goal::Idle;
        if self.sock == Value::Null {
            return TlsEvent::Failed(message);
        }
        let alert = self
            .client
            .as_mut()
            .map(TlsClient::take_alert)
            .unwrap_or_default();
        let outcome = TlsEvent::Failed(message);
        if alert.is_empty() {
            return self.close_socket(outcome);
        }
        self.after_close = Some(outcome);
        self.awaiting = Some(Awaiting::Alert);
        os_ask(
            OsOp::Send,
            vec![
                self.sock.clone(),
                Value::string(codec::base64_encode(&alert)),
            ],
        )
    }
}

/// The stream on its own is a driver that connects and finishes `Ok(null)`; a protocol driver wraps the stream
/// instead and uses `send`/`recv`/`close`.
impl Driver for TlsStream {
    fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
        match TlsStream::resume(self, answer) {
            TlsEvent::Ask(ask) => DriverStep::Ask(ask),
            TlsEvent::Connected | TlsEvent::Sent | TlsEvent::Eof | TlsEvent::Closed => {
                DriverStep::Done(Ok(Value::Null))
            }
            TlsEvent::Data(bytes) => {
                DriverStep::Done(Ok(Value::string(codec::base64_encode(&bytes))))
            }
            TlsEvent::TimedOut(error) => DriverStep::Done(Err(Value::string(error.message))),
            TlsEvent::Failed(message) => DriverStep::Done(Err(Value::string(message))),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use rand_chacha::ChaCha20Rng;
    use rustls::crypto::CryptoProvider;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
    use rustls::server::UnbufferedServerConnection;
    use rustls::sign::{Signer, SigningKey};
    use rustls::time_provider::TimeProvider;
    use rustls::unbuffered::{ConnectionState, EncodeError, UnbufferedStatus};
    use rustls::{ServerConfig, SignatureAlgorithm, SignatureScheme};

    use super::*;
    use crate::driver::{drive, DriveContext, DriveEnd};
    use crate::resumable::{Exchange, HostAnswered, IoAnswer, IoPort, IoRequest};
    use crate::tls::provider::{provider, random};
    use crate::tls::session::{Poll, Recorded, TlsClient};
    use orbital_core_base::value::ValueMap;

    pub(crate) const CERT: &[u8] = include_bytes!("testdata/cert.der");
    const KEY: &[u8] = include_bytes!("testdata/key.der");
    const NOT_BEFORE: u64 = 1_792_209_645;
    pub(crate) const NOW: u64 = 1_792_209_700;
    const ORIGIN: &str = "rediss://localhost:6379";

    fn egress() -> Vec<String> {
        vec![ORIGIN.to_string()]
    }

    pub(crate) fn roots() -> RootCertStore {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(CERT.to_vec()))
            .expect("trust anchor");
        roots
    }

    #[derive(Debug)]
    struct TestKey(p256::ecdsa::SigningKey);

    #[derive(Debug)]
    struct TestSigner(p256::ecdsa::SigningKey);

    impl Signer for TestSigner {
        fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
            use p256::ecdsa::signature::Signer as _;
            let signature: p256::ecdsa::Signature = self.0.sign(message);
            Ok(signature.to_der().as_bytes().to_vec())
        }

        fn scheme(&self) -> SignatureScheme {
            SignatureScheme::ECDSA_NISTP256_SHA256
        }
    }

    impl SigningKey for TestKey {
        fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
            offered
                .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
                .then(|| Box::new(TestSigner(self.0.clone())) as Box<dyn Signer>)
        }

        fn algorithm(&self) -> SignatureAlgorithm {
            SignatureAlgorithm::ECDSA
        }
    }

    #[derive(Debug)]
    struct TestKeys;

    impl rustls::crypto::KeyProvider for TestKeys {
        fn load_private_key(
            &self,
            _: PrivateKeyDer<'static>,
        ) -> Result<Arc<dyn SigningKey>, rustls::Error> {
            use p256::pkcs8::DecodePrivateKey;
            let key = p256::ecdsa::SigningKey::from_pkcs8_der(KEY)
                .map_err(|e| rustls::Error::General(e.to_string()))?;
            Ok(Arc::new(TestKey(key)))
        }
    }

    #[derive(Debug)]
    struct At(u64);

    impl TimeProvider for At {
        fn current_time(&self) -> Option<UnixTime> {
            Some(UnixTime::since_unix_epoch(core::time::Duration::from_secs(
                self.0,
            )))
        }
    }

    pub(crate) struct FakeServer {
        conn: UnbufferedServerConnection,
        rng: Option<ChaCha20Rng>,
        inbound: Vec<u8>,
        outbound: Vec<u8>,
        app_in: Vec<u8>,
        reply: Option<Vec<u8>>,
        error: Option<rustls::Error>,
    }

    impl FakeServer {
        pub(crate) fn new(seed: u8) -> Self {
            let provider = CryptoProvider {
                key_provider: &TestKeys,
                ..provider()
            };
            let config = ServerConfig::builder_with_details(Arc::new(provider), Arc::new(At(NOW)))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .expect("versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(CERT.to_vec())],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec())),
                )
                .expect("server config");
            let mut config = config;
            config.send_tls13_tickets = 0;
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Self {
                conn: UnbufferedServerConnection::new(Arc::new(config)).expect("server"),
                rng: Some(random::drbg([seed; 32])),
                inbound: Vec::new(),
                outbound: Vec::new(),
                app_in: Vec::new(),
                reply: None,
                error: None,
            }
        }

        pub(crate) fn receive(&mut self, bytes: &[u8]) {
            self.inbound.extend_from_slice(bytes);
            let Self {
                conn,
                rng,
                inbound,
                outbound,
                app_in,
                reply,
                error,
            } = self;
            let _scope = random::Scope::enter(rng);
            loop {
                let UnbufferedStatus { mut discard, state } = conn.process_tls_records(inbound);
                let mut stop = false;
                let state = match state {
                    Ok(state) => state,
                    Err(e) => {
                        *error = Some(e);
                        break;
                    }
                };
                match state {
                    ConnectionState::EncodeTlsData(mut state) => {
                        let mut buffer = vec![0u8; 1024];
                        loop {
                            match state.encode(&mut buffer) {
                                Ok(n) => {
                                    outbound.extend_from_slice(&buffer[..n]);
                                    break;
                                }
                                Err(EncodeError::InsufficientSize(s)) => {
                                    buffer.resize(s.required_size, 0)
                                }
                                Err(other) => panic!("{other:?}"),
                            }
                        }
                    }
                    ConnectionState::TransmitTlsData(state) => state.done(),
                    ConnectionState::BlockedHandshake => stop = true,
                    ConnectionState::WriteTraffic(mut state) => match reply.take() {
                        Some(data) => {
                            let mut buffer = vec![0u8; data.len() + 64];
                            let n = state.encrypt(&data, &mut buffer).expect("encrypt");
                            outbound.extend_from_slice(&buffer[..n]);
                        }
                        None => stop = true,
                    },
                    ConnectionState::ReadTraffic(mut state) => {
                        while let Some(record) = state.next_record() {
                            let record = record.expect("record");
                            discard += record.discard;
                            app_in.extend_from_slice(record.payload);
                        }
                        if app_in.as_slice() == b"ping" && reply.is_none() {
                            *reply = Some(b"pong".to_vec());
                        }
                    }
                    ConnectionState::PeerClosed | ConnectionState::Closed => stop = true,
                    other => panic!("unexpected server state {other:?}"),
                }
                inbound.drain(..discard);
                if stop {
                    break;
                }
            }
        }

        pub(crate) fn take(&mut self, want: usize) -> Vec<u8> {
            let n = want.min(self.outbound.len());
            self.outbound.drain(..n).collect()
        }
    }

    struct Host {
        server: FakeServer,
        client_seed: u8,
        clock_secs: u64,
        sent: Vec<Vec<u8>>,
        last_op: Option<OsOp>,
    }

    impl Host {
        fn new(client_seed: u8, clock_secs: u64) -> Self {
            Self {
                server: FakeServer::new(9),
                client_seed,
                clock_secs,
                sent: Vec::new(),
                last_op: None,
            }
        }

        fn answer(&mut self, request: &IoRequest) -> IoAnswer {
            if let IoRequest::Os { op, .. } = request {
                self.last_op = Some(*op);
            }
            match request {
                IoRequest::Os {
                    op: OsOp::TcpConnect,
                    ..
                } => IoAnswer::Os(Ok(Value::object(ValueMap::from_iter([(
                    "sock".into(),
                    Value::Number(1.0),
                )])))),
                IoRequest::Os {
                    op: OsOp::Send,
                    args,
                    ..
                } => {
                    let Value::String(text) = &args[1] else {
                        panic!("send data")
                    };
                    let bytes = codec::base64_decode(text).expect("base64");
                    self.server.receive(&bytes);
                    self.sent.push(bytes);
                    IoAnswer::Os(Ok(Value::object(ValueMap::from_iter([(
                        "bytes".into(),
                        Value::Number(0.0),
                    )]))))
                }
                IoRequest::Os {
                    op: OsOp::Recv,
                    args,
                    ..
                } => {
                    let Value::Number(want) = args[1] else {
                        panic!("recv count")
                    };
                    let data = self.server.take(want as usize);
                    assert!(!data.is_empty(), "the client read with nothing to read");
                    IoAnswer::Os(Ok(Value::object(ValueMap::from_iter([
                        (
                            "data".into(),
                            Value::String(codec::base64_encode(&data).into()),
                        ),
                        ("eof".into(), Value::Bool(false)),
                    ]))))
                }
                IoRequest::Os {
                    op: OsOp::Close, ..
                } => IoAnswer::Os(Ok(Value::Null)),
                IoRequest::Random { bytes } => {
                    IoAnswer::Random(Ok(vec![self.client_seed; *bytes as usize]))
                }
                IoRequest::Clock => IoAnswer::Clock(Ok((self.clock_secs * 1000) as i64)),
                other => panic!("unexpected request {other:?}"),
            }
        }
    }

    struct PingPong {
        tls: TlsStream,
        got: Vec<u8>,
    }

    impl PingPong {
        fn start(egress: &[String]) -> (Self, DriverStep) {
            let (tls, event) = TlsStream::connect_with_roots(ORIGIN, egress, roots());
            let mut driver = Self {
                tls,
                got: Vec::new(),
            };
            let step = driver.advance(event);
            (driver, step)
        }

        fn advance(&mut self, event: TlsEvent) -> DriverStep {
            match event {
                TlsEvent::Ask(ask) => DriverStep::Ask(ask),
                TlsEvent::Connected => {
                    let event = self.tls.send(b"ping");
                    self.advance(event)
                }
                TlsEvent::Sent => {
                    let event = self.tls.recv();
                    self.advance(event)
                }
                TlsEvent::Data(bytes) => {
                    self.got.extend(bytes);
                    let event = if self.got == b"pong" {
                        self.tls.close()
                    } else {
                        self.tls.recv()
                    };
                    self.advance(event)
                }
                TlsEvent::Closed => DriverStep::Done(Ok(Value::String(
                    String::from_utf8_lossy(&self.got).into_owned().into(),
                ))),
                TlsEvent::Eof => DriverStep::Done(Err(Value::string("eof"))),
                TlsEvent::TimedOut(error) => DriverStep::Done(Err(Value::string(error.message))),
                TlsEvent::Failed(message) => DriverStep::Done(Err(Value::string(message))),
            }
        }
    }

    impl Driver for PingPong {
        fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
            let event = self.tls.resume(answer);
            self.advance(event)
        }
    }

    fn step_of(journal: Vec<Exchange>, egress: &[String]) -> (DriveEnd, crate::resumable::StepEnd) {
        let port = IoPort::new(Arc::new(HostAnswered), None, None);
        let (mut driver, step) = PingPong::start(egress);
        port.run_step(journal, || {
            drive(&port, &mut driver, step, DriveContext::default())
        })
    }

    struct Run {
        end: DriveEnd,
        journal: Vec<Exchange>,
        sent: Vec<Vec<u8>>,
        last_op: Option<OsOp>,
        server_error: Option<rustls::Error>,
    }

    fn live(client_seed: u8, clock_secs: u64) -> Run {
        let mut host = Host::new(client_seed, clock_secs);
        let mut journal: Vec<Exchange> = Vec::new();
        loop {
            let (end, step_end) = step_of(journal.clone(), &egress());
            assert!(
                step_end.diverged.is_none(),
                "the replayed driver diverged from its journal"
            );
            if end != DriveEnd::AwaitingHost {
                assert!(step_end.pending.is_empty());
                return Run {
                    end,
                    journal,
                    sent: host.sent,
                    last_op: host.last_op,
                    server_error: host.server.error,
                };
            }
            for request in step_end.pending {
                let answer = host.answer(&request);
                journal.push(Exchange::new(request, answer));
            }
        }
    }

    #[test]
    fn a_handshake_and_an_app_data_exchange_replay_to_identical_sent_bytes() {
        let run = live(7, NOW);
        assert_eq!(run.end, DriveEnd::Done(Ok(Value::string("pong"))));
        assert!(
            run.sent.len() >= 3,
            "client hello, finished and the app write"
        );

        let again = live(7, NOW);
        assert_eq!(again.end, run.end);
        assert_eq!(
            again.sent, run.sent,
            "same seed and time give the same wire bytes"
        );

        for _ in 0..2 {
            let (replayed, step_end) = step_of(run.journal.clone(), &egress());
            assert_eq!(replayed, run.end);
            assert!(step_end.diverged.is_none() && step_end.pending.is_empty());
        }
    }

    #[test]
    fn control_a_different_seed_changes_the_wire_bytes_and_diverges_a_replay() {
        let run = live(7, NOW);
        let other = live(8, NOW);
        assert_eq!(other.end, DriveEnd::Done(Ok(Value::string("pong"))));
        assert_ne!(run.sent[0], other.sent[0]);

        let mut forged = run.journal.clone();
        forged[1] = other.journal[1].clone();
        let (_, step_end) = step_of(forged, &egress());
        assert!(
            step_end.diverged.is_some(),
            "a journaled seed that differs must diverge the replayed sends"
        );
    }

    #[test]
    fn a_certificate_not_yet_valid_fails_the_handshake_and_closes_the_socket() {
        let run = live(7, NOT_BEFORE - 86_400);
        let DriveEnd::Done(Err(Value::String(message))) = run.end else {
            panic!("expected a failure, got {:?}", run.end)
        };
        assert!(
            message.to_lowercase().contains("not valid yet"),
            "{message}"
        );
        assert_eq!(run.last_op, Some(OsOp::Close));
        assert_eq!(run.sent.len(), 2, "the client hello, then the alert");
        assert_eq!(
            run.server_error,
            Some(rustls::Error::AlertReceived(
                rustls::AlertDescription::CertificateExpired
            )),
            "the server received the client's alert"
        );
    }

    #[test]
    fn control_a_certificate_at_the_valid_time_connects() {
        let run = live(7, NOT_BEFORE + 1);
        assert_eq!(run.end, DriveEnd::Done(Ok(Value::string("pong"))));
    }

    #[test]
    fn a_target_outside_egress_is_refused_before_any_ask() {
        let (end, step_end) = step_of(Vec::new(), &["rediss://other.internal:6379".to_string()]);
        assert_eq!(
            end,
            DriveEnd::Done(Err(Value::string(format!("egress denied: {ORIGIN}"))))
        );
        assert!(step_end.pending.is_empty());
    }

    #[test]
    fn control_an_https_egress_entry_never_authorizes_a_tls_tcp_target() {
        let (end, step_end) = step_of(Vec::new(), &["https://localhost:6379".to_string()]);
        assert!(matches!(end, DriveEnd::Done(Err(_))));
        assert!(step_end.pending.is_empty());
    }

    #[test]
    fn a_recorded_transcript_rebuilds_a_session_that_sends_the_same_bytes() {
        let mut server = FakeServer::new(9);
        let mut client = TlsClient::new("localhost", [7u8; 32], NOW, roots())
            .expect("client")
            .recording();
        let mut sent = Vec::new();
        client.write(b"ping");
        let mut received = Vec::new();
        for _ in 0..16 {
            match client.poll().expect("poll") {
                Poll::Transmit(bytes) => {
                    sent.extend_from_slice(&bytes);
                    server.receive(&bytes);
                }
                Poll::Plaintext(bytes) => received.extend(bytes),
                Poll::NeedInbound | Poll::Idle => {
                    let inbound = server.take(usize::MAX);
                    if inbound.is_empty() {
                        break;
                    }
                    client.feed(&inbound);
                }
                Poll::Closed => break,
            }
        }
        assert_eq!(received, b"pong");
        let transcript: Vec<Recorded> = client.transcript().expect("recorded").to_vec();
        let (_, replayed) =
            TlsClient::replay("localhost", [7u8; 32], NOW, roots(), &transcript).expect("replay");
        assert_eq!(replayed, sent);
    }

    fn client_hello(alpn: Vec<Vec<u8>>) -> Vec<u8> {
        let (mut stream, first) =
            TlsStream::upgrade_with("localhost", Value::Number(5.0), roots(), alpn);
        assert_eq!(first, TlsEvent::Ask(DriverAsk::Random { bytes: 32 }));
        stream.resume(DriverAnswer::Random(Ok(vec![7u8; 32])));
        match stream.resume(DriverAnswer::Clock(Ok((NOW * 1000) as i64))) {
            TlsEvent::Ask(DriverAsk::Os {
                op: OsOp::Send,
                args,
                ..
            }) => {
                let Value::String(text) = &args[1] else {
                    panic!("send data")
                };
                codec::base64_decode(text).expect("base64")
            }
            other => panic!("the handshake begins with a raw os/send, got {other:?}"),
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn an_upgrade_offers_exactly_the_alpn_protocols_it_was_given() {
        let offered = client_hello(vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
        assert!(
            contains(&offered, b"h2") && contains(&offered, b"http/1.1"),
            "both names are in the hello"
        );
        assert!(
            offered.len() > client_hello(Vec::new()).len(),
            "control: no ALPN, a smaller hello"
        );
        assert!(
            !contains(&client_hello(Vec::new()), b"http/1.1"),
            "control: none offered, none on the wire"
        );
        assert!(
            !contains(&client_hello(vec![b"h2".to_vec()]), b"http/1.1"),
            "control: only what was named"
        );
    }
}
