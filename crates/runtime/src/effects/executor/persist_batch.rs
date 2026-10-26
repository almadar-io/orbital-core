//! Runs of persists and batches: preparing the pushed write, finishing it, and flushing.

use super::*;

impl EffectExecutor {
    /// Decide how one evaluated persist is sent to the store: as a conditional write (`Push`), refused here
    /// before any I/O (`Refused`, the failed result), or through the read-then-write path (`Legacy`).
    pub(super) fn prepare_push(
        &self,
        operation: PersistOperation,
        entity_type: &str,
        resolved_data: &Value,
        id: Option<&str>,
        ctx: &BindingContext,
        effect: Effect,
        view: &mut RunRows,
    ) -> PersistPrep {
        if self.persistence.is_none() {
            return PersistPrep::Legacy;
        }
        let scope = self.persist_scope(entity_type, operation);
        let data = Self::without_intrinsic(resolved_data, &scope.intrinsic);
        let refuse = |effect: Effect, e: String, code: Option<ServerEffectCode>| {
            let failed = EffectResult::failed(effect, &e);
            PersistPrep::Refused(Box::new(match code {
                Some(code) => failed.with_code(code),
                None => failed,
            }))
        };
        let data = match self.persistence.as_ref().map(|persistence| {
            self.with_derived(persistence.as_ref(), operation, entity_type, data, id, &scope, ctx, view)
        }) {
            Some(Ok(data)) => data,
            Some(Err(DerivedFault::Failed(message))) => {
                return refuse(effect, message, Some(ServerEffectCode::DerivedFailed))
            }
            Some(Err(DerivedFault::Store(message))) => return refuse(effect, message, None),
            None => return PersistPrep::Legacy,
        };
        let (op, row_id, guard) = match operation {
            PersistOperation::Read => return PersistPrep::Legacy,
            PersistOperation::Create | PersistOperation::Upsert => {
                let Some(row_id) = Self::row_id_of(&data).map(str::to_string) else {
                    return PersistPrep::Legacy;
                };
                if operation == PersistOperation::Upsert
                    && self
                        .persist_scope(entity_type, PersistOperation::Update)
                        .policy
                        .is_some()
                {
                    return PersistPrep::Legacy;
                }
                let mut refused_code = None;
                if let Err(denial) = Self::check_create(entity_type, &data, &scope, ctx, &mut refused_code) {
                    return refuse(effect, denial, refused_code);
                }
                let op = if operation == PersistOperation::Create {
                    RowWriteOp::Create {
                        entity_type: entity_type.to_string(),
                        data: data.clone(),
                    }
                } else {
                    RowWriteOp::Upsert {
                        entity_type: entity_type.to_string(),
                        data: data.clone(),
                    }
                };
                (op, row_id, None)
            }
            PersistOperation::Update | PersistOperation::Delete => {
                let row_id = if operation == PersistOperation::Update {
                    self.resolve_update_id(id, &data, ctx)
                } else {
                    self.resolve_id(id, ctx).and_then(|row_id| {
                        if row_id.is_empty() {
                            Err(NO_ROW_KEY.to_string())
                        } else {
                            Ok(row_id)
                        }
                    })
                };
                let row_id = match row_id {
                    Ok(row_id) => row_id,
                    Err(e) => return refuse(effect, e, Some(ServerEffectCode::NoRowKey)),
                };
                let Ok(guard) =
                    Self::write_guard(scope.policy.as_ref(), ctx, &scope.declared_required)
                else {
                    return PersistPrep::Legacy;
                };
                let op = if operation == PersistOperation::Update {
                    RowWriteOp::Update {
                        entity_type: entity_type.to_string(),
                        id: row_id.clone(),
                        data: data.clone(),
                        guard: guard.clone(),
                    }
                } else {
                    RowWriteOp::Delete {
                        entity_type: entity_type.to_string(),
                        id: row_id.clone(),
                        guard: guard.clone(),
                    }
                };
                (op, row_id, guard)
            }
        };
        PersistPrep::Push(Box::new(PreparedWrite {
            effect,
            operation,
            entity_type: entity_type.to_string(),
            id: id.map(str::to_string),
            resolved_data: resolved_data.clone(),
            row_id,
            op,
            guard,
        }))
    }

    /// The persist result of the outcome the store answered for a prepared write.
    pub(super) fn finish_push(prepared: &PreparedWrite, outcome: RowWriteOutcome) -> EffectResult {
        let mut code: Option<ServerEffectCode> = None;
        match Self::outcome_value(
            outcome,
            prepared.operation,
            &prepared.entity_type,
            &prepared.row_id,
            prepared.guard.as_ref(),
            &mut code,
        ) {
            Ok(value) => {
                info!(entity_type = %prepared.entity_type, operation = ?prepared.operation, "Persist effect executed");
                EffectResult::executed_with_value(prepared.effect.clone(), value)
            }
            Err(e) => {
                error!(entity_type = %prepared.entity_type, error = %e, "Persist effect failed");
                let failed = EffectResult::failed(prepared.effect.clone(), &e);
                match code {
                    Some(code) => failed.with_code(code),
                    None => failed,
                }
            }
        }
    }

    /// Run consecutive top-level `persist` effects of one transition as a write run. Plain persists the
    /// store can run as conditional writes go out as ONE `stop` batch; a persist refused here before any I/O,
    /// or one the store cannot push, takes its own path in place. A persist failure is a handled outcome (EXEC-STEP-7),
    /// so after a failure the rest of the run is sent again as a new request: the effects run exactly as they
    /// would one at a time. Results come back in effect order.
    pub fn execute_persist_run(
        &self,
        effects: &[Effect],
        ctx: &BindingContext,
    ) -> Vec<EffectResult> {
        if self.environment.is_client() || self.persistence.is_none() {
            return effects.iter().map(|e| self.execute(e, ctx)).collect();
        }
        let mut out: Vec<Option<EffectResult>> = vec![None; effects.len()];
        let mut pending: Vec<(usize, PreparedWrite)> = Vec::new();
        let mut run_rows = RunRows::new();
        for (k, effect) in effects.iter().enumerate() {
            let Effect::Persist {
                operation,
                entity_type,
                data,
                id,
                ..
            } = effect
            else {
                self.flush_writes(&mut pending, &mut out, ctx);
                out[k] = Some(self.execute(effect, ctx));
                continue;
            };
            let resolved = self.resolve_persist_data(data, ctx);
            let mut result_effect = effect.clone();
            if let Effect::Persist { emit, .. } = &mut result_effect {
                *emit = None;
            }
            match self.prepare_push(
                *operation,
                entity_type,
                resolved.as_ref(),
                id.as_deref(),
                ctx,
                result_effect.clone(),
                &mut run_rows,
            ) {
                PersistPrep::Push(prepared) => pending.push((k, *prepared)),
                PersistPrep::Refused(result) => out[k] = Some(*result),
                PersistPrep::Legacy => {
                    self.flush_writes(&mut pending, &mut out, ctx);
                    out[k] = Some(self.persist_resolved(
                        *operation,
                        entity_type,
                        resolved.as_ref(),
                        id.as_deref(),
                        ctx,
                        result_effect,
                    ));
                }
            }
        }
        self.flush_writes(&mut pending, &mut out, ctx);
        out.into_iter()
            .zip(effects)
            .map(|(result, effect)| {
                result.unwrap_or_else(|| {
                    EffectResult::failed(effect.clone(), "the persist was never run")
                })
            })
            .collect()
    }

    /// Send the pending prepared writes; see [`Self::execute_persist_run`].
    pub(super) fn flush_writes(
        &self,
        pending: &mut Vec<(usize, PreparedWrite)>,
        out: &mut [Option<EffectResult>],
        ctx: &BindingContext,
    ) {
        let Some(persistence) = &self.persistence else {
            return;
        };
        while !pending.is_empty() {
            let batch = RowWriteBatch {
                ops: pending.iter().map(|(_, p)| p.op.clone()).collect(),
                on_failure: RowWriteFailureMode::Stop,
            };
            match persistence.write_batch(&batch) {
                None => {
                    for (k, p) in pending.drain(..) {
                        out[k] = Some(self.persist_resolved(
                            p.operation,
                            &p.entity_type,
                            &p.resolved_data,
                            p.id.as_deref(),
                            ctx,
                            p.effect.clone(),
                        ));
                    }
                }
                Some(Err(e)) => {
                    for (k, p) in pending.drain(..) {
                        error!(entity_type = %p.entity_type, error = %e, "Persist effect failed");
                        out[k] = Some(EffectResult::failed(p.effect, &e));
                    }
                }
                Some(Ok(outcomes)) if outcomes.is_empty() => {
                    for (k, p) in pending.drain(..) {
                        let e = "the host answered a write run with no outcome";
                        out[k] = Some(EffectResult::failed(p.effect, e));
                    }
                }
                Some(Ok(outcomes)) => {
                    let attempted = outcomes.len().min(pending.len());
                    for ((k, p), outcome) in pending.drain(..attempted).zip(outcomes) {
                        out[k] = Some(Self::finish_push(&p, outcome));
                    }
                }
            }
        }
    }

    /// `persist batch`: evaluate `ops` to an array, check every op's shape and local gates (required column,
    /// `@create`, row key) before anything is written, then run the ops in order. A store that runs conditional
    /// batches applies them ALL or NONE; on a failure it rolls the earlier ops back. A store that cannot takes
    /// each op through [`Self::persist_resolved`] and stops at the first failure with earlier ops written.
    /// Success returns `{ operations, completedCount, totalCount }`, the shape
    /// `emit_contract::PERSIST_BATCH_FIELDS` declares.
    pub(super) fn do_persist_batch(&self, ops: &Value, ctx: &BindingContext) -> EffectResult {
        let effect = Effect::PersistBatch {
            ops: ops.clone(),
            emit: None,
        };
        let Some(persistence) = self.persistence.clone() else {
            return EffectResult::noop(effect, "no persistence handler configured");
        };
        let expr: SExpression = ops.clone().into();
        let evaluated = match Evaluator::new().evaluate(&expr, ctx) {
            Ok(v) => v,
            Err(err) => {
                return EffectResult::failed(
                    effect,
                    &format!("persist batch operations failed to evaluate: {err}"),
                )
            }
        };
        let Value::Array(items) = evaluated else {
            return EffectResult::failed(
                effect,
                "persist batch operations must evaluate to an array",
            );
        };
        if items.is_empty() {
            return EffectResult::failed(effect, "Batch requires a non-empty operations array");
        }
        let total = items.len();
        let mut parsed: Vec<BatchOp> = Vec::with_capacity(total);
        for item in &items {
            let tuple = match item {
                Value::Array(t) => t.as_slice(),
                other => {
                    return EffectResult::failed(
                        effect,
                        &format!("Invalid batch operation format: {other:?}"),
                    )
                }
            };
            let action = tuple.first().and_then(Value::as_str).unwrap_or("");
            let entity = tuple.get(1).and_then(Value::as_str).unwrap_or("");
            let arity = match action {
                "create" | "delete" => 3,
                "update" => 4,
                _ => {
                    return EffectResult::failed(
                        effect,
                        &format!("Unknown batch operation action: {action}"),
                    )
                }
            };
            if tuple.len() != arity || entity.is_empty() {
                return EffectResult::failed(
                    effect,
                    &format!("Invalid batch operation format: {item:?}"),
                );
            }
            let (operation, row_id, data): (PersistOperation, Option<String>, Value) = match action
            {
                "create" => (PersistOperation::Create, None, tuple[2].clone()),
                "update" => (
                    PersistOperation::Update,
                    tuple[2].as_str().map(str::to_string),
                    tuple[3].clone(),
                ),
                _ => (
                    PersistOperation::Delete,
                    tuple[2].as_str().map(str::to_string),
                    Value::Null,
                ),
            };
            parsed.push(BatchOp {
                action: action.to_string(),
                entity: entity.to_string(),
                operation,
                row_id,
                data,
            });
        }

        let op_effect = |b: &BatchOp| Effect::Persist {
            operation: b.operation,
            entity_type: b.entity.clone(),
            data: b.data.clone(),
            id: b.row_id.clone(),
            emit: None,
            guard: None,
        };
        let batch_failure = |k: usize, b: &BatchOp, detail: &str, code: Option<ServerEffectCode>| {
            let failed = EffectResult::failed(
                effect.clone(),
                &format!(
                    "Batch operation [{}, {}] failed: {detail}",
                    b.action, b.entity
                ),
            )
            .with_batch_failure(k, k);
            match code {
                Some(code) => failed.with_code(code),
                None => failed,
            }
        };

        let mut prepared: Vec<PreparedWrite> = Vec::with_capacity(total);
        let mut pushable = true;
        let mut batch_rows = RunRows::new();
        for (k, b) in parsed.iter().enumerate().filter(|_| pushable) {
            match self.prepare_push(
                b.operation,
                &b.entity,
                &b.data,
                b.row_id.as_deref(),
                ctx,
                op_effect(b),
                &mut batch_rows,
            ) {
                PersistPrep::Push(p) => prepared.push(*p),
                PersistPrep::Refused(refused) => {
                    let detail = refused.error.clone().unwrap_or_else(|| "not executed".to_string());
                    return batch_failure(k, b, &detail, refused.code);
                }
                PersistPrep::Legacy => {
                    pushable = false;
                    break;
                }
            }
        }

        let mut completed: Vec<Value> = Vec::with_capacity(total);
        if pushable {
            let batch = RowWriteBatch {
                ops: prepared.iter().map(|p| p.op.clone()).collect(),
                on_failure: RowWriteFailureMode::Atomic,
            };
            match persistence.write_batch(&batch) {
                Some(Err(e)) => {
                    return EffectResult::failed(effect, &format!("persist batch failed: {e}"))
                }
                Some(Ok(outcomes)) => {
                    let mut seen = 0;
                    for (k, ((b, p), outcome)) in parsed.iter().zip(&prepared).zip(outcomes).enumerate() {
                        seen += 1;
                        if outcome == RowWriteOutcome::RolledBack {
                            continue;
                        }
                        let result = Self::finish_push(p, outcome);
                        if !result.is_executed() {
                            let detail = result.error.clone().unwrap_or_else(|| "not executed".to_string());
                            return batch_failure(k, b, &detail, result.code);
                        }
                        completed.push(batch_entry(b, &result));
                    }
                    if seen < total {
                        return EffectResult::failed(
                            effect,
                            "persist batch failed: the host answered fewer outcomes than operations",
                        );
                    }
                    return batch_summary(effect, completed, total);
                }
                None => {}
            }
        }

        // The store cannot run the batch atomically: each applied op keeps its pre-image so a later failure can
        // compensate. Not isolation: a concurrent reader may see the intermediate rows (D-A22).
        let mut applied: Vec<(usize, Option<Value>, Option<String>)> = Vec::new();
        for (k, b) in parsed.iter().enumerate() {
            let pre = match (b.operation, b.row_id.as_deref()) {
                (PersistOperation::Update | PersistOperation::Delete, Some(id)) => {
                    persistence.find(&b.entity, id).ok().flatten()
                }
                _ => None,
            };
            let result = self.persist_resolved(
                b.operation,
                &b.entity,
                &b.data,
                b.row_id.as_deref(),
                ctx,
                op_effect(b),
            );
            if !result.is_executed() {
                let detail = result.error.clone().unwrap_or_else(|| "not executed".to_string());
                if !applied.is_empty() {
                    warn!(
                        applied = applied.len(),
                        "kernel.write.batch-compensated: restoring the pre-images of the ops applied before the failure"
                    );
                    for (done, pre, created) in applied.iter().rev() {
                        let op = &parsed[*done];
                        let restored = match (op.operation, pre, created) {
                            (PersistOperation::Create, _, Some(id)) => {
                                persistence.delete(&op.entity, id)
                            }
                            (PersistOperation::Update, Some(row), _) => {
                                let mut restore = row.as_object().cloned().unwrap_or_default();
                                if let Some(data) = op.data.as_object() {
                                    for key in data.keys() {
                                        restore.entry(key.clone()).or_insert(Value::Null);
                                    }
                                }
                                persistence
                                    .update(
                                        &op.entity,
                                        op.row_id.as_deref().unwrap_or(""),
                                        &Value::Object(restore),
                                    )
                                    .map(|_| ())
                            }
                            (PersistOperation::Delete, Some(row), _) => {
                                persistence.create(&op.entity, row).map(|_| ())
                            }
                            _ => Ok(()),
                        };
                        if let Err(e) = restored {
                            error!(entity_type = %op.entity, error = %e, "persist batch compensation failed");
                        }
                    }
                }
                return batch_failure(k, b, &detail, result.code);
            }
            let created = match (&result.return_value, b.operation) {
                (Some(Value::Object(row)), PersistOperation::Create) => {
                    row.get("id").and_then(Value::as_str).map(str::to_string)
                }
                _ => None,
            };
            applied.push((k, pre, created));
            completed.push(batch_entry(b, &result));
        }
        batch_summary(effect, completed, total)
    }
}
