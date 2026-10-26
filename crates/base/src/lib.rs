//! orbital-core-base: types, IR, wire formats and shared contracts.
//!
//! Directories under `src/` are organizational only — every module stays at the crate root
//! (flat public paths) via its `#[path]`. See `README.md`.
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate orbital_core_std as std;

// program — what a program IS: value model + IR + structure
#[path = "program/value.rs"]
pub mod value;
#[path = "program/shared_entity.rs"]
pub mod shared_entity;
#[path = "program/json_value.rs"]
pub mod json_value;
#[path = "program/type_compat/mod.rs"]
pub mod type_compat;
#[path = "program/ir/mod.rs"]
pub mod ir;
#[path = "program/schema/mod.rs"]
pub mod schema;
#[path = "program/slots.rs"]
pub mod slots;
#[path = "program/stamp.rs"]
pub mod stamp;
#[path = "program/signature/mod.rs"]
pub mod signature;

// wire — the serde JSON boundary every argument and result crosses
#[path = "wire/codec.rs"]
pub mod codec;
#[path = "wire/envelope.rs"]
pub mod envelope;
#[path = "wire/bus_event.rs"]
pub mod bus_event;
#[path = "wire/client_effect.rs"]
pub mod client_effect;
#[path = "wire/effect_types.rs"]
pub mod effect_types;
#[path = "wire/http_wire.rs"]
pub mod http_wire;
#[path = "wire/oauth_wire.rs"]
pub mod oauth_wire;
#[path = "wire/row_set.rs"]
pub mod row_set;
#[path = "wire/tab.rs"]
pub mod tab;
#[path = "wire/tab_call.rs"]
pub mod tab_call;
#[cfg(feature = "ts-export")]
#[path = "wire/wire_ts_export.rs"]
pub mod wire_ts_export;

// lang — installed-language data: what every phase validates against
#[path = "lang/language/mod.rs"]
pub mod language;
#[path = "lang/i18n/mod.rs"]
pub mod i18n;
#[path = "lang/language_data.rs"]
pub mod language_data;
#[cfg(feature = "std")]
#[path = "lang/behavior_index.rs"]
pub mod behavior_index;
#[path = "lang/protocols.rs"]
pub mod protocols;
#[cfg(feature = "std")]
#[path = "lang/behavior_packages.rs"]
pub mod behavior_packages;
#[cfg(feature = "std")]
#[path = "lang/renderer_packs.rs"]
pub mod renderer_packs;
#[path = "lang/theme.rs"]
pub mod theme;
#[path = "lang/route.rs"]
pub mod route;

// contract — shared contracts: the kernel enforces them, hosts honor them
#[path = "contract/effect_row.rs"]
pub mod effect_row;
#[path = "contract/emit_contract.rs"]
pub mod emit_contract;
#[path = "contract/persist_batch.rs"]
pub mod persist_batch;
#[path = "contract/runs_on.rs"]
pub mod runs_on;
#[path = "contract/expects.rs"]
pub mod expects;
#[path = "contract/external_inputs.rs"]
pub mod external_inputs;
#[path = "contract/service_params.rs"]
pub mod service_params;
#[path = "contract/listen_match.rs"]
pub mod listen_match;
#[path = "contract/policy.rs"]
pub mod policy;
#[path = "contract/entity_vocabulary.rs"]
pub mod entity_vocabulary;
#[path = "contract/identity.rs"]
pub mod identity;
#[path = "contract/viewer.rs"]
pub mod viewer;
#[path = "contract/dispatch_mode.rs"]
pub mod dispatch_mode;
#[path = "contract/inbound.rs"]
pub mod inbound;
#[path = "contract/credential_admin.rs"]
pub mod credential_admin;
#[path = "contract/sql_guard.rs"]
pub mod sql_guard;
#[path = "contract/events/mod.rs"]
pub mod events;

// host — the ask surface a host must be able to answer
#[path = "host/os.rs"]
pub mod os;
#[path = "host/os_grants.rs"]
pub mod os_grants;
#[path = "host/agent_ports.rs"]
pub mod agent_ports;
#[path = "host/drawable.rs"]
pub mod drawable;

// backend — the shell/backend contract shells implement
#[cfg(feature = "std")]
#[path = "backend/shell.rs"]
pub mod shell;
#[cfg(feature = "std")]
#[path = "backend/shell_protocol.rs"]
pub mod shell_protocol;
#[cfg(feature = "std")]
#[path = "backend/generate.rs"]
pub mod generate;
#[cfg(feature = "std")]
#[path = "backend/project.rs"]
pub mod project;

// security — token & secret shapes (the kernel signs; base holds the wire types)
#[path = "security/jwt_alg.rs"]
pub mod jwt_alg;
#[path = "security/oauth_ask.rs"]
pub mod oauth_ask;
#[path = "security/oauth_client.rs"]
pub mod oauth_client;
#[path = "security/secrets.rs"]
pub mod secrets;

// util — shared plumbing, no domain vocabulary
#[path = "util/error.rs"]
pub mod error;
#[path = "util/sync.rs"]
pub mod sync;
#[path = "util/collections.rs"]
pub mod collections;
#[path = "util/logging.rs"]
pub mod logging;
#[path = "util/messages.rs"]
pub mod messages;
#[path = "util/str_utils.rs"]
pub mod str_utils;
#[path = "util/time_util.rs"]
pub mod time_util;
#[path = "util/cron.rs"]
pub mod cron;
#[cfg(feature = "std")]
#[path = "util/fixtures.rs"]
pub mod fixtures;
