// Adapted from rustls-rustcrypto 0.0.2-alpha (https://github.com/RustCrypto/rustls-rustcrypto),
// Copyright (c) 2023 Steve Fan, ctz (RustCrypto Developers), MIT OR Apache-2.0.
// See THIRD_PARTY_NOTICES.md at the orbital-core root.

//! A rustls `CryptoProvider` built over the RustCrypto crates. Randomness is the session's seeded DRBG
//! (`random`); there is no ambient entropy and no signing key (the kernel is only ever a TLS client).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

mod aead;
mod hash;
mod hmac;
mod kx;
pub mod random;
mod verify;

use std::sync::Arc;

use rustls::crypto::tls12::PrfUsingHmac;
use rustls::crypto::tls13::HkdfUsingHmac;
use rustls::crypto::{
    CipherSuiteCommon, CryptoProvider, KeyExchangeAlgorithm, KeyProvider, SupportedKxGroup,
};
use rustls::pki_types::PrivateKeyDer;
use rustls::sign::SigningKey;
use rustls::{
    CipherSuite, SignatureScheme, SupportedCipherSuite, Tls12CipherSuite, Tls13CipherSuite,
};

#[derive(Debug)]
struct ClientOnly;

impl KeyProvider for ClientOnly {
    fn load_private_key(
        &self,
        _: PrivateKeyDer<'static>,
    ) -> Result<Arc<dyn SigningKey>, rustls::Error> {
        Err(rustls::Error::General(
            "the kernel TLS provider is client only and holds no private keys".into(),
        ))
    }
}

const TLS12_ECDSA_SCHEMES: [SignatureScheme; 2] = [
    SignatureScheme::ECDSA_NISTP256_SHA256,
    SignatureScheme::ECDSA_NISTP384_SHA384,
];

const TLS12_RSA_SCHEMES: [SignatureScheme; 6] = [
    SignatureScheme::RSA_PKCS1_SHA256,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA512,
];

macro_rules! tls12_suite {
    ($name:ident, $suite:ident, $hash:expr, $hmac:expr, $schemes:expr, $aead:expr) => {
        const $name: SupportedCipherSuite = SupportedCipherSuite::Tls12(&Tls12CipherSuite {
            common: CipherSuiteCommon {
                suite: CipherSuite::$suite,
                hash_provider: $hash,
                confidentiality_limit: u64::MAX,
            },
            kx: KeyExchangeAlgorithm::ECDHE,
            sign: &$schemes,
            aead_alg: $aead,
            prf_provider: &PrfUsingHmac($hmac),
        });
    };
}

tls12_suite!(
    ECDSA_AES_128,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    hash::SHA256,
    hmac::SHA256,
    TLS12_ECDSA_SCHEMES,
    &aead::gcm::Tls12Aes128Gcm
);
tls12_suite!(
    ECDSA_AES_256,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    hash::SHA384,
    hmac::SHA384,
    TLS12_ECDSA_SCHEMES,
    &aead::gcm::Tls12Aes256Gcm
);
tls12_suite!(
    ECDSA_CHACHA,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    hash::SHA256,
    hmac::SHA256,
    TLS12_ECDSA_SCHEMES,
    &aead::chacha20::Chacha20Poly1305
);
tls12_suite!(
    RSA_AES_128,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    hash::SHA256,
    hmac::SHA256,
    TLS12_RSA_SCHEMES,
    &aead::gcm::Tls12Aes128Gcm
);
tls12_suite!(
    RSA_AES_256,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    hash::SHA384,
    hmac::SHA384,
    TLS12_RSA_SCHEMES,
    &aead::gcm::Tls12Aes256Gcm
);
tls12_suite!(
    RSA_CHACHA,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    hash::SHA256,
    hmac::SHA256,
    TLS12_RSA_SCHEMES,
    &aead::chacha20::Chacha20Poly1305
);

pub const TLS13_AES_128_GCM_SHA256: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_AES_128_GCM_SHA256,
            hash_provider: hash::SHA256,
            confidentiality_limit: u64::MAX,
        },
        hkdf_provider: &HkdfUsingHmac(hmac::SHA256),
        aead_alg: &aead::gcm::Tls13Aes128Gcm,
        quic: None,
    });

pub const TLS13_AES_256_GCM_SHA384: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_AES_256_GCM_SHA384,
            hash_provider: hash::SHA384,
            confidentiality_limit: u64::MAX,
        },
        hkdf_provider: &HkdfUsingHmac(hmac::SHA384),
        aead_alg: &aead::gcm::Tls13Aes256Gcm,
        quic: None,
    });

pub const TLS13_CHACHA20_POLY1305_SHA256: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
            hash_provider: hash::SHA256,
            confidentiality_limit: u64::MAX,
        },
        hkdf_provider: &HkdfUsingHmac(hmac::SHA256),
        aead_alg: &aead::chacha20::Chacha20Poly1305,
        quic: None,
    });

pub fn cipher_suites() -> Vec<SupportedCipherSuite> {
    vec![
        TLS13_AES_256_GCM_SHA384,
        TLS13_AES_128_GCM_SHA256,
        TLS13_CHACHA20_POLY1305_SHA256,
        ECDSA_AES_256,
        ECDSA_AES_128,
        ECDSA_CHACHA,
        RSA_AES_256,
        RSA_AES_128,
        RSA_CHACHA,
    ]
}

pub fn kx_groups() -> Vec<&'static dyn SupportedKxGroup> {
    kx::ALL_KX_GROUPS.to_vec()
}

pub fn provider() -> CryptoProvider {
    CryptoProvider {
        cipher_suites: cipher_suites(),
        kx_groups: kx_groups(),
        signature_verification_algorithms: verify::ALGORITHMS,
        secure_random: &random::SeededRandom,
        key_provider: &ClientOnly,
    }
}
