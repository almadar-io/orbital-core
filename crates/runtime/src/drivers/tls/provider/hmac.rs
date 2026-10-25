#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
// Adapted from rustls-rustcrypto 0.0.2-alpha (https://github.com/RustCrypto/rustls-rustcrypto),
// Copyright (c) 2023 Steve Fan, ctz (RustCrypto Developers), MIT OR Apache-2.0.
// See THIRD_PARTY_NOTICES.md at the orbital-core root.

use hmac::Mac;
use rustls::crypto::hmac::{Hmac, Key, Tag};
use sha2::{Digest, Sha256, Sha384};

macro_rules! hmac_alg {
    ($name:ident, $alg:ident, $key:ident, $ty:ty) => {
        struct $alg;
        struct $key(hmac::Hmac<$ty>);

        impl Hmac for $alg {
            fn with_key(&self, key: &[u8]) -> Box<dyn Key> {
                Box::new($key(
                    hmac::Hmac::<$ty>::new_from_slice(key)
                        .expect("HMAC accepts a key of any length"),
                ))
            }

            fn hash_output_len(&self) -> usize {
                <$ty as Digest>::output_size()
            }
        }

        impl Key for $key {
            fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> Tag {
                let mut ctx = self.0.clone();
                ctx.update(first);
                for part in middle {
                    ctx.update(part);
                }
                ctx.update(last);
                Tag::new(&ctx.finalize().into_bytes()[..])
            }

            fn tag_len(&self) -> usize {
                <$ty as Digest>::output_size()
            }
        }

        pub const $name: &dyn Hmac = &$alg;
    };
}

hmac_alg!(SHA256, HmacSha256, HmacSha256Key, Sha256);
hmac_alg!(SHA384, HmacSha384, HmacSha384Key, Sha384);
