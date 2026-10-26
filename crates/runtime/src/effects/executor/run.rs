//! Running effects: routing one effect, and the effects that need no store (emit, set, render, navigate, spawn, log, service calls, custom).

use super::*;

impl EffectExecutor {
    /// Execute an effect
    pub fn execute(&self, effect: &Effect, ctx: &BindingContext) -> EffectResult {
        let _span = tracing::trace_span!(target: orbital_core_base::logging::ns::RUNTIME_EFFECTS, "effect", op = effect.operator()).entered();
        if self.environment.is_client() && !matches!(effect, Effect::Custom { .. }) && runs_on(effect.operator()) == RunsOn::Server {
            return self.delegate_to_server(effect.clone());
        }
        if self.environment.is_client() && matches!(effect, Effect::Custom { .. }) {
            match orbital_core_base::runs_on::runs_on_known(effect.operator()) {
                Some(RunsOn::Server) => return self.delegate_to_server(effect.clone()),
                Some(_) => {}
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
            Effect::Http { .. } => {
                EffectResult::noop(effect.clone(), "http is run by the kernel, which owns its host I/O")
            }
            Effect::Redis { .. } => {
                EffectResult::noop(effect.clone(), "redis is run by the kernel, which owns its host I/O")
            }
            Effect::Postgres { .. } => {
                EffectResult::noop(effect.clone(), "postgres is run by the kernel, which owns its host I/O")
            }
            Effect::CancelCall { key } => self.do_cancel_call(key),
            Effect::RandomBytes { .. }
            | Effect::Os { .. }
            | Effect::CredentialStatus { .. }
            | Effect::CredentialForget { .. }
            | Effect::CredentialAdmin { .. }
            | Effect::AccessCheck { .. }
            | Effect::OAuthAuthorize { .. }
            | Effect::OAuthConnectionStatus { .. }
            | Effect::OAuthConnectionForget { .. } => {
                EffectResult::noop(effect.clone(), "host effects are run by the kernel, which owns its host I/O")
            }
            Effect::BrowserPageFacts { .. }
            | Effect::BrowserBeacon { .. }
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
    pub fn execute_all(&self, effects: &[SExpression], ctx: &BindingContext) -> Vec<EffectResult> {
        effects
            .iter()
            .filter_map(orbital_core_base::effect_types::parse_effect)
            .map(|effect| self.execute(&effect, ctx))
            .collect()
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

    pub(super) fn do_set(&self, path: &str, value: &Value, _ctx: &BindingContext) -> EffectResult {
        let effect = Effect::Set {
            path: path.to_string(),
            value: value.clone(),
        };

        // Set works in both environments (modifies context)
        // In practice, the state machine manager handles this
        debug!(path = path, "Set effect (handled by state machine)");
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
        path: &str,
        params: Option<&ValueMap>,
        options: Option<orbital_core_base::client_effect::NavigateOptions>,
    ) -> EffectResult {
        let effect = Effect::Navigate {
            path: path.to_string(),
            params: params.cloned(),
            options,
        };

        // Navigate is client-only
        if self.environment.is_server() {
            return EffectResult::noop(effect, "navigate is client-only");
        }

        if let Some(handler) = &self.navigation_handler {
            match handler.navigate(path, params) {
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

    pub(super) fn do_log(&self, level: LogLevel, message: &str, data: Option<&Value>) -> EffectResult {
        let effect = Effect::Log {
            level,
            message: message.to_string(),
            data: data.cloned(),
        };

        // Log works in both environments; a program's `(log …)` lines share one namespace.
        use orbital_core_base::logging::ns::PROGRAM;
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

        debug!(name = name, args = ?args, "Custom effect (not handled)");
        EffectResult::noop(effect, "custom effects require specific handlers")
    }
}
