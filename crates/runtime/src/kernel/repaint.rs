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
    ) -> Vec<EffectResult> {
        // Resolve every binding through the one canonical walker
        // (`ctx.entity` already carries the current entity here).
        state_render_sources(trait_def, state_name)
            .into_iter()
            .map(|(slot, content)| {
                resolve_effect_result(
                    EffectResult::executed(Effect::RenderUi { slot, content }),
                    ctx,
                )
            })
            .collect()
    }

    /// Keep a trait's just-run `render-ui` effects (`sources`: unresolved, in run order;
    /// `resolved`: the same effects after binding resolution) so [`Self::rerender_stale`] can
    /// repaint them when the entity they read changes.
    pub(super) fn record_renders(
        &self,
        trait_name: &str,
        entity_id: &str,
        sources: Vec<(String, Value)>,
        resolved: &[EffectResult],
        ctx: &BindingContext,
    ) {
        if sources.is_empty() {
            return;
        }
        self.flush_row_writes();
        let resolved = resolved.iter().filter_map(|er| match &er.effect {
            Effect::RenderUi { slot, content } => Some((slot.clone(), content.clone())),
            _ => None,
        });
        let renders = sources
            .into_iter()
            .zip(resolved)
            .map(|((_, source), (slot, content))| (slot, source, content))
            .collect();
        self.renders
            .write()
            .record(trait_name, entity_id, renders, ctx);
    }

    /// Repaint every mounted trait whose last `render-ui` reads an entity that has changed since
    /// it was resolved. Which leaves read the entity is declared in the source: a leaf holding
    /// `@entity` and no `@payload` (the web's `RenderBindingMarker`); everything else stays at
    /// the value it was flushed with. Only a client-role kernel repaints, and only for an entity
    /// the client owns (`[runtime]`, `[shared]`, browser-stored or unbound; the web's
    /// `clientResolvesRenderBindings`), since a persistent entity's bindings are server-resolved.
    /// A trait changed by several writes appears once; a second call with nothing new is empty.
    pub fn rerender_stale(&self) -> Vec<Rerender> {
        if !self.effect_executor.environment().is_client() {
            return Vec::new();
        }
        self.flush_row_writes();
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
            |trait_name, entity_id| self.step_entity_view(trait_name, entity_id),
        );
        stale
            .into_iter()
            .map(|(trait_name, entity_id, repaints)| Rerender {
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
                trait_name,
            })
            .collect()
    }
}
