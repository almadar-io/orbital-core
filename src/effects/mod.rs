//! Effect Executor
//!
//! Environment-aware effect execution system.
//!
//! This module provides:
//! - Typed effect variants (emit, persist, render, navigate, fetch, etc.)
//! - Runtime environment detection (Server vs Client)
//! - Environment-aware routing (execute, no-op, or delegate)
//! - Pluggable handler traits for extensibility
//! - Event response types for client-server communication

mod client_effect;
pub mod effect_row;
pub mod emit_contract;
mod executor;
pub mod live_push;
pub mod persist_batch;
mod response;
pub mod runs_on;
mod types;

pub use client_effect::{ClientEffect, NavigateOptions};
pub use effect_row::{EffectRow, EffectRowBuilder, EffectSite, EffectUse, TICKS_KIND};
pub use emit_contract::{success_emit_key, success_payload, undeclared_reads, ProducedPayload};
pub use executor::{
    EffectDelegate, EffectExecutor, FetchHandler, FetchPage, GameEntityHandler, InMemoryPersistence,
    NavigationHandler, PersistenceHandler, RenderHandler, ServiceHandler, SubstrateFailure,
    SubstrateHandler,
};
pub use live_push::{LiveBroadcastItem, StreamItem};
pub use response::{
    delivery_record_value, tick_event_key, BusEventSource, ClientEffectByTrait, DeliveryRecord,
    DispatchLog, EmittedEvent, EventRequest, EventResponse, ExternalInputRequest, MountSeed,
    RunTick, ServerEffectKind, ServerEffectResult, TraitFrom, TransitionRejection,
    TransitionRejectionCode, BUS_EVENT_SOURCE_FIELDS,
};
pub use runs_on::{
    effect_operator, effect_runs_on, effect_site, entity_arg, ir_effect_site, runs_on, sexpr_runs_server_effect, Residence, RunsOn, EFFECT_SITES, ENTITY_ARGS,
};
pub use types::{
    is_known_effect_name, parse_effect, Effect, EffectOutcome, EffectResult, FetchEmit, LogLevel,
    PersistOperation, RuntimeEnvironment, CLIENT_EFFECT_KINDS,
};
