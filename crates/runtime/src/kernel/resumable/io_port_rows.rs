//! `IoPort` as the kernel's entity source: row asks.

use super::*;

impl EntitySource for IoPort {
    fn read(&self, entity_type: &str, id: &str) -> Result<Arc<Value>, String> {
        match self.find(entity_type, id)? {
            Some(row) => Ok(row),
            None => Err(format!("Entity not found: {entity_type}:{id}")),
        }
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Arc<Value>>, String> {
        if let Some(read) = self.read_before(entity_type, id) {
            return Ok(read);
        }
        match self.ask(|| IoRequest::ReadRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
        }) {
            Asked::Direct => self.source_for(entity_type).find(entity_type, id),
            Asked::Answer(IoAnswer::Row(Ok(Value::Null))) => {
                self.remember_read(entity_type, id, None);
                Ok(None)
            }
            Asked::Answer(IoAnswer::Row(row)) => row.map(|r| {
                let row = Arc::new(r);
                self.remember_read(entity_type, id, Some(row.clone()));
                Some(row)
            }),
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let update = || RowWriteOp::Update {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
            data: data.clone(),
            guard: None,
        };
        if let Some(outcome) = self.bound_single_write(entity_type, update) {
            return match outcome? {
                RowWriteOutcome::Updated { row, .. } => Ok(row),
                RowWriteOutcome::NotFound => Err(format!("Entity not found: {entity_type}:{id}")),
                other => Err(format!("the store driver answered an update with {other:?}")),
            };
        }
        match self.ask(|| IoRequest::WriteRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
            data: data.clone(),
        }) {
            Asked::Direct => self.source_for(entity_type).write(entity_type, id, data),
            Asked::Answer(IoAnswer::Row(row)) => {
                self.forget_reads(entity_type);
                row
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let minted = self.with_minted_id(entity_type, data)?;
        let create = || RowWriteOp::Create { entity_type: entity_type.to_string(), data: minted.as_ref().clone() };
        if let Some(outcome) = self.bound_single_write(entity_type, create) {
            return match outcome? {
                RowWriteOutcome::Created { row } => Ok(row),
                RowWriteOutcome::DuplicateId => Err(format!("Entity already exists: {entity_type}")),
                other => Err(format!("the store driver answered a create with {other:?}")),
            };
        }
        match self.ask(|| IoRequest::CreateRow {
            entity_type: entity_type.to_string(),
            data: data.clone(),
        }) {
            Asked::Direct => self.source_for(entity_type).create(entity_type, data),
            Asked::Answer(IoAnswer::Row(row)) => {
                self.forget_reads(entity_type);
                row
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let delete = || RowWriteOp::Delete { entity_type: entity_type.to_string(), id: id.to_string(), guard: None };
        if let Some(outcome) = self.bound_single_write(entity_type, delete) {
            return match outcome? {
                RowWriteOutcome::Deleted { .. } => Ok(()),
                RowWriteOutcome::NotFound => Err(format!("Entity not found: {entity_type}:{id}")),
                other => Err(format!("the store driver answered a delete with {other:?}")),
            };
        }
        match self.ask(|| IoRequest::DeleteRow {
            entity_type: entity_type.to_string(),
            id: id.to_string(),
        }) {
            Asked::Direct => self.source_for(entity_type).delete(entity_type, id),
            Asked::Answer(IoAnswer::Deleted(done)) => {
                self.forget_reads(entity_type);
                done
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn clear(&self) {
        self.source.clear();
        self.memory.clear();
    }

    fn count(&self) -> usize {
        self.source.count() + self.memory.count()
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String> {
        match self.ask(|| IoRequest::ListRows {
            entity_type: entity_type.to_string(),
        }) {
            Asked::Direct => self.source_for(entity_type).list(entity_type),
            Asked::Answer(IoAnswer::Rows(rows)) => rows,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn scan(
        &self,
        entity_type: &str,
        visit: &mut dyn FnMut(&Arc<Value>) -> ScanControl,
    ) -> Result<(), String> {
        if self.direct() {
            if self.store_bound(entity_type) {
                return Err(self.store_off_path(entity_type));
            }
            return self.source_for(entity_type).scan(entity_type, visit);
        }
        for row in self.list(entity_type)? {
            if visit(&row) == ScanControl::Stop {
                break;
            }
        }
        Ok(())
    }

    fn query_page(&self, entity_type: &str, query: &RowQuery) -> QueryAnswer<RowPage> {
        if self.direct() {
            if self.store_bound(entity_type) {
                return QueryAnswer::Failed(self.store_off_path(entity_type));
            }
            return self.source_for(entity_type).query_page(entity_type, query);
        }
        let capabilities = self.query_capabilities.read().get(entity_type).cloned();
        if let Some(capabilities) = capabilities {
            match query_unsupported_reason(&capabilities, query) {
                Some(reason) => self.notice(
                    "kernel.fetch.pushdown-fallback",
                    entity_type,
                    reason.as_str(),
                    "",
                ),
                None => match self.ask(|| IoRequest::QueryRows {
                    entity_type: entity_type.to_string(),
                    query: query.clone(),
                    project: RowProjection::Rows,
                }) {
                    Asked::Direct => return self.source_for(entity_type).query_page(entity_type, query),
                    Asked::Answer(IoAnswer::Page(page)) => {
                        return page
                            .and_then(|page| {
                                echo_checked(entity_type, query, &page)?;
                                Ok(page)
                            })
                            .into()
                    }
                    Asked::Answer(IoAnswer::QueryUnsupported(detail)) => self.notice(
                        "kernel.fetch.pushdown-fallback",
                        entity_type,
                        "host-declined",
                        &detail,
                    ),
                    Asked::Answer(_) | Asked::Withheld => {
                        return QueryAnswer::Failed(AWAITING_HOST.to_string())
                    }
                },
            }
        }
        let mut run = QueryRun::new(query);
        if let Err(e) = self.scan(entity_type, &mut |row| run.visit(row)) {
            return QueryAnswer::Failed(e);
        }
        run.finish().into()
    }

    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> QueryAnswer<RowIdPage> {
        if self.direct() {
            if self.store_bound(entity_type) {
                return QueryAnswer::Failed(self.store_off_path(entity_type));
            }
            return self.source_for(entity_type).query_ids(entity_type, query);
        }
        let capabilities = self.query_capabilities.read().get(entity_type).cloned();
        if let Some(capabilities) = capabilities {
            let reason = if capabilities.ids_only == Some(true) {
                query_unsupported_reason(&capabilities, query).map(|r| r.as_str())
            } else {
                Some("ids-only")
            };
            match reason {
                Some(reason) => {
                    self.notice("kernel.fetch.pushdown-fallback", entity_type, reason, "")
                }
                None => match self.ask(|| IoRequest::QueryRows {
                    entity_type: entity_type.to_string(),
                    query: query.clone(),
                    project: RowProjection::Ids,
                }) {
                    Asked::Direct => return self.source_for(entity_type).query_ids(entity_type, query),
                    Asked::Answer(IoAnswer::IdPage(page)) => return page.into(),
                    Asked::Answer(IoAnswer::QueryUnsupported(detail)) => self.notice(
                        "kernel.fetch.pushdown-fallback",
                        entity_type,
                        "host-declined",
                        &detail,
                    ),
                    Asked::Answer(_) | Asked::Withheld => {
                        return QueryAnswer::Failed(AWAITING_HOST.to_string())
                    }
                },
            }
        }
        let mut run = QueryRun::new(query);
        if let Err(e) = self.scan(entity_type, &mut |row| run.visit(row)) {
            return QueryAnswer::Failed(e);
        }
        run.finish_ids().into()
    }

    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        if self.local(op.entity_type()) {
            let one = RowWriteBatch { ops: vec![op.clone()], on_failure: RowWriteFailureMode::Stop };
            if let Some(written) = self.local_writes(&one) {
                return Some(written.and_then(|mut outcomes| {
                    outcomes.pop().ok_or_else(|| "the kernel answered a write with no outcome".to_string())
                }));
            }
        }
        if !matches!(op, RowWriteOp::Rekey { .. }) {
            if let Some(outcome) = self.bound_single_write(op.entity_type(), || op.clone()) {
                return Some(outcome);
            }
        }
        if self.direct() {
            if self.store_bound(op.entity_type()) {
                return Some(Err(self.store_off_path(op.entity_type())));
            }
            return self.source.write_where(op).or_else(|| {
                matches!(op, RowWriteOp::Rekey { .. })
                    .then(|| Err("rekey is unsupported by this store".to_string()))
            });
        }
        let batch = RowWriteBatch {
            ops: vec![op.clone()],
            on_failure: RowWriteFailureMode::Stop,
        };
        self.push_writes(&batch).map(|sent| {
            sent.and_then(|mut outcomes| {
                outcomes
                    .pop()
                    .ok_or_else(|| "the host answered a write with no outcome".to_string())
            })
        })
    }

    fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        if let Some(written) = self.local_writes(batch) {
            return Some(written);
        }
        if self.direct() {
            if let Some(op) = batch
                .ops
                .iter()
                .find(|op| self.store_bound(op.entity_type()))
            {
                return Some(Err(self.store_off_path(op.entity_type())));
            }
            return self.source.write_batch(batch).or_else(|| {
                batch
                    .ops
                    .iter()
                    .any(|op| matches!(op, RowWriteOp::Rekey { .. }))
                    .then(|| Err("rekey is unsupported by this store".to_string()))
            });
        }
        self.push_writes(batch)
    }

    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.collections.write().insert(entity_type.to_string(), collection.to_string());
        self.source.bind_collection(entity_type, collection)
    }

    fn declare_indexes(&self, entity_type: &str, indexes: &[orbital_core_base::schema::types::EntityIndex]) {
        self.source.declare_indexes(entity_type, indexes);
        self.memory.declare_indexes(entity_type, indexes);
    }
}
