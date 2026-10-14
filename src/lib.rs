//! Orbital Core - Core types and runtime for Orbital schema execution
//!
//! This crate provides the foundational types and components for the Orbital system:
//! - Schema types for parsing `.orb` files
//! - IR types for intermediate representation (OIR)
//! - S-expression evaluator for guards and effects
//! - State machine core for trait execution
//! - Event bus for cross-orbital communication
//! - Effect executor with environment-aware routing

pub mod behavior_packages;
pub mod sidecar_store;
pub mod cron;
pub mod drawable;
pub mod effects;
pub mod embed;
pub mod error;
pub mod expects;
pub mod evaluator;
pub mod events;
pub mod fixtures;
pub mod generate;
pub mod i18n;
pub mod language;
pub mod language_data;
pub mod lower;
pub mod project;
pub mod identity;
pub mod ir;
pub mod logging;
pub mod messages;
pub mod oal;
pub mod renderer_coverage;
pub mod renderer_packs;
pub mod route;
pub mod runtime;
pub mod schema;
pub mod shell;
pub mod shell_protocol;
pub mod signature;
pub mod slots;
pub mod stamp;
pub mod state_machine;
pub mod str_utils;
pub mod time_util;
pub mod type_compat;
pub mod client;
pub mod desktop_protocol;
pub mod planner;
pub mod render_report;
pub mod verify;
pub mod util;

pub use error::{EvalError, OrbitalError};
pub use ir::field_shape;
pub use evaluator::{BindingContext, BindingContextBuilder, Evaluator, Value, ValueMap};
pub use events::{shared_bus, Event, EventBus, EventFilter, SharedEventBus, SubscriptionId};
pub use schema::*;
pub use slots::{
    is_floating_slot, modal_open_paths, non_main_slot_transitions, overlay_only_traits,
    slot_role, ModalOpenPath, SlotRole, SlotTransition, MAIN_SLOTS, OVERLAY_SLOTS,
};
pub use state_machine::{
    EventPayload, GuardResult, ManagerStats, StateMachineCore, StateMachineManager,
    TraitInstanceId, TraitState, TransitionFailureReason, TransitionResult,
};
pub use type_compat::{
    compat_kind_oir, enum_value_is_member, literal_value_type, oir_type_accepts_kind,
    oir_type_kind, oir_type_satisfies, oir_type_satisfies_field, set_value_type_compatible,
};

pub use effects::{
    parse_effect, BusEventSource, ClientEffect, ClientEffectByTrait, Effect, EffectDelegate,
    EffectExecutor, EffectOutcome, EffectResult, EmittedEvent, EventRequest, EventResponse,
    FetchHandler, FetchPage, GameEntityHandler, InMemoryPersistence, LiveBroadcastItem, LogLevel,
    NavigateOptions, NavigationHandler, PersistOperation, PersistenceHandler, RenderHandler,
    RuntimeEnvironment, ServerEffectKind, ServerEffectResult, ServiceHandler, StreamItem,
    ExternalInputRequest, TraitFrom, TransitionRejection, TransitionRejectionCode,
};

#[cfg(feature = "async")]
pub use events::{shared_async_bus, AsyncEventBus, SharedAsyncEventBus};

pub use runtime::seed::{sample_rows, seed_sample_data, seed_sample_data_as};
pub use runtime::{
    already_delivered_from, resolve_theme, ClientDispatch, DispatchMode, EntitySource,
    InMemoryEntityCache, KernelConfig, KernelError, ListenerInfo, ListenerManager,
    ProcessEventResult, ResolvedTheme, RuntimeKernel, ServerLegCollector, ThemeError, ThemeMode,
    ThemePresets, TickClock, TickDispatch, Calendar, TraitOwners, TraitSnapshot, TriggerCallback,
};

pub use generate::{
    unsupported_operator_marker, Backend, GeneratedFile, GeneratedOutput, ShellMode,
    UNSUPPORTED_OPERATOR_MARKER,
};

pub use shell::{Shell, ShellConfig, ShellError, ShellResult};
