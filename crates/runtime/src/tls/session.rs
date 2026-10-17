//! The sans-I/O TLS client: bytes in, bytes out, no socket and no clock of its own. Everything the handshake
//! needs from outside is an argument: the 32-byte seed of the DRBG, the unix time, the server name and the
//! trust roots. The output is a pure function of those and the inbound ciphertext; tickets and resumption
//! are off, so no state outlives the session.

use std::collections::VecDeque;
use std::sync::Arc;

use rand_chacha::ChaCha20Rng;
use rustls::client::{Resumption, UnbufferedClientConnection};
use rustls::pki_types::{ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};
use rustls::{ClientConfig, RootCertStore};
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
    let mut config = ClientConfig::builder_with_details(
        Arc::new(provider()),
        Arc::new(FixedTime(now_unix_secs)),
    )
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
    .with_root_certificates(roots)
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
        let name = ServerName::try_from(host.to_owned())
            .map_err(|e| TlsError::Tls(format!("invalid server name {host}: {e}")))?;
        let config = client_config(roots, now_unix_secs)?;
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
        let mut client = Self::new(host, seed, now_unix_secs, roots)?.recording();
        let mut sent = client.settle()?;
        for event in transcript {
            match event {
                Recorded::Inbound(bytes) => client.feed(bytes),
                Recorded::Write(bytes) => client.write(bytes),
                Recorded::Close => client.close(),
                Recorded::Eof => client.feed_eof(),
            }
            sent.extend(client.settle()?);
        }
        Ok((client, sent))
    }

    fn settle(&mut self) -> Result<Vec<u8>, TlsError> {
        let mut sent = Vec::new();
        loop {
            match self.poll()? {
                Poll::Transmit(bytes) => sent.extend(bytes),
                Poll::Plaintext(_) => {}
                Poll::NeedInbound | Poll::Idle | Poll::Closed => return Ok(sent),
            }
        }
    }
}
