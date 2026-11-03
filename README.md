<div align="center">

# orbital-core

**The Almadar kernel: IR types, the expression evaluator, the runtime kernel and the host ports every shell embeds.**

[![CI](https://github.com/almadar-io/orbital-core/actions/workflows/ci.yml/badge.svg)](https://github.com/almadar-io/orbital-core/actions/workflows/ci.yml)
[![License: BUSL-1.1](https://img.shields.io/badge/license-BUSL--1.1-blue)](LICENSE)

</div>

<p align="center"><sub>Part of <a href="https://github.com/almadar-io/orb"><b>Orb</b></a>: a programming language of composable state machines. <a href="https://orb.almadar.io">orb.almadar.io</a></sub></p>

`orbital-core` holds the meaning of the Almadar language: the IR/OIR types, the S-expression evaluator, the runtime kernel (dispatch, effects, journal), the schema types and the readers over the installed Almadar language data. Semantics live here once; the compiler, the shells and the embedded hosts carry them and do not re-implement them.

## Where this fits

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/almadar-io/orb/main/static/img/readme/architecture/orbital-core-dark.svg">
  <img src="https://raw.githubusercontent.com/almadar-io/orb/main/static/img/readme/architecture/orbital-core-light.svg" width="100%" alt="Where orbital-core sits in the Almadar architecture">
</picture>

| Upstream (this repo depends on) | Downstream (depends on this repo) |
|---|---|
| None: it depends on no other Orbital crate. | [orbital-lolo](https://github.com/almadar-io/orbital-lolo) and [orbital-compiler](https://github.com/almadar-io/orbital-compiler): parse, validate and generate against core's types |
| | [orbital-shell-typescript](https://github.com/almadar-io/orbital-shell-typescript), [orbital-shell-rust](https://github.com/almadar-io/orbital-shell-rust), [orbital-shell-python](https://github.com/almadar-io/orbital-shell-python), [orbital-shell-ios](https://github.com/almadar-io/orbital-shell-ios), [orbital-shell-android](https://github.com/almadar-io/orbital-shell-android), [orbital-shell-os](https://github.com/almadar-io/orbital-shell-os): embed the kernel |
| | [orbital-wasm](https://github.com/almadar-io/orbital-wasm), [orbital-ffi](https://github.com/almadar-io/orbital-ffi), [orbital-router](https://github.com/almadar-io/orbital-router): host bindings over the kernel |
| | [orbital-canvas](https://github.com/almadar-io/orbital-canvas): depends on core's `orbital-core-base` layer |

## Quick start

The crate reads the Almadar language data (patterns, operators, themes, vocabularies) from installed npm packages, so install those first. The toolchain is pinned to Rust 1.93.0 in `rust-toolchain.toml`.

```bash
npm install --no-save --no-package-lock @almadar/core@^10 @almadar/std@^16 @almadar/lolo-ar@^1 @almadar/lolo-sl@^1
cargo test
```

## What's inside

`orbital-core` is a facade crate over three layers plus a bare-metal helper, all under `crates/`. Each layer is a workspace-style member with its own `Cargo.toml`.

| Path | Crate | Role |
|---|---|---|
| `src/` | `orbital-core` | Facade: re-exports every path of the former single crate through the layers below |
| `crates/base/` | `orbital-core-base` | Layer `base`: IR and shared types, contracts, host and grant types |
| `crates/eval/` | `orbital-core-eval` | Layer `eval`: the evaluator and the language loader |
| `crates/runtime/` | `orbital-core-runtime` | Layer `runtime`: the kernel, effects, the embed API, stores |
| `crates/std/` | | The standard library's paths over `core`, `alloc` and `hashbrown`, for the bare-metal build |

Cargo features: `wire-drivers` (default), `async`, `client-http`. Benchmarks live in `benches/` (`parity_fixtures`, `floor_native`), and test fixtures in `tests/fixtures/`.

The language data is not baked into the crate: it is read from the installed packages at run time.

## Development

- Test everything the way CI does: `ORB="npx orb" bash scripts/gates/t4.sh` runs each crate's tests and ratchets failures against `scripts/gates/known-failures/`. CI also runs the Windows-only file-host and grant tests on a Windows runner.
- CI (`.github/workflows/ci.yml`) runs on push and pull request to `main`. `program-release.yml` is a reusable workflow the shell repos call on a tag to build and publish their programs.
- Open gaps are tracked in [`Almadar_Gaps.md`](Almadar_Gaps.md). Work on `main` only; there are no feature branches.
- Inside the monorepo this repo is the submodule `orbital-rust/crates/orbital-core`, and the workspace patches the git dependency to this checkout.

## Docs

- [Orb documentation](https://orb.almadar.io/docs): the language, the `orb` CLI and how the pieces fit

## License

[Business Source License 1.1](LICENSE): free for non-commercial use and internal evaluation; commercial production use needs a licence from Almadar FZE. Converts to Apache-2.0 on 2030-02-01.
