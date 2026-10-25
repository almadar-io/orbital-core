//! Client-role kernel: one in-process `RuntimeKernel` (Client env) + the ONE
//! `ServerLegCollector` (`KernelConfig.delegate`) + an optional `ServerBridge` that
//! posts the resulting server leg and folds the response back.
//!
//! This is the ONE dispatch path every client-role orbital-client caller drives
//! through (Program B, wave B4, `groovy-inventing-cloud.md`) — `ClientCore`'s
//! `drive_blocking`/`settle` when constructed with [`ClientKernel::new`]
//! (`ClientCore::with_client_kernel`), and the desktop/tui frame loops that build
//! on `ClientCore`. `KernelBridge` (`bridge.rs`, Server env) stays the separate
//! in-process FULL-dispatch path for headless verification and the wasm preview —
//! see the module doc there for why it is not routed through this kernel.
//!
//! Mode semantics (the ONE rule, `crate::DispatchMode::compute`, the trait's declared `dispatch`):
//! - `HybridClientOnly` (`[client]` trait): the seed's own effects never post (`orbital-core`
//!   drops a bug-case leg and logs an error). A leg that remains belongs to a downstream
//!   listener that needs the server, and is posted and folded like `PersistedAwaited`.
//! - `RuntimeOptimistic` (`[optimistic]` trait): the local commit already
//!   happened inside `dispatch_with_server_leg`. The leg (if any server-only
//!   effect fired anyway, e.g. a `call-service` sync call) is posted; on a
//!   `success:false` or transport error the pre-dispatch snapshot is restored.
//! - `PersistedAwaited` (everything else): the leg is posted and its response is
//!   folded (`apply_server_response`) BEFORE this call returns — but the response
//!   handed back to the caller is still the local one (`dispatch.response`); the
//!   fold's job is to land server-assigned data (ids, `entityByTrait`) into the
//!   entity source for the NEXT render, not to replace what this call presents.
//!
//! `dispatch` posts synchronously (`ServerBridge` is a sync trait — `send_request`
//! blocks the calling thread). `ClientCore::drive_blocking`/`GameApp` call it
//! straight from the UI thread (already synchronous, matching the pre-existing
//! design); `DesktopApp`'s separate *threaded* send path (`rendering/desktop.rs`)
//! still owns the backgrounding — it now calls `dispatch` from its worker thread
//! instead of `ServerBridge::send_event_with_options`, so posting still never
//! blocks the egui frame loop.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::client::bridge::{seed_into, ServerBridge};
use crate::client::http::BridgeError;
use crate::embed::EmbeddedKernel;
use crate::kernel::{already_delivered_from, ClientDispatch, TraitSnapshot};
use orbital_core_base::dispatch_mode::DispatchMode;
use crate::effects::response::EventResponse;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_base::value::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Monotonic per-process counter — good enough for a client id that only needs to
/// be unique within one running client (no cross-process/cross-restart identity
/// requirement), without pulling a UUID dependency into this wave.
fn next_client_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("client-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// In-process client-role kernel: Client env, the one `ServerLegCollector`
/// delegate, and an optional transport for the server leg.
pub struct ClientKernel {
    embedded: EmbeddedKernel,
    bridge: Option<Arc<dyn ServerBridge>>,
}

/// A dispatch whose local cascade ran ([`ClientKernel::begin`]) and whose server legs, if any,
/// are still to be posted ([`ClientKernel::settle`]).
#[derive(Debug, Clone)]
pub struct BegunDispatch {
    trait_name: String,
    entity_id: Option<String>,
    event: String,
    /// The local response, ready to paint.
    pub local: EventResponse,
    dispatch: ClientDispatch,
}

impl BegunDispatch {
    /// The traits awaiting this dispatch's server legs (empty when nothing will be posted).
    pub fn awaiting(&self) -> &[crate::kernel::AwaitingTrait] {
        &self.dispatch.awaiting
    }

    /// Whether [`ClientKernel::settle`] has anything to post.
    pub fn has_legs(&self) -> bool {
        !self.dispatch.server_legs.is_empty()
    }
}

/// A mount batch whose seeds' local cascades ran ([`ClientKernel::begin_mount`]); the one
/// `mount` leg per owning orbital is still to be posted ([`ClientKernel::settle_mount`]).
#[derive(Debug, Clone)]
pub struct BegunMount {
    seeds: Vec<(crate::effects::response::MountSeed, BegunDispatch)>,
}

impl BegunMount {
    /// Each seed's trait and local response, in seed order, ready to paint.
    pub fn locals(&self) -> impl Iterator<Item = (&str, &EventResponse)> {
        self.seeds
            .iter()
            .map(|(seed, begun)| (seed.r#trait.as_str(), &begun.local))
    }

    /// The traits awaiting the mount legs.
    pub fn awaiting(&self) -> impl Iterator<Item = &crate::kernel::AwaitingTrait> {
        self.seeds.iter().flat_map(|(_, begun)| begun.awaiting())
    }

    /// Whether [`ClientKernel::settle_mount`] has anything to post.
    pub fn has_legs(&self) -> bool {
        self.seeds.iter().any(|(_, begun)| begun.has_legs())
    }
}

impl ClientKernel {
    /// Register `schema` in a fresh Client-env kernel, delegate wired to the one
    /// `ServerLegCollector`. `bridge = None` runs fully offline — a produced leg
    /// is dropped (warned), and every dispatch resolves from the local outcome
    /// alone (the same shape headless/verify runs need).
    pub fn new(
        schema: OrbitalSchema,
        bridge: Option<Arc<dyn ServerBridge>>,
    ) -> Result<Self, BridgeError> {
        let embedded = EmbeddedKernel::new(schema, next_client_id()).map_err(BridgeError::Server)?;
        Ok(Self { embedded, bridge })
    }

    /// Seed entity rows so a subsequent INIT `fetch`/`list` on `entity_type` returns
    /// them (mirrors `KernelBridge::seed`, sharing the same [`seed_into`] body).
    /// A row-set reference resolved by the kernel's one resolver ([`crate::runtime::RuntimeKernel::resolve_rows`]).
    pub fn resolve_rows(
        &self,
        reference: &orbital_core_base::row_set::RowSetRef,
    ) -> Result<crate::row_query::RowPage, String> {
        self.embedded.resolve_rows(reference)
    }

    pub fn seed(&self, entity_type: &str, rows: Vec<Value>) {
        seed_into(self.embedded.entities().as_ref(), entity_type, &rows);
    }

    /// Bind `@user` for every subsequent dispatch.
    pub fn set_current_user(&self, user: Value) {
        self.embedded.kernel().set_current_user(user);
    }

    /// Set the client's tracked light/dark toggle for every subsequent
    /// `@currentTheme` resolution (§157/C7 `theme-declaration`: `KernelBridge`
    /// already exposes `RuntimeKernel::set_theme_mode` directly — this is the
    /// same passthrough for the `ClientKernel` role, which native shells drive
    /// through `OrbitalKernelBridge`). Never call this to force a default; leave
    /// the toggle unset to let a declared theme key's own `-light`/`-dark`
    /// suffix win.
    pub fn set_theme_mode(&self, mode: orbital_core_base::theme::ThemeMode) {
        self.embedded.kernel().set_theme_mode(mode);
    }

    /// The resolved theme's `canvasTheme` (orbital-core `canvas_theme`): what the Rust-drawn scenes
    /// paint with, as the mobile hosts read it from the kernel snapshot.
    pub fn canvas_theme(&self) -> serde_json::Value {
        let theme = self.embedded.kernel().resolved_theme();
        serde_json::to_value(crate::canvas_theme::canvas_theme(
            &theme.tokens,
        ))
        .unwrap_or_default()
    }

    /// The theme's busy delay for awaiting skeletons, or `None` when the theme leaves them off
    /// (`motion.awaitingSkeleton`, default off as on the web).
    pub fn awaiting_motion(&self) -> Option<std::time::Duration> {
        let theme = self.embedded.kernel().resolved_theme();
        let motion = theme.tokens.motion.as_ref();
        if !motion.and_then(|m| m.awaiting_skeleton).unwrap_or(false) {
            return None;
        }
        let ms = motion
            .and_then(|m| m.busy_delay_ms())
            .unwrap_or(orbital_core_base::schema::types::BASE_BUSY_DELAY_MS);
        Some(std::time::Duration::from_millis(ms))
    }

    /// Whether a transport is attached — `false` runs fully offline (every
    /// server leg is dropped with a warn log; see [`Self::dispatch`]). UI
    /// "Connected"/"Offline" indicators read this instead of reaching into a
    /// `server_bridge` field that a `ClientKernel`-backed core no longer has.
    pub fn is_online(&self) -> bool {
        self.bridge.is_some()
    }

    /// Run one scheduled tick through `RuntimeKernel::dispatch_tick` (the seam `orbital-ffi`'s
    /// frame uses): the tick's own effects, then each `emit` it ran fanned through its
    /// listeners with the same post/fold as [`Self::dispatch`], one leg at a time to the
    /// orbital owning the leg's target trait. The tick's own server effects are discarded
    /// (a tick has no event to replay them under). The returned response merges the tick's
    /// outcome with every fan-out outcome; its `emitted_events` are cleared because the
    /// cascade already delivered them, so the caller's pending-event queue must not replay them.
    pub fn tick(
        &self,
        trait_name: &str,
        tick_name: &str,
        entity_id: Option<&str>,
    ) -> Result<EventResponse, BridgeError> {
        let orbital = self.owning_orbital(trait_name)?;
        let ticked = self
            .embedded
            .kernel()
            .dispatch_tick(
                orbital,
                trait_name,
                entity_id,
                tick_name,
                Some(self.embedded.client_id()),
            )
            .map_err(|e| BridgeError::Server(e.to_string()))?;
        if ticked.discarded_server_effects > 0 {
            tracing::debug!(
                target: orbital_core_base::logging::ns::BRIDGE,
                trait_name = %trait_name, tick = %tick_name,
                discarded = ticked.discarded_server_effects,
                "tick's own server effects discarded",
            );
        }
        let mut outcomes = vec![(trait_name.to_string(), ticked.outcome)];
        for emit in &ticked.emits {
            outcomes.extend(emit.dispatch.outcomes.iter().cloned());
        }
        let mut settled = EventResponse::from_process_results(&outcomes, "", "");
        settled.fold_rerenders(&ticked.rerenders);
        settled.emitted_events.clear();
        for emit in &ticked.emits {
            settled =
                self.post_legs(trait_name, entity_id, &emit.event, &emit.dispatch, settled)?;
            if !settled.success {
                break;
            }
        }
        Ok(settled)
    }

    fn owning_orbital(&self, trait_name: &str) -> Result<&str, BridgeError> {
        self.embedded
            .owning_orbital(trait_name)
            .map_err(BridgeError::Server)
    }

    /// Capture a trait instance's state + entity frame for manual optimistic
    /// rollback (`dispatch` already does this internally for `RuntimeOptimistic`;
    /// exposed for a caller that needs to snapshot outside a dispatch, e.g. before
    /// a batch of local-only edits).
    pub fn snapshot_trait(&self, trait_name: &str, entity_id: Option<&str>) -> TraitSnapshot {
        self.embedded.kernel().snapshot_trait(trait_name, entity_id)
    }

    /// Roll a trait instance back to a prior [`TraitSnapshot`]. Bound traits still painting the
    /// rolled-back value repaint on the next response ([`crate::runtime::RuntimeKernel::rerender_stale`]);
    /// a failed server leg carries that repaint in the response it returns.
    pub fn restore_trait(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        snapshot: TraitSnapshot,
    ) {
        self.embedded.kernel().restore_trait(trait_name, entity_id, snapshot)
    }

    /// Dispatch one event through the seed trait's declared [`DispatchMode`] — the
    /// three-mode circuit documented on this module.
    /// Declare the page's mount before its lifecycle events fire: a routed
    /// delivery reaches none of `traits` before that trait's own INIT.
    pub fn remount<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.embedded.kernel().remount_traits(traits);
    }

    pub fn dispatch(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        payload: Value,
    ) -> Result<crate::effects::response::EventResponse, BridgeError> {
        let begun = self.begin(trait_name, entity_id, event, payload)?;
        let base = begun.local.clone();
        self.post_legs(
            &begun.trait_name,
            begun.entity_id.as_deref(),
            &begun.event,
            &begun.dispatch,
            base,
        )
    }

    /// The local half of [`Self::dispatch`]: the cascade runs in-process and its response is
    /// ready to paint; nothing is posted. Hand the result to [`Self::settle`] (off the UI thread)
    /// for the server legs.
    pub fn begin(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        payload: Value,
    ) -> Result<BegunDispatch, BridgeError> {
        let orbital = self.owning_orbital(trait_name)?;
        let dispatch = self
            .embedded
            .kernel()
            .dispatch_with_server_leg(
                orbital,
                trait_name,
                entity_id,
                event,
                payload,
                Some(self.embedded.client_id()),
            )
            .map_err(|e| BridgeError::Server(e.to_string()))?;
        let mut local = dispatch.response.clone();
        // The cascade inside `dispatch_with_server_leg` already delivered every emit to its
        // listeners; the caller's pending-event queue must not replay them to the emitter.
        local.emitted_events.clear();
        Ok(BegunDispatch {
            trait_name: trait_name.to_string(),
            entity_id: entity_id.map(str::to_string),
            event: event.to_string(),
            local,
            dispatch,
        })
    }

    /// The server half: post `begun`'s legs and fold them. The response carries only what the
    /// server added (its effect results, repaints, or the rejection), never the local response
    /// again, which the caller already painted.
    pub fn settle(&self, begun: &BegunDispatch) -> Result<EventResponse, BridgeError> {
        let added = EventResponse {
            success: true,
            ..Default::default()
        };
        self.post_legs(
            &begun.trait_name,
            begun.entity_id.as_deref(),
            &begun.event,
            &begun.dispatch,
            added,
        )
    }

    /// The local half of a mount: every seed's lifecycle event runs in-process, in order, and
    /// each local response is ready to paint before anything is posted (JS twin:
    /// `createClientKernel.dispatchMount`).
    pub fn begin_mount(
        &self,
        seeds: &[crate::effects::response::MountSeed],
        payload: Value,
    ) -> Result<BegunMount, BridgeError> {
        let seeds = seeds
            .iter()
            .map(|seed| {
                Ok((
                    seed.clone(),
                    self.begin(&seed.r#trait, None, seed.event.as_str(), payload.clone())?,
                ))
            })
            .collect::<Result<Vec<_>, BridgeError>>()?;
        Ok(BegunMount { seeds })
    }

    /// The server half of a mount: the seeds with server work are posted as ONE `mount` leg per
    /// owning orbital (the server replays them in order) and each leg is folded once. A failed
    /// leg rolls back every optimistic seed it carried. JS twin: `postMountLeg`.
    pub fn settle_mount(&self, begun: &BegunMount) -> Result<EventResponse, BridgeError> {
        let mut settled = EventResponse {
            success: true,
            ..Default::default()
        };
        let mut groups: Vec<(
            &str,
            Option<crate::effects::response::LegResidence>,
            Vec<&(crate::effects::response::MountSeed, BegunDispatch)>,
        )> = Vec::new();
        for posted in begun.seeds.iter().filter(|(_, b)| b.has_legs()) {
            let orbital = self.owning_orbital(&posted.0.r#trait)?;
            let residence = posted.1.dispatch.server_legs.first().and_then(|leg| leg.residence);
            match groups.iter_mut().find(|(o, r, _)| *o == orbital && *r == residence) {
                Some((_, _, group)) => group.push(posted),
                None => groups.push((orbital, residence, vec![posted])),
            }
        }
        if groups.is_empty() {
            return Ok(settled);
        }
        let Some(bridge) = &self.bridge else {
            tracing::warn!(
                target: orbital_core_base::logging::ns::BRIDGE,
                seeds = begun.seeds.len(),
                "offline client kernel: dropping the mount legs",
            );
            return Ok(settled);
        };
        for (orbital, _, group) in groups {
            let request = mount_request(&group, self.embedded.client_id());
            let rollback = || {
                for (seed, b) in &group {
                    if let Some(snapshot) = b.dispatch.snapshot.clone() {
                        self.embedded.kernel().restore_trait(&seed.r#trait, None, snapshot);
                    }
                }
            };
            let response = match bridge.send_request(orbital, &request) {
                Ok(response) => response,
                Err(e) => {
                    rollback();
                    return Err(e);
                }
            };
            if !response.success {
                rollback();
                let mut rejected = response;
                rejected.fold_rerenders(&self.embedded.kernel().rerender_stale());
                return Ok(rejected);
            }
            let already = group
                .iter()
                .flat_map(|(_, b)| already_delivered_from(&b.dispatch))
                .collect();
            self.embedded.kernel()
                .apply_server_response(&response, &already)
                .map_err(|e| BridgeError::Server(e.to_string()))?;
            settled = settled.with_server_effect_results(&response);
            settled.fold_rerenders(&self.embedded.kernel().rerender_stale());
        }
        Ok(settled)
    }

    /// Post `dispatch`'s server legs and fold the responses, per the dispatch's
    /// [`DispatchMode`]. Each leg goes to the orbital owning its `target_trait`. `base` is the
    /// response presented when nothing is posted; server effect results are appended to it.
    fn post_legs(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        event: &str,
        dispatch: &ClientDispatch,
        base: EventResponse,
    ) -> Result<EventResponse, BridgeError> {
        // A `local` seed never produces a leg for its OWN effects (`orbital-core` drops that
        // bug case), so a leg on a `HybridClientOnly` dispatch belongs to a downstream listener
        // that needs the server and is awaited like `PersistedAwaited`.
        if dispatch.server_legs.is_empty() {
            return Ok(base);
        }

        let Some(bridge) = &self.bridge else {
            tracing::warn!(
                target: orbital_core_base::logging::ns::BRIDGE,
                trait_name = %trait_name, event = %event, mode = ?dispatch.mode,
                "offline client kernel: dropping server legs",
            );
            return Ok(base);
        };

        let already = already_delivered_from(dispatch);
        let mut settled = base;
        let repainted =
            |settled: &mut EventResponse| settled.fold_rerenders(&self.embedded.kernel().rerender_stale());
        for leg in &dispatch.server_legs {
            let leg_orbital =
                self.owning_orbital(leg.target_trait.as_deref().unwrap_or_default())?;
            let posted = bridge.send_request(leg_orbital, leg);
            match dispatch.mode {
                DispatchMode::RuntimeOptimistic => match posted {
                    Ok(server_response) if server_response.success => {
                        self.embedded.kernel()
                            .apply_server_response(&server_response, &already)
                            .map_err(|e| BridgeError::Server(e.to_string()))?;
                        settled = settled.with_server_effect_results(&server_response);
                        repainted(&mut settled);
                    }
                    Ok(mut server_response) => {
                        if let Some(snapshot) = dispatch.snapshot.clone() {
                            self.embedded.kernel().restore_trait(trait_name, entity_id, snapshot);
                            repainted(&mut server_response);
                        }
                        return Ok(server_response);
                    }
                    Err(e) => {
                        if let Some(snapshot) = dispatch.snapshot.clone() {
                            self.embedded.kernel().restore_trait(trait_name, entity_id, snapshot);
                        }
                        return Err(e);
                    }
                },
                DispatchMode::PersistedAwaited | DispatchMode::HybridClientOnly => {
                    // Awaited: a transport/server failure here means the event this
                    // trait fundamentally depends on the server for did not happen —
                    // propagate rather than silently presenting the unconfirmed local
                    // response.
                    let server_response = posted?;
                    self.embedded.kernel()
                        .apply_server_response(&server_response, &already)
                        .map_err(|e| BridgeError::Server(e.to_string()))?;
                    settled = settled.with_server_effect_results(&server_response);
                    repainted(&mut settled);
                }
            }
        }
        Ok(settled)
    }
}

/// One `mount` leg for `group`: the seeds in order, and the union of their legs' stateless
/// addressing (`traits`, `entityByTrait`), each trait at its first occurrence.
fn mount_request(
    group: &[&(crate::effects::response::MountSeed, BegunDispatch)],
    client_id: &str,
) -> crate::effects::response::EventRequest {
    let legs: Vec<&crate::effects::response::EventRequest> = group
        .iter()
        .flat_map(|(_, b)| b.dispatch.server_legs.iter())
        .collect();
    crate::embed::mount_leg_request(
        group.iter().map(|(seed, _)| seed.clone()).collect(),
        &legs,
        legs.first().copied(),
        legs.first().and_then(|leg| leg.entity_id.clone()),
        client_id,
    )
}

#[cfg(test)]
mod tests {
    use orbital_core_base::value::ValueMap;
    use super::*;
    use crate::entity_source::EntitySource;
    use {orbital_core_base::bus_event::{BusEventSource, EmittedEvent}, crate::effects::response::EventResponse};
    use orbital_core_base::sync::Mutex;
    use std::collections::HashMap;

    /// Riya (2026-09-24): a mount's seeder INIT must not reach a sibling before
    /// that sibling's own INIT. `local` traits keep the dispatch client-only.
    #[test]
    fn remount_holds_a_seed_until_the_receivers_init() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "Boot",
            "orbitals": [{
                "name": "Level",
                "entity": { "name": "World", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    { "name": "Seeder", "dispatch": "client", "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "SEED", {}]] }] } },
                    { "name": "Body", "dispatch": "client",
                      "listens": [{ "event": "SEED", "triggers": "SET", "source": { "kind": "trait", "trait": "Seeder" } }],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "ready" }, { "name": "seeded" }],
                        "transitions": [
                            { "from": "idle", "to": "ready", "event": "INIT" },
                            { "from": "ready", "to": "seeded", "event": "SET" }
                        ] } }
                ]
            }]
        }))
        .unwrap();
        let kernel = ClientKernel::new(schema, None).unwrap();
        kernel.remount(["Seeder", "Body"]);
        kernel
            .dispatch("Seeder", None, "INIT", Value::Null)
            .unwrap();
        let response = kernel.dispatch("Body", None, "INIT", Value::Null).unwrap();
        assert_eq!(response.states.get("Body"), Some(&"seeded".to_string()));
    }

    fn persisted_awaited_with_watcher_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [
                    { "name": "id", "type": "string" },
                    { "name": "title", "type": "string" }
                ] },
                "traits": [
                    {
                        "name": "Persistor",
                        "linkedEntity": "Note",
                        "stateMachine": {
                            "states": [
                                { "name": "idle", "isInitial": true },
                                { "name": "created" }
                            ],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT" },
                                {
                                    "from": "idle", "to": "created", "event": "CREATE",
                                    "effects": [
                                        ["set", "@entity.title", "Hello"],
                                        ["persist", "create", "Note", {}],
                                        ["emit", "CREATED", {}]
                                    ]
                                }
                            ]
                        }
                    },
                    {
                        "name": "Watcher",
                        "listens": [{
                            "event": "CREATED", "triggers": "WATCH",
                            "source": { "kind": "trait", "trait": "Persistor" }
                        }],
                        "stateMachine": {
                            "states": [
                                { "name": "idle", "isInitial": true },
                                { "name": "watched" },
                                { "name": "watched-twice" }
                            ],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT" },
                                { "from": "idle", "to": "watched", "event": "WATCH" },
                                { "from": "watched", "to": "watched-twice", "event": "WATCH" }
                            ]
                        }
                    }
                ]
            }]
        }))
        .unwrap()
    }

    /// `LocalToggle` declares `dispatch: "client"` but still
    /// carries a `persist` effect — same deliberately-malformed fixture
    /// `orbital-core`'s own `client_env_hybrid_local_trait_never_produces_server_leg`
    /// uses, reproduced here so the ClientKernel-level test proves the bridge is
    /// never even consulted, not just that the leg comes back empty.
    fn hybrid_with_stray_persist_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [{
                    "name": "LocalToggle",
                    "linkedEntity": "Note",
                    "dispatch": "client",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "created" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "created", "event": "CREATE",
                                "effects": [["persist", "create", "Note", {}]]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// `Move` declares `dispatch: "optimistic"` (RuntimeOptimistic).
    /// Its `MOVE` transition carries a `call-service` alongside the `set`, so it DOES
    /// still produce a server leg despite the entity being runtime-only (e.g. a
    /// multiplayer sync call) — the scenario `RuntimeOptimistic` rollback exists for.
    fn runtime_optimistic_with_call_service_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": {
                    "name": "Cursor", "persistence": "runtime",
                    "fields": [{ "name": "id", "type": "string" }, { "name": "x", "type": "number" }]
                },
                "traits": [{
                    "name": "Move",
                    "linkedEntity": "Cursor",
                    "dispatch": "optimistic",
                    "stateMachine": {
                        "states": [
                            { "name": "idle", "isInitial": true },
                            { "name": "moved" }
                        ],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            {
                                "from": "idle", "to": "moved", "event": "MOVE",
                                "effects": [
                                    ["set", "@entity.x", 5],
                                    ["call-service", "SyncService", "move", {}]
                                ]
                            }
                        ]
                    }
                }]
            }]
        }))
        .unwrap()
    }

    /// Records every call and panics — proves a code path that must never touch the
    /// bridge really doesn't.
    struct PanicBridge;
    impl ServerBridge for PanicBridge {
        fn send_event_with_options(
            &self,
            _orbital: &str,
            _event: &str,
            _entity_id: Option<&str>,
            _trait_name: Option<&str>,
            _payload: Value,
        ) -> Result<EventResponse, BridgeError> {
            panic!("PanicBridge::send_event_with_options must never be called");
        }
        fn send_request(
            &self,
            _orbital: &str,
            _request: &crate::effects::response::EventRequest,
        ) -> Result<EventResponse, BridgeError> {
            panic!("PanicBridge::send_request must never be called");
        }
    }

    /// Scripted responses, one per call, returned in order.
    struct ScriptedBridge {
        responses: Mutex<Vec<Result<EventResponse, String>>>,
        calls: Mutex<Vec<crate::effects::response::EventRequest>>,
    }
    impl ScriptedBridge {
        fn new(responses: Vec<Result<EventResponse, String>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(Vec::new()),
            }
        }
    }
    impl ServerBridge for ScriptedBridge {
        fn send_event_with_options(
            &self,
            _orbital: &str,
            _event: &str,
            _entity_id: Option<&str>,
            _trait_name: Option<&str>,
            _payload: Value,
        ) -> Result<EventResponse, BridgeError> {
            panic!("ScriptedBridge only answers send_request in these tests");
        }
        fn send_request(
            &self,
            _orbital: &str,
            request: &crate::effects::response::EventRequest,
        ) -> Result<EventResponse, BridgeError> {
            self.calls.lock().push(request.clone());
            match self.responses.lock().remove(0) {
                Ok(r) => Ok(r),
                Err(e) => Err(BridgeError::Network(e)),
            }
        }
    }

    #[test]
    fn client_kernel_hybrid_local_never_posts() {
        let bridge: Arc<dyn ServerBridge> = Arc::new(PanicBridge);
        let kernel = ClientKernel::new(hybrid_with_stray_persist_schema(), Some(bridge)).unwrap();
        let response = kernel
            .dispatch("LocalToggle", None, "CREATE", Value::Null)
            .unwrap();
        assert!(response.success);
        assert_eq!(
            response.states.get("LocalToggle"),
            Some(&"created".to_string())
        );
    }

    /// A `local` seed whose cascade reaches a listener that needs the server posts that
    /// listener's leg (orbital-core keeps it: `client_env_local_seed_posts_its_cascade_server_work`).
    #[test]
    fn a_local_seed_posts_the_server_work_its_cascade_reaches() {
        let schema: OrbitalSchema = serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": { "name": "Note", "fields": [{ "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
                "traits": [
                    { "name": "Composer", "dispatch": "client", "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "SUBMIT", "effects": [["emit", "SUBMITTED", {}]] }
                        ] } },
                    { "name": "Persistor", "linkedEntity": "Note",
                      "listens": [{ "event": "SUBMITTED", "triggers": "SAVE", "source": { "kind": "trait", "trait": "Composer" } }],
                      "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }, { "name": "saved" }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "saved", "event": "SAVE", "effects": [["persist", "create", "Note", { "title": "x" }]] }
                        ] } }
                ]
            }]
        }))
        .unwrap();
        let server = Arc::new(ScriptedBridge::new(vec![Ok(EventResponse::success(""))]));
        let kernel = ClientKernel::new(schema, Some(Arc::clone(&server) as Arc<dyn ServerBridge>)).unwrap();
        kernel.dispatch("Composer", None, "INIT", Value::Null).unwrap();
        kernel.dispatch("Persistor", None, "INIT", Value::Null).unwrap();
        kernel.dispatch("Composer", None, "SUBMIT", Value::Null).unwrap();
        let calls = server.calls.lock();
        assert_eq!(calls.len(), 1, "the listener's server work is posted");
        assert_eq!(calls[0].target_trait.as_deref(), Some("Persistor"));
    }

    #[test]
    fn client_kernel_persisted_awaited_posts_leg_and_folds() {
        let mut row = ValueMap::default();
        row.insert("id".to_string(), Value::String("n-1".to_string()));
        row.insert(
            "title".to_string(),
            Value::String("From server".to_string()),
        );
        let mut data = HashMap::new();
        data.insert("Note".to_string(), vec![Value::Object(row)]);

        let server_response = EventResponse {
            success: true,
            data: Some(data),
            // Echo of the SAME event this client already delivered locally
            // (`CREATED` from `Persistor`) — must be deduped, not re-dispatched.
            emitted_events: vec![EmittedEvent {
                event: "CREATED".to_string(),
                payload: None,
                source: Some(BusEventSource::from_trait("Persistor")),
            }],
            ..Default::default()
        };
        let bridge: Arc<dyn ServerBridge> =
            Arc::new(ScriptedBridge::new(vec![Ok(server_response)]));
        let kernel =
            ClientKernel::new(persisted_awaited_with_watcher_schema(), Some(bridge)).unwrap();

        let response = kernel
            .dispatch("Persistor", None, "CREATE", Value::Null)
            .unwrap();

        // The PRESENTED response is the local one (fold lands data for the next
        // render, it doesn't replace what THIS call returns).
        assert!(response.success);
        assert!(
            response.data.is_none(),
            "presented response stays the local one, not the server's"
        );
        assert_eq!(
            response.states.get("Persistor"),
            Some(&"created".to_string())
        );

        // The fold landed the server's data into the entity source.
        let folded = kernel.embedded.entities().read("Note", "n-1").unwrap();
        assert_eq!(
            folded.as_object().and_then(|o| o.get("title")),
            Some(&Value::String("From server".to_string()))
        );

        // The echoed CREATED was already locally delivered (it cascaded to Watcher
        // as part of the SAME local dispatch that built the leg) — the fold must
        // not re-fire Watcher's listener a second time.
        let watcher_state = kernel
            .embedded
            .kernel()
            .get_state("Watcher", Some("singleton"))
            .unwrap();
        assert_eq!(
            watcher_state.current_state, "watched",
            "an echoed already-delivered event must not re-dispatch and advance Watcher again"
        );
    }

    #[test]
    fn begin_paints_locally_and_posts_nothing_until_settle() {
        let bridge = Arc::new(ScriptedBridge::new(vec![Ok(EventResponse {
            success: true,
            ..Default::default()
        })]));
        let kernel = ClientKernel::new(
            persisted_awaited_with_watcher_schema(),
            Some(bridge.clone() as Arc<dyn ServerBridge>),
        )
        .unwrap();
        let begun = kernel
            .begin("Persistor", None, "CREATE", Value::Null)
            .unwrap();
        assert_eq!(
            begun.local.states.get("Persistor"),
            Some(&"created".to_string())
        );
        assert!(
            bridge.calls.lock().is_empty(),
            "nothing posted before settle"
        );
        assert!(
            begun.awaiting().iter().any(|a| a.trait_name == "Persistor"),
            "the persisting trait awaits its leg: {:?}",
            begun.awaiting()
        );
        let delta = kernel.settle(&begun).unwrap();
        assert_eq!(bridge.calls.lock().len(), 1);
        assert!(
            delta.states.is_empty(),
            "control: settle adds only what the server answered"
        );
        assert!(delta.client_effects.as_ref().is_none_or(Vec::is_empty));
    }

    fn settled_effect_results(
        server: EventResponse,
    ) -> Option<Vec<crate::effects::response::ServerEffectResult>> {
        let bridge: Arc<dyn ServerBridge> = Arc::new(ScriptedBridge::new(vec![Ok(server)]));
        let kernel =
            ClientKernel::new(persisted_awaited_with_watcher_schema(), Some(bridge)).unwrap();
        kernel
            .dispatch("Persistor", None, "CREATE", Value::Null)
            .unwrap()
            .effect_results
    }

    #[test]
    fn a_server_denied_persist_reaches_the_settled_response() {
        let denied = crate::effects::response::ServerEffectResult {
            effect: crate::effects::response::ServerEffectKind::Persist,
            action: Some("create".into()),
            entity_type: Some("Note".into()),
            name: None,
            data: None,
            success: false,
            denied: Some(true),
            error: Some("access denied".into()),
            code: None,
            target: None,
            routed: None,
        };
        let results = settled_effect_results(EventResponse {
            success: true,
            effect_results: Some(vec![denied.clone()]),
            ..Default::default()
        })
        .expect("the server's verdict is carried");
        assert!(results.contains(&denied));
    }

    #[test]
    fn control_a_server_answer_with_no_outcomes_adds_no_server_results() {
        let results = settled_effect_results(EventResponse {
            success: true,
            ..Default::default()
        });
        assert!(results.iter().flatten().all(|r| r.denied.is_none()));
    }

    #[test]
    fn client_kernel_runtime_optimistic_restores_on_error() {
        let server_response = EventResponse::error("sync rejected");
        let bridge: Arc<dyn ServerBridge> =
            Arc::new(ScriptedBridge::new(vec![Ok(server_response)]));
        let kernel =
            ClientKernel::new(runtime_optimistic_with_call_service_schema(), Some(bridge)).unwrap();

        // INIT first so the pre-MOVE snapshot captures a real prior state
        // ("idle") rather than the empty default for a trait that has never run.
        kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();

        // Local optimistic commit.
        let response = kernel.dispatch("Move", None, "MOVE", Value::Null).unwrap();
        assert!(
            !response.success,
            "the presented response is the server's success:false"
        );
        assert_eq!(
            kernel
                .embedded
                .kernel()
                .get_state("Move", Some("singleton"))
                .unwrap()
                .current_state,
            "idle",
            "RuntimeOptimistic must restore the pre-dispatch snapshot on a disagreeing response"
        );
        assert!(
            kernel.embedded.entities().read("Cursor", "singleton").is_err(),
            "rollback must leave the entity absent, matching the pre-dispatch snapshot"
        );
    }

    fn optimistic_with_bound_sibling_schema() -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({
            "name": "App",
            "orbitals": [{
                "name": "Main",
                "entity": {
                    "name": "Cursor", "persistence": "runtime", "shared": true,
                    "fields": [{ "name": "id", "type": "string" }, { "name": "x", "type": "number", "default": 0 }]
                },
                "traits": [
                    {
                        "name": "Move", "linkedEntity": "Cursor", "dispatch": "optimistic",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }, { "name": "moved" }],
                            "transitions": [
                                { "from": "idle", "to": "idle", "event": "INIT" },
                                { "from": "idle", "to": "moved", "event": "MOVE", "effects": [
                                    ["set", "@entity.x", 5],
                                    ["call-service", "SyncService", "move", {}]
                                ] }
                            ]
                        }
                    },
                    {
                        "name": "Hud", "linkedEntity": "Cursor", "dispatch": "client",
                        "stateMachine": {
                            "states": [{ "name": "idle", "isInitial": true }],
                            "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [
                                ["render-ui", "hud", { "type": "typography", "content": "@entity.x" }]
                            ] }]
                        }
                    }
                ]
            }]
        }))
        .unwrap()
    }

    fn hud_paints(response: &EventResponse) -> Vec<f64> {
        response
            .client_effects_by_trait
            .iter()
            .flatten()
            .filter(|e| e.trait_name == "Hud")
            .filter_map(|e| e.effect.render_ui_value())
            .filter_map(|v| serde_json::to_value(v).ok()?["content"].as_f64())
            .collect()
    }

    fn mounted_optimistic(server: Result<EventResponse, String>) -> ClientKernel {
        let bridge: Arc<dyn ServerBridge> = Arc::new(ScriptedBridge::new(vec![server]));
        let kernel =
            ClientKernel::new(optimistic_with_bound_sibling_schema(), Some(bridge)).unwrap();
        kernel.dispatch("Hud", None, "INIT", Value::Null).unwrap();
        kernel.dispatch("Move", None, "INIT", Value::Null).unwrap();
        kernel
    }

    #[test]
    fn a_failed_optimistic_leg_repaints_the_bound_sibling_with_the_restored_value() {
        let kernel = mounted_optimistic(Ok(EventResponse::error("sync rejected")));
        let response = kernel.dispatch("Move", None, "MOVE", Value::Null).unwrap();
        assert!(!response.success);
        assert_eq!(
            hud_paints(&response),
            vec![0.0],
            "the rolled-back value, not the optimistic 5"
        );
    }

    #[test]
    fn a_transport_failure_leaves_the_restored_repaint_for_the_next_response() {
        let kernel = mounted_optimistic(Err("down".into()));
        assert!(kernel.dispatch("Move", None, "MOVE", Value::Null).is_err());
        let mut next = EventResponse::default();
        next.fold_rerenders(&kernel.embedded.kernel().rerender_stale());
        assert_eq!(
            hud_paints(&next),
            vec![0.0],
            "nothing was consumed with no response to carry it"
        );
    }

    #[test]
    fn control_a_successful_optimistic_leg_adds_no_repaint_beyond_the_commit() {
        let kernel = mounted_optimistic(Ok(EventResponse {
            success: true,
            ..Default::default()
        }));
        let response = kernel.dispatch("Move", None, "MOVE", Value::Null).unwrap();
        assert!(response.success);
        assert_eq!(
            hud_paints(&response),
            vec![5.0],
            "one repaint, the committed value"
        );
        assert!(kernel.embedded.kernel().rerender_stale().is_empty());
    }

    #[test]
    fn client_kernel_offline_returns_local_response() {
        let kernel = ClientKernel::new(persisted_awaited_with_watcher_schema(), None).unwrap();
        let response = kernel
            .dispatch("Persistor", None, "CREATE", Value::Null)
            .unwrap();
        assert!(response.success);
        assert_eq!(
            response.states.get("Persistor"),
            Some(&"created".to_string())
        );
        // Offline: nothing was ever posted, so the server-assigned row never lands.
        assert!(kernel.embedded.entities().read("Note", "n-1").is_err());
    }

    struct RecordingBridge {
        posts: Mutex<Vec<(String, String)>>,
    }
    impl RecordingBridge {
        fn new() -> Self {
            Self {
                posts: Mutex::new(Vec::new()),
            }
        }
    }
    impl ServerBridge for RecordingBridge {
        fn send_event_with_options(
            &self,
            _orbital: &str,
            _event: &str,
            _entity_id: Option<&str>,
            _trait_name: Option<&str>,
            _payload: Value,
        ) -> Result<EventResponse, BridgeError> {
            panic!("only send_request is used");
        }
        fn send_request(
            &self,
            orbital: &str,
            request: &crate::effects::response::EventRequest,
        ) -> Result<EventResponse, BridgeError> {
            self.posts
                .lock()
                .push((orbital.to_string(), request.event.clone()));
            Ok(EventResponse {
                success: true,
                ..Default::default()
            })
        }
    }

    fn ticker_trait(ticks: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "name": "Ticker", "dispatch": "client",
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": "active" }],
                "transitions": [
                    { "from": "idle", "to": "idle", "event": "INIT" },
                    { "from": "idle", "to": "active", "event": "GO" }
                ]
            },
            "ticks": ticks
        })
    }

    fn listener_trait() -> serde_json::Value {
        serde_json::json!({
            "name": "Listener", "dispatch": "client",
            "listens": [{ "event": "BEAT", "triggers": "HEARD", "source": { "kind": "trait", "trait": "Ticker" } }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }, { "name": "heard" }],
                "transitions": [{ "from": "idle", "to": "heard", "event": "HEARD" }]
            }
        })
    }

    fn persistor_trait() -> serde_json::Value {
        serde_json::json!({
            "name": "Persistor", "linkedEntity": "Task",
            "listens": [{ "event": "BEAT", "triggers": "DO_CREATE", "source": { "kind": "orbital", "orbital": "Alpha", "trait": "Ticker" } }],
            "stateMachine": {
                "states": [{ "name": "idle", "isInitial": true }],
                "transitions": [{ "from": "idle", "to": "idle", "event": "DO_CREATE",
                    "effects": [["persist", "create", "Task", {}]] }]
            }
        })
    }

    fn orbital(name: &str, entity: &str, traits: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "entity": { "name": entity, "fields": [{ "name": "id", "type": "string" }] },
            "traits": traits
        })
    }

    fn schema_of(orbitals: Vec<serde_json::Value>) -> OrbitalSchema {
        serde_json::from_value(serde_json::json!({ "name": "App", "orbitals": orbitals })).unwrap()
    }

    const BEAT_TICK: &str =
        r#"[{ "name": "beat", "interval": 1000, "effects": [["emit", "BEAT", {}]] }]"#;
    const QUIET_TICK: &str = r#"[{ "name": "beat", "interval": 1000, "effects": [] }]"#;

    fn ticks(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw).unwrap()
    }

    fn fan_out_kernel(tick: &str) -> ClientKernel {
        let schema = schema_of(vec![orbital(
            "Main",
            "Note",
            vec![ticker_trait(ticks(tick)), listener_trait()],
        )]);
        let kernel = ClientKernel::new(schema, None).unwrap();
        kernel
            .dispatch("Ticker", None, "INIT", Value::Null)
            .unwrap();
        kernel
    }

    #[test]
    fn a_tick_emit_fans_out_to_its_listener() {
        let kernel = fan_out_kernel(BEAT_TICK);
        let response = kernel.tick("Ticker", "beat", None).unwrap();
        assert_eq!(
            response.states.get("Listener").map(String::as_str),
            Some("heard")
        );
        assert_eq!(
            kernel
                .embedded
                .kernel()
                .get_state("Listener", Some("singleton"))
                .unwrap()
                .current_state,
            "heard"
        );
        assert!(
            response.emitted_events.is_empty(),
            "the cascade already delivered the emit; the caller's queue must not replay it"
        );
    }

    #[test]
    fn control_a_tick_without_an_emit_leaves_the_listener_idle() {
        let kernel = fan_out_kernel(QUIET_TICK);
        let response = kernel.tick("Ticker", "beat", None).unwrap();
        assert!(response.transitioned);
        assert!(!response.states.contains_key("Listener"));
        assert!(kernel
            .embedded
            .kernel()
            .get_state("Listener", Some("singleton"))
            .is_none_or(|s| s.current_state == "idle"));
    }

    fn two_orbital_kernel(
        listener_orbital_is_ticker_orbital: bool,
        bridge: Arc<RecordingBridge>,
    ) -> ClientKernel {
        let schema = if listener_orbital_is_ticker_orbital {
            schema_of(vec![
                orbital(
                    "Alpha",
                    "Task",
                    vec![ticker_trait(ticks(BEAT_TICK)), persistor_trait()],
                ),
                orbital("Beta", "Other", vec![listener_trait_named("Bystander")]),
            ])
        } else {
            schema_of(vec![
                orbital("Alpha", "Note", vec![ticker_trait(ticks(BEAT_TICK))]),
                orbital("Beta", "Task", vec![persistor_trait()]),
            ])
        };
        let kernel = ClientKernel::new(schema, Some(bridge as Arc<dyn ServerBridge>)).unwrap();
        kernel
            .dispatch("Ticker", None, "INIT", Value::Null)
            .unwrap();
        kernel
    }

    fn listener_trait_named(name: &str) -> serde_json::Value {
        let mut t = listener_trait();
        t["name"] = serde_json::Value::String(name.to_string());
        t
    }

    #[test]
    fn a_cross_orbital_tick_leg_posts_to_the_owning_orbital() {
        let bridge = Arc::new(RecordingBridge::new());
        let kernel = two_orbital_kernel(false, Arc::clone(&bridge));
        kernel.tick("Ticker", "beat", None).unwrap();
        assert_eq!(
            *bridge.posts.lock(),
            vec![("Beta".to_string(), "DO_CREATE".to_string())]
        );
    }

    #[test]
    fn control_a_same_orbital_tick_leg_posts_to_that_orbital() {
        let bridge = Arc::new(RecordingBridge::new());
        let kernel = two_orbital_kernel(true, Arc::clone(&bridge));
        kernel.tick("Ticker", "beat", None).unwrap();
        assert_eq!(
            *bridge.posts.lock(),
            vec![("Alpha".to_string(), "DO_CREATE".to_string())]
        );
    }

    #[test]
    fn a_cross_orbital_dispatch_leg_posts_to_the_owning_orbital() {
        let bridge = Arc::new(RecordingBridge::new());
        let schema = schema_of(vec![
            orbital(
                "Alpha",
                "Note",
                vec![serde_json::json!({
                    "name": "Ticker", "dispatch": "client",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT" },
                            { "from": "idle", "to": "idle", "event": "FIRE", "effects": [["emit", "BEAT", {}]] }
                        ]
                    }
                })],
            ),
            orbital("Beta", "Task", vec![persistor_trait()]),
        ]);
        let kernel =
            ClientKernel::new(schema, Some(Arc::clone(&bridge) as Arc<dyn ServerBridge>)).unwrap();
        kernel
            .dispatch("Ticker", None, "INIT", Value::Null)
            .unwrap();
        kernel
            .dispatch("Ticker", None, "FIRE", Value::Null)
            .unwrap();
        assert_eq!(
            *bridge.posts.lock(),
            vec![("Beta".to_string(), "DO_CREATE".to_string())]
        );
    }

    fn applies_to_kernel(applies_to: serde_json::Value) -> ClientKernel {
        let mut tick = serde_json::json!({ "name": "beat", "interval": 1000, "effects": [["emit", "BEAT", {}]] });
        if !applies_to.is_null() {
            tick["appliesTo"] = applies_to;
        }
        let schema = schema_of(vec![orbital(
            "Main",
            "Note",
            vec![ticker_trait(serde_json::json!([tick])), listener_trait()],
        )]);
        let kernel = ClientKernel::new(schema, None).unwrap();
        kernel
            .dispatch("Ticker", None, "INIT", Value::Null)
            .unwrap();
        kernel
    }

    #[test]
    fn a_tick_applies_to_another_state_does_not_fire() {
        let kernel = applies_to_kernel(serde_json::json!(["active"]));
        let response = kernel.tick("Ticker", "beat", None).unwrap();
        assert!(!response.transitioned);
        assert!(
            !response.states.contains_key("Listener"),
            "no emit, no fan-out"
        );
    }

    #[test]
    fn control_a_tick_applies_to_the_current_state_fires() {
        let kernel = applies_to_kernel(serde_json::json!(["active"]));
        kernel.dispatch("Ticker", None, "GO", Value::Null).unwrap();
        let response = kernel.tick("Ticker", "beat", None).unwrap();
        assert!(response.transitioned);
        assert_eq!(
            response.states.get("Listener").map(String::as_str),
            Some("heard")
        );
    }

    #[test]
    fn control_an_absent_or_empty_applies_to_means_every_state() {
        for applies_to in [serde_json::Value::Null, serde_json::json!([])] {
            let kernel = applies_to_kernel(applies_to);
            let response = kernel.tick("Ticker", "beat", None).unwrap();
            assert!(response.transitioned);
        }
    }

    fn mount_schema() -> OrbitalSchema {
        let loading = |label: &str| serde_json::json!(["render-ui", "main", { "type": "skeleton", "variant": "text", "content": label }]);
        serde_json::from_value(serde_json::json!({
            "name": "MountApp",
            "orbitals": [{
                "name": "PageOrbital",
                "entity": { "name": "Item", "fields": [{ "name": "id", "type": "string" }] },
                "traits": [
                    { "name": "List", "linkedEntity": "Item", "stateMachine": {
                        "states": [{ "name": "loading", "isInitial": true }, { "name": "ready" }],
                        "transitions": [
                            { "from": "loading", "to": "loading", "event": "INIT", "effects": [["fetch", "Item", { "emit": { "success": "LOADED" } }], loading("list")] },
                            { "from": "loading", "to": "ready", "event": "LOADED" }
                        ] } },
                    { "name": "Stats", "linkedEntity": "Item", "stateMachine": {
                        "states": [{ "name": "loading", "isInitial": true }],
                        "transitions": [
                            { "from": "loading", "to": "loading", "event": "LOAD", "effects": [["fetch", "Item", {}], loading("stats")] }
                        ] } },
                    { "name": "Title", "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "transitions": [
                            { "from": "idle", "to": "idle", "event": "INIT", "effects": [loading("title")] }
                        ] } }
                ]
            }]
        }))
        .unwrap()
    }

    /// The in-process server ([`crate::client::bridge::KernelBridge`]) behind a request recorder.
    struct RecordingServer {
        server: crate::client::bridge::KernelBridge,
        sent: Mutex<Vec<crate::effects::response::EventRequest>>,
        fail: bool,
    }
    impl ServerBridge for RecordingServer {
        fn send_event_with_options(
            &self,
            _orbital: &str,
            _event: &str,
            _entity_id: Option<&str>,
            _trait_name: Option<&str>,
            _payload: Value,
        ) -> Result<EventResponse, BridgeError> {
            panic!("RecordingServer only answers send_request");
        }
        fn send_request(
            &self,
            orbital: &str,
            request: &crate::effects::response::EventRequest,
        ) -> Result<EventResponse, BridgeError> {
            self.sent.lock().push(request.clone());
            if self.fail {
                return Err(BridgeError::Network("network down".into()));
            }
            self.server.send_request(orbital, request)
        }
    }

    fn mount_kernel(fail: bool) -> (ClientKernel, Arc<RecordingServer>) {
        let server = Arc::new(RecordingServer {
            server: crate::client::bridge::KernelBridge::new(mount_schema()).unwrap(),
            sent: Mutex::new(Vec::new()),
            fail,
        });
        let kernel = ClientKernel::new(
            mount_schema(),
            Some(Arc::clone(&server) as Arc<dyn ServerBridge>),
        )
        .unwrap();
        (kernel, server)
    }

    fn seed(t: &str, e: &str) -> crate::effects::response::MountSeed {
        crate::effects::response::MountSeed {
            r#trait: t.to_string(),
            event: crate::effects::response::LifecycleEvent::parse(e).expect("a lifecycle event"),
        }
    }

    #[test]
    fn a_mount_paints_every_seed_then_posts_one_mount_leg_for_the_server_backed_seeds() {
        let (kernel, server) = mount_kernel(false);
        let begun = kernel
            .begin_mount(
                &[
                    seed("List", "INIT"),
                    seed("Stats", "LOAD"),
                    seed("Title", "INIT"),
                ],
                Value::Null,
            )
            .unwrap();
        assert_eq!(
            begun
                .locals()
                .map(|(t, _)| t.to_string())
                .collect::<Vec<_>>(),
            ["List", "Stats", "Title"]
        );
        assert!(
            server.sent.lock().is_empty(),
            "nothing is posted before settle"
        );
        let mut awaiting: Vec<&str> = begun.awaiting().map(|a| a.trait_name.as_str()).collect();
        awaiting.sort();
        assert_eq!(
            awaiting,
            ["List", "Stats"],
            "control: the local-only Title awaits nothing"
        );

        kernel.settle_mount(&begun).unwrap();
        let sent = server.sent.lock();
        assert_eq!(sent.len(), 1, "one mount leg, not one request per seed");
        assert_eq!(
            sent[0].mount,
            Some(vec![seed("List", "INIT"), seed("Stats", "LOAD")])
        );
        drop(sent);
        assert_eq!(
            kernel.snapshot_trait("List", None).state,
            "ready",
            "the folded leg delivered LOADED"
        );
    }

    #[test]
    fn a_mount_with_no_server_backed_seed_posts_nothing() {
        let (kernel, server) = mount_kernel(false);
        let begun = kernel
            .begin_mount(&[seed("Title", "INIT")], Value::Null)
            .unwrap();
        assert!(!begun.has_legs());
        kernel.settle_mount(&begun).unwrap();
        assert!(server.sent.lock().is_empty());
    }

    #[test]
    fn a_failed_mount_leg_is_reported() {
        let (kernel, server) = mount_kernel(true);
        let begun = kernel
            .begin_mount(&[seed("List", "INIT"), seed("Title", "INIT")], Value::Null)
            .unwrap();
        assert!(kernel.settle_mount(&begun).is_err());
        assert_eq!(server.sent.lock().len(), 1);
    }
}
