<!-- Gap ledger for this repo: the source of truth for its open gaps. -->
# orbital-core — open gaps

Every open gap this repo owns lives here. This file is the source of truth; the monorepo's `docs/Almadar_Gaps.md` only rolls it up.

- **One entry per gap:** `- **<code>** — <what is wrong and where>. <owning package> [mechanical|architectural] — <evidence, prevention rung>`.
- **Codes:** new gaps use this repo's prefix `G-ORBITAL-CORE-`. Take the "Next code" below, then bump it in the same edit. Codes are never reused or renamed.

- **G-ORBITAL-CORE-002** — `EmbeddedKernel::frame` costs about 0.11 ms per ticking trait. On the iOS simulator (iPhone 17 Pro, release C-ABI lib from `make ios-lib`), a frame with 100 ticking traits takes p50 11.4 ms and with 1 000 takes p50 557 ms / p95 752 ms, so every one of 600 frames misses the 16.7 ms budget and the R1-device frame gate (2 ms) by about 280×. Dispatch is fine (p50 0.15 ms over 10^4 events). Suspects: the frame path in `runtime/kernel.rs` and `effects/executor.rs` (per-trait entity layering, per-tick response building, render-store work per tick). Fix: profile one frame (allocation count + time per stage, like `tests/dispatch_alloc_count.rs`), then cut per-trait work so cost grows with traits that actually change. `orbital-core` [architectural] — evidence: `@almadar-io/parity` device rows (`frame-60hz-100`, `frame-60hz-1000`), `orbital-shell-ios/tests/device-bench/`; prevention rung: none of the language rungs (performance), gated by the device frame row.
- **Close by deleting.** Remove the entry in the same commit as the fix.

- **G-ORBITAL-CORE-036** — Two `orbital-core-runtime` tests are red on the 2026-10-20 tree because of uncommitted in-flight work adding `firedArm` to the transition response (`crates/runtime/src/effects/response.rs`, `tests/client_only_ignores_host_state.rs`): `tests/embed_session.rs` `every_script_matches_the_c_abi_golden` (the golden `tests/fixtures/embed/scripts/basic-entity.json` step 4 has no `firedArm`) and `tests/resumable_dispatch.rs` `each_hop_of_a_cascade_records_the_entity_before_and_after_it` (line 2290 expects `firedArm` 0 on the hop's observed view, which is `null`). Whoever owns the `firedArm` change refreshes the C-ABI golden and sets `firedArm` on the cascade hop view. `orbital-core` [mechanical] — prevention rung: tooling regression gate (the crate suite). Found by the 2026-10-20 night run; another session's files, not edited.

Next code: `G-ORBITAL-CORE-037`







- **G-ORBITAL-CORE-010** — Required Core clippy recovery run completes with warnings: ordinary expression/signature/test findings, private PersistPrep carrying a680-byte EffectResult inline, and existing public enum/error/layout findings. Actual evidence: workflow results/core-partial-clippy.log, COMMAND_EXIT0 with warnings; this is not zero-warning acceptance. Private size regression subsequently failed680>24 (core-private-persist-layout-red.log, exit101), then private boxing/ordinary repairs were authored; GREEN not started before owner pause2026-10-16. Owner approved public API compatibility via precise item-local expectations backed by constructor/wire/owned-result/layout-growth tests; guards/expectations are not yet implemented. Public boxing would break source APIs. Preserve all public constructors, owned returned data and wire bytes; no blanket allows, extra Await allocation or ignored warning floor. Prevention verdict: none of the .lolo/validate/verify rungs (Rust representation/lint and fixture hygiene, not authored programs); source clippy plus private-size, public-constructor/wire and allocation/replay regressions own this class. [architectural]



- **G-ORBITAL-CORE-016** — A mixed-type list handed to a `[T]` operator whose `T` only types that list (`array/len ["a" 1]`) is refused at run time, so `(array/len (json/parse s Row))` raises when `Row = [string | number]` parses a mixed array: the kernel has no declared type at run time (`json/parse`'s shape is never evaluated). Needs a type-carrying parse result or the shape resolved structurally into the IR; `orb validate` already accepts it. Owner: `eval/src/evaluator/operators/arg_types.rs` + `json.rs`. Prevention rung: `orb validate` is correct; a run-time-only gap, covered by a twin test once the shape is carried. [architectural]

- **G-ORBITAL-CORE-017** — `Value::loose_eq` still coerces `1` = `"1"` and `true` = `1` (base `value.rs`), used by `array/includes`, `array/unique`, `array/index-of`, `validate/oneOf|noneOf`; `=` is type-checked first but these are not. EXEC-EVAL-1 dropped loose equality for operators, so the remaining arms are a drift. Owner: `base/src/value.rs`. Corpus check needed before removing. [mechanical]



