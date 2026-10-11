//! Language i18n for the compiled path: a central vocabulary table
//! (`tables`), per-language canonicalization (`lang`), structural `.orb`
//! JSON translation (`translate`), and the coverage gate (`coverage`).
//! `orbital-lolo` (the `.lolo` parser) and `orbital-cli` (`.orb` translate /
//! `orb i18n`) both build on this crate so there is exactly one vocabulary
//! owner for the compiled path.

pub mod coverage;
pub mod lang;
pub mod localize;
pub mod packs;
pub mod tables;
pub mod translate;

#[cfg(test)]
mod orb_keys_test;
