#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
// Adapted from rustls-rustcrypto 0.0.2-alpha (https://github.com/RustCrypto/rustls-rustcrypto),
// Copyright (c) 2023 Steve Fan, ctz (RustCrypto Developers), MIT OR Apache-2.0.
// See THIRD_PARTY_NOTICES.md at the orbital-core root.

use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::alg_id;
use rustls::pki_types::{AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm};
use rustls::SignatureScheme;
use sha2::{Digest, Sha256, Sha384, Sha512};

macro_rules! ecdsa {
    ($name:ident, $ty:ident, $key_alg:expr, $sig_alg:expr, $curve:ident, $hash:ty) => {
        #[derive(Debug)]
        struct $ty;

        impl SignatureVerificationAlgorithm for $ty {
            fn public_key_alg_id(&self) -> AlgorithmIdentifier {
                $key_alg
            }

            fn signature_alg_id(&self) -> AlgorithmIdentifier {
                $sig_alg
            }

            fn verify_signature(
                &self,
                public_key: &[u8],
                message: &[u8],
                signature: &[u8],
            ) -> Result<(), InvalidSignature> {
                use $curve::ecdsa::signature::hazmat::PrehashVerifier;
                let signature =
                    $curve::ecdsa::Signature::from_der(signature).map_err(|_| InvalidSignature)?;
                let key = $curve::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| InvalidSignature)?;
                key.verify_prehash(&<$hash>::digest(message), &signature)
                    .map_err(|_| InvalidSignature)
            }
        }

        const $name: &dyn SignatureVerificationAlgorithm = &$ty;
    };
}

ecdsa!(
    ECDSA_P256_SHA256,
    EcdsaP256Sha256,
    alg_id::ECDSA_P256,
    alg_id::ECDSA_SHA256,
    p256,
    Sha256
);
ecdsa!(
    ECDSA_P256_SHA384,
    EcdsaP256Sha384,
    alg_id::ECDSA_P256,
    alg_id::ECDSA_SHA384,
    p256,
    Sha384
);
ecdsa!(
    ECDSA_P384_SHA256,
    EcdsaP384Sha256,
    alg_id::ECDSA_P384,
    alg_id::ECDSA_SHA256,
    p384,
    Sha256
);
ecdsa!(
    ECDSA_P384_SHA384,
    EcdsaP384Sha384,
    alg_id::ECDSA_P384,
    alg_id::ECDSA_SHA384,
    p384,
    Sha384
);

macro_rules! rsa_alg {
    ($name:ident, $ty:ident, $sig_alg:expr, $verifying_key:ty, $signature:ty) => {
        #[derive(Debug)]
        struct $ty;

        impl SignatureVerificationAlgorithm for $ty {
            fn public_key_alg_id(&self) -> AlgorithmIdentifier {
                alg_id::RSA_ENCRYPTION
            }

            fn signature_alg_id(&self) -> AlgorithmIdentifier {
                $sig_alg
            }

            fn verify_signature(
                &self,
                public_key: &[u8],
                message: &[u8],
                signature: &[u8],
            ) -> Result<(), InvalidSignature> {
                use rsa::pkcs1::DecodeRsaPublicKey;
                use rsa::signature::Verifier;
                let key =
                    rsa::RsaPublicKey::from_pkcs1_der(public_key).map_err(|_| InvalidSignature)?;
                let signature = <$signature>::try_from(signature).map_err(|_| InvalidSignature)?;
                <$verifying_key>::new(key)
                    .verify(message, &signature)
                    .map_err(|_| InvalidSignature)
            }
        }

        const $name: &dyn SignatureVerificationAlgorithm = &$ty;
    };
}

rsa_alg!(
    RSA_PKCS1_SHA256,
    RsaPkcs1Sha256,
    alg_id::RSA_PKCS1_SHA256,
    rsa::pkcs1v15::VerifyingKey<Sha256>,
    rsa::pkcs1v15::Signature
);
rsa_alg!(
    RSA_PKCS1_SHA384,
    RsaPkcs1Sha384,
    alg_id::RSA_PKCS1_SHA384,
    rsa::pkcs1v15::VerifyingKey<Sha384>,
    rsa::pkcs1v15::Signature
);
rsa_alg!(
    RSA_PKCS1_SHA512,
    RsaPkcs1Sha512,
    alg_id::RSA_PKCS1_SHA512,
    rsa::pkcs1v15::VerifyingKey<Sha512>,
    rsa::pkcs1v15::Signature
);
rsa_alg!(
    RSA_PSS_SHA256,
    RsaPssSha256,
    alg_id::RSA_PSS_SHA256,
    rsa::pss::VerifyingKey<Sha256>,
    rsa::pss::Signature
);
rsa_alg!(
    RSA_PSS_SHA384,
    RsaPssSha384,
    alg_id::RSA_PSS_SHA384,
    rsa::pss::VerifyingKey<Sha384>,
    rsa::pss::Signature
);
rsa_alg!(
    RSA_PSS_SHA512,
    RsaPssSha512,
    alg_id::RSA_PSS_SHA512,
    rsa::pss::VerifyingKey<Sha512>,
    rsa::pss::Signature
);

pub static ALGORITHMS: WebPkiSupportedAlgorithms = WebPkiSupportedAlgorithms {
    all: &[
        ECDSA_P256_SHA256,
        ECDSA_P256_SHA384,
        ECDSA_P384_SHA256,
        ECDSA_P384_SHA384,
        RSA_PKCS1_SHA256,
        RSA_PKCS1_SHA384,
        RSA_PKCS1_SHA512,
        RSA_PSS_SHA256,
        RSA_PSS_SHA384,
        RSA_PSS_SHA512,
    ],
    mapping: &[
        (
            SignatureScheme::ECDSA_NISTP384_SHA384,
            &[ECDSA_P384_SHA384, ECDSA_P256_SHA384],
        ),
        (
            SignatureScheme::ECDSA_NISTP256_SHA256,
            &[ECDSA_P256_SHA256, ECDSA_P384_SHA256],
        ),
        (SignatureScheme::RSA_PKCS1_SHA256, &[RSA_PKCS1_SHA256]),
        (SignatureScheme::RSA_PKCS1_SHA384, &[RSA_PKCS1_SHA384]),
        (SignatureScheme::RSA_PKCS1_SHA512, &[RSA_PKCS1_SHA512]),
        (SignatureScheme::RSA_PSS_SHA256, &[RSA_PSS_SHA256]),
        (SignatureScheme::RSA_PSS_SHA384, &[RSA_PSS_SHA384]),
        (SignatureScheme::RSA_PSS_SHA512, &[RSA_PSS_SHA512]),
    ],
};
