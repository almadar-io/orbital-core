// Adapted from rustls-rustcrypto 0.0.2-alpha (https://github.com/RustCrypto/rustls-rustcrypto),
// Copyright (c) 2023 Steve Fan, ctz (RustCrypto Developers), MIT OR Apache-2.0.
// See THIRD_PARTY_NOTICES.md at the orbital-core root.

use rustls::crypto::{ActiveKeyExchange, SharedSecret, SupportedKxGroup};
use rustls::NamedGroup;

use super::random::with_rng;

fn no_randomness() -> rustls::Error {
    rustls::Error::FailedToGetRandomBytes
}

#[derive(Debug)]
pub struct X25519;

struct X25519Exchange {
    secret: x25519_dalek::StaticSecret,
    public: x25519_dalek::PublicKey,
}

impl SupportedKxGroup for X25519 {
    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }

    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, rustls::Error> {
        let mut seed = [0u8; 32];
        with_rng(|rng| rand_chacha::rand_core::RngCore::fill_bytes(rng, &mut seed))
            .map_err(|_| no_randomness())?;
        let secret = x25519_dalek::StaticSecret::from(seed);
        let public = x25519_dalek::PublicKey::from(&secret);
        Ok(Box::new(X25519Exchange { secret, public }))
    }
}

impl ActiveKeyExchange for X25519Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, rustls::Error> {
        let peer: [u8; 32] = peer
            .try_into()
            .map_err(|_| rustls::Error::from(rustls::PeerMisbehaved::InvalidKeyShare))?;
        let shared = self
            .secret
            .diffie_hellman(&x25519_dalek::PublicKey::from(peer));
        if !shared.was_contributory() {
            return Err(rustls::PeerMisbehaved::InvalidKeyShare.into());
        }
        Ok(SharedSecret::from(shared.as_bytes().as_slice()))
    }

    fn pub_key(&self) -> &[u8] {
        self.public.as_bytes()
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

#[derive(Debug)]
pub struct SecP256R1;

struct SecP256R1Exchange {
    secret: p256::ecdh::EphemeralSecret,
    public: Box<[u8]>,
}

impl SupportedKxGroup for SecP256R1 {
    fn name(&self) -> NamedGroup {
        NamedGroup::secp256r1
    }

    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, rustls::Error> {
        let secret = with_rng(|rng| p256::ecdh::EphemeralSecret::random(rng))
            .map_err(|_| no_randomness())?;
        let public = p256::PublicKey::from(&secret).to_sec1_bytes();
        Ok(Box::new(SecP256R1Exchange { secret, public }))
    }
}

impl ActiveKeyExchange for SecP256R1Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, rustls::Error> {
        let peer = p256::PublicKey::from_sec1_bytes(peer)
            .map_err(|_| rustls::Error::from(rustls::PeerMisbehaved::InvalidKeyShare))?;
        Ok(SharedSecret::from(
            self.secret
                .diffie_hellman(&peer)
                .raw_secret_bytes()
                .as_slice(),
        ))
    }

    fn pub_key(&self) -> &[u8] {
        &self.public
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::secp256r1
    }
}

pub const ALL_KX_GROUPS: &[&dyn SupportedKxGroup] = &[&X25519, &SecP256R1];
