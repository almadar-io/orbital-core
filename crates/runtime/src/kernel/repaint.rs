//! Render bookkeeping: a state's render effects, recording paints, and repainting stale traits.

use super::*;

impl RuntimeKernel {
    /// Get render-ui effects for a state to show UI when no transition happens.
    pub(super) fn get_state_render_effects(
        &self,
        trait_def: &TraitDefinition,
        state_name: &str,
        ctx: &BindingContext,
        _entity_data: &SharedEntity,
    ) -> Result<Vec<EffectResult>, KernelError> {
        // Resolve every binding through the one canonical walker
        // (`ctx.entity` already carries the current entity here).
        Ok(state_render_sources(trait_def, state_name)?
            .into_iter()
            .map(|(slot, content)| {
                resolve_effect_result(
                    EffectResult::executed(Effect::RenderUi { slot, content }),
                    ctx,
                )
            })
            .collect())
    }

    /// `result` with its bindings resolved; a `render-ui` resolves under what its slot kept from its last render's
    /// maps (EXEC-RENDER-9).
    pub(super) fn resolve_render_result(
        &self,
        trait_name: &str,
        entity_id: &str,
        mut result: EffectResult,
        ctx: &BindingContext,
    ) -> EffectResult {
        if let Effect::RenderUi { slot, content } = &mut result.effect {
            if let Ok(slot) = resolve_render_slot(slot.clone(), ctx) {
                let mut kept = ctx.clone();
                {
                    let mut store = self.renders.write();
                    *content = store.share_source_with_previous(trait_name, entity_id, &slot, std::mem::take(content));
                    kept.set_map_memo(store.map_memo(trait_name, entity_id, &slot));
                }
                return resolve_effect_result(result, &kept);
            }
        }
        resolve_effect_result(result, ctx)
    }

    /// Keep a trait's just-run `render-ui` effects (`sources`: unresolved, in run order;
    /// `resolved`: the same effects after binding resolution) so [`Self::rerender_stale`] can
    /// repaint them when the entity they read changes.
    pub(super) fn record_renders(
        &self,
        trait_name: &str,
        entity_id: &str,
        sources: Vec<RenderSource>,
        resolved: &mut [EffectResult],
        ctx: &BindingContext,
    ) {
        if sources.is_empty() {
            return;
        }
        self.flush_row_writes();
        {
            let store = self.renders.read();
            for er in resolved.iter_mut() {
                if let Effect::RenderUi { slot, content } = &mut er.effect {
                    *content = store.share_with_previous(trait_name, entity_id, slot, std::mem::take(content));
                }
            }
        }
        let resolved = resolved.iter().filter_map(|er| match &er.effect {
            Effect::RenderUi { slot, content } => Some((slot.clone(), content.clone())),
            _ => None,
        });
        let renders = sources
            .into_iter()
            .zip(resolved)
            .map(|((_, source, scope), (slot, content))| (slot, source, content, scope))
            .collect();
        self.renders
            .write()
            .record(trait_name, entity_id, renders, ctx);
    }

    /// Repaint every mounted trait whose last `render-ui` reads an entity that has changed since
    /// it was resolved. Which leaves read the entity is declared in the source: a leaf holding
    /// `@entity` and no `@payload` (the web's `RenderBindingMarker`); everything else stays at
    /// the value it was flushed with. Only a client-role kernel, or one running client traits for a browser, repaints, and only for an entity
    /// the client owns (`[runtime]`, `[shared]`, browser-stored or unbound; the web's
    /// `clientResolvesRenderBindings`), since a persistent entity's bindings are server-resolved.
    /// A trait changed by several writes appears once; a second call with nothing new is empty.
    pub fn rerender_stale(&self) -> Vec<Rerender> {
        // A kernel that runs client traits for a browser (a hosted guest, EXEC-ENDPOINT-3a) repaints as the client would.
        if !self.effect_executor.environment().is_client() && !self.runs_client_traits() {
            return Vec::new();
        }
        self.flush_row_writes();
        let rows_written = self.renders.write().take_rows_written();
        let user = self.session_user().ok();
        let stale = self.renders.write().stale(
            |trait_name| {
                let entity_type = self
                    .registry.trait_definitions
                    .read()
                    .get(trait_name)
                    .and_then(|d| d.linked_entity.clone())
                    .unwrap_or_else(|| trait_name.to_string());
                self.registry.entity_definitions
                    .read()
                    .get(&entity_type)
                    .is_none_or(|d| d.shared || d.is_client_resident())
            },
            |trait_name, entity_id| {
                let entity_type = self
                    .registry.trait_definitions
                    .read()
                    .get(trait_name)
                    .and_then(|d| d.linked_entity.clone())
                    .unwrap_or_else(|| trait_name.to_string());
                let slot = self.frame_slot(trait_name, &entity_type, entity_id);
                let own = self.shared_intrinsic_fields(&entity_type).map(|_| self.own_frame_slot(trait_name, entity_id));
                let now = {
                    let frames = self.frames.read();
                    Seen {
                        frame: frames.version(&slot),
                        own: own.as_ref().and_then(|own| frames.version(own)),
                        epoch: frames.epoch(),
                        row: self.instance_row_at(&slot, &entity_type, entity_id),
                    }
                };
                let mut seen = self.repaint_seen.write();
                let last = seen.get_mut(trait_name).and_then(|by_id| by_id.get_mut(entity_id));
                let moved = match last {
                    Some(last) => {
                        let moved = !last.same(&now);
                        *last = now;
                        moved
                    }
                    None => {
                        seen.entry(trait_name.to_string()).or_default().insert(entity_id.to_string(), now);
                        true
                    }
                };
                drop(seen);
                (moved || rows_written.contains(&entity_type))
                    .then(|| self.read_trait_entity_at(&slot, trait_name, &entity_type, entity_id).to_value())
            },
            user.as_ref(),
            |trait_name, flushed, user| {
                let mut ctx = flushed.clone();
                ctx.set_user(user.clone());
                let def = self.registry.trait_definitions.read().get(trait_name).cloned();
                if let Some(def) = def {
                    ctx.set_config(bind_user_forwards(self.trait_config_defaults(&def), user));
                }
                ctx
            },
        );
        stale
            .into_iter()
            .map(|(trait_name, entity_id, repaints, faults)| Rerender {
                state: self.current_state_name(&trait_name, Some(&entity_id)),
                entity: self.step_entity_view(&trait_name, &entity_id),
                rows_changed: repaints
                    .iter()
                    .filter(|r| r.rows_changed)
                    .map(|r| r.slot.clone())
                    .collect(),
                effects: repaints
                    .into_iter()
                    .map(|r| Effect::RenderUi {
                        slot: r.slot,
                        content: r.content,
                    })
                    .collect(),
                faults,
                trait_name,
            })
            .collect()
    }
}

/// What one rendered instance's `@entity` was read from at a repaint check.
pub(crate) struct Seen {
    frame: Option<u64>,
    own: Option<u64>,
    epoch: u64,
    row: Option<Arc<Value>>,
}

impl Seen {
    fn same(&self, now: &Seen) -> bool {
        let row = match (&self.row, &now.row) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        row && self.frame == now.frame && self.own == now.own && self.epoch == now.epoch
    }
}
