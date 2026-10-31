//! EXEC-RENDER family: what a step paints and when it repaints.

#[path = "../support/mod.rs"]
mod support;

mod conformance;
mod callsite_capture;
mod entity_bound_rerender;
mod let_scope_render;
mod render_provenance;
mod server_messages;
mod unset_config_knob;
