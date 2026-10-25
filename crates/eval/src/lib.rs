//! orbital-core-eval: the one evaluator, the one lowering.
//!
//! Directories under `src/` are organizational only — every module stays at the crate root
//! (flat public paths) via its `#[path]`. See `README.md`.
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate orbital_core_std as std;

#[cfg(test)]
#[path = "tests/codec_tests.rs"]
pub mod codec_tests;
pub mod evaluator;
#[cfg(test)]
#[path = "tests/ir_expr_tests.rs"]
pub mod ir_expr_tests;
#[cfg(test)]
#[path = "tests/language_data_tests.rs"]
pub mod language_data_tests;
#[path = "language/language_loader.rs"]
pub mod language_loader;
#[cfg(test)]
#[path = "tests/language_registry_tests.rs"]
pub mod language_registry_tests;
#[path = "language/literal_type.rs"]
pub mod literal_type;
pub mod lower;
pub mod proof;
pub mod state_machine;
#[cfg(test)]
#[path = "tests/type_compat_tests.rs"]
pub mod type_compat_tests;
pub mod util;
