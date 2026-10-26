//! One persist: its scope, policy and derived fields, guards, and the create, update and upsert writes.

use super::*;

impl EffectExecutor {
    pub(super) fn do_persist(
        &self,
        operation: PersistOperation,
        entity_type: &str,
        data: &Value,
        id: Option<&str>,
        guard: Option<&Value>,
        ctx: &BindingContext,
    ) -> EffectResult {
        let effect = Effect::Persist {
            operation,
            entity_type: entity_type.to_string(),
            data: data.clone(),
            id: id.map(|s| s.to_string()),
            emit: None,
            guard: guard.cloned(),
        };

        if self.persistence.is_some() {
            // Resolve bindings in data before persisting
            let resolved_data = self.resolve_persist_data(data, ctx);
            self.persist_resolved(
                operation,
                entity_type,
                resolved_data.as_ref(),
                id,
                ctx,
                effect,
            )
        } else {
            EffectResult::noop(effect, "no persistence handler configured")
        }
    }

    /// The `where: (fn row body)` guard of a single persist as a predicate over the row: the lambda's parameter
    /// names the row, so `@row.f` reads field `f` (compiled by the one `compile_predicate`).
    pub(super) fn row_where(effect: &Effect, ctx: &BindingContext, declared_required: &[String]) -> Result<Option<RowPredicate>, String> {
        let Effect::Persist { guard: Some(guard), .. } = effect else {
            return Ok(None);
        };
        let lambda = guard.as_array().filter(|items| items.len() == 3 && items[0].as_str() == Some("fn"));
        let (param, body) = match lambda {
            Some(items) => match (&items[1], &items[2]) {
                (Value::String(param), body) => (param.clone(), body),
                (Value::Array(params), body) if params.len() == 1 && params[0].as_str().is_some() => {
                    (params[0].as_str().unwrap_or_default().to_string(), body)
                }
                _ => return Err("persist `where:` must be a one-parameter (fn row …)".to_string()),
            },
            None => return Err("persist `where:` must be a one-parameter (fn row …)".to_string()),
        };
        if reads_entity(body) {
            return Err("persist `where:` reads the row through its parameter, never `@entity`".to_string());
        }
        let body = bind_row_param(body, &format!("@{param}"));
        compile_predicate(&body, ctx, PredicateRole::Filter, &|f| f == "id" || declared_required.iter().any(|r| r == f))
            .map(Some)
            .map_err(|why| format!("persist `where:` is not a row predicate: {why:?}"))
    }

    /// A row the `where:` guard rejects is `not-found`, the same as an absent row.
    pub(super) fn where_holds(
        row_where: Option<&RowPredicate>,
        existing: Value,
        entity_type: &str,
        entity_id: &str,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        match row_where {
            Some(guard) if !guard.matches(&existing) => {
                *code = Some(ServerEffectCode::NotFound);
                Err(format!("{entity_type} {entity_id} not found"))
            }
            _ => Ok(existing),
        }
    }

    /// The row a persist update/delete addresses. An absent row is a failed write coded
    /// `not-found`; a store failure is a plain failure.
    pub(super) fn existing_row(
        persistence: &dyn PersistenceHandler,
        entity_type: &str,
        entity_id: &str,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        match persistence.find(entity_type, entity_id)? {
            Some(row) => Ok(row),
            None => {
                *code = Some(ServerEffectCode::NotFound);
                Err(format!("{entity_type} {entity_id} not found"))
            }
        }
    }

    /// The access directive, required columns and `@intrinsic` fields that govern one persist.
    pub(super) fn persist_scope(&self, entity_type: &str, operation: PersistOperation) -> PersistScope {
        let Some(table) = self.entity_policies.as_ref() else {
            return PersistScope::default();
        };
        let table = table.read();
        let Some(policies) = table.get(entity_type) else {
            return PersistScope::default();
        };
        PersistScope {
            policy: match operation {
                PersistOperation::Create | PersistOperation::Upsert => policies.create.clone(),
                PersistOperation::Update => policies.update.clone(),
                PersistOperation::Delete => policies.delete.clone(),
                PersistOperation::Read => None,
            },
            required_fields: policies.required_create_fields.clone(),
            declared_required: policies.required_fields.clone(),
            intrinsic: policies.intrinsic_fields.clone(),
            derived: policies.derived_fields.clone(),
        }
    }

    /// `data` with the entity's `@derived` fields computed from the row being written, so the policies, the
    /// required-column check and the store all see them. A create or upsert computes from the incoming row; an
    /// update computes from the row as it stands with the patch laid over it and adds the derived values to the
    /// patch. The row as it stands is the batch's own earlier write when `view` holds one, else the stored row, so
    /// a create followed by an update in one run recomputes from the created row. An expression that fails is a
    /// named failure: nothing is written.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn with_derived(
        &self,
        persistence: &dyn PersistenceHandler,
        operation: PersistOperation,
        entity_type: &str,
        data: Value,
        id: Option<&str>,
        scope: &PersistScope,
        ctx: &BindingContext,
        view: &mut RunRows,
    ) -> Result<Value, DerivedFault> {
        if scope.derived.is_empty() {
            return Ok(data);
        }
        match operation {
            PersistOperation::Create | PersistOperation::Upsert => {
                let computed = apply_derived(&scope.derived, &data, ctx, operation, entity_type)
                    .map_err(|f| DerivedFault::Failed(f.0))?;
                if let Some(row_id) = Self::row_id_of(&computed) {
                    view.insert((entity_type.to_string(), row_id.to_string()), Some(computed.clone()));
                }
                Ok(computed)
            }
            PersistOperation::Update => {
                let Ok(entity_id) = self.resolve_update_id(id, &data, ctx) else {
                    return Ok(data);
                };
                let key = (entity_type.to_string(), entity_id);
                let existing = match view.get(&key) {
                    Some(row) => row.clone(),
                    None => persistence.find(entity_type, &key.1).map_err(DerivedFault::Store)?,
                };
                let Some(existing) = existing else {
                    return Ok(data);
                };
                let mut merged = existing.as_object().cloned().unwrap_or_default();
                for (field, value) in data.as_object().cloned().unwrap_or_default() {
                    merged.insert(field, value);
                }
                let computed = apply_derived(&scope.derived, &Value::Object(merged), ctx, operation, entity_type)
                    .map_err(|f| DerivedFault::Failed(f.0))?;
                let mut patch = data.as_object().cloned().unwrap_or_default();
                for (name, _) in &scope.derived {
                    if let Some(value) = computed.as_object().and_then(|o| o.get(name)) {
                        patch.insert(name.clone(), value.clone());
                    }
                }
                view.insert(key, Some(computed));
                Ok(Value::Object(patch))
            }
            PersistOperation::Delete => {
                if let Ok(entity_id) = self.resolve_id(id, ctx) {
                    if !entity_id.is_empty() {
                        view.insert((entity_type.to_string(), entity_id), None);
                    }
                }
                Ok(data)
            }
            PersistOperation::Read => Ok(data),
        }
    }

    /// `@intrinsic` fields are the trait's frame, never a column: a write leaves them out.
    pub(super) fn without_intrinsic(resolved_data: &Value, intrinsic: &[String]) -> Value {
        match resolved_data {
            Value::Object(fields) if fields.keys().any(|k| intrinsic.contains(k)) => Value::Object(
                fields
                    .iter()
                    .filter(|(k, _)| !intrinsic.contains(k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// The checks of a `persist create` that read only the incoming data: required columns, then the
    /// `@create` directive.
    pub(super) fn check_create(
        entity_type: &str,
        data: &Value,
        scope: &PersistScope,
        ctx: &BindingContext,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<(), String> {
        let missing = missing_required_fields(&scope.required_fields, data);
        if !missing.is_empty() {
            return Err(format!(
                "persist create {entity_type}: required field(s) {} missing",
                missing.join(", ")
            ));
        }
        check_mutation_access(
            PersistOperation::Create,
            data,
            scope.policy.as_ref(),
            ctx,
            entity_type,
        )
        .map_err(|denied| Self::refused(denied, code))
    }

    /// An access policy refused a mutation: the failure is coded `denied` from the typed refusal.
    pub(super) fn refused(denied: crate::entity_access::AccessDenied, code: &mut Option<ServerEffectCode>) -> String {
        *code = Some(ServerEffectCode::Denied);
        denied.into_message()
    }

    /// What one write outcome means as a persist result. A guarded write must carry the pre-image it was
    /// tested on, and the guard must hold on it: a host that ignored the guard fails loudly here.
    pub(super) fn outcome_value(
        outcome: RowWriteOutcome,
        operation: PersistOperation,
        entity_type: &str,
        entity_id: &str,
        guard: Option<&RowPredicate>,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        if let Some(guard) = guard {
            if let RowWriteOutcome::Updated { before, .. } | RowWriteOutcome::Deleted { before } =
                &outcome
            {
                if !before.as_ref().is_some_and(|b| guard.matches(b)) {
                    error!(
                        entity_type = entity_type,
                        id = entity_id,
                        "the host applied a guarded write its guard rejects, or returned no pre-image"
                    );
                    return Err(format!(
                        "{entity_type} {entity_id}: the host applied a write whose guard does not hold on the row it replaced"
                    ));
                }
            }
        }
        match (outcome, operation) {
            (RowWriteOutcome::Created { row }, PersistOperation::Create | PersistOperation::Upsert)
            | (RowWriteOutcome::Updated { row, .. }, PersistOperation::Update | PersistOperation::Upsert) => Ok(row),
            (RowWriteOutcome::DuplicateId, _) => {
                *code = Some(ServerEffectCode::DuplicateId);
                Err(format!("{entity_type} {entity_id} already exists"))
            }
            (RowWriteOutcome::Deleted { .. }, PersistOperation::Delete) => Ok(Value::Null),
            (RowWriteOutcome::NotFound, _) => {
                *code = Some(ServerEffectCode::NotFound);
                Err(format!("{entity_type} {entity_id} not found"))
            }
            (RowWriteOutcome::Refused, _) => {
                *code = Some(ServerEffectCode::Denied);
                Err(format!(
                    "@{operation:?} denied: the declared access policy for `{entity_type}` rejected this row"
                ))
            }
            (RowWriteOutcome::Failed { message }, _) => Err(message),
            (RowWriteOutcome::RolledBack, _) => Err(format!(
                "{entity_type}: the write was rolled back with its batch"
            )),
            (other, _) => Err(format!(
                "{entity_type}: the host answered a {operation:?} with {other:?}"
            )),
        }
    }

    /// The guard of an `@update`/`@delete` directive as a predicate; `None` when no directive is declared.
    /// `Err` = the directive is not expressible or does not specialise: the caller takes the read path, which
    /// words the denial.
    pub(super) fn write_guard(
        policy: Option<&Value>,
        ctx: &BindingContext,
        declared_required: &[String],
    ) -> Result<Option<RowPredicate>, ()> {
        match policy {
            None => Ok(None),
            Some(policy) => compile_predicate(policy, ctx, PredicateRole::Filter, &|f| {
                f == "id" || declared_required.iter().any(|r| r == f)
            })
            .map(Some)
            .map_err(|_| ()),
        }
    }

    /// One `persist` the store may run as a conditional write: `None` = take the read-then-write path.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pushed_write(
        &self,
        persistence: &dyn PersistenceHandler,
        operation: PersistOperation,
        entity_type: &str,
        entity_id: &str,
        data: &Value,
        policy: Option<&Value>,
        declared_required: &[String],
        row_where: Option<&RowPredicate>,
        ctx: &BindingContext,
        code: &mut Option<ServerEffectCode>,
    ) -> Option<Result<Value, String>> {
        let policy_guard = Self::write_guard(policy, ctx, declared_required).ok()?;
        let has_policy_guard = policy_guard.is_some();
        let guard = match (policy_guard, row_where) {
            (None, None) => None,
            (Some(g), None) => Some(g),
            (None, Some(w)) => Some(w.clone()),
            (Some(g), Some(w)) => Some(RowPredicate::All { of: vec![g, w.clone()] }),
        };
        let op = match operation {
            PersistOperation::Create | PersistOperation::Upsert | PersistOperation::Read => {
                return None
            }
            PersistOperation::Update => RowWriteOp::Update {
                entity_type: entity_type.to_string(),
                id: entity_id.to_string(),
                data: data.clone(),
                guard: guard.clone(),
            },
            PersistOperation::Delete => RowWriteOp::Delete {
                entity_type: entity_type.to_string(),
                id: entity_id.to_string(),
                guard: guard.clone(),
            },
        };
        let mut outcome = match persistence.write_where(&op)? {
            Ok(outcome) => outcome,
            Err(e) => return Some(Err(e)),
        };
        if let (RowWriteOutcome::Refused, Some(row_where)) = (&outcome, row_where) {
            let rejected_by_where = !has_policy_guard
                || match persistence.find(entity_type, entity_id) {
                    Ok(Some(row)) => !row_where.matches(&row),
                    Ok(None) => true,
                    Err(e) => return Some(Err(e)),
                };
            if rejected_by_where {
                outcome = RowWriteOutcome::NotFound;
            }
        }
        Some(Self::outcome_value(
            outcome,
            operation,
            entity_type,
            entity_id,
            guard.as_ref(),
            code,
        ))
    }

    /// The string id an incoming row names, when it names one.
    pub(super) fn row_id_of(data: &Value) -> Option<&str> {
        data.as_object()
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }

    /// A `persist create` that is not pushed down: a row naming a taken id fails `duplicate-id` (EXEC-STEP-8);
    /// a row naming none is minted an id by the store.
    pub(super) fn create_row(
        &self,
        persistence: &dyn PersistenceHandler,
        entity_type: &str,
        data: &Value,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        if let Some(id) = Self::row_id_of(data) {
            let op = RowWriteOp::Create {
                entity_type: entity_type.to_string(),
                data: data.clone(),
            };
            if let Some(written) = persistence.write_where(&op) {
                return written.and_then(|outcome| {
                    Self::outcome_value(
                        outcome,
                        PersistOperation::Create,
                        entity_type,
                        id,
                        None,
                        code,
                    )
                });
            }
            if persistence.find(entity_type, id)?.is_some() {
                *code = Some(ServerEffectCode::DuplicateId);
                return Err(format!("{entity_type} {id} already exists"));
            }
        }
        persistence.create(entity_type, data)
    }

    /// A `persist upsert` that is not pushed as part of a run: the `@update` directive (when declared) is
    /// tested on the row it replaces, then the store creates or replaces the row by id (EXEC-STEP-8).
    pub(super) fn upsert_row(
        &self,
        persistence: &dyn PersistenceHandler,
        entity_type: &str,
        data: &Value,
        ctx: &BindingContext,
        code: &mut Option<ServerEffectCode>,
    ) -> Result<Value, String> {
        let Some(id) = Self::row_id_of(data) else {
            *code = Some(ServerEffectCode::NoRowKey);
            return Err(format!("persist upsert {entity_type}: the row names no id"));
        };
        let update_policy = self
            .persist_scope(entity_type, PersistOperation::Update)
            .policy;
        let existing = if update_policy.is_some() {
            persistence.find(entity_type, id)?
        } else {
            None
        };
        if let Some(existing) = &existing {
            check_mutation_access(
                PersistOperation::Update,
                existing,
                update_policy.as_ref(),
                ctx,
                entity_type,
            )
            .map_err(|denied| Self::refused(denied, code))?;
        }
        let op = RowWriteOp::Upsert {
            entity_type: entity_type.to_string(),
            data: data.clone(),
        };
        if let Some(written) = persistence.write_where(&op) {
            return written.and_then(|outcome| {
                Self::outcome_value(outcome, PersistOperation::Upsert, entity_type, id, None, code)
            });
        }
        let existing = match existing {
            Some(row) => Some(row),
            None => persistence.find(entity_type, id)?,
        };
        let Some(existing) = existing else {
            return persistence.create(entity_type, data);
        };
        let mut row = data.as_object().cloned().unwrap_or_default();
        if let Some(created_at) = existing.as_object().and_then(|o| o.get("createdAt")) {
            row.insert("createdAt".to_string(), created_at.clone());
        }
        persistence.delete(entity_type, id)?;
        persistence.create(entity_type, &Value::Object(row))
    }

    /// One persist whose `data` is already evaluated: the access-policy gate,
    /// required-column check and store write shared by the single forms and each
    /// op of a `persist batch`.
    pub(super) fn persist_resolved(
        &self,
        operation: PersistOperation,
        entity_type: &str,
        resolved_data: &Value,
        id: Option<&str>,
        ctx: &BindingContext,
        effect: Effect,
    ) -> EffectResult {
        if let Some(persistence) = &self.persistence {
            let scope = self.persist_scope(entity_type, operation);
            let data = Self::without_intrinsic(resolved_data, &scope.intrinsic);
            let data = match self.with_derived(
                persistence.as_ref(),
                operation,
                entity_type,
                data,
                id,
                &scope,
                ctx,
                &mut RunRows::new(),
            ) {
                Ok(data) => data,
                Err(DerivedFault::Failed(message)) => {
                    error!(entity_type = entity_type, error = %message, "Persist effect failed");
                    return EffectResult::failed(effect, &message).with_code(ServerEffectCode::DerivedFailed);
                }
                Err(DerivedFault::Store(message)) => {
                    error!(entity_type = entity_type, error = %message, "Persist effect failed");
                    return EffectResult::failed(effect, &message);
                }
            };
            let resolved_data = &data;
            let policy = scope.policy.as_ref();

            let mut code: Option<ServerEffectCode> = None;
            let row_where = match Self::row_where(&effect, ctx, &scope.declared_required) {
                Ok(row_where) => row_where,
                Err(message) => return EffectResult::failed(effect, &message),
            };
            let result = match operation {
                PersistOperation::Create => {
                    match Self::check_create(entity_type, resolved_data, &scope, ctx, &mut code) {
                        Err(denial) => Err(denial),
                        Ok(()) => self.create_row(
                            persistence.as_ref(),
                            entity_type,
                            resolved_data,
                            &mut code,
                        ),
                    }
                }
                PersistOperation::Upsert => {
                    match Self::check_create(entity_type, resolved_data, &scope, ctx, &mut code) {
                        Err(denial) => Err(denial),
                        Ok(()) => self.upsert_row(
                            persistence.as_ref(),
                            entity_type,
                            resolved_data,
                            ctx,
                            &mut code,
                        ),
                    }
                }
                PersistOperation::Read => match self.resolve_id(id, ctx) {
                    Err(e) => Err(e),
                    Ok(entity_id) => persistence.read(entity_type, entity_id.as_str()),
                },
                PersistOperation::Update => match self.resolve_update_id(id, resolved_data, ctx) {
                    Err(e) => {
                        code = Some(ServerEffectCode::NoRowKey);
                        Err(e)
                    }
                    Ok(entity_id) => match self.pushed_write(
                        persistence.as_ref(),
                        operation,
                        entity_type,
                        &entity_id,
                        resolved_data,
                        policy,
                        &scope.declared_required,
                        row_where.as_ref(),
                        ctx,
                        &mut code,
                    ) {
                        Some(pushed) => pushed,
                        None => Self::existing_row(
                            persistence.as_ref(),
                            entity_type,
                            &entity_id,
                            &mut code,
                        )
                        .and_then(|existing| Self::where_holds(row_where.as_ref(), existing, entity_type, &entity_id, &mut code))
                        .and_then(|existing| {
                            if policy.is_some() {
                                check_mutation_access(
                                    operation,
                                    &existing,
                                    policy,
                                    ctx,
                                    entity_type,
                                )
                                .map_err(|denied| Self::refused(denied, &mut code))?;
                            }
                            persistence.update(entity_type, &entity_id, resolved_data)
                        }),
                    },
                },
                PersistOperation::Delete => match self.resolve_id(id, ctx) {
                    Err(e) => {
                        code = Some(ServerEffectCode::NoRowKey);
                        Err(e)
                    }
                    Ok(entity_id) if entity_id.is_empty() => {
                        code = Some(ServerEffectCode::NoRowKey);
                        Err(NO_ROW_KEY.to_string())
                    }
                    Ok(entity_id) => match self.pushed_write(
                        persistence.as_ref(),
                        operation,
                        entity_type,
                        &entity_id,
                        resolved_data,
                        policy,
                        &scope.declared_required,
                        row_where.as_ref(),
                        ctx,
                        &mut code,
                    ) {
                        Some(pushed) => pushed,
                        None => Self::existing_row(
                            persistence.as_ref(),
                            entity_type,
                            &entity_id,
                            &mut code,
                        )
                        .and_then(|existing| Self::where_holds(row_where.as_ref(), existing, entity_type, &entity_id, &mut code))
                        .and_then(|existing| {
                            if policy.is_some() {
                                check_mutation_access(
                                    operation,
                                    &existing,
                                    policy,
                                    ctx,
                                    entity_type,
                                )
                                .map_err(|denied| Self::refused(denied, &mut code))?;
                            }
                            persistence
                                .delete(entity_type, &entity_id)
                                .map(|()| Value::Null)
                        }),
                    },
                },
            };

            match result {
                Ok(value) => {
                    info!(entity_type = entity_type, operation = ?operation, "Persist effect executed");
                    EffectResult::executed_with_value(effect, value)
                }
                Err(e) => {
                    error!(entity_type = entity_type, error = %e, "Persist effect failed");
                    let failed = EffectResult::failed(effect, &e);
                    match code {
                        Some(code) => failed.with_code(code),
                        None => failed,
                    }
                }
            }
        } else {
            EffectResult::noop(effect, "no persistence handler configured")
        }
    }
}
