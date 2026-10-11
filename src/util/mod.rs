//! Small cross-crate utilities that don't belong to any one schema/IR
//! concern — `orbital-core` is the most-upstream crate every consumer
//! (`orbital-lolo`, `orbital-compiler`, `orbital-cli`) already depends on.

pub mod canonical_json;
pub mod visit_key;

pub use canonical_json::canonical_json;
pub use visit_key::dispatch_visit_key;
