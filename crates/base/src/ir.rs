//! Orbital Intermediate Representation (OIR)
//!
//! The OIR is the universal intermediate representation for multi-backend code generation.
//! It is language-agnostic and contains no target-specific fields.
//!
//! # Architecture
//!
//! ```text
//! .orb Schema → Parse → Validate → Enrich → Resolve → OIR → Backend → Generated Code
//! ```
//!
//! # Design Principles
//!
//! 1. **Language-Agnostic**: No target-specific types (no `tsType`, no `rustType`)
//! 2. **Serializable**: Full JSON and MessagePack support
//! 3. **Complete**: Contains all information needed for code generation
//! 4. **Validated**: By the time we have OIR, all validation has passed

pub mod effects;
pub mod embedded_traits;
pub mod gated_traits;
pub mod expr;
pub mod literal;
pub mod pretty;
pub mod render;
pub mod types;
pub mod validation;

pub use effects::*;
pub use embedded_traits::*;
pub use gated_traits::*;
pub use expr::*;
pub use literal::*;
pub use pretty::*;
pub use render::*;
pub use types::*;
pub use validation::*;
