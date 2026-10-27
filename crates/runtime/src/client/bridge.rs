//! Server bridge abstraction.
//!
//! The client talks to a server runtime through one trait, regardless of transport:
//! `SyncServerBridge` (HTTP, production) or `KernelBridge` (in-process, headless
//! verification + embedded runs). Both produce the same `crate::EventResponse`
//! built by the **canonical** `EventResponse::from_process_result` — no per-transport
//! conversion logic.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::client::http::BridgeError;
use crate::row_query::RowPage;
use orbital_core_base::row_set::RowSetRef;
use crate::effects::response::{EventRequest, EventResponse};
use orbital_core_base::value::Value;

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

    /// `reference` resolved by the server that holds the rows, for the viewer this bridge is signed in as (the
    /// server's one resolver, `@read` applied there). A bridge with no such route refuses rather than answering
    /// an empty page.
    fn resolve_rows(&self, _reference: &RowSetRef) -> Result<RowPage, BridgeError> {
        Err(BridgeError::InvalidRequest(
            "this bridge cannot resolve a row set".to_string(),
        ))
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

    fn resolve_rows(&self, reference: &RowSetRef) -> Result<RowPage, BridgeError> {
        crate::client::http::SyncServerBridge::resolve_rows(self, reference)
    }
}

/// Seed one entity's rows into `entities` (defaulting an absent `id` to
/// `"singleton"`, matching every INIT-time fetch/list). Shared by `KernelBridge::
/// seed` and `client_kernel::ClientKernel::seed` so the two in-process bridges
/// don't carry two copies of the same loop.
pub(crate) fn seed_into(
    entities: &crate::entity_source::InMemoryEntityCache,
    entity_type: &str,
    rows: &[Value],
) {
    use crate::entity_source::EntitySource;
    for row in rows {
        let _ = entities.create(entity_type, row);
    }
}

/// In-process bridge: registers a schema in `crate::RuntimeKernel` and answers
/// events directly (no HTTP). Used by the headless verifier (Layer 2) and embedded runs.
pub struct KernelBridge {
    kernel: crate::kernel::RuntimeKernel,
    schema: orbital_core_base::schema::types::OrbitalSchema,
    /// Typed handle to the same cache the kernel reads through `Arc<dyn EntitySource>`.
    /// Kept so callers can [`seed`](Self::seed) rows before driving INIT.
    entities: std::sync::Arc<crate::entity_source::InMemoryEntityCache>,
    /// Answers every ask a dispatch makes (`os/*`, `http`, credentials, rows) under the program's grants and
    /// egress, as `orbital-server` does; without one an ask fails as having no host.
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    host: Option<std::sync::Arc<crate::native_host::NativeHost>>,
}

impl KernelBridge {
    /// Register a resolved schema in a fresh server-environment kernel.
    pub fn new(schema: orbital_core_base::schema::types::OrbitalSchema) -> Result<Self, BridgeError> {
        use {crate::entity_source::{EntitySource, InMemoryEntityCache}, crate::config::KernelConfig, crate::kernel::RuntimeKernel};
        use std::sync::Arc;

        // Keep a typed clone before erasing to `Arc<dyn EntitySource>` for the kernel —
        // writes through it are visible to the kernel immediately (shared allocation).
        let entities = Arc::new(InMemoryEntityCache::new());
        let kernel = RuntimeKernel::new(
            orbital_core_base::effect_types::RuntimeEnvironment::Server,
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
            #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
            host: None,
        })
    }

    /// Answer this bridge's asks in process through a [`NativeHost`](crate::native_host::NativeHost) built over the
    /// bridge's own rows, with the registered program's egress and `os/` grants, so a standalone binary runs a
    /// program that touches the operating system without a server.
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub fn host_natively(
        mut self,
        transport: std::sync::Arc<dyn crate::http_service::Transport>,
        secrets: std::sync::Arc<dyn crate::http_service::SecretSource>,
        handles: std::sync::Arc<crate::os_host::OsHandles>,
        on_event: std::sync::Arc<crate::native_host::StreamEvents>,
    ) -> Self {
        let host = crate::native_host::NativeHost::new(
            std::sync::Arc::clone(&self.entities) as std::sync::Arc<dyn crate::entity_source::EntitySource>,
            transport,
            secrets,
            self.kernel.egress(),
            self.kernel.os_grants(),
            handles,
            on_event,
        );
        self.host = Some(std::sync::Arc::new(host));
        self
    }

    /// Run a dispatch: through the native host when there is one, else straight through with no host.
    fn run(
        &self,
        hosted: impl FnOnce() -> Result<crate::resumable::Progress<crate::resumable::DispatchOutcomes>, crate::resumable::DispatchError>,
        bare: impl FnOnce() -> Result<crate::resumable::DispatchOutcomes, crate::kernel::KernelError>,
    ) -> Result<crate::resumable::DispatchOutcomes, BridgeError> {
        #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
        if let Some(host) = &self.host {
            return hosted()
                .and_then(|progress| crate::resumable::drive_concurrent(&self.kernel, progress, &|request| host.answer(request)))
                .map_err(|e| BridgeError::Server(e.to_string()));
        }
        #[cfg(target_arch = "wasm32")]
        let _ = hosted;
        bare().map_err(|e| BridgeError::Server(e.to_string()))
    }

    /// Seed entity rows so a subsequent INIT `fetch`/`list` on `entity_type` returns them.
    /// Each row carries its own `id` (defaulting to `"singleton"` if absent). Call before
    /// `init_all_traits_blocking`.
    pub fn seed(&self, entity_type: &str, rows: Vec<Value>) {
        seed_into(&self.entities, entity_type, &rows);
    }

    /// Bind `@user` for every subsequent event this bridge dispatches.
    pub fn set_current_user(&self, user: Value) {
        self.kernel.set_current_user(user);
    }

    /// The inline trait names of `orbital`, in declaration order.
    fn orbital_traits(&self, orbital: &str) -> Vec<String> {
        use orbital_core_base::schema::types::TraitReference;
        self.schema
            .orbitals
            .iter()
            .find(|o| o.name == orbital)
            .map(|o| {
                o.traits
                    .iter()
                    .filter_map(|tr| match tr {
                        TraitReference::Inline(td) => Some(td.name.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A request that names no trait: every trait of `orbital` whose held state handles the event
    /// runs it (the server route's discovery); none handling it is a `no-dispatchable-traits`
    /// rejection.
    fn send_discovered_event(
        &self,
        orbital: &str,
        event: &str,
        entity_id: Option<&str>,
        payload: Value,
    ) -> Result<EventResponse, BridgeError> {
        let candidates = self.orbital_traits(orbital);
        let handlers = self.kernel.discover_handlers(&candidates, entity_id, event);
        let held = self.kernel.held_states(&candidates, entity_id);
        let outcomes = if handlers.is_empty() {
            Vec::new()
        } else {
            self.run(
                || self.kernel.start_dispatch_each(&handlers, entity_id, event, payload.clone(), None, None),
                || self.kernel.dispatch_each(&handlers, entity_id, event, payload.clone()),
            )?
        };
        Ok(EventResponse::from_discovery(&outcomes, &handlers, event, held))
    }
}

impl ServerBridge for KernelBridge {
    fn resolve_rows(&self, reference: &RowSetRef) -> Result<RowPage, BridgeError> {
        self.kernel
            .resolve_rows(reference, &self.kernel.get_current_user())
            .map_err(BridgeError::Server)
    }

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
        let entity_id = request.entity_id.as_deref();
        let payload = request.payload.clone().unwrap_or(Value::Null);
        let outcomes = self.run(
            || self.kernel.start_dispatch_mount(seeds, entity_id, payload.clone()),
            || self.kernel.dispatch_mount(seeds, entity_id, payload.clone()),
        )?;
        Ok(EventResponse::from_process_results(
            &outcomes,
            &seeds[0].r#trait,
            seeds[0].event.as_str(),
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
        let Some(trait_name) = trait_name.map(str::to_string) else {
            return self.send_discovered_event(orbital, event, entity_id, payload);
        };
        // Full cross-trait dispatch: the target trait plus every trait that listens for
        // this event (and any it emits) — so a button's CREATE reaches the modal trait
        // that listens for it. `dispatch` is the canonical fan-out in orbital-core.
        let outcomes = self.run(
            || self.kernel.start_dispatch(&trait_name, entity_id, event, payload.clone(), None, None),
            || self.kernel.dispatch(&trait_name, entity_id, event, payload.clone()),
        )?;
        let response = EventResponse::from_process_results(&outcomes, &trait_name, event);
        tracing::debug!(
            target: orbital_core_base::logging::ns::BRIDGE,
            orbital = %orbital,
            trait_name = %trait_name,
            event = %event,
            traits_fired = outcomes.len(),
            success = response.success,
            data_types = ?response.data.as_ref().map(|d| d.keys().collect::<Vec<_>>()).unwrap_or_default(),
            client_effects = response.client_effects_by_trait.as_ref().map_or(0, |e| e.len()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_service::{HttpRequest, HttpResponse, Transport, TransportError};
    use serde_json::json;
    use std::sync::Arc;

    struct Offline;

    impl Transport for Offline {
        fn send(&self, _: &HttpRequest) -> Result<HttpResponse, TransportError> {
            Err(TransportError::Failed("offline".to_string()))
        }
    }

    fn bridge(granted: &std::path::Path, path: &std::path::Path) -> KernelBridge {
        let (granted, path) = (granted.canonicalize().unwrap(), path.canonicalize().unwrap());
        let to = |state: &str, event: &str| {
            json!({ "from": "reading", "to": state, "event": event,
                "effects": [["render-ui", "main", { "type": "typography", "content": "@payload" }]] })
        };
        let schema = serde_json::from_value(json!({
            "name": "Reader",
            "osGrants": { "fs": [{ "glob": format!("{}/**", granted.display()), "modes": ["read"] }] },
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Doc", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "Read", "linkedEntity": "Doc",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "reading" }, { "name": "read" }, { "name": "failed" }],
                        "events": [{ "key": "READ" }, { "key": "READ_OK" }, { "key": "READ_FAILED" }],
                        "transitions": [
                            { "from": "idle", "to": "reading", "event": "READ",
                              "effects": [["os/read-file", path.display().to_string(),
                                           { "emit": { "success": "READ_OK", "failure": "READ_FAILED" } }]] },
                            to("read", "READ_OK"),
                            to("failed", "READ_FAILED")
                        ]
                    }
                }]
            }]
        }))
        .unwrap();
        KernelBridge::new(schema).unwrap()
    }

    fn hosted(b: KernelBridge) -> KernelBridge {
        b.host_natively(
            Arc::new(Offline),
            Arc::new(crate::credentials::Credentials::from_env(Default::default())),
            Arc::new(crate::os_host::OsHandles::default()),
            Arc::new(|_: Option<&crate::resumable::CallSite>, _: Value| {}),
        )
    }

    fn read(b: &KernelBridge) -> (String, String) {
        let response = b.send_event_with_options("Main", "READ", None, Some("Read"), Value::Null).unwrap();
        (b.kernel.held_states(&["Read".to_string()], None)["Read"].clone(), format!("{response:?}"))
    }

    #[test]
    fn a_granted_os_read_through_the_native_host_delivers_the_file_to_the_success_emit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        std::fs::write(&file, "hello from disk").unwrap();
        let (state, response) = read(&hosted(bridge(dir.path(), &file)));
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode("hello from disk");
        assert_eq!(state, "read", "{response}");
        assert!(response.contains(&format!("\"data\": String(\"{data}\")")), "{response}");
    }

    #[test]
    fn control_an_ungranted_path_is_denied_by_the_same_host() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let file = other.path().join("secret.txt");
        std::fs::write(&file, "do not read").unwrap();
        let (state, response) = read(&hosted(bridge(dir.path(), &file)));
        assert_eq!(state, "failed", "{response}");
        assert!(!response.contains("do not read"), "{response}");
        assert!(response.to_lowercase().contains("denied"), "{response}");
    }

    #[test]
    fn control_a_bridge_with_no_host_answers_no_os_ask() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.txt");
        std::fs::write(&file, "hello from disk").unwrap();
        let (state, response) = read(&bridge(dir.path(), &file));
        assert_eq!(state, "failed", "{response}");
        assert!(response.contains("no http host") || response.to_lowercase().contains("unsupported"), "{response}");
    }
}
