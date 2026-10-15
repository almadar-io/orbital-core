<!-- Gap ledger for this repo: the source of truth for its open gaps. -->
# orbital-core — open gaps

Every open gap this repo owns lives here. This file is the source of truth; the monorepo's `docs/Almadar_Gaps.md` only rolls it up.

- **One entry per gap:** `- **<code>** — <what is wrong and where>. <owning package> [mechanical|architectural] — <evidence, prevention rung>`.
- **Codes:** new gaps use this repo's prefix `G-ORBITAL-CORE-`. Take the "Next code" below, then bump it in the same edit. Codes are never reused or renamed.

- **G-ORBITAL-CORE-002** — `EmbeddedKernel::frame` costs about 0.11 ms per ticking trait. On the iOS simulator (iPhone 17 Pro, release C-ABI lib from `make ios-lib`), a frame with 100 ticking traits takes p50 11.4 ms and with 1 000 takes p50 557 ms / p95 752 ms, so every one of 600 frames misses the 16.7 ms budget and the R1-device frame gate (2 ms) by about 280×. Dispatch is fine (p50 0.15 ms over 10^4 events). Suspects: the frame path in `runtime/kernel.rs` and `effects/executor.rs` (per-trait entity layering, per-tick response building, render-store work per tick). Fix: profile one frame (allocation count + time per stage, like `tests/dispatch_alloc_count.rs`), then cut per-trait work so cost grows with traits that actually change. `orbital-core` [architectural] — evidence: `@almadar-io/parity` device rows (`frame-60hz-100`, `frame-60hz-1000`), `orbital-shell-ios/tests/device-bench/`; prevention rung: none of the language rungs (performance), gated by the device frame row.
- **Close by deleting.** Remove the entry in the same commit as the fix.

Next code: `G-ORBITAL-CORE-003`

