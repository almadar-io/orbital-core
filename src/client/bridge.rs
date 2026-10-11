//! Server bridge abstraction.
//!
//! The client talks to a server runtime through one trait, regardless of transport:
//! `SyncServerBridge` (HTTP, production) or `KernelBridge` (in-process, headless
//! verification + embedded runs). Both produce the same `crate::EventResponse`
//! built by the **canonical** `EventResponse::from_process_result` — no per-transport
//! conversion logic.

use crate::client::http::BridgeError;
use crate::{EventRequest, EventResponse, Value};

/// Abstract event transport. `Send + Sync` so the desktop/game clients can move an
/// `Arc<dyn ServerBridge>` into their background request thread.
pub trait ServerBridge: Send + Sync {
    fn send_event_with_options(
        &self,
        orbital: &str,
        event: &str,
        entity_id: Option<&str>,
        trait_name: Option<&str>,
        payload: Value,
    ) -> Result<EventResponse, BridgeError>;

    /// Post a full `EventRequest` — the [`crate::runtime::RuntimeKernel::
    /// dispatch_with_server_leg`] "server leg" (`event` + `traits`/`entityByTrait`/
    /// `clientId`, not just the narrower `(event, entity_id, trait_name, payload)`
    /// shape [`Self::send_event_with_options`] takes). Default maps down to
    /// [`Self::send_event_with_options`], which drops `traits`/`entity_by_trait`/
    /// `client_id` — a transport that can carry the full request overrides this
    /// (`SyncServerBridge`, over HTTP, does).
    fn send_request(
        &self,
        orbital: &str,
        request: &EventRequest,
    ) -> Result<EventResponse, BridgeError> {
        self.send_event_with_options(
            orbital,
            &request.event,
            request.entity_id.as_deref(),
            request.target_trait.as_deref(),
            request.payload.clone().unwrap_or(Value::Null),
        )
    }

    /// Run one scheduled tick of `trait_name` (effects + guard) against the in-memory
    /// entity and return the resulting client effects, so a game's per-frame/interval
    /// `set` effects advance the entity locally. Only the in-process [`KernelBridge`]
    /// runs ticks; an HTTP server runs its own scheduler, so [`SyncServerBridge`]
    /// returns an empty response. Default = no-op for any other bridge.
    fn tick(
        &self,
        _orbital: &str,
        _trait_name: &str,
        _tick_name: &str,
        _entity_id: Option<&str>,
    ) -> Result<EventResponse, BridgeError> {
        Ok(EventResponse::success(""))
    }
}

#[cfg(feature = "client-http")]
impl ServerBridge for crate::client::http::SyncServerBridge {
    fn send_event_with_options(
        &self,
        orbital: &str,
        event: &str,
        entity_id: Option<&str>,
        trait_name: Option<&str>,
        payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        // Disambiguate from the trait method (same name).
        crate::client::http::SyncServerBridge::send_event_with_options(
            self, orbital, event, entity_id, trait_name, payload,
        )
    }

    fn send_request(
        &self,
        orbital: &str,
        request: &EventRequest,
    ) -> Result<EventResponse, BridgeError> {
        // Disambiguate from the trait method (same name) — posts `request` AS-IS,
        // preserving `traits`/`entity_by_trait`/`client_id` the default impl drops.
        crate::client::http::SyncServerBridge::send_request(self, orbital, request)
    }
}

/// Seed one entity's rows into `entities` (defaulting an absent `id` to
/// `"singleton"`, matching every INIT-time fetch/list). Shared by `KernelBridge::
/// seed` and `client_kernel::ClientKernel::seed` so the two in-process bridges
/// don't carry two copies of the same loop.
pub(crate) fn seed_into(
    entities: &crate::InMemoryEntityCache,
    entity_type: &str,
    rows: &[Value],
) {
    use crate::EntitySource;
    for row in rows {
        let _ = entities.create(entity_type, row);
    }
}

/// In-process bridge: registers a schema in `crate::RuntimeKernel` and answers
/// events directly (no HTTP). Used by the headless verifier (Layer 2) and embedded runs.
pub struct KernelBridge {
    kernel: crate::runtime::RuntimeKernel,
    schema: crate::OrbitalSchema,
    /// Typed handle to the same cache the kernel reads through `Arc<dyn EntitySource>`.
    /// Kept so callers can [`seed`](Self::seed) rows before driving INIT.
    entities: std::sync::Arc<crate::runtime::InMemoryEntityCache>,
}

impl KernelBridge {
    /// Register a resolved schema in a fresh server-environment kernel.
    pub fn new(schema: crate::OrbitalSchema) -> Result<Self, BridgeError> {
        use crate::runtime::{
            EntitySource, InMemoryEntityCache, KernelConfig, RuntimeKernel,
        };
        use std::sync::Arc;

        // Keep a typed clone before erasing to `Arc<dyn EntitySource>` for the kernel —
        // writes through it are visible to the kernel immediately (shared allocation).
        let entities = Arc::new(InMemoryEntityCache::new());
        let kernel = RuntimeKernel::new(
            crate::RuntimeEnvironment::Server,
            Arc::clone(&entities) as Arc<dyn EntitySource>,
            KernelConfig::default(),
        );
        kernel
            .register_schema(schema.clone())
            .map_err(|e| BridgeError::Server(format!("register_schema: {e}")))?;
        Ok(Self {
            kernel,
            schema,
            entities,
        })
    }

    /// Seed entity rows so a subsequent INIT `fetch`/`list` on `entity_type` returns them.
    /// Each row carries its own `id` (defaulting to `"singleton"` if absent). Call before
    /// `init_all_traits_blocking`.
    pub fn seed(&self, entity_type: &str, rows: Vec<Value>) {
        seed_into(&self.entities, entity_type, &rows);
    }

    /// Seed synthesized sample rows for every inline entity lacking authored `instances`,
    /// via the canonical `crate::runtime::seed_sample_data` (the kernel already
    /// materialized entities that declare `instances`). Used by the headless verifiers
    /// and the screenshot probe so tables/grids render real columns + data.
    pub fn seed_sample(&self, schema: &crate::OrbitalSchema, count: usize) {
        crate::runtime::seed_sample_data(self.entities.as_ref(), schema, count);
    }

    /// [`seed_sample`](Self::seed_sample) stamping owner columns with an explicitly
    /// supplied viewer instead of `ALMADAR_PERSONA`. Pairs with
    /// [`set_current_user`](Self::set_current_user): a harness that binds a viewer
    /// programmatically must seed rows that viewer actually owns, or an
    /// ownership-scoped `@read` matches nothing and a correct app probes as empty.
    pub fn seed_sample_as(
        &self,
        schema: &crate::OrbitalSchema,
        count: usize,
        viewer_id: Option<String>,
    ) {
        crate::runtime::seed_sample_data_as(
            self.entities.as_ref(),
            schema,
            count,
            viewer_id,
        );
    }

    /// Bind `@user` for every subsequent event this bridge dispatches.
    pub fn set_current_user(&self, user: Value) {
        self.kernel.set_current_user(user);
    }

    /// Resolve the trait to dispatch to: an explicit one, else the named orbital's first
    /// trait (mirrors the server route's selection).
    fn resolve_trait(&self, orbital: &str, trait_name: Option<&str>) -> Option<String> {
        use crate::TraitReference;
        if let Some(t) = trait_name {
            return Some(t.to_string());
        }
        self.schema
            .orbitals
            .iter()
            .find(|o| o.name == orbital)
            .and_then(|o| o.traits.first())
            .and_then(|tr| match tr {
                TraitReference::Inline(td) => Some(td.name.clone()),
                _ => None,
            })
    }
}

impl ServerBridge for KernelBridge {
    /// A `mount` request runs every seed's lifecycle event in one dispatch, as
    /// `orbital-server`'s event route does; anything else is a single event.
    fn send_request(
        &self,
        orbital: &str,
        request: &EventRequest,
    ) -> Result<EventResponse, BridgeError> {
        let Some(seeds) = request.mount.as_ref().filter(|seeds| !seeds.is_empty()) else {
            return self.send_event_with_options(
                orbital,
                &request.event,
                request.entity_id.as_deref(),
                request.target_trait.as_deref(),
                request.payload.clone().unwrap_or(Value::Null),
            );
        };
        let outcomes = self
            .kernel
            .dispatch_mount(
                seeds,
                request.entity_id.as_deref(),
                request.payload.clone().unwrap_or(Value::Null),
            )
            .map_err(|e| BridgeError::Server(e.to_string()))?;
        Ok(EventResponse::from_process_results(
            &outcomes,
            &seeds[0].r#trait,
            &seeds[0].event,
        ))
    }

    fn send_event_with_options(
        &self,
        orbital: &str,
        event: &str,
        entity_id: Option<&str>,
        trait_name: Option<&str>,
        payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        let trait_name = self.resolve_trait(orbital, trait_name).ok_or_else(|| {
            BridgeError::InvalidRequest(format!("no dispatchable trait for orbital '{orbital}'"))
        })?;
        // Full cross-trait dispatch: the target trait plus every trait that listens for
        // this event (and any it emits) — so a button's CREATE reaches the modal trait
        // that listens for it. `dispatch` is the canonical fan-out in orbital-core.
        let outcomes = self
            .kernel
            .dispatch(&trait_name, entity_id, event, payload)
            .map_err(|e| BridgeError::Server(e.to_string()))?;
        let response = EventResponse::from_process_results(&outcomes, &trait_name, event);
        tracing::debug!(
            target: crate::logging::ns::BRIDGE,
            orbital = %orbital,
            trait_name = %trait_name,
            event = %event,
            traits_fired = outcomes.len(),
            success = response.success,
            data_types = ?response.data.as_ref().map(|d| d.keys().collect::<Vec<_>>()).unwrap_or_default(),
            client_effects = response.client_effects.as_ref().map_or(0, |e| e.len()),
            "bridge:event",
        );
        Ok(response)
    }

    fn tick(
        &self,
        _orbital: &str,
        trait_name: &str,
        tick_name: &str,
        entity_id: Option<&str>,
    ) -> Result<EventResponse, BridgeError> {
        let result = self
            .kernel
            .process_tick(trait_name, entity_id, tick_name)
            .map_err(|e| BridgeError::Server(e.to_string()))?;
        Ok(EventResponse::from_process_result(trait_name, &result))
    }
}
