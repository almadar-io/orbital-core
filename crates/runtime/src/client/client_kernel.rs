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
    /// The entities whose rows live in the server's database (not client-resident): a row-set reference
    /// to one is resolved by the server that holds the rows, never against this client's cache.
    server_hosted: std::collections::HashSet<String>,
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
    ) -> Result<Self, crate::kernel::EntryError> {
        let mut server_hosted = std::collections::HashSet::new();
        for orbital in &schema.orbitals {
            let entities = std::iter::once(&orbital.entity).chain(orbital.auxiliary_entities.iter().flatten());
            for entity in entities {
                if let orbital_core_base::schema::types::EntityRef::Inline(def) = entity {
                    if !def.is_client_resident() {
                        server_hosted.insert(def.name.clone());
                    }
                }
            }
        }
        let embedded = EmbeddedKernel::new(schema, next_client_id())?;
        Ok(Self { embedded, bridge, server_hosted })
    }

    /// Seed entity rows so a subsequent INIT `fetch`/`list` on `entity_type` returns
    /// them (mirrors `KernelBridge::seed`, sharing the same [`seed_into`] body).
    /// A row-set reference resolved by the kernel's one resolver ([`crate::runtime::RuntimeKernel::resolve_rows`]).
    pub fn resolve_rows(
        &self,
        reference: &orbital_core_base::row_set::RowSetRef,
    ) -> Result<crate::row_query::RowPage, String> {
        match &self.bridge {
            Some(bridge) if self.server_hosted.contains(&reference.entity_type) => {
                bridge.resolve_rows(reference).map_err(|e| e.to_string())
            }
            _ => self.embedded.resolve_rows(reference),
        }
    }

    pub fn seed(&self, entity_type: &str, rows: Vec<Value>) -> Result<(), String> {
        seed_into(self.embedded.kernel(), self.embedded.entities().as_ref(), entity_type, &rows)
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
        let mut settled = self.embedded.kernel().response_for(&outcomes, "", "");
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

    /// One host frame of the kernel's tick clock (`EmbeddedKernel::frame`, the loop every host runs): every due tick
    /// runs once, with the kernel's frame sealing and `[visible]` mounts, and each emit cascade it began is posted
    /// and folded like a dispatch's. The responses to paint, in order: the frame's own, then each cascade's. Empty when
    /// nothing was due.
    pub fn frame(&self, now_ms: u64) -> Result<Vec<EventResponse>, BridgeError> {
        let Some(parts) = self.embedded.frame_client(now_ms).map_err(BridgeError::Server)? else {
            return Ok(Vec::new());
        };
        for error in &parts.errors {
            tracing::error!(target: orbital_core_base::logging::ns::BRIDGE, %error, "frame tick failed");
        }
        if parts.fired.is_empty() {
            return Err(BridgeError::Server(parts.errors.join("; ")));
        }
        let mut frame = parts.response;
        frame.emitted_events.clear();
        let mut responses = vec![frame];
        for (trait_name, (_, dispatch)) in parts.dispatches {
            let mut base = dispatch.response.clone();
            base.emitted_events.clear();
            let event = dispatch.seed.1.clone();
            responses.push(self.post_legs(&trait_name, None, &event, &dispatch, base)?);
        }
        Ok(responses)
    }

    /// How many `(trait, tick)` pairs the schema declares.
    pub fn tick_count(&self) -> usize {
        self.embedded.tick_count()
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
        self.embedded.remount(traits.into_iter().map(Into::into).collect());
    }

    /// Add `traits` to the current mount without dropping those already mounted (`EmbeddedKernel::mount`).
    pub fn mount<I, S>(&self, traits: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.embedded.mount(traits.into_iter().map(Into::into).collect());
    }

    /// The host's report of whether `trait_name`'s rendered output is on screen: a `[visible]` tick of an
    /// off-screen trait does not fire (EXEC-TICK-4).
    pub fn set_trait_on_screen(&self, trait_name: &str, on_screen: bool) {
        self.embedded.set_trait_on_screen(trait_name, on_screen);
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
            settled = self.fold_answer(bridge.as_ref(), &response, &already, settled)?;
            settled.fold_rerenders(&self.embedded.kernel().rerender_stale());
        }
        Ok(settled)
    }

    /// Fold a posted answer, then post and fold each leg the fold itself owes the host (an own continuation such as
    /// a refetch after a persist's success), in order, until none is owed (EXEC-DIST-10).
    fn fold_answer(
        &self,
        bridge: &dyn ServerBridge,
        answer: &EventResponse,
        already: &std::collections::HashSet<(String, String)>,
        mut settled: EventResponse,
    ) -> Result<EventResponse, BridgeError> {
        let mut owed = std::collections::VecDeque::new();
        let applied = self.embedded.apply_server_response(answer, already).map_err(BridgeError::Server)?;
        owed.extend(applied.server_legs);
        settled = settled.with_server_effect_results(answer);
        while let Some(routed) = owed.pop_front() {
            let next = bridge.send_request(&routed.orbital, &routed.leg)?;
            let applied = self
                .embedded
                .apply_server_response(&next, &std::collections::HashSet::new())
                .map_err(BridgeError::Server)?;
            owed.extend(applied.server_legs);
            settled = settled.with_server_effect_results(&next);
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
                        settled = self.fold_answer(bridge.as_ref(), &server_response, &already, settled)?;
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
                    settled = self.fold_answer(bridge.as_ref(), &server_response, &already, settled)?;
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
mod tests;
