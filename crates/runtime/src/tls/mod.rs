//! The kernel's TLS: a rustls `CryptoProvider` over the RustCrypto primitives and a sans-I/O client stream
//! over the host's raw `os/tcp-*` bytes. A session is a pure function of (seed, time, inbound ciphertext):
//! the seed is a journaled `Random` ask, the time the journaled clock, and no session is ever resumed.

pub mod provider;
pub mod session;
pub mod stream;

pub use session::{Poll, TlsClient, TlsError};
pub use stream::{TlsEvent, TlsStream};
