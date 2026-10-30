//! `IoPort`'s service, substrate and stream asks.

use super::*;

impl IoPort {
    /// `batch` when it touches kernel-owned `[runtime]` rows: all of them are written in the kernel's memory
    /// (journaled inside a step); a batch that also touches a host-store entity is refused and writes nothing.
    /// `None` when no op is a `[runtime]` row.
    pub(super) fn local_writes(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        let local = batch.ops.iter().filter(|op| self.local(op.entity_type())).count();
        if local == 0 {
            return None;
        }
        if local != batch.ops.len() {
            return Some(Err(
                "a write batch cannot mix [runtime] rows, which live in the kernel, with rows of a host store".to_string(),
            ));
        }
        if self.direct() {
            return Some(
                self.memory
                    .write_batch(batch)
                    .unwrap_or_else(|| Err("the kernel's row memory has no conditional writes".to_string())),
            );
        }
        Some(match self.ask(|| IoRequest::WriteRows { batch: batch.clone() }) {
            Asked::Answer(IoAnswer::Wrote(outcomes)) => outcomes,
            Asked::Direct | Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        })
    }

    /// Run `batch` here and send its rows as one compare-and-commit (EXEC-STORE-5); a conflict re-reads and
    /// re-runs, at most three times.
    pub(super) fn commit_writes(&self, batch: &RowWriteBatch) -> Result<Vec<RowWriteOutcome>, String> {
        for _ in 0..3 {
            let mut tables: HashMap<String, RowTable> = HashMap::new();
            let mut read: Vec<(String, String, Option<Arc<Value>>)> = Vec::new();
            for op in &batch.ops {
                let entity = op.entity_type();
                for id in touched_ids(op) {
                    if read.iter().any(|(e, i, _)| e == entity && *i == id) {
                        continue;
                    }
                    let row = self.find(entity, &id)?;
                    if let Some(row) = &row {
                        tables.entry(entity.to_string()).or_default().insert(id.clone(), row.clone());
                    }
                    read.push((entity.to_string(), id, row));
                }
            }
            let outcomes = apply_write_batch_across(&mut tables, batch, &mut |_| CreateIds::Strict, &mut orbital_core_base::intern::KeyVocabulary::default());
            let rows: Vec<RowCommit> = read
                .iter()
                .map(|(entity, id, before)| {
                    let after = tables.get(entity).and_then(|t| t.get(id));
                    let write = match (before, after) {
                        (Some(_), None) => RowCommitWrite::Delete,
                        (Some(b), Some(a)) if b == a => RowCommitWrite::Keep,
                        (_, Some(a)) => RowCommitWrite::Put { row: (**a).clone() },
                        (None, None) => RowCommitWrite::Keep,
                    };
                    RowCommit { entity_type: entity.clone(), id: id.clone(), expected: before.as_ref().map(|b| (**b).clone()), write }
                })
                .collect();
            for (entity, _, _) in &read {
                self.forget_reads(entity);
            }
            match self.ask(|| IoRequest::CommitRows { rows: rows.clone() }) {
                Asked::Answer(IoAnswer::Committed(Ok(true))) => return Ok(outcomes),
                Asked::Answer(IoAnswer::Committed(Ok(false))) => continue,
                Asked::Answer(IoAnswer::Committed(Err(error))) => return Err(error),
                Asked::Direct | Asked::Answer(_) | Asked::Withheld => return Err(AWAITING_HOST.to_string()),
            }
        }
        Err("the rows kept changing".to_string())
    }

    /// Send `batch` as one `WriteRows` when the host declared capabilities covering every op; `None` when it
    /// did not (or declined) and nothing was written.
    pub(super) fn push_writes(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        let commits = {
            let declared = self.write_capabilities.read();
            !batch.ops.is_empty()
                && batch.ops.iter().all(|op| declared.get(op.entity_type()).and_then(|c| c.commit_rows) == Some(true))
        };
        if commits {
            return Some(self.commit_writes(batch));
        }
        let rekey = batch
            .ops
            .iter()
            .any(|op| matches!(op, RowWriteOp::Rekey { .. }));
        {
            let declared = self.write_capabilities.read();
            for op in batch
                .ops
                .iter()
                .filter(|op| matches!(op, RowWriteOp::Rekey { .. }))
            {
                if declared.get(op.entity_type()).and_then(|c| c.rekey) != Some(true) {
                    return Some(Err("rekey is unsupported by this store".to_string()));
                }
            }
            for op in &batch.ops {
                let entity = op.entity_type();
                let capabilities = match declared.get(entity) {
                    Some(capabilities) => capabilities,
                    None if rekey => {
                        return Some(Err(
                            "write capabilities are missing for a rekey batch".to_string()
                        ))
                    }
                    None => return None,
                };
                let reason = if !capabilities.conditional {
                    Some("not-conditional")
                } else if batch.on_failure == RowWriteFailureMode::Atomic
                    && batch.ops.len() > 1
                    && !capabilities.atomic_batch
                {
                    Some("atomic-batch")
                } else if batch.on_failure == RowWriteFailureMode::Stop
                    && batch.ops.len() > 1
                    && !capabilities.stop_runs
                {
                    Some("stop-run")
                } else {
                    op.guard()
                        .and_then(|g| guard_unsupported_reason(capabilities, g))
                        .map(|r| r.as_str())
                };
                if let Some(reason) = reason {
                    let tag = if rekey {
                        "kernel.write.pushdown-refused"
                    } else {
                        "kernel.write.pushdown-fallback"
                    };
                    self.notice(tag, entity, reason, "");
                    if rekey {
                        return Some(Err(format!(
                            "rekey batch is unsupported by this store: {reason}"
                        )));
                    }
                    return None;
                }
            }
        }
        match self.ask(|| IoRequest::WriteRows {
            batch: batch.clone(),
        }) {
            Asked::Direct => self
                .source
                .write_batch(batch)
                .or_else(|| rekey.then(|| Err("rekey is unsupported by this store".to_string()))),
            Asked::Answer(IoAnswer::Wrote(outcomes)) => {
                for op in &batch.ops {
                    self.forget_reads(op.entity_type());
                }
                Some(outcomes)
            }
            Asked::Answer(IoAnswer::WriteUnsupported(detail)) => {
                let entity = batch.ops.first().map_or("", |op| op.entity_type());
                let tag = if rekey {
                    "kernel.write.pushdown-refused"
                } else {
                    "kernel.write.pushdown-fallback"
                };
                self.notice(tag, entity, "host-declined", &detail);
                if rekey {
                    Some(Err(format!(
                        "rekey batch was refused by the host: {detail}"
                    )))
                } else {
                    None
                }
            }
            Asked::Answer(_) | Asked::Withheld => Some(Err(AWAITING_HOST.to_string())),
        }
    }
}

/// A host's page must hold only rows the query's policy admits: a mis-implemented adapter cannot widen what a
/// viewer sees.
pub(super) fn echo_checked(entity_type: &str, query: &RowQuery, page: &RowPage) -> Result<(), String> {
    if let Some(policy) = &query.policy {
        if page.rows.iter().any(|row| !policy.matches(row)) {
            tracing::error!(
                target: orbital_core_base::logging::ns::RUNTIME_FETCH,
                entity = %entity_type,
                "the host returned a row its @read policy rejects"
            );
            return Err(format!(
                "fetch {entity_type}: the host returned a row that the declared @read policy rejects"
            ));
        }
    }
    Ok(())
}

impl ServiceHandler for IoPort {
    fn call(
        &self,
        service: &str,
        method: &str,
        args: &Value,
        key: Option<&str>,
        caller: Option<&ServiceCaller>,
    ) -> Result<Value, String> {
        match self.ask(|| IoRequest::CallService {
            service: service.to_string(),
            method: method.to_string(),
            args: args.clone(),
            key: key.map(str::to_string),
            caller: caller.cloned(),
            site: self.site.lock().clone(),
        }) {
            Asked::Direct => match &self.service {
                Some(handler) => handler.call(service, method, args, key, caller),
                None => Err("no service handler configured".to_string()),
            },
            Asked::Answer(IoAnswer::Service(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(CALL_CANCELLED.to_string())
            }
            Asked::Answer(IoAnswer::ServiceDeferred) => {
                *self.verdict.lock() = Some(CallVerdict::Deferred);
                Ok(Value::Null)
            }
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    fn cancel(&self, key: &str) -> Result<(), String> {
        if self.withdraw_pending_call(key) {
            return Ok(());
        }
        match self.ask(|| IoRequest::CancelCall {
            key: key.to_string(),
        }) {
            Asked::Direct => match &self.service {
                Some(handler) => handler.cancel(key),
                None => Err("no service handler configured".to_string()),
            },
            Asked::Answer(IoAnswer::Cancelled(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }
}

impl IoPort {
    pub(crate) fn page_facts(&self, fields: &[PageFactField]) -> Result<PageFacts, String> {
        match self.ask(|| IoRequest::PageFacts { fields: fields.to_vec() }) {
            Asked::Direct => Err(NO_PAGE_FACTS_HOST.to_string()),
            Asked::Answer(IoAnswer::PageFacts(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    /// One `browser/prompt` ask. A host that answers it cancelled records the verdict for the kernel's route.
    pub(crate) fn prompt(&self, text: &str, schema: &str, key: Option<&str>) -> Result<String, ModelFailure> {
        match self.ask(|| IoRequest::Prompt {
            text: text.to_string(),
            schema: schema.to_string(),
            key: key.map(str::to_string),
        }) {
            Asked::Direct => Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_PROMPT_HOST)),
            Asked::Answer(IoAnswer::Prompt(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(ModelFailure::new(ModelFailureKind::Failed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(ModelFailure::new(ModelFailureKind::Failed, AWAITING_HOST)),
        }
    }

    /// One `browser/embed` ask, as [`Self::prompt`].
    pub(crate) fn embed(&self, texts: &[String], key: Option<&str>) -> Result<Vec<Vec<f64>>, ModelFailure> {
        match self.ask(|| IoRequest::Embed { texts: texts.to_vec(), key: key.map(str::to_string) }) {
            Asked::Direct => Err(ModelFailure::new(ModelFailureKind::Unconfigured, NO_EMBED_HOST)),
            Asked::Answer(IoAnswer::Embed(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(ModelFailure::new(ModelFailureKind::Failed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(ModelFailure::new(ModelFailureKind::Failed, AWAITING_HOST)),
        }
    }

    /// One raw `browser/tab-*` ask. A host that answers it cancelled records the verdict for the kernel's route.
    pub(crate) fn tab(&self, ask: &TabAsk, key: Option<&str>) -> Result<Value, TabFailure> {
        match self.ask(|| IoRequest::Tab { ask: ask.clone(), key: key.map(str::to_string) }) {
            Asked::Direct => Err(TabFailure::new(TabFailureKind::Unavailable, NO_TAB_HOST)),
            Asked::Answer(IoAnswer::Tab(out)) => out,
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(TabFailure::new(TabFailureKind::Failed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(TabFailure::new(TabFailureKind::Failed, AWAITING_HOST)),
        }
    }

    pub(crate) fn secret(&self, name: &str, principal: Option<&str>) -> Result<Option<Secret<String>>, String> {
        match self.ask(|| IoRequest::Secret {
            name: name.to_string(),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::Secret(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn store_secret(
        &self,
        name: &str,
        value: &str,
        ttl_ms: u64,
        principal: Option<&str>,
    ) -> Result<(), String> {
        match self.ask(|| IoRequest::SecretSet {
            name: name.to_string(),
            value: Secret::new(value.to_string()),
            ttl_ms,
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::SecretSet(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn rotate_secret(&self, name: &str, value: &str, principal: Option<&str>) -> Result<(), String> {
        match self.ask(|| IoRequest::SecretRotate {
            name: name.to_string(),
            value: Secret::new(value.to_string()),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::SecretRotate(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    /// One `os/<op>` ask, journaled. A streaming op, and a keyed op that defers its completion under a declared
    /// route, carries the call site its events dispatch to; the host may answer the latter `ServiceDeferred`.
    pub(crate) fn os(&self, op: OsOp, args: &[Value], key: Option<&str>) -> Result<Value, OsError> {
        match self.ask(|| IoRequest::Os {
            op,
            args: args.to_vec(),
            key: key.map(str::to_string),
            site: self.os_site(op, key),
        }) {
            Asked::Direct => Err(OsError::new(OsErrorKind::Unsupported, NO_PORT_HOST)),
            Asked::Answer(IoAnswer::Os(out)) => out.map(Secret::into_inner),
            Asked::Answer(IoAnswer::ServiceDeferred) => {
                *self.verdict.lock() = Some(CallVerdict::Deferred);
                Ok(Value::Null)
            }
            Asked::Answer(IoAnswer::ServiceCancelled) => {
                *self.verdict.lock() = Some(CallVerdict::Cancelled);
                Err(OsError::new(OsErrorKind::Closed, CALL_CANCELLED))
            }
            Asked::Answer(_) | Asked::Withheld => Err(OsError::new(OsErrorKind::Io, AWAITING_HOST)),
        }
    }

    pub(crate) fn peek_call_verdict(&self) -> Option<CallVerdict> {
        *self.verdict.lock()
    }

    pub(crate) fn os_site(&self, op: OsOp, key: Option<&str>) -> Option<CallSite> {
        let site = self.site.lock().clone()?;
        let routed = site.emit.success.is_some() || site.emit.failure.is_some();
        ((op.is_stream() && (op != OsOp::StdinRead || key.is_some()))
            || (op.defers_completion() && key.is_some() && routed))
            .then_some(site)
    }

    pub(crate) fn clock(&self) -> Result<i64, String> {
        if let Some(clock) = &self.clock {
            return Ok(clock() as i64);
        }
        match self.ask(|| IoRequest::Clock) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::Clock(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn random(&self, bytes: u32) -> Result<Vec<u8>, String> {
        match self.ask(|| IoRequest::Random { bytes }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::Random(out)) => out.map(Secret::into_inner),
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_status(&self, name: &str, principal: Option<&str>) -> Result<crate::credential_admin::CredentialStanding, String> {
        match self.ask(|| IoRequest::CredentialStatus {
            name: name.to_string(),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStatus(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_forget(&self, name: &str, principal: Option<&str>) -> Result<(), String> {
        match self.ask(|| IoRequest::CredentialForget {
            name: name.to_string(),
            principal: principal.map(str::to_string),
        }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialForget(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_list(&self) -> Result<Option<StoreListing>, String> {
        match self.ask(|| IoRequest::CredentialStoreList) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreList(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_read(&self, name: &str) -> Result<Option<String>, String> {
        match self.ask(|| IoRequest::CredentialStoreRead { name: name.to_string() }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreRead(out)) => out.map(|value| value.map(Secret::into_inner)),
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_write(&self, service: &str, name: &str, value: &str) -> Result<(), String> {
        match self.ask(|| IoRequest::CredentialStoreWrite {
            service: service.to_string(),
            name: name.to_string(),
            value: Secret::new(value.to_string()),
        }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreWrite(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_remove(&self, name: &str) -> Result<bool, String> {
        match self.ask(|| IoRequest::CredentialStoreRemove { name: name.to_string() }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreRemove(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_store_rotate(&self) -> Result<RotateCounts, String> {
        match self.ask(|| IoRequest::CredentialStoreRotate) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialStoreRotate(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_mode_read(&self) -> Result<Option<String>, String> {
        match self.ask(|| IoRequest::CredentialModeRead) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialModeRead(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    pub(crate) fn credential_mode_write(&self, mode: &str) -> Result<String, String> {
        match self.ask(|| IoRequest::CredentialModeWrite { mode: mode.to_string() }) {
            Asked::Direct => Err(NO_PORT_HOST.to_string()),
            Asked::Answer(IoAnswer::CredentialModeWrite(out)) => out,
            Asked::Answer(_) | Asked::Withheld => Err(AWAITING_HOST.to_string()),
        }
    }

    /// One cross-unit delivery, journaled. A kernel with no step in flight has no router to hand it to: the
    /// delivery is refused `down`, so the emitter sees it instead of the emit vanishing.
    pub(crate) fn deliver(&self, envelope: &Envelope, site: CallSite) -> DeliverAsk {
        match self.ask(|| IoRequest::Deliver {
            envelope: envelope.clone(),
            key: None,
            site,
        }) {
            Asked::Answer(IoAnswer::Deliver(out)) => DeliverAsk::Answered(out),
            Asked::Direct | Asked::Answer(_) => DeliverAsk::Answered(Err(DeliverRefusal {
                kind: DeliverRefusalKind::Down,
                id: envelope.id.clone(),
            })),
            Asked::Withheld => DeliverAsk::Suspended,
        }
    }
}

/// What a [`IoPort::deliver`] came back with: the host's answer, or no answer yet (the step is suspended).
pub(crate) enum DeliverAsk {
    Answered(Result<Accepted, DeliverRefusal>),
    Suspended,
}

pub(super) const NO_PORT_HOST: &str = "no port host is configured for this kernel";

impl SubstrateHandler for IoPort {
    fn run(&self, op: &str, args: &[Value]) -> Result<Value, SubstrateFailure> {
        let (key, site) = self.substrate_site();
        match self.ask(|| IoRequest::Substrate {
            op: op.to_string(),
            args: args.to_vec(),
            key,
            site,
        }) {
            Asked::Direct => match &self.substrate {
                Some(handler) => handler.run(op, args),
                None => Err(SubstrateFailure {
                    message: format!("no substrate host configured for {op}"),
                    errors: Vec::new(),
                }),
            },
            Asked::Answer(IoAnswer::Substrate(out)) => out,
            Asked::Answer(IoAnswer::ServiceDeferred) => {
                *self.verdict.lock() = Some(CallVerdict::Deferred);
                Ok(Value::Null)
            }
            Asked::Answer(_) | Asked::Withheld => Err(SubstrateFailure {
                message: AWAITING_HOST.to_string(),
                errors: Vec::new(),
            }),
        }
    }
}
