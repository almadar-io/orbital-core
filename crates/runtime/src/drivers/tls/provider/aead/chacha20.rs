#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
// Adapted from rustls-rustcrypto 0.0.2-alpha (https://github.com/RustCrypto/rustls-rustcrypto),
// Copyright (c) 2023 Steve Fan, ctz (RustCrypto Developers), MIT OR Apache-2.0.
// See THIRD_PARTY_NOTICES.md at the orbital-core root.

use chacha20poly1305::aead::{AeadInPlace, KeyInit, KeySizeUser};
use chacha20poly1305::ChaCha20Poly1305;
use rustls::crypto::cipher::{
    self, AeadKey, InboundOpaqueMessage, InboundPlainMessage, Iv, KeyBlockShape, MessageDecrypter,
    MessageEncrypter, OutboundOpaqueMessage, OutboundPlainMessage, PrefixedPayload,
    Tls12AeadAlgorithm, Tls13AeadAlgorithm, UnsupportedOperationError,
};
use rustls::{ConnectionTrafficSecrets, ContentType, ProtocolVersion};

use super::{DecryptBufferAdapter, EncryptBufferAdapter};

const OVERHEAD: usize = 16;

pub struct Chacha20Poly1305;

fn cipher_of(key: &AeadKey) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new_from_slice(key.as_ref())
        .expect("rustls hands over a key of the algorithm's key_len")
}

impl Tls13AeadAlgorithm for Chacha20Poly1305 {
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        Box::new(Tls13Cipher(cipher_of(&key), iv))
    }

    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        Box::new(Tls13Cipher(cipher_of(&key), iv))
    }

    fn key_len(&self) -> usize {
        ChaCha20Poly1305::key_size()
    }

    fn extract_keys(
        &self,
        _: AeadKey,
        _: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Err(UnsupportedOperationError)
    }
}

impl Tls12AeadAlgorithm for Chacha20Poly1305 {
    fn encrypter(&self, key: AeadKey, iv: &[u8], _: &[u8]) -> Box<dyn MessageEncrypter> {
        Box::new(Tls12Cipher(cipher_of(&key), Iv::copy(iv)))
    }

    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        Box::new(Tls12Cipher(cipher_of(&key), Iv::copy(iv)))
    }

    fn key_block_shape(&self) -> KeyBlockShape {
        KeyBlockShape {
            enc_key_len: 32,
            fixed_iv_len: 12,
            explicit_nonce_len: 0,
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

struct Tls13Cipher(ChaCha20Poly1305, Iv);

impl MessageEncrypter for Tls13Cipher {
    fn encrypt(
        &mut self,
        m: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, rustls::Error> {
        let total_len = self.encrypted_payload_len(m.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        payload.extend_from_chunks(&m.payload);
        payload.extend_from_slice(&m.typ.to_array());
        let nonce = cipher::Nonce::new(&self.1, seq).0;
        let aad = cipher::make_tls13_aad(total_len);
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
        payload_len + 1 + OVERHEAD
    }
}

impl MessageDecrypter for Tls13Cipher {
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

struct Tls12Cipher(ChaCha20Poly1305, Iv);

impl MessageEncrypter for Tls12Cipher {
    fn encrypt(
        &mut self,
        m: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, rustls::Error> {
        let total_len = self.encrypted_payload_len(m.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        payload.extend_from_chunks(&m.payload);
        let nonce = cipher::Nonce::new(&self.1, seq).0;
        let aad = cipher::make_tls12_aad(seq, m.typ, m.version, m.payload.len());
        self.0
            .encrypt_in_place(&nonce.into(), &aad, &mut EncryptBufferAdapter(&mut payload))
            .map_err(|_| rustls::Error::EncryptError)?;
        Ok(OutboundOpaqueMessage::new(m.typ, m.version, payload))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + OVERHEAD
    }
}

impl MessageDecrypter for Tls12Cipher {
    fn decrypt<'a>(
        &mut self,
        mut m: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, rustls::Error> {
        let len = m.payload.len();
        if len < OVERHEAD {
            return Err(rustls::Error::DecryptError);
        }
        let nonce = cipher::Nonce::new(&self.1, seq).0;
        let aad = cipher::make_tls12_aad(seq, m.typ, m.version, len - OVERHEAD);
        self.0
            .decrypt_in_place(
                &nonce.into(),
                &aad,
                &mut DecryptBufferAdapter(&mut m.payload),
            )
            .map_err(|_| rustls::Error::DecryptError)?;
        Ok(m.into_plain_message())
    }
}
