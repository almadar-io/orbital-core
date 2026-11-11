//! Running effects: routing one effect, and the effects that need no store (emit, set, render, navigate, spawn, log, service calls, custom).

use super::*;

impl EffectExecutor {
    /// Execute an effect
    pub fn execute(&self, effect: &Effect, ctx: &BindingContext) -> EffectResult {
        let _span = tracing::trace_span!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, "effect", op = effect.operator()).entered();
        if let Effect::Custom { name, args, .. } = effect {
            if orbital_core_base::runs_on::is_trace_subscription(name, args) {
                let client = self.environment.is_client();
                match self.trace_site(args) {
                    Err(reason) => return EffectResult::failed(effect.clone(), &reason),
                    Ok(RunsOn::Client) if client => return self.ask_client_host(effect.clone()),
                    Ok(RunsOn::Client) => {
                        return EffectResult::failed(
                            effect.clone(),
                            "program/live trace: the traits it observes run in the browser; open it from a client-run trait (EXEC-DIST-14)",
                        )
                    }
                    Ok(_) if client => return self.delegate_to_server(effect.clone()),
                    Ok(_) => {}
                }
            }
        }
        if self.environment.is_client() {
            if let Effect::CancelCall { key } = effect {
                if let Some(held) = self.host_held_streams(key) {
                    return held;
                }
            }
        }
        if self.environment.is_client() && !matches!(effect, Effect::Custom { .. }) && runs_on(effect.operator()) == RunsOn::Server {
            return self.delegate_to_server(effect.clone());
        }
        if self.environment.is_client() && matches!(effect, Effect::Custom { .. }) {
            match orbital_core_base::runs_on::runs_on_known(effect.operator()) {
                Some(RunsOn::Server) => return self.delegate_to_server(effect.clone()),
                Some(RunsOn::Client) => return self.ask_client_host(effect.clone()),
                Some(RunsOn::Any) => {}
                None => {
                    let reason = format!(
                        "{} cannot be placed: no operator table is installed and the program carries no operatorSites entry for it; recompile the client program",
                        effect.operator()
                    );
                    tracing::error!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, op = effect.operator(), "{reason}");
                    return EffectResult::failed(effect.clone(), &reason);
                }
            }
        }
        match effect {
            Effect::Emit { event, payload } => self.do_emit(event, payload),
            Effect::Set { path, value } => self.do_set(path, value, ctx),
            Effect::Persist {
                operation,
                entity_type,
                data,
                id,
                emit: _,
                guard,
            } => self.do_persist(*operation, entity_type, data, id.as_deref(), guard.as_ref(), ctx),
            Effect::PersistBatch { ops, emit: _ } => self.do_persist_batch(ops, ctx),
            Effect::Fetch {
                entity_type,
                id,
                filter,
                limit,
                offset,
                order_by,
                include,
                deliver,
                total,
                nearest,
                key: _,
                emit: _,
            } => self.do_fetch(
                entity_type,
                id.as_ref(),
                filter.as_ref(),
                limit.as_ref(),
                offset.as_ref(),
                order_by.as_ref(),
                include.as_ref(),
                *deliver,
                total.unwrap_or(RowTotalMode::Exact),
                nearest.as_ref(),
                ctx,
            ),
            Effect::CallService {
                service,
                method,
                args,
                key,
                ..
            } => match key.as_ref().map(call_key_text).transpose() {
                Ok(key) => self.do_call_service(service, method, args, key, ctx),
                Err(error) => EffectResult::failed(effect.clone(), &error.to_string()),
            },
            Effect::CancelCall { key } => self.do_cancel_call(key),
            Effect::RandomBytes { .. }
            | Effect::Os { .. }
            | Effect::CredentialStatus { .. }
            | Effect::CredentialForget { .. }
            | Effect::CredentialConnect { .. }
            | Effect::CredentialAdmin { .. }
            | Effect::AccessCheck { .. } => {
                EffectResult::noop(effect.clone(), "host effects are run by the kernel, which owns its host I/O")
            }
            Effect::BrowserPageFacts { .. }
            | Effect::BrowserPrompt { .. }
            | Effect::BrowserEmbed { .. }
            | Effect::BrowserTab { .. } => {
                EffectResult::noop(effect.clone(), "browser host effects are run by the kernel, which owns its host I/O")
            }
            Effect::RenderUi { slot, content } => self.do_render_ui(slot, content),
            Effect::Navigate {
                path,
                params,
                options,
            } => self.do_navigate(path, params.as_ref(), options.clone()),
            Effect::NavigateBack => self.do_navigate_back(),
            Effect::Spawn { entity_type, props } => self.do_spawn(entity_type, props),
            Effect::Despawn { entity_id } => self.do_despawn(entity_id),
            Effect::Log {
                level,
                message,
                data,
            } => self.do_log(*level, message, data.as_ref()),
            Effect::Custom { name, args, .. } => self.do_custom(name, args),
        }
    }

    /// Execute multiple effects from S-expressions
    pub fn execute_all(
        &self,
        effects: &[SExpression],
        ctx: &BindingContext,
    ) -> Result<Vec<EffectResult>, orbital_core_base::language_data::LanguageDataError> {
        let mut results = Vec::new();
        for expr in effects {
            if let Some(effect) = orbital_core_base::effect_types::parse_effect(expr)? {
                results.push(self.execute(&effect, ctx));
            }
        }
        Ok(results)
    }

    // ========================================================================
    // Effect Handlers
    // ========================================================================

    /// A `runsOn: server` effect on a client host: hand it to the delegate, or report it skipped.
    pub(super) fn delegate_to_server(&self, effect: Effect) -> EffectResult {
        if let Some(delegate) = &self.delegate {
            return match delegate.delegate(&effect, RuntimeEnvironment::Server) {
                Ok(()) => EffectResult::delegated(effect, RuntimeEnvironment::Server),
                Err(e) => EffectResult::failed(effect, &e),
            };
        }
        let reason = format!("{} is server-only", effect.operator());
        EffectResult::noop(effect, &reason)
    }

    /// Where a trace subscription runs (EXEC-DIST-14): on the client when every trait it names is client-run, on the
    /// server when every one is server-awaited; a mix, an empty list or a name no trait has is refused by name.
    fn trace_site(&self, args: &[Value]) -> Result<RunsOn, String> {
        let modes = self
            .trait_modes
            .as_ref()
            .ok_or("program/live trace: this kernel has no trait table to place it by")?;
        let names: Vec<&str> = match args.first().and_then(|o| o.get_path("traits")) {
            Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if names.is_empty() {
            return Err("program/live trace: name the traits it observes in `traits`".to_string());
        }
        let mut client = Vec::new();
        let mut server = Vec::new();
        for name in &names {
            match modes(name) {
                None => return Err(format!("program/live trace: `{name}` is not a registered trait")),
                Some(mode) if mode.steps_in_browser(self.environment.is_client()) => client.push(*name),
                Some(_) => server.push(*name),
            }
        }
        match (client.is_empty(), server.is_empty()) {
            (false, true) => Ok(RunsOn::Client),
            (true, false) => Ok(RunsOn::Server),
            _ => Err(format!(
                "program/live trace: {} run in the browser and {} on the server; one subscription observes one site",
                client.join(", "),
                server.join(", ")
            )),
        }
    }

    /// A `cancel-call` on a client of the trace streams its host holds: handed to the host as a `host-ask`. `None`
    /// when the key names none of them (the call is the server's); a mix of held and server keys is refused.
    fn host_held_streams(&self, key: &Value) -> Option<EffectResult> {
        let keys: Vec<&str> = match key {
            Value::String(only) => vec![only.as_str()],
            Value::Array(named) => named.iter().filter_map(Value::as_str).collect(),
            _ => return None,
        };
        let mut held = self.host_streams.lock();
        let ours = keys.iter().filter(|k| held.contains(**k)).count();
        if ours == 0 {
            return None;
        }
        let effect = Effect::CancelCall { key: key.clone() };
        if ours != keys.len() {
            return Some(EffectResult::failed(effect, "cancel-call: these keys mix trace streams the browser holds with calls the server holds; cancel them apart"));
        }
        for k in keys {
            held.remove(k);
        }
        Some(EffectResult::host_asked(effect))
    }

    /// A `runsOn: client` host-port effect on a client: the dispatch hands it to the client host as a `host-ask`
    /// client effect, which answers it. The host's answer is no part of this dispatch, so an effect that routes a
    /// completion (`emit`, a key) has nowhere to deliver it and fails by name.
    pub(super) fn ask_client_host(&self, effect: Effect) -> EffectResult {
        if let Effect::Custom { name, args, key, emit } = &effect {
            if orbital_core_base::runs_on::is_trace_subscription(name, args) {
                let key = key.as_ref().and_then(Value::as_str).filter(|k| !k.is_empty());
                let streamed = emit.as_ref().is_some_and(|e| e.on_message.is_some());
                return match key {
                    Some(key) if streamed => {
                        self.host_streams.lock().insert(key.to_string());
                        EffectResult::host_asked(effect)
                    }
                    _ => EffectResult::failed(effect, "program/live trace streams to its onMessage route: give it a key and an onMessage route"),
                };
            }
            let routed = emit.as_ref().is_some_and(|e| e.success.is_some() || e.failure.is_some());
            let reason = if emit.as_ref().is_some_and(|e| e.cancelled.is_some() || e.on_message.is_some()) {
                Some(format!("{name} runs on the client host, which answers a host ask once and cannot cancel or stream it: declare only emit.success and emit.failure"))
            } else if key.is_some() && !routed {
                Some(format!("{name} runs on the client host: a key names a completion, so declare emit.success or emit.failure"))
            } else if key.as_ref().is_some_and(|k| k.as_str().is_none_or(str::is_empty)) {
                Some(format!("{name}: the key must be one non-empty text"))
            } else {
                None
            };
            if let Some(reason) = reason {
                return EffectResult::failed(effect, &reason);
            }
        }
        EffectResult::host_asked(effect)
    }

    pub(super) fn do_emit(&self, event: &str, payload: &Value) -> EffectResult {
        let effect = Effect::Emit {
            event: event.to_string(),
            payload: payload.clone(),
        };

        // Emit works in both environments
        if let Some(bus) = &self.event_bus {
            bus.emit(Event::new(event, payload.clone()));
            debug!(event = event, "Emitted event");
            EffectResult::executed(effect)
        } else {
            EffectResult::noop(effect, "no event bus configured")
        }
    }

    pub(super) fn do_set(&self, path: &orbital_core_base::value::Shared<String>, value: &Value, _ctx: &BindingContext) -> EffectResult {
        let effect = Effect::Set {
            path: path.clone(),
            value: value.clone(),
        };

        // Set works in both environments (modifies context)
        // In practice, the state machine manager handles this
        debug!(path = path.as_str(), "Set effect (handled by state machine)");
        EffectResult::executed(effect)
    }

    pub(super) fn do_call_service(
        &self,
        service: &str,
        method: &str,
        args: &Value,
        key: Option<&str>,
        ctx: &BindingContext,
    ) -> EffectResult {
        let effect = Effect::CallService {
            service: service.to_string(),
            method: method.to_string(),
            args: args.clone(),
            key: key.map(Value::from),
            emit: None,
        };

        if let Some(handler) = &self.service_handler {
            match handler.call(
                service,
                method,
                args,
                key,
                ServiceCaller::of_user(&ctx.user).as_ref(),
            ) {
                Ok(value) => EffectResult::executed_with_value(effect, value),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no service handler configured")
        }
    }

    pub(super) fn do_cancel_call(&self, key: &Value) -> EffectResult {
        let effect = Effect::CancelCall { key: key.clone() };

        let keys: Vec<&str> = match key {
            Value::String(only) => vec![only.as_str()],
            Value::Array(named) => {
                let mut keys = Vec::with_capacity(named.len());
                for each in named {
                    match each {
                        Value::String(text) => keys.push(text.as_str()),
                        other => {
                            return EffectResult::failed(
                                effect,
                                &format!("cancel-call: key resolved to {}", other.to_string_value()),
                            )
                        }
                    }
                }
                keys
            }
            other => {
                return EffectResult::failed(
                    effect,
                    &format!("cancel-call: key resolved to {}", other.to_string_value()),
                )
            }
        };

        if let Some(handler) = self.service_handler.as_ref().or(self.call_canceller.as_ref()) {
            for each in keys {
                if let Err(e) = handler.cancel(each) {
                    return EffectResult::failed(effect, &e);
                }
            }
            EffectResult::executed(effect)
        } else {
            EffectResult::noop(effect, "no service handler configured")
        }
    }

    pub(super) fn do_render_ui(&self, slot: &str, content: &Value) -> EffectResult {
        let effect = Effect::RenderUi {
            slot: slot.to_string(),
            content: content.clone(),
        };

        // Render is client-only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "render-ui is client-only");
        }

        if let Some(handler) = &self.render_handler {
            match handler.render(slot, content) {
                Ok(()) => EffectResult::executed(effect),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no render handler configured")
        }
    }

    pub(super) fn do_navigate(
        &self,
        path: &Value,
        params: Option<&ValueMap>,
        options: Option<orbital_core_base::effect_types::NavigateOperands>,
    ) -> EffectResult {
        let effect = Effect::Navigate {
            path: path.clone(),
            params: params.cloned(),
            options,
        };

        // Navigate is client-only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "navigate is client-only");
        }

        if let Some(handler) = &self.navigation_handler {
            let Some(route) = path.as_str() else {
                return EffectResult::failed(effect, "navigate: the path is not text");
            };
            match handler.navigate(route, params) {
                Ok(()) => EffectResult::executed(effect),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no navigation handler configured")
        }
    }

    pub(super) fn do_navigate_back(&self) -> EffectResult {
        let effect = Effect::NavigateBack;

        // Navigate-back is client-only, like navigate
        if self.environment.is_server() {
            return EffectResult::noop(effect, "navigate-back is client-only");
        }

        if let Some(handler) = &self.navigation_handler {
            match handler.navigate_back() {
                Ok(()) => EffectResult::executed(effect),
                // A host without a stack is a noop, not a failure — headless
                // harnesses still count the transition as fired.
                Err(e) => EffectResult::noop(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no navigation handler configured")
        }
    }

    pub(super) fn do_spawn(&self, entity_type: &str, props: &Value) -> EffectResult {
        let effect = Effect::Spawn {
            entity_type: entity_type.to_string(),
            props: props.clone(),
        };

        // Spawn is client/game only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "spawn is client-only");
        }

        if let Some(handler) = &self.game_handler {
            match handler.spawn(entity_type, props) {
                Ok(id) => EffectResult::executed_with_value(effect, Value::string(id)),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no game handler configured")
        }
    }

    pub(super) fn do_despawn(&self, entity_id: &str) -> EffectResult {
        let effect = Effect::Despawn {
            entity_id: entity_id.to_string(),
        };

        // Despawn is client/game only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "despawn is client-only");
        }

        if let Some(handler) = &self.game_handler {
            match handler.despawn(entity_id) {
                Ok(()) => EffectResult::executed(effect),
                Err(e) => EffectResult::failed(effect, &e),
            }
        } else {
            EffectResult::noop(effect, "no game handler configured")
        }
    }

    pub(super) fn do_log(&self, level: LogLevel, message: &Value, data: Option<&Value>) -> EffectResult {
        let effect = Effect::Log {
            level,
            message: message.clone(),
            data: data.cloned(),
        };

        // Log works in both environments; a program's `(log …)` lines share one namespace.
        use orbital_core_base::logging::ns::PROGRAM;
        let message = message.to_string_value();
        match level {
            LogLevel::Debug => debug!(target: PROGRAM, data = ?data, "{message}"),
            LogLevel::Info => info!(target: PROGRAM, data = ?data, "{message}"),
            LogLevel::Warn => warn!(target: PROGRAM, data = ?data, "{message}"),
            LogLevel::Error => error!(target: PROGRAM, data = ?data, "{message}"),
        }

        EffectResult::executed(effect)
    }

    pub(super) fn do_custom(&self, name: &str, args: &[Value]) -> EffectResult {
        let effect = Effect::Custom {
            name: name.to_string(),
            args: args.to_vec(),
            key: None,
            emit: None,
        };

        if orbital_core_base::ir::effects::is_substrate_operator(name) {
            if let Some(handler) = &self.substrate_handler {
                return match handler.run(name, args) {
                    Ok(value) => EffectResult::executed_with_value(effect, value),
                    Err(failure) => {
                        let mut result = EffectResult::failed(effect, &failure.message);
                        if !failure.errors.is_empty() {
                            result.return_value = Some(Value::array(failure.errors));
                        }
                        result
                    }
                };
            }
            if self.environment.is_server() {
                return EffectResult::failed(
                    effect,
                    &format!("no substrate host configured for {name}"),
                );
            }
            return EffectResult::noop(effect, "substrate operators run on the server leg");
        }

        let reason = format!("no executor for effect `{name}` on this host");
        tracing::error!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, op = name, "{reason}");
        EffectResult::failed(effect, &reason)
    }
}
