//! The sans-I/O TLS client: bytes in, bytes out, no socket and no clock of its own. Everything the handshake
//! needs from outside is an argument: the 32-byte seed of the DRBG, the unix time, the server name and the
//! trust roots. The output is a pure function of those and the inbound ciphertext; tickets and resumption
//! are off, so no state outlives the session.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::VecDeque;
use std::sync::Arc;

use rand_chacha::ChaCha20Rng;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{Resumption, UnbufferedClientConnection};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::{ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use thiserror::Error;

use super::provider::{provider, random};

#[derive(Debug, Error, Clone, PartialEq)]
pub enum TlsError {
    #[error("tls: {0}")]
    Tls(String),
    #[error("tls: the peer closed the connection without a close_notify")]
    UnexpectedEof,
    #[error("tls: unsupported connection state {0}")]
    Unsupported(String),
}

impl From<rustls::Error> for TlsError {
    fn from(error: rustls::Error) -> Self {
        Self::Tls(error.to_string())
    }
}

#[derive(Debug)]
struct FixedTime(u64);

impl TimeProvider for FixedTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(core::time::Duration::from_secs(
            self.0,
        )))
    }
}

pub fn web_roots() -> RootCertStore {
    RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

pub fn client_config(
    roots: RootCertStore,
    now_unix_secs: u64,
) -> Result<Arc<ClientConfig>, TlsError> {
    client_config_alpn(roots, now_unix_secs, &[])
}

/// [`client_config`] offering `alpn` protocol names in order (none: no ALPN extension).
pub fn client_config_alpn(
    roots: RootCertStore,
    now_unix_secs: u64,
    alpn: &[Vec<u8>],
) -> Result<Arc<ClientConfig>, TlsError> {
    let mut config = ClientConfig::builder_with_details(
        Arc::new(provider()),
        Arc::new(FixedTime(now_unix_secs)),
    )
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.resumption = Resumption::disabled();
    config.alpn_protocols = alpn.to_vec();
    Ok(Arc::new(config))
}

/// Trust roots from PEM text (`-----BEGIN CERTIFICATE-----` blocks), for a server whose certificate a private CA signed.
pub fn roots_from_pem(pem: &str) -> Result<RootCertStore, TlsError> {
    let mut roots = RootCertStore::empty();
    let mut found = 0;
    for block in pem.split("-----BEGIN CERTIFICATE-----").skip(1) {
        let body = block
            .split("-----END CERTIFICATE-----")
            .next()
            .unwrap_or_default();
        let der =
            orbital_core_base::codec::base64_decode(&body.split_whitespace().collect::<String>())
                .map_err(|e| TlsError::Tls(format!("a root certificate is not base64: {e}")))?;
        roots
            .add(CertificateDer::from(der))
            .map_err(|e| TlsError::Tls(format!("a root certificate is refused: {e}")))?;
        found += 1;
    }
    if found == 0 {
        return Err(TlsError::Tls(
            "the root certificate text holds no certificate".to_string(),
        ));
    }
    Ok(roots)
}

/// The peer's certificate is not judged (a server whose certificate nobody can vouch for, `sslmode=require`): the link
/// is encrypted and the handshake signatures are still checked, but any server can answer. Use it only where the
/// protocol's own mode asks for encryption without authentication.
#[derive(Debug)]
struct EncryptOnly(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for EncryptOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// The configuration of a session that encrypts without authenticating the server.
pub fn encrypt_only_config(now_unix_secs: u64) -> Result<Arc<ClientConfig>, TlsError> {
    let crypto = provider();
    let algorithms = crypto.signature_verification_algorithms;
    let mut config =
        ClientConfig::builder_with_details(Arc::new(crypto), Arc::new(FixedTime(now_unix_secs)))
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(EncryptOnly(algorithms)))
            .with_no_client_auth();
    config.resumption = Resumption::disabled();
    Ok(Arc::new(config))
}

/// What happened to the session, in order; replaying them into a fresh session reproduces its bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum Recorded {
    Inbound(Vec<u8>),
    Write(Vec<u8>),
    Close,
    Eof,
}

/// What the session needs next.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    /// Ciphertext the host must send.
    Transmit(Vec<u8>),
    /// Decrypted application bytes from the peer.
    Plaintext(Vec<u8>),
    /// The session waits for inbound ciphertext.
    NeedInbound,
    /// Established, nothing queued to send.
    Idle,
    /// The peer closed with a close_notify.
    Closed,
}

pub struct TlsClient {
    conn: UnbufferedClientConnection,
    rng: Option<ChaCha20Rng>,
    inbound: Vec<u8>,
    encoded: Vec<u8>,
    writes: VecDeque<Vec<u8>>,
    close_requested: bool,
    close_sent: bool,
    eof: bool,
    record: Option<Vec<Recorded>>,
}

enum Step {
    Return(Poll),
    Continue,
}

impl TlsClient {
    pub fn new(
        host: &str,
        seed: [u8; 32],
        now_unix_secs: u64,
        roots: RootCertStore,
    ) -> Result<Self, TlsError> {
        Self::with_config(host, seed, client_config(roots, now_unix_secs)?)
    }

    /// [`TlsClient::new`] offering `alpn` protocol names in order.
    pub fn new_with_alpn(
        host: &str,
        seed: [u8; 32],
        now_unix_secs: u64,
        roots: RootCertStore,
        alpn: &[Vec<u8>],
    ) -> Result<Self, TlsError> {
        Self::with_config(host, seed, client_config_alpn(roots, now_unix_secs, alpn)?)
    }

    /// A session under `config` (see [`client_config`], [`encrypt_only_config`]).
    pub fn with_config(
        host: &str,
        seed: [u8; 32],
        config: Arc<ClientConfig>,
    ) -> Result<Self, TlsError> {
        let name = ServerName::try_from(host.to_owned())
            .map_err(|e| TlsError::Tls(format!("invalid server name {host}: {e}")))?;
        let mut rng = Some(random::drbg(seed));
        let conn = {
            let _scope = random::Scope::enter(&mut rng);
            UnbufferedClientConnection::new(config, name)?
        };
        Ok(Self {
            conn,
            rng,
            inbound: Vec::new(),
            encoded: Vec::new(),
            writes: VecDeque::new(),
            close_requested: false,
            close_sent: false,
            eof: false,
            record: None,
        })
    }

    /// The protocol the peer chose from the ALPN names offered, once the handshake has settled it.
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.conn.alpn_protocol().map(<[u8]>::to_vec)
    }

    pub fn recording(mut self) -> Self {
        self.record = Some(Vec::new());
        self
    }

    pub fn transcript(&self) -> Option<&[Recorded]> {
        self.record.as_deref()
    }

    pub fn feed(&mut self, ciphertext: &[u8]) {
        if let Some(record) = &mut self.record {
            record.push(Recorded::Inbound(ciphertext.to_vec()));
        }
        self.inbound.extend_from_slice(ciphertext);
    }

    pub fn feed_eof(&mut self) {
        if let Some(record) = &mut self.record {
            record.push(Recorded::Eof);
        }
        self.eof = true;
    }

    pub fn write(&mut self, plaintext: &[u8]) {
        if let Some(record) = &mut self.record {
            record.push(Recorded::Write(plaintext.to_vec()));
        }
        self.writes.push_back(plaintext.to_vec());
    }

    pub fn close(&mut self) {
        if let Some(record) = &mut self.record {
            record.push(Recorded::Close);
        }
        self.close_requested = true;
    }

    /// Drive the session until it has something to report.
    pub fn poll(&mut self) -> Result<Poll, TlsError> {
        loop {
            if let Step::Return(poll) = self.advance()? {
                return Ok(poll);
            }
        }
    }

    fn advance(&mut self) -> Result<Step, TlsError> {
        let Self {
            conn,
            rng,
            inbound,
            encoded,
            writes,
            close_requested,
            close_sent,
            eof,
            ..
        } = self;
        let _scope = random::Scope::enter(rng);
        let UnbufferedStatus { mut discard, state } = conn.process_tls_records(inbound);
        let state = match state {
            Ok(state) => state,
            Err(error) => {
                inbound.drain(..discard);
                return Err(error.into());
            }
        };
        let step = match state {
            ConnectionState::EncodeTlsData(mut state) => {
                let mut buffer = vec![0u8; 1024];
                loop {
                    match state.encode(&mut buffer) {
                        Ok(n) => {
                            encoded.extend_from_slice(&buffer[..n]);
                            break;
                        }
                        Err(EncodeError::InsufficientSize(needed)) => {
                            buffer.resize(needed.required_size, 0)
                        }
                        Err(other) => return Err(TlsError::Tls(other.to_string())),
                    }
                }
                Step::Continue
            }
            ConnectionState::TransmitTlsData(state) => {
                state.done();
                if encoded.is_empty() {
                    Step::Continue
                } else {
                    Step::Return(Poll::Transmit(std::mem::take(encoded)))
                }
            }
            ConnectionState::BlockedHandshake => {
                if *eof {
                    return Err(TlsError::UnexpectedEof);
                }
                Step::Return(Poll::NeedInbound)
            }
            ConnectionState::WriteTraffic(mut state) => {
                if let Some(data) = writes.pop_front() {
                    let mut buffer = vec![0u8; data.len() + 64];
                    loop {
                        match state.encrypt(&data, &mut buffer) {
                            Ok(n) => {
                                buffer.truncate(n);
                                break;
                            }
                            Err(EncryptError::InsufficientSize(needed)) => {
                                buffer.resize(needed.required_size, 0)
                            }
                            Err(other) => return Err(TlsError::Tls(other.to_string())),
                        }
                    }
                    Step::Return(Poll::Transmit(buffer))
                } else if *close_requested && !*close_sent {
                    let mut buffer = vec![0u8; 64];
                    loop {
                        match state.queue_close_notify(&mut buffer) {
                            Ok(n) => {
                                buffer.truncate(n);
                                break;
                            }
                            Err(EncryptError::InsufficientSize(needed)) => {
                                buffer.resize(needed.required_size, 0)
                            }
                            Err(other) => return Err(TlsError::Tls(other.to_string())),
                        }
                    }
                    *close_sent = true;
                    Step::Return(Poll::Transmit(buffer))
                } else {
                    Step::Return(Poll::Idle)
                }
            }
            ConnectionState::ReadTraffic(mut state) => {
                let mut plaintext = Vec::new();
                while let Some(record) = state.next_record() {
                    let record = record?;
                    discard += record.discard;
                    plaintext.extend_from_slice(record.payload);
                }
                if plaintext.is_empty() {
                    Step::Continue
                } else {
                    Step::Return(Poll::Plaintext(plaintext))
                }
            }
            ConnectionState::PeerClosed | ConnectionState::Closed => Step::Return(Poll::Closed),
            other => return Err(TlsError::Unsupported(format!("{other:?}"))),
        };
        inbound.drain(..discard);
        Ok(step)
    }

    /// After a fatal error rustls holds the alert it owes the peer: the bytes to send before the socket closes.
    pub fn take_alert(&mut self) -> Vec<u8> {
        let mut alert = Vec::new();
        while let Ok(step) = self.advance() {
            match step {
                Step::Return(Poll::Transmit(bytes)) => alert.extend(bytes),
                Step::Continue => {}
                Step::Return(_) => break,
            }
        }
        alert.extend(std::mem::take(&mut self.encoded));
        alert
    }

    /// Rebuild a session from its recorded transcript: the same seed, time and events give the same bytes.
    /// Returns the session and the ciphertext it sent while replaying.
    pub fn replay(
        host: &str,
        seed: [u8; 32],
        now_unix_secs: u64,
        roots: RootCertStore,
        transcript: &[Recorded],
    ) -> Result<(Self, Vec<u8>), TlsError> {
        let replayed = Self::replay_with_alpn(host, seed, now_unix_secs, roots, &[], transcript)?;
        Ok((replayed.client, replayed.sent))
    }

    /// [`TlsClient::replay`] for a session that offered `alpn`, also returning every plaintext byte the transcript
    /// decrypted, in order, so a caller can tell which of them it has already handed on.
    pub fn replay_with_alpn(
        host: &str,
        seed: [u8; 32],
        now_unix_secs: u64,
        roots: RootCertStore,
        alpn: &[Vec<u8>],
        transcript: &[Recorded],
    ) -> Result<Replayed, TlsError> {
        let mut client = Self::new_with_alpn(host, seed, now_unix_secs, roots, alpn)?.recording();
        let mut sent = Vec::new();
        let mut plaintext = Vec::new();
        client.settle(&mut sent, &mut plaintext)?;
        for event in transcript {
            match event {
                Recorded::Inbound(bytes) => client.feed(bytes),
                Recorded::Write(bytes) => client.write(bytes),
                Recorded::Close => client.close(),
                Recorded::Eof => client.feed_eof(),
            }
            client.settle(&mut sent, &mut plaintext)?;
        }
        Ok(Replayed { client, sent, plaintext })
    }

    fn settle(&mut self, sent: &mut Vec<u8>, plaintext: &mut Vec<u8>) -> Result<(), TlsError> {
        loop {
            match self.poll()? {
                Poll::Transmit(bytes) => sent.extend(bytes),
                Poll::Plaintext(bytes) => plaintext.extend(bytes),
                Poll::NeedInbound | Poll::Idle | Poll::Closed => return Ok(()),
            }
        }
    }
}

/// A session rebuilt from its transcript.
pub struct Replayed {
    pub client: TlsClient,
    /// The ciphertext the session sent while replaying.
    pub sent: Vec<u8>,
    /// Every plaintext byte the transcript decrypted, in order.
    pub plaintext: Vec<u8>,
}
