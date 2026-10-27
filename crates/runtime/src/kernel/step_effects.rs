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
        let Some(effect) = orbital_core_base::effect_types::parse_effect(effect_expr) else {
            return;
        };
        let principal = || ServiceCaller::of_user(&ctx.user).map(|caller| caller.principal);
        match &effect {
            Effect::Set { path, value } => {
                let Some(evaluated_value) =
                    set_value(&effect, path, value, ctx, evaluator, effect_results)
                else {
                    return;
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
                emit: Some(routes), ..
            } if !routes.is_empty() => {
                let result = self
                    .effect_executor
                    .execute(&effect, ctx)
                    .with_declared_routes(routes);
                let completion = fetch_completion_emit(routes, &result);
                push_with_completion(effect_results, result, completion);
            }
            Effect::Persist {
                emit: Some(routes), ..
            }
            | Effect::PersistBatch {
                emit: Some(routes), ..
            } if !routes.is_empty() => {
                let result = self
                    .effect_executor
                    .execute(&effect, ctx)
                    .with_declared_routes(routes);
                let completion = persist_completion_emit(routes, &effect, &result, ctx);
                push_with_completion(effect_results, result, completion);
            }
            Effect::CallService { emit, .. } => {
                self.exec_call_service(&effect, emit, ctx, effect_results)
            }
            Effect::Http { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::HTTP,
                |e, _| self.run_http(e, principal().as_deref(), ctx.now),
            ),
            Effect::Redis { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::DATABASE,
                |e, _| self.run_redis(e, principal().as_deref()),
            ),
            Effect::Postgres { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::DATABASE,
                |e, _| self.run_postgres(e, principal().as_deref()),
            ),
            Effect::Os { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::OS,
                |e, _| self.run_os(e),
            ),
            Effect::BrowserBeacon { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::BEACON,
                |e, _| self.run_beacon(e),
            ),
            Effect::BrowserPrompt { emit, .. } | Effect::BrowserEmbed { emit, .. } => self
                .exec_call(
                    &effect,
                    emit.as_ref(),
                    ctx,
                    effect_results,
                    CallShape::BROWSER,
                    |e, key| self.run_on_device_model(e, key),
                ),
            Effect::BrowserTab { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::BROWSER,
                |e, _| self.run_browser_tab(e),
            ),
            Effect::AccessCheck { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::HOST,
                |e, _| self.run_access_check(e),
            ),
            Effect::BrowserPageFacts { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::PAGE_FACTS,
                |e, _| self.run_page_facts(e),
            ),
            Effect::Custom { name, emit, .. }
                if orbital_core_base::agent_ports::is_agent_port_operator(name) =>
            {
                self.exec_agent_port(&effect, emit, ctx, effect_results)
            }
            Effect::Custom {
                emit: Some(op_emit),
                ..
            } if !op_emit.is_empty() || op_emit.on_message.is_some() => {
                self.exec_substrate_op(&effect, op_emit, ctx, effect_results)
            }
            Effect::Custom { .. } => {
                let Some(resolved) = resolved_custom_or_failed(&effect, ctx, effect_results) else {
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
            | Effect::OAuthConnectionForget { emit, .. } => self.exec_call(
                &effect,
                emit.as_ref(),
                ctx,
                effect_results,
                CallShape::HOST,
                |e, _| self.run_host_effect(e, ctx),
            ),
            // An emit resolves now, against this leaf's scope (`let` locals, earlier sets), as JS does at emit time.
            Effect::CancelCall { .. } | Effect::Emit { .. } => {
                if let Some(resolved) = resolved_or_failed(&effect, ctx, effect_results) {
                    effect_results.push(self.effect_executor.execute(&resolved, ctx));
                }
            }
            _ => effect_results.push(self.effect_executor.execute(&effect, ctx)),
        }
    }

    /// A call-shaped effect: resolve it, run it (in place on the client role when its shape says so), settle its
    /// verdict, then deliver its completion.
    fn exec_call(
        &self,
        effect: &Effect,
        emit: Option<&FetchEmit>,
        ctx: &BindingContext,
        effect_results: &mut Vec<EffectResult>,
        shape: CallShape,
        run: impl FnOnce(&Effect, Option<&str>) -> EffectResult,
    ) {
        let Some(resolved) = resolved_or_failed(effect, ctx, effect_results) else {
            return;
        };
        let key = resolved.call_key().map(str::to_string);
        let (mut result, verdict) =
            if shape.client_executes && self.effect_executor.environment().is_client() {
                (self.effect_executor.execute(&resolved, ctx), None)
            } else {
                if shape.at_call_site {
                    self.io.set_call_site(self.call_site_of(emit));
                }
                let result = run(&resolved, key.as_deref());
                let verdict = shape.settles.and_then(|_| self.io.take_call_verdict());
                if shape.at_call_site {
                    self.io.set_call_site(None);
                }
                (result, verdict)
            };
        if verdict.is_some_and(|v| shape.settles.is_some_and(|settles| settles.contains(&v))) {
            result = EffectResult::executed(resolved);
        }
        let echoed = if shape.echoes_key {
            key.as_deref()
        } else {
            None
        };
        let completion = emit.and_then(|e| keyed_call_completion_emit(e, &result, verdict, echoed));
        push_with_completion(effect_results, result, completion);
    }

    /// `call-service`: a key already used in this step fails the call; otherwise it runs at its call site.
    fn exec_call_service(
        &self,
        effect: &Effect,
        emit: &Option<FetchEmit>,
        ctx: &BindingContext,
        effect_results: &mut Vec<EffectResult>,
    ) {
        let Some(resolved) = resolved_or_failed(effect, ctx, effect_results) else {
            return;
        };
        let key = resolved.call_key().map(str::to_string);
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
        let completion = emit
            .as_ref()
            .and_then(|e| keyed_call_completion_emit(e, &result, verdict, key.as_deref()));
        push_with_completion(effect_results, result, completion);
    }

    /// An agent-port operator: a substrate call keyed by its evaluated key; a deferred call completes later.
    fn exec_agent_port(
        &self,
        effect: &Effect,
        emit: &Option<FetchEmit>,
        ctx: &BindingContext,
        effect_results: &mut Vec<EffectResult>,
    ) {
        let key = match effect {
            Effect::Custom { key: Some(key), .. } => ctx
                .resolve_value_checked(key)
                .ok()
                .and_then(|k| k.as_str().map(str::to_string)),
            _ => None,
        };
        self.io
            .set_substrate_call(self.call_site_of(emit.as_ref()), key.clone());
        let mut result = if self.effect_executor.environment().is_client() {
            self.effect_executor.execute(effect, ctx)
        } else {
            self.run_agent_port(effect, ctx)
        };
        let verdict = self.io.take_call_verdict();
        self.io.set_call_site(None);
        if let Effect::Custom { emit: routes, .. } = &mut result.effect {
            routes.clone_from(emit);
        }
        let completion = emit
            .as_ref()
            .filter(|_| verdict != Some(CallVerdict::Deferred))
            .and_then(|e| agent_ports::agent_port_completion_emit(e, &result, key.as_deref()));
        push_with_completion(effect_results, result, completion);
    }

    /// A routed substrate operator (`emit:` or `onMessage`): a substrate call; a deferred call completes later.
    fn exec_substrate_op(
        &self,
        effect: &Effect,
        op_emit: &FetchEmit,
        ctx: &BindingContext,
        effect_results: &mut Vec<EffectResult>,
    ) {
        let Some(resolved) = resolved_custom_or_failed(effect, ctx, effect_results) else {
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
        push_with_completion(effect_results, result, completion);
    }

    /// Give each form/detail/column-bearing node of a server-role `render-ui` the options of its relation
    /// fields (`relationsData`); the browser role reads its own store.
    pub(super) fn inject_relation_options(&self, trait_name: &str, results: &mut [EffectResult]) {
        if !self.config.environment.is_server() {
            return;
        }
        let linked = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|def| def.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        let defs = self.registry.entity_definitions.read();
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
        let (trait_name, follow_on) = self.step.step_site.read().clone()?;
        let entity_id = self
            .step
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

/// How a call-shaped effect runs around its runner.
#[derive(Clone, Copy)]
struct CallShape {
    /// The client role executes the effect in place instead of running it.
    client_executes: bool,
    /// The call records its call site while it runs.
    at_call_site: bool,
    /// When set, the call's verdict is read, and these verdicts leave the effect executed with no answer.
    settles: Option<&'static [CallVerdict]>,
    /// The completion echoes the call's key.
    echoes_key: bool,
}

impl CallShape {
    const HTTP: CallShape = CallShape {
        client_executes: true,
        at_call_site: true,
        settles: Some(&[CallVerdict::Cancelled, CallVerdict::Deferred]),
        echoes_key: true,
    };
    const DATABASE: CallShape = CallShape {
        client_executes: true,
        at_call_site: true,
        settles: None,
        echoes_key: true,
    };
    const OS: CallShape = CallShape {
        client_executes: true,
        at_call_site: true,
        settles: Some(&[CallVerdict::Cancelled]),
        echoes_key: true,
    };
    const BEACON: CallShape = CallShape {
        client_executes: false,
        at_call_site: true,
        settles: Some(&[CallVerdict::Cancelled]),
        echoes_key: true,
    };
    const BROWSER: CallShape = CallShape {
        client_executes: false,
        at_call_site: false,
        settles: Some(&[CallVerdict::Cancelled]),
        echoes_key: true,
    };
    const HOST: CallShape = CallShape {
        client_executes: true,
        at_call_site: false,
        settles: None,
        echoes_key: false,
    };
    const PAGE_FACTS: CallShape = CallShape {
        client_executes: false,
        at_call_site: false,
        settles: None,
        echoes_key: false,
    };
}

/// The value a `set` stores: a list or map literal is evaluated, a whole binding resolves (to null when it resolves to
/// nothing; its text is never stored), and a value that fails to evaluate fails the effect.
fn set_value(
    effect: &Effect,
    path: &str,
    value: &Value,
    ctx: &BindingContext,
    evaluator: &Evaluator,
    effect_results: &mut Vec<EffectResult>,
) -> Option<Value> {
    if value.as_array().is_some() || value.as_object().is_some() {
        match evaluator.evaluate(&value_to_sexpr(value), ctx) {
            Ok(v) => Some(v),
            Err(err) => {
                tracing::error!(path = %path, error = %err, "kernel: set value failed to evaluate");
                effect_results.push(EffectResult::failed(effect.clone(), &err.to_string()));
                None
            }
        }
    } else if let Some(s) = value.as_str() {
        if BindingContext::binding_token_end(s, 0) == Some(s.len()) {
            Some(ctx.resolve_binding(s).unwrap_or(Value::Null))
        } else if s.starts_with('@') {
            Some(ctx.resolve_value(value).into_owned())
        } else {
            Some(value.clone())
        }
    } else {
        Some(value.clone())
    }
}

fn push_with_completion(
    effect_results: &mut Vec<EffectResult>,
    result: EffectResult,
    completion: Option<EffectResult>,
) {
    effect_results.push(result);
    effect_results.extend(completion.map(EffectResult::as_completion));
}
