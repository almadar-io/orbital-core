#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
// Adapted from rustls-rustcrypto 0.0.2-alpha (https://github.com/RustCrypto/rustls-rustcrypto),
// Copyright (c) 2023 Steve Fan, ctz (RustCrypto Developers), MIT OR Apache-2.0.
// See THIRD_PARTY_NOTICES.md at the orbital-core root.

use aes_gcm::aead::generic_array::typenum::Unsigned;
use aes_gcm::aead::{AeadCore, AeadInPlace, KeyInit, KeySizeUser};
use rustls::crypto::cipher::{
    self, AeadKey, InboundOpaqueMessage, InboundPlainMessage, Iv, KeyBlockShape, MessageDecrypter,
    MessageEncrypter, OutboundOpaqueMessage, OutboundPlainMessage, PrefixedPayload,
    Tls12AeadAlgorithm, Tls13AeadAlgorithm, UnsupportedOperationError,
};
use rustls::{ConnectionTrafficSecrets, ContentType, ProtocolVersion};

use super::{DecryptBufferAdapter, EncryptBufferAdapter};

const TLS12_EXPLICIT_NONCE_LEN: usize = 8;
const TAG_LEN: usize = 16;
const TLS12_OVERHEAD: usize = TLS12_EXPLICIT_NONCE_LEN + TAG_LEN;

fn key_of<A: KeyInit>(key: &AeadKey) -> A {
    A::new_from_slice(key.as_ref()).expect("rustls hands over a key of the algorithm's key_len")
}

macro_rules! gcm {
    ($tls13:ident, $tls13_cipher:ident, $tls12:ident, $tls12_enc:ident, $tls12_dec:ident, $aead:ty) => {
        pub struct $tls13;

        impl Tls13AeadAlgorithm for $tls13 {
            fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
                Box::new($tls13_cipher(key_of::<$aead>(&key), iv))
            }

            fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
                Box::new($tls13_cipher(key_of::<$aead>(&key), iv))
            }

            fn key_len(&self) -> usize {
                <$aead as KeySizeUser>::key_size()
            }

            fn extract_keys(
                &self,
                _: AeadKey,
                _: Iv,
            ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
                Err(UnsupportedOperationError)
            }
        }

        struct $tls13_cipher($aead, Iv);

        impl MessageEncrypter for $tls13_cipher {
            fn encrypt(
                &mut self,
                m: OutboundPlainMessage<'_>,
                seq: u64,
            ) -> Result<OutboundOpaqueMessage, rustls::Error> {
                let total_len = self.encrypted_payload_len(m.payload.len());
                let mut payload = PrefixedPayload::with_capacity(total_len);
                let nonce = cipher::Nonce::new(&self.1, seq).0;
                let aad = cipher::make_tls13_aad(total_len);
                payload.extend_from_chunks(&m.payload);
                payload.extend_from_slice(&m.typ.to_array());
                self.0
                    .encrypt_in_place(&nonce.into(), &aad, &mut EncryptBufferAdapter(&mut payload))
                    .map_err(|_| rustls::Error::EncryptError)?;
                Ok(OutboundOpaqueMessage::new(
                    ContentType::ApplicationData,
                    ProtocolVersion::TLSv1_2,
                    payload,
                ))
            }

            fn encrypted_payload_len(&self, payload_len: usize) -> usize {
                payload_len + 1 + TAG_LEN
            }
        }

        impl MessageDecrypter for $tls13_cipher {
            fn decrypt<'a>(
                &mut self,
                mut m: InboundOpaqueMessage<'a>,
                seq: u64,
            ) -> Result<InboundPlainMessage<'a>, rustls::Error> {
                let payload = &mut m.payload;
                let nonce = cipher::Nonce::new(&self.1, seq).0;
                let aad = cipher::make_tls13_aad(payload.len());
                self.0
                    .decrypt_in_place(&nonce.into(), &aad, &mut DecryptBufferAdapter(payload))
                    .map_err(|_| rustls::Error::DecryptError)?;
                m.into_tls13_unpadded_message()
            }
        }

        pub struct $tls12;

        impl Tls12AeadAlgorithm for $tls12 {
            fn encrypter(
                &self,
                key: AeadKey,
                write_iv: &[u8],
                explicit: &[u8],
            ) -> Box<dyn MessageEncrypter> {
                let mut iv = [0u8; 12];
                iv[..4].copy_from_slice(write_iv);
                iv[4..].copy_from_slice(explicit);
                Box::new($tls12_enc(key_of::<$aead>(&key), iv))
            }

            fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
                let mut fixed = [0u8; 4];
                fixed.copy_from_slice(iv);
                Box::new($tls12_dec(key_of::<$aead>(&key), fixed))
            }

            fn key_block_shape(&self) -> KeyBlockShape {
                KeyBlockShape {
                    enc_key_len: <$aead as KeySizeUser>::key_size(),
                    fixed_iv_len: 4,
                    explicit_nonce_len: TLS12_EXPLICIT_NONCE_LEN,
                }
            }

            fn extract_keys(
                &self,
                _: AeadKey,
                _: &[u8],
                _: &[u8],
            ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
                Err(UnsupportedOperationError)
            }
        }

        struct $tls12_enc($aead, [u8; 12]);

        impl MessageEncrypter for $tls12_enc {
            fn encrypt(
                &mut self,
                m: OutboundPlainMessage<'_>,
                seq: u64,
            ) -> Result<OutboundOpaqueMessage, rustls::Error> {
                let total_len = self.encrypted_payload_len(m.payload.len());
                let mut payload = PrefixedPayload::with_capacity(total_len);
                let nonce = cipher::Nonce::new(&self.1.into(), seq).0;
                let aad = cipher::make_tls12_aad(seq, m.typ, m.version, m.payload.len());
                payload.extend_from_slice(&nonce[4..]);
                payload.extend_from_chunks(&m.payload);
                let tag = self
                    .0
                    .encrypt_in_place_detached(
                        &nonce.into(),
                        &aad,
                        &mut payload.as_mut()[TLS12_EXPLICIT_NONCE_LEN..],
                    )
                    .map_err(|_| rustls::Error::EncryptError)?;
                payload.extend_from_slice(tag.as_ref());
                Ok(OutboundOpaqueMessage::new(m.typ, m.version, payload))
            }

            fn encrypted_payload_len(&self, payload_len: usize) -> usize {
                payload_len + TLS12_EXPLICIT_NONCE_LEN + <$aead as AeadCore>::TagSize::USIZE
            }
        }

        struct $tls12_dec($aead, [u8; 4]);

        impl MessageDecrypter for $tls12_dec {
            fn decrypt<'a>(
                &mut self,
                mut m: InboundOpaqueMessage<'a>,
                seq: u64,
            ) -> Result<InboundPlainMessage<'a>, rustls::Error> {
                let len = m.payload.len();
                if len < TLS12_OVERHEAD {
                    return Err(rustls::Error::DecryptError);
                }
                let mut nonce = [0u8; 12];
                nonce[..4].copy_from_slice(&self.1);
                nonce[4..].copy_from_slice(&m.payload[..TLS12_EXPLICIT_NONCE_LEN]);
                let aad = cipher::make_tls12_aad(seq, m.typ, m.version, len - TLS12_OVERHEAD);
                let tag_pos = len - TAG_LEN;
                {
                    let body = &mut m.payload[TLS12_EXPLICIT_NONCE_LEN..];
                    let (message, tag) = body.split_at_mut(tag_pos - TLS12_EXPLICIT_NONCE_LEN);
                    self.0
                        .decrypt_in_place_detached(
                            &nonce.into(),
                            &aad,
                            message,
                            aes_gcm::Tag::from_slice(tag),
                        )
                        .map_err(|_| rustls::Error::DecryptError)?;
                }
                m.payload.rotate_left(TLS12_EXPLICIT_NONCE_LEN);
                m.payload.truncate(tag_pos - TLS12_EXPLICIT_NONCE_LEN);
                Ok(m.into_plain_message())
            }
        }
    };
}

gcm!(
    Tls13Aes128Gcm,
    Tls13Aes128GcmCipher,
    Tls12Aes128Gcm,
    Tls12Aes128GcmEncrypter,
    Tls12Aes128GcmDecrypter,
    aes_gcm::Aes128Gcm
);
gcm!(
    Tls13Aes256Gcm,
    Tls13Aes256GcmCipher,
    Tls12Aes256Gcm,
    Tls12Aes256GcmEncrypter,
    Tls12Aes256GcmDecrypter,
    aes_gcm::Aes256Gcm
);
