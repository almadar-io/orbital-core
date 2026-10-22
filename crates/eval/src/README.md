# orbital-core-eval layout

Directories here are **organizational only**. Every module is declared at the crate root in
`lib.rs` with a `#[path]` attribute, so public paths stay flat (`orbital_core_eval::lower`, not
`orbital_core_eval::language::loader`) and downstream crates never change.

- The map of where each module lives is `lib.rs` — keep it the single source of truth.
- Do **not** add a `mod` declaration inside another module that points at these files: that would
  create a second path for the same code (a duplicate owner).
- When moving a file, update its `#[path]` in `lib.rs` and any relative `include_str!`/`include_bytes!`
  paths it contains.
