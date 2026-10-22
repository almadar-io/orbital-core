//! Shared runtime kernel for Orbital applications.
//!
//! The `RuntimeKernel` owns the canonical runtime lifecycle — schema registration,
//! trait resolution, event processing, effect execution, state rendering, and
//! listener wiring — so that `orbital-client` and `orbital-server` become thin
//! wrappers around a single source of truth.
//!
//! Directories under `src/` are organizational only — every module stays at the crate root
//! (flat public paths) via its `#[path]`. See `README.md`.

// kernel — the one dispatch loop, worklist and step machinery
#[path = "kernel/config.rs"]
pub mod config;
#[path = "kernel/callsite_capture.rs"]
pub mod callsite_capture;
#[path = "kernel/dispatch_memory.rs"]
pub mod dispatch_memory;
#[path = "kernel/dispatch_trace.rs"]
pub mod dispatch_trace;
#[path = "kernel/listener.rs"]
pub mod listener;
#[path = "kernel/mock_answer.rs"]
pub mod mock_answer;
#[path = "kernel/mock_random.rs"]
pub mod mock_random;
#[path = "kernel/payload_validation.rs"]
pub mod payload_validation;
#[path = "kernel/persona.rs"]
pub mod persona;
#[path = "kernel/planner.rs"]
pub mod planner;
#[path = "kernel/process_result.rs"]
pub mod process_result;
#[path = "kernel/program_machine.rs"]
pub mod program_machine;
#[path = "kernel/projected_payload.rs"]
pub mod projected_payload;
#[path = "kernel/relations.rs"]
pub mod relations;
#[path = "kernel/resumable.rs"]
pub mod resumable;
#[path = "kernel/seed.rs"]
pub mod seed;
#[path = "kernel/server_leg.rs"]
pub mod server_leg;
#[path = "kernel/shared_entity.rs"]
pub mod shared_entity;
#[path = "kernel/tick_clock.rs"]
pub mod tick_clock;
#[path = "kernel/trait_owners.rs"]
pub mod trait_owners;
pub mod kernel;

// client — the client-role embedding of the same kernel
#[path = "client/canvas_theme.rs"]
pub mod canvas_theme;
#[path = "client/desktop_protocol.rs"]
pub mod desktop_protocol;
#[path = "client/embed_index.rs"]
pub mod embed_index;
#[path = "client/motion.rs"]
pub mod motion;
#[path = "client/render_store.rs"]
pub(crate) mod render_store;
pub mod client;

// server — the server-role embedding and its HTTP seam
#[path = "server/auth.rs"]
pub mod auth;
#[path = "server/http_service.rs"]
pub mod http_service;
#[path = "server/stream_ticket.rs"]
pub mod stream_ticket;

// stores — row stores: kernel drivers over raw ports (EXEC-STORE)
#[cfg(not(target_arch = "wasm32"))]
#[path = "stores/couchdb.rs"]
pub mod couchdb;
#[path = "stores/entity_access.rs"]
pub mod entity_access;
#[path = "stores/entity_source.rs"]
pub mod entity_source;
#[path = "stores/firestore.rs"]
pub mod firestore;
#[cfg(all(feature = "wire-drivers", not(target_arch = "wasm32")))]
#[path = "stores/pg_rows.rs"]
pub mod pg_rows;
#[path = "stores/row_query.rs"]
pub mod row_query;
#[path = "stores/sidecar_store.rs"]
pub mod sidecar_store;
#[cfg(not(target_arch = "wasm32"))]
#[path = "stores/store_config.rs"]
pub mod store_config;
#[path = "stores/store_driver.rs"]
pub mod store_driver;
#[path = "stores/vector_store.rs"]
pub mod vector_store;

// drivers — the sans-I/O step machines stores speak over (§3: `http` is one primitive)
#[cfg(feature = "wire-drivers")]
#[path = "drivers/byte_stream.rs"]
pub mod byte_stream;
#[path = "drivers/driver.rs"]
pub mod driver;
#[path = "drivers/http_request.rs"]
pub mod http_request;
#[cfg(feature = "wire-drivers")]
#[path = "drivers/postgres.rs"]
pub mod postgres;
#[cfg(feature = "wire-drivers")]
#[path = "drivers/redis.rs"]
pub mod redis;
#[path = "drivers/tab.rs"]
pub mod tab;
#[cfg(feature = "wire-drivers")]
#[path = "drivers/tls/mod.rs"]
pub mod tls;

// security — auth, tokens and secrets (the kernel signs; hosts verify)
#[path = "security/credential_admin.rs"]
pub mod credential_admin;
#[cfg(not(target_arch = "wasm32"))]
#[path = "security/credential_store.rs"]
pub mod credential_store;
#[cfg(not(target_arch = "wasm32"))]
#[path = "security/credentials.rs"]
pub mod credentials;
#[path = "security/jwt.rs"]
pub mod jwt;
#[path = "security/lenient.rs"]
pub mod lenient;
#[path = "security/oauth/mod.rs"]
pub mod oauth;
#[path = "security/oauth_flow.rs"]
pub mod oauth_flow;
#[path = "security/sigv4.rs"]
pub mod sigv4;
#[path = "security/webpush.rs"]
pub mod webpush;

// host — the native OS hosts and their transports (EXEC-HOST)
#[path = "host/agent_ports.rs"]
pub mod agent_ports;
#[cfg(not(target_arch = "wasm32"))]
#[path = "host/kv_host.rs"]
pub mod kv_host;
#[cfg(not(target_arch = "wasm32"))]
#[path = "host/native_host.rs"]
pub mod native_host;
#[cfg(feature = "native-transport")]
#[path = "host/native_transport.rs"]
pub mod native_transport;
#[cfg(not(target_arch = "wasm32"))]
#[path = "host/os_host/mod.rs"]
pub mod os_host;

// effects — the effect pipeline (EXEC-EFFECTS)
pub mod effects;

// embed — the client embedding surface (EXEC-EMBED)
pub mod embed;

// verify — Layer 2 render verification (EXEC-VERIFY)
#[path = "verify/render_report.rs"]
pub mod render_report;
pub mod verify;

pub use config::{DispatchClock, KernelConfig};
pub use orbital_core_base::dispatch_mode::{compute as compute_dispatch_mode, DispatchMode};
pub use embed_index::EmbedIndex;
pub use entity_access::{apply_read_access, check_mutation_access, collect_user_field_literals, role_satisfying_policy, AccessDenied, scope_fetched_rows, EntityAccessPolicies, EntityAccessTable};
pub use orbital_core_base::entity_vocabulary::{identity_field, user_field_of, vocabulary, EQUALITY_OPS};
pub use entity_source::{EntitySource, InMemoryEntityCache};
pub use kernel::{
    already_delivered_from, AwaitingTrait, ClientDispatch, KernelError, Rerender, RuntimeKernel,
    StatefulLeg, TickDispatch, TickEmit, TraitSnapshot, CROSS_TRAIT_CASCADE_CAP,
};
pub use listener::{ListenerInfo, ListenerManager, TriggerCallback};
pub use persona::{
    default_probe_persona, find_persona_in_roster, identity_roster, persona_from_env_for_schema,
    persona_from_env_spec, resolve_persona_spec, EnvPersona, DEFAULT_IDENTITY_ROSTER_COUNT,
    PERSONA_ENV,
};
pub use process_result::{
    CascadeCut, DeliveryEntry, EffectRun, EffectRunStatus, EntitySummary, OrbitalDetail, OrbitalInfo, ProcessEventResult,
    RuntimeStats,
};
pub use projected_payload::projected_payload;
pub use seed::{cross_relation_value, identity_roster_rows, instance_rows, mock_row_id, sample_rows, seed_mock_dataset, seed_sample_data, seed_sample_data_as};
pub use orbital_core_base::policy::{identity_entity_def, identity_entity_name, owner_columns_from_policy, owner_fields_from_schema, stamp_owner};
pub use server_leg::ServerLegCollector;
pub use orbital_core_base::theme::{resolve_theme, ResolvedTheme, ThemeError, ThemeMode, ThemePresets};
pub use tick_clock::{Calendar, TickClock};
pub use trait_owners::TraitOwners;
