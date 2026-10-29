//! `EntitySourceHandler`: the bridge from persist and fetch effects to the kernel's entity source.

use super::*;

/// Bridges the executor's Fetch and Persist effects to the kernel's entity
/// source so list views resolve real data and mutations actually land.
pub(super) struct EntitySourceHandler {
    pub(super) source: Arc<dyn EntitySource>,
    pub(super) io: Arc<IoPort>,
    pub(super) entity_policies: Arc<RwLock<EntityAccessTable>>,
    pub(super) entity_definitions: Arc<RwLock<HashMap<String, Arc<EntityDefinition>>>>,
    /// Entity types the kernel has written since the render store last heard (EXEC-RENDER-3).
    pub(super) written: Arc<orbital_core_base::sync::Mutex<BTreeSet<String>>>,
}

impl EntitySourceHandler {
    pub(super) fn note_write(&self, entity_type: &str) {
        self.written.lock().insert(entity_type.to_string());
    }
}

impl PersistenceHandler for EntitySourceHandler {
    fn suspended(&self) -> bool {
        self.io.awaiting_host()
    }
    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let created = self.source.create(entity_type, data)?;
        self.note_write(entity_type);
        Ok(created)
    }
    fn read(&self, entity_type: &str, id: &str) -> Result<Value, String> {
        self.source.read(entity_type, id).map(|row| (*row).clone())
    }
    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Value>, String> {
        self.source
            .find(entity_type, id)
            .map(|row| row.map(|r| (*r).clone()))
    }
    fn update(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let written = self.source.write(entity_type, id, data)?;
        self.note_write(entity_type);
        Ok(written)
    }
    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        self.source.delete(entity_type, id)?;
        self.note_write(entity_type);
        Ok(())
    }
    fn list(&self, entity_type: &str) -> Result<Vec<Value>, String> {
        self.source
            .list(entity_type)
            .map(|rows| rows.iter().map(|row| (**row).clone()).collect())
    }
    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        let written = self.source.write_where(op);
        if let Some(Ok(outcome)) = &written {
            if outcome.applied() {
                self.note_write(op.entity_type());
            }
        }
        written
    }
    fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        let written = self.source.write_batch(batch);
        if let Some(Ok(outcomes)) = &written {
            for (op, outcome) in batch.ops.iter().zip(outcomes) {
                if outcome.applied() {
                    self.note_write(op.entity_type());
                }
            }
        }
        written
    }
}

impl EntitySourceHandler {
    /// The collection fetch as a [`RowQuery`], or why it is not expressible (the interpreter runs it).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn compile_fetch(
        policy: Option<&Value>,
        filter: Option<&Value>,
        order: Option<&RowOrder>,
        limit: Option<usize>,
        offset: Option<usize>,
        total: RowTotalMode,
        nearest: Option<&RowNearest>,
        ctx: &BindingContext,
        required: &dyn Fn(&str) -> bool,
    ) -> Result<RowQuery, NotExpressible> {
        if let Some(order) = order {
            if order.field.contains('.') || order.field.contains('[') {
                return Err(NotExpressible::Shape(
                    "an order field that is a path".to_string(),
                ));
            }
        }
        Ok(RowQuery {
            policy: policy
                .map(|p| compile_predicate(p, ctx, PredicateRole::Policy, required))
                .transpose()?,
            filter: filter
                .map(|f| compile_predicate(f, ctx, PredicateRole::Filter, required))
                .transpose()?,
            order: order.cloned(),
            offset,
            limit,
            total: (total == RowTotalMode::None).then_some(RowTotalMode::None),
            nearest: nearest.map(|n| Box::new(n.clone())),
        })
    }

    /// The fields `entity_type`'s predicates may compare as present: `id` and the entity's required ones.
    pub(super) fn required_fields_of(&self, entity_type: &str) -> HashSet<String> {
        self.entity_policies
            .read()
            .get(entity_type)
            .map(|p| p.required_fields.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// `reference` resolved against the rows as they are now, for the viewer `ctx` carries: the ids that still
    /// match the reference's query and the viewer's `@read`, in the reference's order, with `total` re-counted over
    /// the query's matches (ignoring order and window). A row that is gone or now hidden is omitted. The two reads
    /// are the same `QueryRows` a fetch issues, so the store's capabilities and the `@read` policy apply exactly
    /// as they do there.
    pub(super) fn resolve_rows(&self, reference: &RowSetRef, ctx: &BindingContext) -> Result<RowPage, String> {
        let entity_type = reference.entity_type.as_str();
        let policy = self
            .entity_policies
            .read()
            .get(entity_type)
            .and_then(|p| p.read.clone());
        let required_fields = self.required_fields_of(entity_type);
        let required = |field: &str| field == "id" || required_fields.contains(field);
        let viewer_policy = policy
            .as_ref()
            .map(|p| compile_predicate(p, ctx, PredicateRole::Policy, &required))
            .transpose();
        let Ok(viewer_policy) = viewer_policy else {
            return self.resolve_rows_interpreted(reference, policy.as_ref(), ctx);
        };
        let combined = and_predicates(reference.query.policy.clone(), viewer_policy);
        let member = RowPredicate::Member {
            field: "id".to_string(),
            values: reference
                .ids
                .iter()
                .map(|id| Value::string(id.clone()))
                .collect(),
        };
        let uncounted =
            reference.query.total == Some(RowTotalMode::None) || reference.query.nearest.is_some();
        let wanted = RowQuery {
            policy: combined.clone(),
            filter: Some(
                and_predicates(reference.query.filter.clone(), Some(member))
                    .unwrap_or(RowPredicate::Const { value: true }),
            ),
            total: Some(RowTotalMode::None),
            ..RowQuery::default()
        };
        let count = RowQuery {
            policy: combined,
            filter: reference.query.filter.clone(),
            limit: Some(0),
            ..RowQuery::default()
        };
        let rows = match self.source.query_page(entity_type, &wanted) {
            QueryAnswer::Rows(rows) => rows,
            QueryAnswer::Declined(reason) => {
                log_store_declined(entity_type, &reason);
                return self.resolve_rows_interpreted(reference, policy.as_ref(), ctx);
            }
            QueryAnswer::Failed(message) => return Err(message),
        };
        let total = if uncounted {
            None
        } else {
            match self.source.query_page(entity_type, &count) {
                QueryAnswer::Rows(counted) => Some(counted.total),
                QueryAnswer::Declined(reason) => {
                    log_store_declined(entity_type, &reason);
                    return self.resolve_rows_interpreted(reference, policy.as_ref(), ctx);
                }
                QueryAnswer::Failed(message) => return Err(message),
            }
        };
        let by_id: HashMap<String, &Value> = rows
            .rows
            .iter()
            .map(|row| (super::row_query::row_id(row), row))
            .collect();
        let rows: Vec<Value> = reference
            .ids
            .iter()
            .filter_map(|id| by_id.get(id).map(|row| (*row).clone()))
            .collect();
        Ok(RowPage {
            total: total.unwrap_or(rows.len()),
            rows,
        })
    }

    /// [`Self::resolve_rows`] for a `@read` policy that is not a row predicate: the table is listed and the
    /// interpreter applies the policy.
    pub(super) fn resolve_rows_interpreted(
        &self,
        reference: &RowSetRef,
        policy: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<RowPage, String> {
        let entity_type = reference.entity_type.as_str();
        let mut visible: Vec<Value> = Vec::new();
        self.source.scan(entity_type, &mut |row| {
            visible.push((**row).clone());
            super::row_query::ScanControl::Continue
        })?;
        apply_read_access(&mut visible, policy, None, ctx, entity_type);
        let visible: Vec<Arc<Value>> = visible.into_iter().map(Arc::new).collect();
        let mut page = resolve_row_set(&visible, reference, None);
        if reference.query.total == Some(RowTotalMode::None) {
            page.total = page.rows.len();
        }
        Ok(page)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn fetch_rows(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        order: Option<&RowOrder>,
        limit: Option<usize>,
        offset: Option<usize>,
        include: Option<&Vec<String>>,
        total_mode: RowTotalMode,
        nearest: Option<&RowNearest>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        let policy = self
            .entity_policies
            .read()
            .get(entity_type)
            .and_then(|p| p.read.clone());
        let mut tier = "interpreted";
        let mut compiled: Option<FetchPage> = None;
        // EXEC-FETCH-1: a collection fetch with no authored order is ordered by `createdAt`, then `id`.
        let default_order = default_row_order();
        let plain_order = match order {
            None if id.is_none() && nearest.is_none() => Some(&default_order),
            other => other,
        };
        if id.is_none() {
            let required_fields: Vec<String> = self
                .entity_policies
                .read()
                .get(entity_type)
                .map(|p| p.required_fields.clone())
                .unwrap_or_default();
            let required_fields: HashSet<String> = required_fields.into_iter().collect();
            let required = |field: &str| field == "id" || required_fields.contains(field);
            match Self::compile_fetch(
                policy.as_ref(),
                filter,
                order,
                limit,
                offset,
                total_mode,
                nearest,
                ctx,
                &required,
            ) {
                Ok(query) => {
                    tier = match self.io.query_capabilities_of(entity_type) {
                        Some(caps) if query_unsupported_reason(&caps, &query).is_none() => "pushed",
                        _ => "compiled",
                    };
                    match self.source.query_page(entity_type, &query) {
                        QueryAnswer::Rows(page) => {
                            compiled = Some(FetchPage {
                                rows: page.rows,
                                total: page.total,
                            });
                        }
                        QueryAnswer::Declined(reason) => {
                            log_store_declined(entity_type, &reason);
                            tier = "interpreted";
                        }
                        QueryAnswer::Failed(message) => return Err(message),
                    }
                }
                Err(why) => {
                    tracing::warn!(
                        target: orbital_core_base::logging::ns::RUNTIME_FETCH,
                        entity = %entity_type,
                        reason = why.reason(),
                        detail = %why,
                        "kernel.fetch.pushdown-fallback",
                    );
                }
            }
        }
        tracing::debug!(
            target: orbital_core_base::logging::ns::RUNTIME_FETCH,
            entity = %entity_type,
            tier,
            "kernel.fetch.tier",
        );
        let FetchPage {
            rows: mut entities,
            total,
        } = match compiled {
            Some(page) => page,
            None => {
                let mut entities = match id {
                    Some(id) => match self.source.find(entity_type, id) {
                        Ok(Some(v)) if !v.is_null() => vec![(*v).clone()],
                        Ok(_) => Vec::new(),
                        Err(_) if self.io.suspended() => Vec::new(),
                        Err(failure) => return Err(failure),
                    },
                    None => self
                        .source
                        .list(entity_type)?
                        .iter()
                        .map(|row| (**row).clone())
                        .collect(),
                };
                tracing::debug!(
                    target: orbital_core_base::logging::ns::RUNTIME_FETCH,
                    entity = %entity_type,
                    id = ?id,
                    has_filter = filter.is_some(),
                    limit = ?limit,
                    returned = entities.len(),
                    "fetch",
                );
                // The env-agnostic entity source lists unfiltered, so the declared
                // predicate is applied here, through the shared owner every FetchHandler
                // uses (C-FETCH-FILTER-NOT-APPLIED-BY-ENTITY-SOURCE-HANDLER). The
                // entity's declared `@read` policy (if any) is ANDed in alongside the
                // call-site filter — the un-bypassable half of row visibility.
                let (window_limit, window_offset) = if plain_order.is_some() {
                    (None, None)
                } else {
                    (limit, offset)
                };
                let total = scope_fetched_rows(
                    &mut entities,
                    policy.as_ref(),
                    filter,
                    window_limit,
                    window_offset,
                    ctx,
                    entity_type,
                );
                if let Some(order) = plain_order {
                    entities = page_ordered_rows(entities, order, offset, limit);
                }
                match nearest {
                    Some(nearest) => {
                        let ranked =
                            nearest_rows(entities.into_iter().map(Arc::new).collect(), nearest)?;
                        let rows: Vec<Value> = ranked.iter().map(|row| (**row).clone()).collect();
                        FetchPage {
                            total: rows.len(),
                            rows,
                        }
                    }
                    None => FetchPage {
                        rows: entities,
                        total,
                    },
                }
            }
        };
        if let Some(include) = include.filter(|i| !i.is_empty()) {
            super::relations::populate_relations(
                &*self.source,
                &self.entity_definitions.read(),
                &mut entities,
                entity_type,
                include,
                0,
                &mut HashSet::new(),
            );
            if self.io.suspended() {
                return Err(super::resumable::AWAITING_HOST.to_string());
            }
        }
        Ok(FetchPage {
            rows: entities,
            total,
        })
    }
}

impl FetchHandler for EntitySourceHandler {
    fn suspended(&self) -> bool {
        self.io.awaiting_host()
    }
    #[allow(clippy::too_many_arguments)]
    fn fetch(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        limit: Option<usize>,
        offset: Option<usize>,
        include: Option<&Vec<String>>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        self.fetch_rows(
            entity_type,
            id,
            filter,
            None,
            limit,
            offset,
            include,
            RowTotalMode::Exact,
            None,
            ctx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_ordered(
        &self,
        entity_type: &str,
        id: Option<&str>,
        filter: Option<&Value>,
        order: Option<&RowOrder>,
        limit: Option<usize>,
        offset: Option<usize>,
        include: Option<&Vec<String>>,
        total: RowTotalMode,
        nearest: Option<&RowNearest>,
        ctx: &BindingContext,
    ) -> Result<FetchPage, String> {
        self.fetch_rows(
            entity_type,
            id,
            filter,
            order,
            limit,
            offset,
            include,
            total,
            nearest,
            ctx,
        )
    }

    fn fetch_row_set(
        &self,
        entity_type: &str,
        filter: Option<&Value>,
        order: Option<&RowOrder>,
        limit: Option<usize>,
        offset: Option<usize>,
        total: RowTotalMode,
        nearest: Option<&RowNearest>,
        ctx: &BindingContext,
    ) -> Result<RowSetRef, String> {
        let policy = self
            .entity_policies
            .read()
            .get(entity_type)
            .and_then(|p| p.read.clone());
        let required_fields = self.required_fields_of(entity_type);
        let required = |field: &str| field == "id" || required_fields.contains(field);
        let query = Self::compile_fetch(
            policy.as_ref(),
            filter,
            order,
            limit,
            offset,
            total,
            nearest,
            ctx,
            &required,
        )
        .map_err(|why| {
            format!(
                "{entity_type}: `deliver: ids` needs a fetch expressible as a row query ({why})"
            )
        })?;
        tracing::debug!(
            target: orbital_core_base::logging::ns::RUNTIME_FETCH,
            entity = %entity_type,
            tier = match self.io.query_capabilities_of(entity_type) {
                Some(caps) if query_unsupported_reason(&caps, &query).is_none() => "pushed",
                _ => "compiled",
            },
            "kernel.fetch.tier",
        );
        let page = match self.source.query_ids(entity_type, &query) {
            QueryAnswer::Rows(page) => page,
            QueryAnswer::Declined(reason) => {
                log_store_declined(entity_type, &reason);
                return Err(format!(
                    "{entity_type}: the store cannot answer a fetch by reference ({reason})"
                ));
            }
            QueryAnswer::Failed(message) => return Err(message),
        };
        Ok(RowSetRef {
            entity_type: entity_type.to_string(),
            ids: page.ids,
            total: page.total,
            query,
        })
    }
}

pub(super) fn log_store_declined(entity_type: &str, reason: &str) {
    tracing::debug!(
        target: orbital_core_base::logging::ns::RUNTIME_FETCH,
        entity = %entity_type,
        reason = %reason,
        "kernel.fetch.store-declined",
    );
}
