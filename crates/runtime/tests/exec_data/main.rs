//! EXEC-DATA family: rows, frames and their addressing.

#[path = "../support/mod.rs"]
mod support;

mod conformance;
mod kernel_get_state_singleton;
mod persist_batch;
mod persist_create_required_fields;
mod persist_effect_codes;
mod persist_intrinsic_strip;
mod persist_success_emit;
