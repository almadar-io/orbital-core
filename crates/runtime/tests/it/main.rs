//! One test binary for the runtime integration suites (P-036). Each EXEC family keeps its directory.
#![recursion_limit = "256"]

#[path = "../support/mod.rs"]
mod support;

#[path = "../exec_agent/main.rs"]
mod exec_agent;
#[path = "../exec_conc/main.rs"]
mod exec_conc;
#[path = "../exec_data/main.rs"]
mod exec_data;
#[path = "../exec_dist/main.rs"]
mod exec_dist;
#[path = "../exec_eval/main.rs"]
mod exec_eval;
#[path = "../exec_fetch/main.rs"]
mod exec_fetch;
#[path = "../exec_http/main.rs"]
mod exec_http;
#[path = "../exec_queue/main.rs"]
mod exec_queue;
#[path = "../exec_render/main.rs"]
mod exec_render;
#[path = "../exec_seed/main.rs"]
mod exec_seed;
#[path = "../exec_step/main.rs"]
mod exec_step;
#[path = "../exec_store/main.rs"]
mod exec_store;
#[path = "../exec_tick/main.rs"]
mod exec_tick;
#[path = "../host_effects.rs"]
mod host_effects;
