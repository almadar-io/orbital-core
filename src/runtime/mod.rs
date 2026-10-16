//! Shared runtime kernel for Orbital applications.
//!
//! The `RuntimeKernel` owns the canonical runtime lifecycle — schema registration,
//! trait resolution, event processing, effect execution, state rendering, and
//! listener wiring — so that `orbital-client` and `orbital-server` become thin
//! wrappers around a single source of truth.

pub mod callsite_capture;
pub mod canvas_theme;
pub mod config;
pub mod dispatch_memory;
pub mod dispatch_mode;
pub mod embed_index;
pub mod entity_access;
pub mod entity_source;
pub mod external_inputs;
pub mod http_request;
pub mod http_service;
pub mod oauth_flow;
pub mod os;
pub mod os_grants;
#[cfg(not(target_arch = "wasm32"))]
pub mod os_host;
pub mod kernel;
pub mod key_chord;
pub mod listener;
pub mod mock_answer;
pub mod mock_random;
pub mod motion;
pub mod payload_validation;
pub mod persona;
pub mod process_result;
pub mod projected_payload;
pub mod relations;
pub mod relay;
pub(crate) mod render_store;
pub mod resumable;
pub mod row_query;
pub mod seed;
pub mod server_leg;
pub mod service_params;
pub mod theme;
pub mod tick_clock;
pub mod trait_owners;

pub use config::{DispatchClock, KernelConfig};
pub use dispatch_mode::{compute as compute_dispatch_mode, DispatchMode};
pub use embed_index::EmbedIndex;
pub use entity_access::{
    apply_read_access, check_mutation_access, collect_user_field_literals, identity_field,
    is_access_denied_error, role_satisfying_policy, scope_fetched_rows, user_field_of, vocabulary,
    EntityAccessPolicies, EntityAccessTable, EQUALITY_OPS,
};
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
    DeliveryEntry, EffectRun, EffectRunStatus, OrbitalDetail, OrbitalInfo, ProcessEventResult,
    RuntimeStats,
};
pub use projected_payload::projected_payload;
pub use seed::{
    cross_relation_value, identity_entity_def, identity_entity_name, identity_roster_rows,
    instance_rows, mock_row_id, owner_columns_from_policy, owner_fields_from_schema, sample_rows,
    seed_mock_dataset, seed_sample_data, seed_sample_data_as, stamp_owner,
};
pub use server_leg::ServerLegCollector;
pub use theme::{resolve_theme, ResolvedTheme, ThemeError, ThemeMode, ThemePresets};
pub use tick_clock::{Calendar, TickClock};
pub use trait_owners::TraitOwners;
