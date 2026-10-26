//! EXEC-DIST family: the client and server roles, legs and folds.

#[path = "../support/mod.rs"]
mod support;

mod conformance;
mod client_browser_leg;
mod client_core_server_resolve;
mod client_fetch_success_emit;
mod client_keyless_http;
mod client_only_ignores_host_state;
mod client_stateful_leg;
mod embed_fold_follow_on;
mod embed_host_posting;
mod fold_host_ran_listeners;
mod runs_on_delegation;
mod server_emit_stamp;
mod server_frame_echo;
mod server_held_states;
mod server_leg_retarget;
mod server_leg_seed;
mod server_payload_validation;
mod server_persist_success_mark;
mod server_transitions;
