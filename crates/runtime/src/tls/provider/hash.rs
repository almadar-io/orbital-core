// Adapted from rustls-rustcrypto 0.0.2-alpha (https://github.com/RustCrypto/rustls-rustcrypto),
// Copyright (c) 2023 Steve Fan, ctz (RustCrypto Developers), MIT OR Apache-2.0.
// See THIRD_PARTY_NOTICES.md at the orbital-core root.

use rustls::crypto::hash::{Context, Hash, HashAlgorithm, Output};
use sha2::{Digest, Sha256, Sha384};

macro_rules! sha {
    ($name:ident, $hash:ident, $ctx:ident, $ty:ty, $algo:expr) => {
        struct $hash;
        struct $ctx($ty);

        impl Hash for $hash {
            fn start(&self) -> Box<dyn Context> {
                Box::new($ctx(<$ty>::new()))
            }

            fn hash(&self, data: &[u8]) -> Output {
                Output::new(&<$ty>::digest(data)[..])
            }

            fn output_len(&self) -> usize {
                <$ty as Digest>::output_size()
            }

            fn algorithm(&self) -> HashAlgorithm {
                $algo
            }
        }

        impl Context for $ctx {
            fn fork_finish(&self) -> Output {
                Output::new(&self.0.clone().finalize()[..])
            }

            fn fork(&self) -> Box<dyn Context> {
                Box::new($ctx(self.0.clone()))
            }

            fn finish(self: Box<Self>) -> Output {
                Output::new(&self.0.finalize()[..])
            }

            fn update(&mut self, data: &[u8]) {
                self.0.update(data);
            }
        }

        pub const $name: &dyn Hash = &$hash;
    };
}

sha!(
    SHA256,
    Sha256Hash,
    Sha256Context,
    Sha256,
    HashAlgorithm::SHA256
);
sha!(
    SHA384,
    Sha384Hash,
    Sha384Context,
    Sha384,
    HashAlgorithm::SHA384
);
