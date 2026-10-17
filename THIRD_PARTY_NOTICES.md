# Third-party notices

## rustls-rustcrypto 0.0.2-alpha

Source: https://github.com/RustCrypto/rustls-rustcrypto
License: MIT OR Apache-2.0 (used here under the MIT terms below)

The rustls `CryptoProvider` in `crates/runtime/src/tls/provider/` (the AEAD, hash, HMAC, key exchange and
signature verification modules, and the cipher suite tables in `provider.rs`) is adapted from this crate. Each
adapted file carries a header naming this origin. The key exchange and the `SecureRandom` draw from the session's
seeded DRBG instead of the operating system, the Ed25519, P-384 key exchange, QUIC and signing code is dropped,
and the macros are rewritten without `paste`.

```
Copyright (c) 2023 Steve Fan, ctz

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
