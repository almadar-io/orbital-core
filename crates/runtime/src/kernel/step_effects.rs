//! Running a step's effects: the traced run, effect expressions, and each leaf effect.

use super::*;

impl RuntimeKernel {
    /// Execute effects and capture Set-effect data.
    pub(super) fn execute_effects_with_entity(
        &self,
        effects: &[SExpression],
        ctx: &BindingContext,
    ) -> (Vec<EffectResult>, Value) {
        let (effect_results, entity_data, _, _) = self.execute_effects_traced(effects, ctx);
        (effect_results, entity_data)
    }

    /// `execute_effects_with_entity`, also reporting how each authored effect ran.
    pub(super) fn execute_effects_traced(
        &self,
        effects: &[SExpression],
        ctx: &BindingContext,
    ) -> (Vec<EffectResult>, Value, Vec<EffectRun>, Option<String>) {
        let evaluator = Evaluator::new();
        let mut effect_results = Vec::new();
        let mut entity_data = PendingSets::new(ctx);
        let mut runs = Vec::with_capacity(effects.len());
        let mut failure = None;

        let mut run_results: HashMap<usize, EffectResult> = HashMap::new();
        for (index, effect_expr) in effects.iter().enumerate() {
            let _effect = tracing::trace_span!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, "effect", op = ?effect_expr.operator()).entered();
            if failure.is_some() {
                runs.push(EffectRun {
                    effect: effect_expr.clone(),
                    status: EffectRunStatus::Skipped,
                    error: None,
                });
                continue;
            }
            let before = effect_results.len();
            if !run_results.contains_key(&index) {
                let run = persist_run(effects, index);
                if run.len() >= 2 {
                    let mirrored = entity_data.apply(ctx);
                    let results = self.effect_executor.execute_persist_run(&run, &mirrored);
                    run_results
                        .extend(results.into_iter().enumerate().map(|(k, r)| (index + k, r)));
                }
            }
            if let Some(result) = run_results.remove(&index) {
                let declared = orbital_core_base::effect_types::parse_effect(effect_expr);
                let result = match declared.as_ref().and_then(Effect::emit_routes) {
                    Some(routes) => result.with_declared_routes(routes),
                    None => result,
                };
                let completion = match declared {
                    Some(effect @ Effect::Persist { .. }) => match &effect {
                        Effect::Persist {
                            emit: Some(persist_emit),
                            ..
                        } if !persist_emit.is_empty() => {
                            persist_completion_emit(persist_emit, &effect, &result, ctx)
                        }
                        _ => None,
                    },
                    _ => None,
                };
                effect_results.push(result);
                effect_results.extend(completion.map(EffectResult::as_completion));
            } else {
                self.exec_effect_expr(
                    effect_expr,
                    ctx,
                    &evaluator,
                    &mut effect_results,
                    &mut entity_data,
                );
            }
            let ran = &effect_results[before..];
            let failed = ran.iter().find(|r| r.is_failed());
            let status = if failed.is_some() {
                EffectRunStatus::Failed
            } else if ran.iter().any(|r| r.is_executed() || r.is_delegated()) {
                EffectRunStatus::Executed
            } else {
                EffectRunStatus::Skipped
            };
            runs.push(EffectRun {
                effect: effect_expr.clone(),
                status,
                error: failed.and_then(|r| r.error.clone()),
            });
            if failure.is_none() {
                failure = ran
                    .iter()
                    .find(|r| is_thrown_failure(r))
                    .map(|thrown| thrown.error.clone().unwrap_or_default());
            }
        }

        if failure.is_some() {
            effect_results.retain(is_thrown_failure);
        }

        (
            effect_results,
            Value::object(entity_data.sets),
            runs,
            failure,
        )
    }

    /// Execute a single top-level effect expression.
    ///
    /// Leaf effects (`set`/`render-ui`/`emit`/`fetch`/…) go through the normal
    /// effect handler so entity writes + render flushing happen. The compound
    /// forms `let`/`if`/`do`/`when` are control flow, not effects: their parts
    /// are evaluated through the canonical `Evaluator` (the same `LetOp`/`IfOp`
    /// scope rules `eval_expr` uses) and their body effects are dispatched
    /// recursively, so a top-level `(let ((a v)…) (do (set …) (render-ui …)))`
    /// — a physics tick — actually runs instead of being dropped as a no-op
    /// `Effect::Custom`.
    pub(super) fn exec_effect_expr(
        &self,
        effect_expr: &SExpression,
        ctx: &BindingContext,
        evaluator: &Evaluator,
        effect_results: &mut Vec<EffectResult>,
        entity_data: &mut PendingSets,
    ) {
        let secreted = self.with_scrypt_salts(effect_expr, ctx, evaluator);
        let ctx = secreted.as_ref().unwrap_or(ctx);
        // Only control flow reads the mirrored `@entity` here; a leaf mirrors it itself so it can
        // release the view before its `set` writes into it in place.
        if let SExpression::Array(items) = effect_expr {
            if let Some(SExpression::String(head)) = items.first() {
                if matches!(head.as_str(), "let" | "do" | "if" | "when") {
                    let mirrored = entity_data.apply(ctx);
                    let ctx: &BindingContext = &mirrored;
                    match head.as_str() {
                        "let" => {
                            // [ "let", [[name, valExpr], …], body… ]
                            let mut child_ctx = ctx.child();
                            if let Some(SExpression::Array(bindings)) = items.get(1) {
                                for binding in bindings {
                                    if let SExpression::Array(pair) = binding {
                                        if let (Some(SExpression::String(name)), Some(value_expr)) =
                                            (pair.first(), pair.get(1))
                                        {
                                            if let Ok(value) =
                                                evaluator.evaluate(value_expr, &child_ctx)
                                            {
                                                child_ctx.set_local(name, value);
                                            }
                                        }
                                    }
                                }
                            }
                            for body in items.iter().skip(2) {
                                self.exec_effect_expr(
                                    body,
                                    &child_ctx,
                                    evaluator,
                                    effect_results,
                                    entity_data,
                                );
                                if effect_results.iter().any(is_thrown_failure) {
                                    break;
                                }
                            }
                        }
                        "do" => {
                            for body in items.iter().skip(1) {
                                self.exec_effect_expr(
                                    body,
                                    ctx,
                                    evaluator,
                                    effect_results,
                                    entity_data,
                                );
                                if effect_results.iter().any(is_thrown_failure) {
                                    break;
                                }
                            }
                        }
                        "if" => {
                            // [ "if", cond, then, else? ]
                            if let Some(cond) = items.get(1) {
                                let truthy = evaluator
                                    .evaluate(cond, ctx)
                                    .map(|v| v.is_truthy())
                                    .unwrap_or(false);
                                let branch = if truthy { items.get(2) } else { items.get(3) };
                                if let Some(branch) = branch {
                                    self.exec_effect_expr(
                                        branch,
                                        ctx,
                                        evaluator,
                                        effect_results,
                                        entity_data,
                                    );
                                }
                            }
                        }
                        "when" => {
                            // [ "when", cond, body… ]
                            if let Some(cond) = items.get(1) {
                                let truthy = evaluator
                                    .evaluate(cond, ctx)
                                    .map(|v| v.is_truthy())
                                    .unwrap_or(false);
                                if truthy {
                                    for body in items.iter().skip(2) {
                                        self.exec_effect_expr(
                                            body,
                                            ctx,
                                            evaluator,
                                            effect_results,
                                            entity_data,
                                        );
                                        if effect_results.iter().any(is_thrown_failure) {
                                            break;
                                        }
                                    }
                                }
                            }
                            return;
                        }
                        _ => {}
                    }
                    return;
                }
            }
        }

        self.exec_leaf_effect(effect_expr, ctx, evaluator, effect_results, entity_data);
    }

    /// `ctx` with the value of every credential a `hash/scrypt` in `expr` names as its salt, resolved through the host's
    /// `Secret` ask. The value lives only in the evaluation context: it is not a binding and never reaches program data.
    /// `None` when `expr` hashes nothing or every salt is already held.
    pub(super) fn with_scrypt_salts(
        &self,
        expr: &SExpression,
        ctx: &BindingContext,
        evaluator: &Evaluator,
    ) -> Option<BindingContext> {
        fn collect<'a>(expr: &'a SExpression, out: &mut Vec<&'a SExpression>) {
            if let SExpression::Array(items) = expr {
                if matches!(items.first(), Some(SExpression::String(head)) if head == "hash/scrypt")
                {
                    out.extend(items.get(2));
                }
                for item in items {
                    collect(item, out);
                }
            }
        }
        let mut salts = Vec::new();
        collect(expr, &mut salts);
        let mut held: Option<BindingContext> = None;
        for salt in salts {
            let current = held.as_ref().unwrap_or(ctx);
            let Ok(Value::String(name)) = evaluator.evaluate(salt, current) else {
                continue;
            };
            if current.secret(&name).is_some() {
                continue;
            }
            if let Ok(Some(value)) = self.io.secret(&name, None) {
                if !value.is_empty() {
                    held = Some(current.clone().with_secret(&name, value));
                }
            }
        }
        held
    }

    /// Execute a single leaf effect (`set`/`render-ui`/`emit`/`fetch`/…) via the
    /// canonical effect handler, capturing Set-effect data.
    pub(super) fn exec_leaf_effect(
        &self,
        effect_expr: &SExpression,
        ctx: &BindingContext,
        evaluator: &Evaluator,
        effect_results: &mut Vec<EffectResult>,
        entity_data: &mut PendingSets,
    ) {
        let mirrored = entity_data.apply(ctx);
        let ctx: &BindingContext = &mirrored;
        if let Some(effect) = orbital_core_base::effect_types::parse_effect(effect_expr) {
            match &effect {
                Effect::Set { path, value } => {
                    let evaluated_value = if value.as_array().is_some()
                        || value.as_object().is_some()
                    {
                        let sexpr = value_to_sexpr(value);
                        match evaluator.evaluate(&sexpr, ctx) {
                            Ok(v) => v,
                            Err(err) => {
                                // A value that fails to evaluate is a failed effect, never stored raw.
                                tracing::error!(path = %path, error = %err, "kernel: set value failed to evaluate");
                                effect_results
                                    .push(EffectResult::failed(effect.clone(), &err.to_string()));
                                return;
                            }
                        }
                    } else if let Some(s) = value.as_str() {
                        if BindingContext::binding_token_end(s, 0) == Some(s.len()) {
                            // A whole-string binding that resolves to nothing reads as null, as every
                            // other read of it does; its text is never stored.
                            ctx.resolve_binding(s).unwrap_or(Value::Null)
                        } else if s.starts_with('@') {
                            ctx.resolve_value(value).into_owned()
                        } else {
                            value.clone()
                        }
                    } else {
                        value.clone()
                    };

                    let field_name = path.strip_prefix("@entity.").unwrap_or(path);
                    drop(mirrored);
                    entity_data.insert(field_name.to_string(), evaluated_value.clone());

                    effect_results.push(EffectResult::executed(Effect::Set {
                        path: path.clone(),
                        value: evaluated_value,
                    }));
                }
                Effect::Fetch {
                    emit: Some(fetch_emit),
                    ..
                } if !fetch_emit.is_empty() => {
                    let result = self
                        .effect_executor
                        .execute(&effect, ctx)
                        .with_declared_routes(fetch_emit);
                    let completion = fetch_completion_emit(fetch_emit, &result);
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Persist {
                    emit: Some(persist_emit),
                    ..
                } if !persist_emit.is_empty() => {
                    let result = self
                        .effect_executor
                        .execute(&effect, ctx)
                        .with_declared_routes(persist_emit);
                    let completion = persist_completion_emit(persist_emit, &effect, &result, ctx);
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::PersistBatch {
                    emit: Some(batch_emit),
                    ..
                } if !batch_emit.is_empty() => {
                    let result = self
                        .effect_executor
                        .execute(&effect, ctx)
                        .with_declared_routes(batch_emit);
                    let completion = persist_completion_emit(batch_emit, &effect, &result, ctx);
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::CallService { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::CallService { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    let reused = key.as_deref().is_some_and(|k| {
                        effect_results.iter().any(|r| {
                            matches!(&r.effect, Effect::CallService { key: Some(prev), .. } if prev.as_str() == Some(k))
                        })
                    });
                    let (mut result, verdict) = if reused {
                        let message = format!(
                            "call-service key '{}' is already used by another call in this step",
                            key.as_deref().unwrap_or_default()
                        );
                        (EffectResult::failed(resolved.clone(), &message), None)
                    } else {
                        self.io.set_call_site(self.call_site_of(emit.as_ref()));
                        let result = self.effect_executor.execute(&resolved, ctx);
                        let verdict = self.io.take_call_verdict();
                        self.io.set_call_site(None);
                        (result, verdict)
                    };
                    if verdict == Some(CallVerdict::Cancelled) {
                        result = EffectResult::executed(resolved);
                    } else if let Effect::CallService { emit: routes, .. } = &mut result.effect {
                        routes.clone_from(emit);
                    }
                    let completion = emit.as_ref().and_then(|e| {
                        keyed_call_completion_emit(e, &result, verdict, key.as_deref())
                    });
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Http { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::Http { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    let (result, verdict) = if self.effect_executor.environment().is_client() {
                        (self.effect_executor.execute(&resolved, ctx), None)
                    } else {
                        self.io.set_call_site(self.call_site_of(emit.as_ref()));
                        let principal =
                            ServiceCaller::of_user(&ctx.user).map(|caller| caller.principal);
                        let mut result = self.run_http(&resolved, principal.as_deref(), ctx.now);
                        let verdict = self.io.take_call_verdict();
                        self.io.set_call_site(None);
                        if matches!(
                            verdict,
                            Some(CallVerdict::Cancelled | CallVerdict::Deferred)
                        ) {
                            result = EffectResult::executed(resolved);
                        }
                        (result, verdict)
                    };
                    let completion = emit.as_ref().and_then(|e| {
                        keyed_call_completion_emit(e, &result, verdict, key.as_deref())
                    });
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Redis { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::Redis { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    let result = if self.effect_executor.environment().is_client() {
                        self.effect_executor.execute(&resolved, ctx)
                    } else {
                        self.io.set_call_site(self.call_site_of(emit.as_ref()));
                        let principal =
                            ServiceCaller::of_user(&ctx.user).map(|caller| caller.principal);
                        let result = self.run_redis(&resolved, principal.as_deref());
                        self.io.set_call_site(None);
                        result
                    };
                    let completion = emit
                        .as_ref()
                        .and_then(|e| keyed_call_completion_emit(e, &result, None, key.as_deref()));
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Postgres { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::Postgres { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    let result = if self.effect_executor.environment().is_client() {
                        self.effect_executor.execute(&resolved, ctx)
                    } else {
                        self.io.set_call_site(self.call_site_of(emit.as_ref()));
                        let principal =
                            ServiceCaller::of_user(&ctx.user).map(|caller| caller.principal);
                        let result = self.run_postgres(&resolved, principal.as_deref());
                        self.io.set_call_site(None);
                        result
                    };
                    let completion = emit
                        .as_ref()
                        .and_then(|e| keyed_call_completion_emit(e, &result, None, key.as_deref()));
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Os { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::Os { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    let (result, verdict) = if self.effect_executor.environment().is_client() {
                        (self.effect_executor.execute(&resolved, ctx), None)
                    } else {
                        self.io.set_call_site(self.call_site_of(emit.as_ref()));
                        let mut result = self.run_os(&resolved);
                        let verdict = self.io.take_call_verdict();
                        self.io.set_call_site(None);
                        if verdict == Some(CallVerdict::Cancelled) {
                            result = EffectResult::executed(resolved);
                        }
                        (result, verdict)
                    };
                    let completion = emit.as_ref().and_then(|e| {
                        keyed_call_completion_emit(e, &result, verdict, key.as_deref())
                    });
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::BrowserBeacon { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::BrowserBeacon { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    self.io.set_call_site(self.call_site_of(emit.as_ref()));
                    let mut result = self.run_beacon(&resolved);
                    let verdict = self.io.take_call_verdict();
                    self.io.set_call_site(None);
                    if verdict == Some(CallVerdict::Cancelled) {
                        result = EffectResult::executed(resolved);
                    }
                    let completion = emit.as_ref().and_then(|e| {
                        keyed_call_completion_emit(e, &result, verdict, key.as_deref())
                    });
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::BrowserPrompt { emit, .. } | Effect::BrowserEmbed { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::BrowserPrompt { key, .. } | Effect::BrowserEmbed { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    let mut result = self.run_on_device_model(&resolved, key.as_deref());
                    let verdict = self.io.take_call_verdict();
                    if verdict == Some(CallVerdict::Cancelled) {
                        result = EffectResult::executed(resolved);
                    }
                    let completion = emit.as_ref().and_then(|e| {
                        keyed_call_completion_emit(e, &result, verdict, key.as_deref())
                    });
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::BrowserTab { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let key = match &resolved {
                        Effect::BrowserTab { key, .. } => {
                            key.as_ref().and_then(Value::as_str).map(str::to_string)
                        }
                        _ => None,
                    };
                    let mut result = self.run_browser_tab(&resolved);
                    let verdict = self.io.take_call_verdict();
                    if verdict == Some(CallVerdict::Cancelled) {
                        result = EffectResult::executed(resolved);
                    }
                    let completion = emit.as_ref().and_then(|e| {
                        keyed_call_completion_emit(e, &result, verdict, key.as_deref())
                    });
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::AccessCheck { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let result = if self.effect_executor.environment().is_client() {
                        self.effect_executor.execute(&resolved, ctx)
                    } else {
                        self.run_access_check(&resolved)
                    };
                    let completion = emit
                        .as_ref()
                        .and_then(|e| keyed_call_completion_emit(e, &result, None, None));
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::BrowserPageFacts { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let result = self.run_page_facts(&resolved);
                    let completion = emit
                        .as_ref()
                        .and_then(|e| keyed_call_completion_emit(e, &result, None, None));
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Custom { name, emit, .. }
                    if orbital_core_base::agent_ports::is_agent_port_operator(name) =>
                {
                    let key = match &effect {
                        Effect::Custom { key: Some(key), .. } => ctx
                            .resolve_value_checked(key)
                            .ok()
                            .and_then(|k| k.as_str().map(str::to_string)),
                        _ => None,
                    };
                    self.io
                        .set_substrate_call(self.call_site_of(emit.as_ref()), key.clone());
                    let mut result = if self.effect_executor.environment().is_client() {
                        self.effect_executor.execute(&effect, ctx)
                    } else {
                        self.run_agent_port(&effect, ctx)
                    };
                    let verdict = self.io.take_call_verdict();
                    self.io.set_call_site(None);
                    if let Effect::Custom { emit: routes, .. } = &mut result.effect {
                        routes.clone_from(emit);
                    }
                    let completion = emit
                        .as_ref()
                        .filter(|_| verdict != Some(CallVerdict::Deferred))
                        .and_then(|e| {
                            agent_ports::agent_port_completion_emit(e, &result, key.as_deref())
                        });
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Custom {
                    emit: Some(op_emit),
                    ..
                } if !op_emit.is_empty() || op_emit.on_message.is_some() => {
                    let Some(resolved) = resolved_custom_or_failed(&effect, ctx, effect_results)
                    else {
                        return;
                    };
                    let key = custom_key(&resolved);
                    self.io
                        .set_substrate_call(self.call_site_of(Some(op_emit)), key.clone());
                    let mut result = self.effect_executor.execute(&resolved, ctx);
                    let verdict = self.io.take_call_verdict();
                    self.io.set_call_site(None);
                    if let Effect::Custom { emit: routes, .. } = &mut result.effect {
                        *routes = Some(op_emit.clone());
                    }
                    let completion = (verdict != Some(CallVerdict::Deferred))
                        .then(|| substrate_completion_emit(op_emit, &result, key.as_deref()))
                        .flatten();
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Custom { .. } => {
                    let Some(resolved) = resolved_custom_or_failed(&effect, ctx, effect_results)
                    else {
                        return;
                    };
                    effect_results.push(self.effect_executor.execute(&resolved, ctx));
                }
                Effect::CancelCall { .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    effect_results.push(self.effect_executor.execute(&resolved, ctx));
                }
                Effect::RandomBytes { emit, .. }
                | Effect::CredentialStatus { emit, .. }
                | Effect::CredentialForget { emit, .. }
                | Effect::CredentialAdmin { emit, .. }
                | Effect::OAuthAuthorize { emit, .. }
                | Effect::OAuthConnectionStatus { emit, .. }
                | Effect::OAuthConnectionForget { emit, .. } => {
                    let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) else {
                        return;
                    };
                    let result = if self.effect_executor.environment().is_client() {
                        self.effect_executor.execute(&resolved, ctx)
                    } else {
                        self.run_host_effect(&resolved, ctx)
                    };
                    let completion = emit
                        .as_ref()
                        .and_then(|e| keyed_call_completion_emit(e, &result, None, None));
                    effect_results.push(result);
                    effect_results.extend(completion.map(EffectResult::as_completion));
                }
                Effect::Emit { .. } => {
                    // Resolve now, against this leaf's scope (`let` locals, earlier sets), as JS does at emit time.
                    if let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) {
                        effect_results.push(self.effect_executor.execute(&resolved, ctx));
                    }
                }
                _ => {
                    effect_results.push(self.effect_executor.execute(&effect, ctx));
                }
            }
        }
    }

    /// Give each form/detail/column-bearing node of a server-role `render-ui` the options of its relation
    /// fields (`relationsData`); the browser role reads its own store.
    pub(super) fn inject_relation_options(&self, trait_name: &str, results: &mut [EffectResult]) {
        if !self.config.environment.is_server() {
            return;
        }
        let linked = self
            .trait_definitions
            .read()
            .get(trait_name)
            .and_then(|def| def.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        let defs = self.entity_definitions.read();
        for result in results {
            if let Effect::RenderUi { content, .. } = &mut result.effect {
                super::relations::inject_relation_options(
                    &*self.entity_source,
                    &defs,
                    &linked,
                    content,
                );
            }
        }
    }

    /// The call site of a `call-service` made by the step in flight.
    pub(super) fn call_site_of(&self, emit: Option<&FetchEmit>) -> Option<CallSite> {
        let (trait_name, follow_on) = self.step_site.read().clone()?;
        let entity_id = self
            .step_frame
            .read()
            .clone()
            .unwrap_or_else(|| RUNTIME_SINGLETON.to_string());
        let orbital = self.emit_source(&trait_name).orbital;
        let routes = emit.map_or_else(CallRoutes::default, |e| CallRoutes {
            success: e.success.clone(),
            failure: e.failure.clone(),
            cancelled: e.cancelled.clone(),
            on_message: e.on_message.clone(),
        });
        Some(CallSite {
            orbital,
            trait_name,
            follow_on,
            entity_id,
            emit: routes,
        })
    }

    /// The seed an unseeded `prob/*` stream of this dispatch takes: one journaled `Random` ask, made on first use, so a
    /// replay from the journal draws the same values and two dispatches draw different ones. Built once per kernel
    /// (the dispatch's own draw state lives in its context), so a dispatch only clones it.
    pub(super) fn prob_entropy(&self) -> EntropySource {
        self.prob_entropy.clone()
    }
}
