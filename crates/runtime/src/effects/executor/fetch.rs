//! The fetch effect and its failure.

use super::*;

impl EffectExecutor {
    #[allow(clippy::too_many_arguments)] // mirrors FetchHandler::fetch query params
    /// A failed fetch. The fetch handler reports a refusal only as a message, so the failure carries no code but
    /// the by-id miss's `not-found` (a typed fetch refusal is a contract gap, `G-FETCH-REFUSAL`).
    pub(super) fn fetch_failed(effect: Effect, error: &str) -> EffectResult {
        EffectResult::failed(effect, error)
    }

    /// A store error is a failure unless the step is suspended on host I/O, where it only says "no answer yet".
    pub(super) fn log_store_failure(&self, effect: &str, entity_type: &str, error: &str) {
        let suspended = self.persistence.as_ref().is_some_and(|p| p.suspended())
            || self.fetch_handler.as_ref().is_some_and(|f| f.suspended());
        if suspended {
            debug!(entity_type = entity_type, "{effect} effect suspended on host I/O");
        } else {
            error!(entity_type = entity_type, error = %error, "{effect} effect failed");
        }
    }

    pub(super) fn do_fetch(
        &self,
        entity_type: &str,
        id: Option<&Value>,
        filter: Option<&Value>,
        limit: Option<&Value>,
        offset: Option<&Value>,
        order_by: Option<&Value>,
        include: Option<&Vec<String>>,
        deliver: FetchDeliver,
        total_mode: RowTotalMode,
        nearest: Option<&Value>,
        ctx: &BindingContext,
    ) -> EffectResult {
        let effect = Effect::Fetch {
            entity_type: entity_type.to_string(),
            id: id.cloned(),
            filter: filter.cloned(),
            limit: limit.cloned(),
            offset: offset.cloned(),
            order_by: order_by.cloned(),
            include: include.cloned(),
            deliver,
            total: (total_mode == RowTotalMode::None).then_some(RowTotalMode::None),
            nearest: nearest.cloned(),
            emit: None,
        };
        let counted = |total: usize| (total_mode == RowTotalMode::Exact).then_some(total);

        if let Some(fetch_handler) = &self.fetch_handler {
            let nearest = match self.resolve_nearest(nearest, id, order_by, limit, offset, ctx) {
                Ok(nearest) => nearest,
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    return Self::fetch_failed(effect, &e);
                }
            };
            let resolved = self.resolve_fetch_id(id, ctx).and_then(|id| {
                Ok((
                    id,
                    self.resolve_limit(limit, ctx)?,
                    self.resolve_page_option(offset, ctx)?,
                ))
            });
            let order = match self.resolve_order_spec(order_by, ctx) {
                Ok(order) => order,
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    return Self::fetch_failed(effect, &e);
                }
            };
            let (resolved_id, limit, offset) = match resolved {
                Ok(v) => v,
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    return Self::fetch_failed(effect, &e);
                }
            };
            let interpolated_filter = filter.map(|f| interpolate_filter_trait_refs(lower_object_filter(f).as_ref(), ctx));
            if deliver == FetchDeliver::Ids {
                if resolved_id.is_some() || include.is_some_and(|i| !i.is_empty()) {
                    let e = format!(
                        "{entity_type}: `deliver: ids` applies to a collection fetch with no `id` or `include`"
                    );
                    error!(entity_type = entity_type, error = %e, "Fetch effect failed");
                    return Self::fetch_failed(effect, &e);
                }
                return match fetch_handler.fetch_row_set(
                    entity_type,
                    interpolated_filter.as_ref(),
                    order.as_ref(),
                    limit,
                    offset,
                    total_mode,
                    nearest.as_deref(),
                    ctx,
                ) {
                    Ok(reference) => {
                        info!(
                            entity_type = entity_type,
                            count = reference.ids.len(),
                            total = reference.total,
                            "Fetch effect delivered by reference"
                        );
                        EffectResult::executed_row_set(effect, &reference, counted(reference.total))
                    }
                    Err(e) => {
                        self.log_store_failure("Fetch", entity_type, &e);
                        Self::fetch_failed(effect, &e)
                    }
                };
            }
            let fetched = fetch_handler.fetch_ordered(
                entity_type,
                resolved_id.as_deref(),
                interpolated_filter.as_ref(),
                order.as_ref(),
                limit,
                offset,
                include,
                total_mode,
                nearest.as_deref(),
                ctx,
            );
            match fetched {
                // A by-id miss (absent or `@read`-refused alike) is a failure, as `EffectExecutor.ts`.
                Ok(page) if page.rows.is_empty() && resolved_id.is_some() => {
                    let e = format!(
                        "{entity_type} {} not found",
                        resolved_id.as_deref().unwrap_or_default()
                    );
                    EffectResult::failed(effect, &e).with_code(ServerEffectCode::NotFound)
                }
                Ok(FetchPage { rows, total }) => {
                    info!(
                        entity_type = entity_type,
                        count = rows.len(),
                        total,
                        "Fetch effect executed"
                    );
                    EffectResult::executed_fetch(effect, Value::array(rows), entity_type, counted(total))
                }
                Err(e) => {
                    self.log_store_failure("Fetch", entity_type, &e);
                    Self::fetch_failed(effect, &e)
                }
            }
        } else {
            EffectResult::noop(effect, "no fetch handler configured")
        }
    }
}
