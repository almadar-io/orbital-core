# orbital-core-runtime layout

Directories here are **organizational only**. Every module is declared at the crate root in
`lib.rs` with a `#[path]` attribute, so public paths stay flat (`orbital_core_runtime::jwt`, not
`orbital_core_runtime::security::jwt`) and downstream crates never change.

- The map of where each module lives is `lib.rs` — keep it the single source of truth.
- Do **not** add a `mod` declaration inside another module that points at these files: that would
  create a second path for the same code (a duplicate owner).
- When moving a file, update its `#[path]` in `lib.rs` and any relative `include_str!`/`include_bytes!`
  paths it contains.

## Mixed directories (known)

Three group directories also hold the **real children** of a root module that stays at `src/`:

- `kernel/` — children of `src/kernel.rs`, plus flat kernel modules (`config`, `persona`, …).
- `client/` — children of `src/client.rs`, plus flat client modules (`canvas_theme`, `motion`, …).
- `verify/` — children of `src/verify.rs`, plus `render_report.rs`.

The root files stay at `src/` so their default child resolution keeps working; the flat modules
live alongside them in the same directory. Do not split these directories without also moving the
root file (and converting it to `mod.rs` style, as done for `host/os_host`, `security/oauth`).
