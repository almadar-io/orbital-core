//! Shared client circuit.
//!
//! `ClientCore` owns the non-render state machine + closed-circuit logic that the
//! `DesktopApp`, `GameApp` (and any future client) all need: the entity cache, the
//! `ui_slots`/`trait_content` render index, form state, and the `process_response`
//! → `execute_client_effect` → `parse_pattern` pipeline. The backends keep only
//! their window/renderer glue and delegate every circuit operation here, so there
//! is ONE event circuit and ONE `@trait` index, not one copy per backend.
//!
//! The async (threaded) send path stays in `DesktopApp` (it drives the egui frame
//! loop); the synchronous [`ClientCore::drive_blocking`] path serves `GameApp` and
//! the Layer-2 verifier.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::value::ValueMap;
use crate::client::client_kernel::ClientKernel;
use crate::client::helpers::{format_value, get_string, SlotContent};
use orbital_core_base::client_effect::ClientEffect;
use crate::effects::response::EventResponse;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

/// Pull the target entity id from an event payload (`row.id`, `id`, or `data.id`).
pub fn extract_entity_id(payload: &Value) -> Option<String> {
    let obj = payload.as_object()?;
    let as_id = |v: Option<&Value>| v.and_then(|x| x.as_str()).map(|s| s.to_string());
    as_id(
        obj.get("row")
            .and_then(|r| r.as_object())
            .and_then(|r| r.get("id")),
    )
    .or_else(|| as_id(obj.get("id")))
    .or_else(|| {
        as_id(
            obj.get("data")
                .and_then(|d| d.as_object())
                .and_then(|d| d.get("id")),
        )
    })
}

/// Shared, render-agnostic client state + circuit.
pub struct ClientCore {
    /// Orbital schema (resolved).
    pub schema: OrbitalSchema,
    /// Server bridge (HTTP or in-process kernel) for the closed circuit. Legacy
    /// path — a core constructed with [`ClientKernel::new`] (`Self::client_kernel`
    /// is `Some`) drives `drive_blocking`/`drive_tick_blocking` through the client
    /// kernel instead; this field stays populated ONLY for `KernelBridge`/verify
    /// callers ([`ClientCore::new`]), which never set `client_kernel`.
    pub server_bridge: Option<Arc<dyn crate::client::bridge::ServerBridge>>,
    /// The client-role kernel (Client env + `ServerLegCollector` + mode-aware
    /// post/fold), when this core was built with [`ClientCore::with_client_kernel`].
    /// `drive_blocking`/`drive_tick_blocking` prefer this over `server_bridge`.
    pub client_kernel: Option<Arc<ClientKernel>>,
    /// Active orbital index.
    pub current_orbital_idx: usize,
    /// Active trait name.
    pub current_trait_name: String,
    /// Active state.
    pub current_state: String,
    /// UI slots populated from render-ui effects.
    pub ui_slots: HashMap<String, Vec<SlotContent>>,
    /// Per-trait `main` render-ui content, keyed by trait name — resolves `@trait.X`.
    pub trait_content: ValueMap,
    /// Form field values for the active form.
    pub form_data: HashMap<String, String>,
    /// Entity cache populated from server responses.
    pub entities: HashMap<String, Vec<Value>>,
    /// Status message (text, is_error).
    pub status_message: Option<(String, bool)>,
    /// Events queued during rendering, fired on the next frame.
    /// Events queued during a frame to be re-fired afterward. Each carries its
    /// **origin trait** (`Some` for an effect-emitted follow-up, which must be
    /// re-dispatched to the trait that emitted it — e.g. a `fetch`'s `BrowseItemLoaded`
    /// back to the browse trait; `None` for a user action, which targets the active trait).
    pub pending_events: Vec<(String, Option<String>, Value)>,
    /// Entity currently being edited (for form pre-population).
    pub editing_entity: Option<Value>,
    /// Traits a host embeds (`traitEmbedIds`): they render at the host's `@trait.X` site,
    /// never as a `main` writer of their own.
    embedded: std::collections::HashSet<String>,
    /// The compiler's `awaitRender` predictions outside portal slots, the dispatches awaiting
    /// their server legs, and the theme's busy delay (`None`: the theme shows no skeletons).
    /// The page on screen (its name); `main` shows only its traits' writers, as the web and
    /// native routers do. `None` when the orbital declares no pages.
    pub current_page: Option<String>,
    /// The current page's route params (`:param` segments, then the navigate params, which win):
    /// the payload its traits' INIT runs with (the web router's `initPayload`).
    pub route_params: ValueMap,
    /// Set when a `navigate` entered a page: the host mounts it ([`Self::mount_page_blocking`]).
    pub page_entered: bool,
    page_history: Vec<PageVisit>,
    await_rows: Vec<orbital_core_base::ir::types::AwaitRenderRow>,
    awaiting: Vec<AwaitingEntry>,
    next_await_token: u64,
    awaiting_motion: Option<std::time::Duration>,
}

struct AwaitingEntry {
    token: u64,
    since: web_time::Instant,
    renders: Vec<orbital_core_base::ir::types::AwaitRender>,
}

impl ClientCore {
    /// Build a core for `schema` wired to an optional bridge, seeded with the first
    /// orbital's first trait as the active trait.
    pub fn new(
        schema: OrbitalSchema,
        server_bridge: Option<Arc<dyn crate::client::bridge::ServerBridge>>,
    ) -> Self {
        let current_trait_name = schema
            .orbitals
            .first()
            .and_then(|orbital| orbital.traits.first())
            .and_then(|t| t.trait_name())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        let embedded = embedded_traits(&schema);
        let current_page = initial_page(&schema, 0);

        Self {
            schema,
            server_bridge,
            client_kernel: None,
            current_orbital_idx: 0,
            current_trait_name,
            current_state: "browsing".to_string(),
            ui_slots: HashMap::new(),
            trait_content: ValueMap::default(),
            form_data: HashMap::new(),
            entities: HashMap::new(),
            status_message: None,
            pending_events: Vec::new(),
            editing_entity: None,
            embedded,
            current_page,
            route_params: ValueMap::default(),
            page_entered: false,
            page_history: Vec::new(),
            await_rows: Vec::new(),
            awaiting: Vec::new(),
            next_await_token: 0,
            awaiting_motion: None,
        }
    }

    /// A row-set reference resolved for the viewer it is signed in as, by the store's location: the client-role
    /// kernel when there is one, else the server bridge (the server holds the rows and applies `@read`); `Err`
    /// when this core has neither. Renderers reach `@read`-checked rows only through here.
    pub fn resolve_rows(
        &self,
        reference: &orbital_core_base::row_set::RowSetRef,
    ) -> Result<crate::row_query::RowPage, String> {
        match (&self.client_kernel, &self.server_bridge) {
            (Some(kernel), _) => kernel.resolve_rows(reference),
            (None, Some(bridge)) => bridge.resolve_rows(reference).map_err(|e| e.to_string()),
            (None, None) => Err("no store to resolve a row set against".to_string()),
        }
    }

    /// Build a core driven by the client-role kernel (`client_kernel.rs`) instead
    /// of a bare [`crate::client::bridge::ServerBridge`] — `drive_blocking`/
    /// `drive_tick_blocking` route through [`ClientKernel::dispatch`]/[`ClientKernel::tick`],
    /// which apply the mode-aware post/fold circuit (`HybridClientOnly` never
    /// posts, `RuntimeOptimistic` snapshots + rolls back, `PersistedAwaited` folds
    /// before presenting). `server_bridge` stays `None` on this path.
    pub fn with_client_kernel(schema: OrbitalSchema, client_kernel: Arc<ClientKernel>) -> Self {
        let mut core = Self::new(schema, None);
        core.awaiting_motion = client_kernel.awaiting_motion();
        core.client_kernel = Some(client_kernel);
        core
    }

    /// Whether this core has a live transport — a `client_kernel` core reads its
    /// own [`ClientKernel::is_online`] (its `server_bridge` field stays `None`),
    /// a legacy bridge core reads `server_bridge.is_some()` directly.
    pub fn is_online(&self) -> bool {
        match &self.client_kernel {
            Some(k) => k.is_online(),
            None => self.server_bridge.is_some(),
        }
    }

    /// The active orbital's name (server addressing).
    pub fn orbital_name(&self) -> String {
        self.schema
            .orbitals
            .get(self.current_orbital_idx)
            .map(|o| o.name.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    }

    /// Trait names of the active orbital (for the per-trait INIT sweep).
    pub fn current_orbital_traits(&self) -> Vec<String> {
        self.schema
            .orbitals
            .get(self.current_orbital_idx)
            .map(|o| {
                o.traits
                    .iter()
                    .filter_map(|t| t.trait_name().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Index of the orbital that owns `trait_name` (for switching the active orbital to a
    /// trait being driven in isolation, e.g. by the screenshot frame-walker).
    pub fn orbital_index_of_trait(&self, trait_name: &str) -> Option<usize> {
        self.schema
            .orbitals
            .iter()
            .position(|o| o.traits.iter().any(|t| t.trait_name() == Some(trait_name)))
    }

    /// Fire `INIT` for every trait of the active orbital, synchronously (no thread).
    /// Used by `GameApp` and the Layer-2 verifier.
    pub fn init_all_traits_blocking(&mut self) {
        let traits = self.current_orbital_traits();
        self.mount_traits_blocking(&traits, Value::Null);
    }

    fn mount_traits_blocking(&mut self, traits: &[String], payload: Value) {
        if traits.is_empty() {
            self.drive_blocking("INIT", None, payload);
            return;
        }
        if let Some(client_kernel) = self.client_kernel.clone() {
            client_kernel.remount(traits.iter().cloned());
            let seeds: Vec<crate::effects::response::MountSeed> = traits
                .iter()
                .map(|t| crate::effects::response::MountSeed {
                    r#trait: t.clone(),
                    event: crate::effects::response::LifecycleEvent::Init,
                })
                .collect();
            let settled = client_kernel
                .begin_mount(&seeds, payload)
                .and_then(|begun| {
                    for (trait_name, local) in begun.locals() {
                        self.process_response("INIT", Some(trait_name.to_string()), local.clone());
                    }
                    client_kernel.settle_mount(&begun)
                });
            match settled {
                Ok(response) => self.process_response("INIT", None, response),
                Err(e) => {
                    tracing::error!(target: "almadar::client", error = %e, "mount refused");
                    self.status_message = Some((format!("Error: {e}"), true));
                }
            }
            self.mount_entered_page();
            return;
        }
        for trait_name in traits {
            self.drive_blocking("INIT", Some(trait_name), payload.clone());
        }
    }

    /// Mount the page a `navigate` just entered.
    fn mount_entered_page(&mut self) {
        if std::mem::take(&mut self.page_entered) {
            self.mount_page_blocking();
        }
    }

    /// The traits a page mount runs: the current page's, in its declared order (the orbital's
    /// every trait when it declares no pages).
    pub fn page_mount_traits(&self) -> Vec<String> {
        let page = self.current_page.as_deref().and_then(|name| {
            pages_of(&self.schema, self.current_orbital_idx).find(|p| p.name == name)
        });
        match page {
            Some(def) => def
                .traits
                .iter()
                .flatten()
                .filter_map(|t| t.trait_name().map(str::to_string))
                .collect(),
            None => self.current_orbital_traits(),
        }
    }

    /// The INIT payload of a page mount: its route params (`null` when it has none).
    pub fn page_mount_payload(&self) -> Value {
        if self.route_params.is_empty() {
            Value::Null
        } else {
            Value::object(self.route_params.clone())
        }
    }

    /// Mount the current page: its traits' INIT in one round trip, with the route params.
    pub fn mount_page_blocking(&mut self) {
        let traits = self.page_mount_traits();
        let payload = self.page_mount_payload();
        self.mount_traits_blocking(&traits, payload);
    }

    /// Dispatch a control's event as the trait whose content rendered it (`source`), or the
    /// active trait when the content carries none.
    pub fn dispatch_control(&mut self, event: &str, source: Option<&str>, payload: Value) {
        let origin = source
            .map(str::to_string)
            .unwrap_or_else(|| self.current_trait_name.clone());
        self.drive_blocking(event, Some(&origin), payload);
    }

    /// What a host paints in `slot`: `main` holds the current page's writers only.
    pub fn slot_contents(&self, slot: &str) -> Vec<SlotContent> {
        if slot == "main" {
            return self.page_main_contents();
        }
        self.ui_slots.get(slot).cloned().unwrap_or_default()
    }

    /// Drain the emitted-event queue, driving each follow-up through the bridge until
    /// quiescent (or `max_rounds` is hit). Mirrors the desktop frame loop's per-frame
    /// `pending_events.drain(..)` pump, headlessly — so a trait that emits a load-complete
    /// event on INIT actually advances from its loading state to its loaded state (where
    /// the data table renders), instead of being frozen on the spinner. The round cap
    /// prevents a runaway emit cycle from spinning forever.
    pub fn settle(&mut self, max_rounds: usize) {
        for _ in 0..max_rounds {
            if self.pending_events.is_empty() {
                return;
            }
            for (event, origin_trait, payload) in std::mem::take(&mut self.pending_events) {
                self.drive_blocking(&event, origin_trait.as_deref(), payload);
            }
        }
    }

    /// Drive one event synchronously through the client kernel (preferred) or the
    /// legacy bridge, and process the response.
    pub fn drive_blocking(&mut self, event: &str, trait_name: Option<&str>, payload: Value) {
        if let Some(client_kernel) = self.client_kernel.clone() {
            // `ClientKernel::dispatch` requires an explicit trait — fall back to
            // the active trait, mirroring the bridge path's server-side default.
            let target_trait = trait_name
                .map(str::to_string)
                .unwrap_or_else(|| self.current_trait_name.clone());
            let entity_id = extract_entity_id(&payload);
            tracing::debug!(
                target: "almadar::client",
                event = %event, trait = %target_trait, entity_id = ?entity_id,
                "drive_blocking: client_kernel dispatch"
            );
            match client_kernel.dispatch(&target_trait, entity_id.as_deref(), event, payload) {
                Ok(response) => {
                    self.process_response(event, trait_name.map(|s| s.to_string()), response);
                    self.mount_entered_page();
                }
                Err(e) => {
                    tracing::error!(target: "almadar::client", event = %event, trait = %target_trait, error = %e, "drive_blocking: dispatch refused");
                    self.status_message = Some((format!("Error: {e}"), true));
                }
            }
            return;
        }

        let Some(bridge) = self.server_bridge.clone() else {
            tracing::debug!(target: "almadar::client", event = %event, "drive_blocking: offline (no bridge)");
            self.handle_offline_event(event);
            return;
        };
        let orbital_name = self.orbital_name();
        let entity_id = extract_entity_id(&payload);
        tracing::debug!(
            target: "almadar::client",
            orbital = %orbital_name,
            event = %event,
            trait = ?trait_name,
            entity_id = ?entity_id,
            "drive_blocking: dispatch"
        );
        if let Ok(response) = bridge.send_event_with_options(
            &orbital_name,
            event,
            entity_id.as_deref(),
            trait_name,
            payload,
        ) {
            tracing::debug!(
                target: "almadar::client",
                event = %event,
                success = response.success,
                client_effects = response.client_effects.as_ref().map_or(0, |e| e.len()),
                "drive_blocking: response"
            );
            self.process_response(event, trait_name.map(|s| s.to_string()), response);
            tracing::debug!(
                target: "almadar::client",
                event = %event,
                state = %self.current_state,
                ui_slots = ?self.ui_slots.keys().collect::<Vec<_>>(),
                trait_content = ?self.trait_content.keys().collect::<Vec<_>>(),
                "drive_blocking: applied"
            );
        }
    }

    /// Fire one scheduled tick of `trait_name` through the client kernel (preferred)
    /// or the legacy bridge, and process the response, so the tick's `set` effects
    /// advance the in-memory entity (and any render-ui it emits re-renders). Same
    /// circuit as `drive_blocking`, but the canonical `process_tick` path.
    pub fn drive_tick_blocking(&mut self, trait_name: &str, tick_name: &str) {
        if let Some(client_kernel) = self.client_kernel.clone() {
            if let Ok(response) = client_kernel.tick(trait_name, tick_name, None) {
                self.process_response(tick_name, Some(trait_name.to_string()), response);
            }
            return;
        }

        let Some(bridge) = self.server_bridge.clone() else {
            return;
        };
        let orbital_name = self.orbital_name();
        if let Ok(response) = bridge.tick(&orbital_name, trait_name, tick_name, None) {
            self.process_response(tick_name, Some(trait_name.to_string()), response);
        }
    }

    /// Read-only `(trait_name, tick_name, interval_ms)` schedule for every tick declared on
    /// the active orbital's traits. `interval_ms` = `None` means a per-frame tick. Sourced
    /// from the resolved schema (`TraitDefinition.ticks`) the client already holds — the
    /// scheduler needs no extra round trip.
    pub fn tick_schedule(&self) -> Vec<(String, String, Option<u64>)> {
        use orbital_core_base::schema::types::TraitReference;
        let Some(orbital) = self.schema.orbitals.get(self.current_orbital_idx) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for tr in &orbital.traits {
            if let TraitReference::Inline(td) = tr {
                if let Some(ticks) = &td.ticks {
                    for tick in ticks {
                        out.push((td.name.clone(), tick.name.clone(), tick.interval_ms()));
                    }
                }
            }
        }
        out
    }

    /// Drive and display `trait_name` (its orbital and page), clearing what was shown.
    pub fn set_active_trait(&mut self, trait_name: &str) {
        if let Some(idx) = self.orbital_index_of_trait(trait_name) {
            self.current_orbital_idx = idx;
        }
        self.current_trait_name = trait_name.to_string();
        self.show_page_of(trait_name);
        self.ui_slots.clear();
        self.trait_content.clear();
    }

    /// Switch the active orbital (caller re-inits to repopulate).
    pub fn set_orbital(&mut self, idx: usize) {
        if let Some(orbital) = self.schema.orbitals.get(idx) {
            self.current_orbital_idx = idx;
            self.current_trait_name = orbital
                .traits
                .first()
                .and_then(|t| t.trait_name())
                .unwrap_or("Unknown")
                .to_string();
            self.ui_slots.clear();
            self.trait_content.clear();
            self.current_page = initial_page(&self.schema, idx);
            self.route_params.clear();
        }
    }

    /// The current page's `main` writers, in first-write order (every writer when the orbital
    /// declares no pages).
    pub fn page_main_contents(&self) -> Vec<SlotContent> {
        let page_traits = self.page_traits();
        self.ui_slots
            .get("main")
            .into_iter()
            .flatten()
            .filter(|c| match (&page_traits, c.source_trait.as_deref()) {
                (Some(traits), Some(src)) => traits.contains(src),
                (Some(_), None) => false,
                (None, _) => true,
            })
            .cloned()
            .collect()
    }

    /// The current page's trait names, or `None` when there is no page to scope by.
    pub fn page_traits(&self) -> Option<std::collections::HashSet<String>> {
        let page = self.current_page.as_deref()?;
        let def = pages_of(&self.schema, self.current_orbital_idx).find(|p| p.name == page)?;
        Some(
            def.traits
                .iter()
                .flatten()
                .filter_map(|t| t.trait_name().map(str::to_string))
                .collect(),
        )
    }

    /// Show the current orbital's first page that holds `trait_name` (unchanged when none does).
    pub fn show_page_of(&mut self, trait_name: &str) {
        let page = pages_of(&self.schema, self.current_orbital_idx)
            .find(|p| {
                p.traits
                    .iter()
                    .flatten()
                    .any(|t| t.trait_name() == Some(trait_name))
            })
            .map(|p| p.name.clone());
        if page.is_some() {
            self.current_page = page;
        }
    }

    /// Move to the page whose path matches `path` (`:param` segments match any value), in this
    /// orbital first, then any other, with its route params merged under `params` (explicit wins,
    /// as the web router merges them); an unknown path keeps the page and says so.
    pub fn navigate_to(&mut self, path: &str, params: Option<&Value>) {
        let here = pages_of(&self.schema, self.current_orbital_idx).find_map(|p| {
            route_params(&p.path, path).map(|r| (self.current_orbital_idx, p.name.clone(), r))
        });
        let found = here.or_else(|| {
            (0..self.schema.orbitals.len()).find_map(|idx| {
                pages_of(&self.schema, idx)
                    .find_map(|p| route_params(&p.path, path).map(|r| (idx, p.name.clone(), r)))
            })
        });
        let Some((idx, page, mut route)) = found else {
            tracing::warn!(target: "almadar::client", path = %path, "navigate: no page matches");
            self.status_message = Some((format!("No page at {path}"), true));
            return;
        };
        if let Some(Value::Object(explicit)) = params {
            route.extend(explicit.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        self.page_history.push(PageVisit {
            orbital_idx: self.current_orbital_idx,
            page: self.current_page.clone(),
            params: std::mem::take(&mut self.route_params),
        });
        self.enter_page(idx, Some(page), route);
    }

    /// Return to the previous page and its params (`navigate-back`); with no history the page stays.
    pub fn navigate_back(&mut self) {
        let Some(visit) = self.page_history.pop() else {
            tracing::warn!(target: "almadar::client", "navigate-back: no previous page");
            self.status_message = Some(("No previous page".to_string(), true));
            return;
        };
        self.enter_page(visit.orbital_idx, visit.page, visit.params);
    }

    fn enter_page(&mut self, idx: usize, page: Option<String>, params: ValueMap) {
        if idx != self.current_orbital_idx {
            self.set_orbital(idx);
        }
        self.current_page = page;
        self.route_params = params;
        self.page_entered = true;
    }

    /// Minimal offline handling (no bridge): keep the circuit coherent without a server.
    pub fn handle_offline_event(&mut self, event: &str) {
        match event {
            "INIT" => self.ui_slots.clear(),
            "CANCEL" => {
                self.form_data.clear();
                self.editing_entity = None;
            }
            _ => {}
        }
    }

    /// The kernel's `canvasTheme`, when this core is driven by a [`ClientKernel`].
    pub fn canvas_theme(&self) -> Option<serde_json::Value> {
        self.client_kernel
            .as_ref()
            .map(|kernel| kernel.canvas_theme())
    }

    /// The compiler's predictions (`crate::ir::await_render_rows_from_resolved`); a
    /// portal slot never opens for a skeleton, as on the web.
    pub fn set_await_renders(&mut self, rows: Vec<orbital_core_base::ir::types::AwaitRenderRow>) {
        self.await_rows = rows
            .into_iter()
            .filter(|r| !orbital_core_base::slots::is_portal_slot(&r.slot))
            .collect();
    }

    /// The theme's busy delay, or `None` when it turns awaiting skeletons off.
    pub fn set_awaiting_motion(&mut self, delay: Option<std::time::Duration>) {
        self.awaiting_motion = delay;
    }

    /// Raise the predictions for a dispatch whose server legs are in flight; `None` when the
    /// theme shows no skeletons or nothing is predicted. End it with [`Self::end_awaiting`].
    pub fn begin_awaiting(
        &mut self,
        awaiting: &[crate::kernel::AwaitingTrait],
        now: web_time::Instant,
    ) -> Option<u64> {
        self.awaiting_motion?;
        let renders: Vec<_> = self
            .await_rows
            .iter()
            .filter(|row| {
                awaiting.iter().any(|a| {
                    row.request_trait == a.trait_name
                        && row.event == a.event
                        && row.from.as_ref().is_none_or(|from| from.contains(&a.from))
                })
            })
            .map(|row| orbital_core_base::ir::types::AwaitRender {
                trait_name: row.trait_name.clone(),
                slot: row.slot.clone(),
                skeleton: row.skeleton.clone(),
            })
            .collect();
        if renders.is_empty() {
            return None;
        }
        self.next_await_token += 1;
        self.awaiting.push(AwaitingEntry {
            token: self.next_await_token,
            since: now,
            renders,
        });
        Some(self.next_await_token)
    }

    pub fn end_awaiting(&mut self, token: u64) {
        self.awaiting.retain(|entry| entry.token != token);
    }

    fn visible_awaiting(
        &self,
        now: web_time::Instant,
    ) -> impl Iterator<Item = &orbital_core_base::ir::types::AwaitRender> {
        let delay = self.awaiting_motion.unwrap_or_default();
        self.awaiting
            .iter()
            .filter(move |entry| now.duration_since(entry.since) >= delay)
            .flat_map(|entry| entry.renders.iter())
    }

    fn skeleton_content(&self, render: &orbital_core_base::ir::types::AwaitRender) -> Option<SlotContent> {
        let node = serde_json::json!({ "type": "skeleton", "spec": render.skeleton });
        let mut content = self.parse_pattern(&serde_json::from_value(node).ok()?)?;
        content.source_trait = Some(render.trait_name.clone());
        Some(content)
    }

    /// Skeletons standing in for the `main` writers still awaiting their first content.
    pub fn awaiting_main_contents(&self, now: web_time::Instant) -> Vec<SlotContent> {
        let written: Vec<&str> = self
            .ui_slots
            .get("main")
            .map(|c| c.iter().filter_map(|c| c.source_trait.as_deref()).collect())
            .unwrap_or_default();
        let page_traits = self.page_traits();
        let mut out: Vec<SlotContent> = Vec::new();
        for render in self.visible_awaiting(now) {
            let fresh = render.slot == "main"
                && page_traits
                    .as_ref()
                    .is_none_or(|t| t.contains(&render.trait_name))
                && !self.embedded.contains(&render.trait_name)
                && !written.contains(&render.trait_name.as_str())
                && !out
                    .iter()
                    .any(|c| c.source_trait.as_deref() == Some(render.trait_name.as_str()));
            if fresh {
                out.extend(self.skeleton_content(render));
            }
        }
        out
    }

    /// `trait_content` with a skeleton for an embedded trait that awaits its first content (its
    /// host's `@trait.X` frame shows it).
    pub fn trait_content_with_awaiting(&self, now: web_time::Instant) -> ValueMap {
        let mut content = self.trait_content.clone();
        for render in self.visible_awaiting(now) {
            if self.embedded.contains(&render.trait_name)
                && !content.contains_key(&render.trait_name)
            {
                if let Some(skeleton) = self.skeleton_content(render) {
                    content.insert(render.trait_name.clone(), skeleton.props);
                }
            }
        }
        content
    }

    /// Every empty non-`main`, non-portal slot with a visible prediction, with its skeleton.
    pub fn awaiting_slot_contents(&self, now: web_time::Instant) -> Vec<(String, SlotContent)> {
        let mut slots: Vec<String> = self.visible_awaiting(now).map(|r| r.slot.clone()).collect();
        slots.sort();
        slots.dedup();
        slots
            .into_iter()
            .filter_map(|slot| {
                self.awaiting_slot_content(&slot, now)
                    .map(|content| (slot, content))
            })
            .collect()
    }

    /// Whether any dispatch still awaits its server legs (the frame loop keeps repainting so a
    /// skeleton appears once the busy delay passes).
    pub fn has_awaiting(&self) -> bool {
        !self.awaiting.is_empty()
    }

    /// A skeleton for a non-`main`, non-portal slot that is still empty.
    pub fn awaiting_slot_content(&self, slot: &str, now: web_time::Instant) -> Option<SlotContent> {
        if slot == "main" || self.ui_slots.get(slot).is_some_and(|c| !c.is_empty()) {
            return None;
        }
        let render = self
            .visible_awaiting(now)
            .find(|r| r.slot == slot && !self.embedded.contains(&r.trait_name))?;
        self.skeleton_content(render)
    }

    /// `src`'s `main` render: replaces its earlier one in place (first-write order, as web
    /// `useUISlots.aggregateSlot` and the native shells stack them), appends a first one, and
    /// a `None` (a `null` render) clears only `src`'s.
    fn write_main(&mut self, src: &str, component: Option<SlotContent>) {
        let main = self.ui_slots.entry("main".to_string()).or_default();
        let at = main
            .iter()
            .position(|c| c.source_trait.as_deref() == Some(src));
        match (component, at) {
            (Some(mut component), Some(i)) => {
                component.source_trait = Some(src.to_string());
                main[i] = component;
            }
            (Some(mut component), None) => {
                component.source_trait = Some(src.to_string());
                main.push(component);
            }
            (None, Some(i)) => {
                main.remove(i);
            }
            (None, None) => {}
        }
        if main.is_empty() {
            self.ui_slots.remove("main");
        }
    }

    /// Process a server response into state/entities/slots. `req_trait` = the trait the
    /// event was fired for (`None` = the active trait). A background per-trait INIT for
    /// `@trait` indexing must not disturb the active trait's view, but still completes
    /// its closed circuit (re-queued emits).
    pub fn process_response(
        &mut self,
        event: &str,
        req_trait: Option<String>,
        response: EventResponse,
    ) {
        if !response.success {
            self.status_message = Some((
                response
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string()),
                true,
            ));
            return;
        }

        let effective_trait = req_trait.unwrap_or_else(|| self.current_trait_name.clone());
        let active = self.current_trait_name.clone();

        for (entity_type, entities) in response.data.iter().flatten() {
            debug!(entity_type = %entity_type, count = entities.len(), "Caching entities");
            self.entities.insert(entity_type.clone(), entities.clone());
        }

        // The trait that produced each client effect — `clientEffectsByTrait` is the ONE
        // spelling of source attribution (§156: no more `content.sourceTrait` stamping).
        // Falls back to the request's trait when the response carries no by-trait array
        // (an old/offline bridge, or a response with no client effects at all).
        let paired: Vec<(String, ClientEffect)> = match &response.client_effects_by_trait {
            Some(by_trait) => by_trait
                .iter()
                .map(|e| (e.trait_name.clone(), e.effect.clone()))
                .collect(),
            None => response
                .client_effects
                .iter()
                .flatten()
                .map(|e| (effective_trait.clone(), e.clone()))
                .collect(),
        };

        // Every trait writes its own slots; an embedded trait never writes `main` (its host
        // splices it in via `@trait.X`).
        let embedded = self.embedded.clone();
        let is_main = |slot: &str| orbital_core_base::slots::slot_role(slot) == orbital_core_base::slots::SlotRole::Main;
        let writes = |slot: &str, src: &str| !is_main(slot) || !embedded.contains(src);

        // Index every render-ui's (config+props merged) content under its SOURCE trait so
        // `@trait.X` composition resolves each peer's current view — including peers that
        // render only to an overlay slot (e.g. a popover). A `main` render-ui always wins
        // over an overlay one for the same source, so a peer's overlay content never
        // shadows its primary view.
        for (src, effect) in &paired {
            if let ClientEffect::RenderUi { slot, config, .. } = effect {
                if config.is_none() {
                    if is_main(slot) {
                        self.trait_content.shift_remove(src);
                    }
                } else if let Some(content) = effect.render_ui_value() {
                    if is_main(slot) {
                        self.trait_content.insert(src.clone(), content);
                    } else {
                        self.trait_content.entry(src.clone()).or_insert(content);
                    }
                }
            }
        }

        // Advance the displayed state from the whole-orbital snapshot only when the
        // ACTIVE trait is present in it (a background per-trait INIT elsewhere must not
        // clobber the view the user is looking at).
        if let Some(new_state) = response.states.get(&active) {
            self.current_state = new_state.clone();
        }

        // Track the edited entity for form pre-population.
        if event == "EDIT" || event == "VIEW" {
            for (_entity_type, entities) in response.data.iter().flatten() {
                if entities.len() == 1 {
                    self.editing_entity = Some(entities[0].clone());
                    if let Some(obj) = entities[0].as_object() {
                        self.form_data.clear();
                        for (key, value) in obj {
                            self.form_data.insert(key.clone(), format_value(value));
                        }
                    }
                    break;
                }
            }
        }
        if event == "CANCEL" || event == "SAVE" || event == "INIT" {
            self.editing_entity = None;
        }

        // Clear the non-`main` slots about to be re-rendered; `main` is per source (below).
        // A `null` config leaves the slot cleared (parse_pattern → None below).
        let to_clear: std::collections::HashSet<String> = paired
            .iter()
            .filter_map(|(src, effect)| match effect {
                ClientEffect::RenderUi { slot, .. } if writes(slot, src) && !is_main(slot) => {
                    Some(slot.clone())
                }
                _ => None,
            })
            .collect();
        for slot in &to_clear {
            self.ui_slots.remove(slot);
        }

        for (src, effect) in paired {
            let rendered = effect.render_ui_value();
            match effect {
                ClientEffect::RenderUi { slot, .. } => {
                    if !writes(&slot, &src) {
                        continue; // embedded `main`: spliced by its host
                    }
                    if is_main(&slot) {
                        let component = rendered.and_then(|content| self.parse_pattern(&content));
                        self.write_main(&src, component);
                        continue;
                    }
                    if let Some(content) = rendered {
                        if let Some(mut component) = self.parse_pattern(&content) {
                            component.source_trait = Some(src);
                            self.ui_slots.entry(slot).or_default().push(component);
                        }
                    }
                }
                other => self.execute_client_effect(other, &src),
            }
        }

        // `emit` no longer appears in `clientEffects` (§156) — it surfaces as
        // `emittedEvents`, each carrying its own producing trait via `source.trait`, so
        // a cascade's follow-up re-dispatches to the trait that actually emitted it
        // (falling back to this response's trait only when unattributed).
        for emitted in response.emitted_events {
            let origin = emitted
                .source
                .and_then(|s| s.r#trait)
                .unwrap_or_else(|| effective_trait.clone());
            self.pending_events.push((
                emitted.event,
                Some(origin),
                emitted.payload.unwrap_or(Value::Null),
            ));
        }

        self.status_message = None;
    }

    /// Execute a non-render client-side effect (navigate/navigate-back → status).
    /// `origin_trait` is the trait this effect came from. `render-ui` is handled inline
    /// by [`process_response`] (it needs the per-slot suppression rule); this covers the
    /// two navigation variants, and stays defensive for a direct `RenderUi` call.
    pub fn execute_client_effect(&mut self, effect: ClientEffect, origin_trait: &str) {
        match &effect {
            ClientEffect::RenderUi { slot, .. } => {
                let slot = slot.clone();
                if let Some(content) = effect.render_ui_value() {
                    if let Some(mut component) = self.parse_pattern(&content) {
                        component.source_trait = Some(origin_trait.to_string());
                        self.ui_slots.entry(slot).or_default().push(component);
                    }
                }
            }
            ClientEffect::Navigate { path, params, .. } => {
                debug!(path = %path, "Navigate effect");
                self.navigate_to(path, params.as_ref());
            }
            ClientEffect::NavigateBack => {
                debug!("Navigate-back effect");
                self.navigate_back();
            }
        }
    }

    /// Parse a render-ui pattern into a [`SlotContent`] (registry provides a fallback).
    pub fn parse_pattern(&self, pattern: &Value) -> Option<SlotContent> {
        let pattern_type = get_string(pattern, "type")?;
        Some(SlotContent {
            pattern: pattern_type,
            props: pattern.clone(),
            entity: None,
            source_trait: None,
        })
    }
}

/// The inline pages of orbital `idx` (a resolved schema inlines them).
fn pages_of(
    schema: &OrbitalSchema,
    idx: usize,
) -> impl Iterator<Item = &orbital_core_base::schema::types::PageDefinition> {
    schema
        .orbitals
        .get(idx)
        .and_then(|o| o.pages.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|p| match p {
            orbital_core_base::schema::types::PageRef::Inline(def) => Some(def),
            _ => None,
        })
}

/// The page an orbital opens on: the one marked initial, else its first.
fn initial_page(schema: &OrbitalSchema, idx: usize) -> Option<String> {
    let mut pages = pages_of(schema, idx).peekable();
    let first = pages.peek().map(|p| p.name.clone());
    pages
        .find(|p| p.is_initial == Some(true))
        .map(|p| p.name.clone())
        .or(first)
}

/// The page a visit was on, for `navigate-back`.
#[derive(Debug, Clone)]
struct PageVisit {
    orbital_idx: usize,
    page: Option<String>,
    params: ValueMap,
}

/// A route pattern matched against a path segment by segment: a `:param` segment matches any
/// value and binds it. `None` when the path is not this route.
fn route_params(pattern: &str, path: &str) -> Option<ValueMap> {
    let route: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let actual: Vec<&str> = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/')
        .split('/')
        .collect();
    if route.len() != actual.len() {
        return None;
    }
    let mut params = ValueMap::default();
    for (r, a) in route.iter().zip(&actual) {
        match r.strip_prefix(':') {
            Some(name) => {
                params.insert(name.to_string(), Value::string((*a).to_string()));
            }
            None if r == a => {}
            None => return None,
        }
    }
    Some(params)
}

/// Every trait some host embeds (orbital-core [`EmbedIndex`] over the schema's inline traits).
fn embedded_traits(schema: &OrbitalSchema) -> std::collections::HashSet<String> {
    use orbital_core_base::schema::types::TraitReference;
    let defs: HashMap<String, orbital_core_base::schema::types::TraitDefinition> = schema
        .orbitals
        .iter()
        .flat_map(|orbital| orbital.traits.iter())
        .filter_map(|t| match t {
            TraitReference::Inline(def) => Some((def.name.clone(), def.clone())),
            _ => None,
        })
        .collect();
    let index = crate::embed_index::EmbedIndex::build(&defs);
    defs.into_keys()
        .filter(|name| !index.ancestors(name).is_empty())
        .collect()
}

#[cfg(test)]
mod tests;
