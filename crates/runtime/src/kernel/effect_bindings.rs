//! Resolving an effect's bindings against the step's context before it runs.

use super::*;

/// Resolve every `@`-binding in a client-facing effect's content/payload against `ctx`,
/// via the canonical `BindingContext::resolve_value`. Non-client effects pass through.
pub(super) fn resolve_effect_bindings(effect: Effect, ctx: &BindingContext) -> Result<Effect, EvalError> {
    Ok(match effect {
        Effect::RenderUi { slot, content } => {
            let slot = resolve_render_slot(slot, ctx)?;
            let resolved = ctx.resolve_value_checked(&content)?.into_owned();
            debug!(
                target: "almadar::kernel",
                slot = %slot,
                before = %content,
                after = %resolved,
                "kernel: resolve render-ui bindings"
            );
            Effect::RenderUi {
                slot,
                content: resolved,
            }
        }
        Effect::Emit { event, payload } => Effect::Emit {
            event,
            payload: ctx.resolve_dropping_unset_checked(&payload)?,
        },
        Effect::Navigate {
            path,
            params,
            options,
        } => Effect::Navigate {
            path,
            params: params
                .map(|p| {
                    p.iter()
                        .map(|(k, v)| Ok((k.clone(), ctx.resolve_value_checked(v)?.into_owned())))
                        .collect::<Result<_, EvalError>>()
                })
                .transpose()?,
            options: options.map(|o| orbital_core_base::client_effect::NavigateOptions {
                crumb: o.crumb.map(|c| {
                    ctx.resolve_value(&Value::string(c))
                        .into_owned()
                        .to_string_value()
                }),
            }),
        },
        Effect::CallService {
            service,
            method,
            args,
            key,
            emit,
        } => Effect::CallService {
            service,
            method,
            args: ctx.resolve_value_checked(&args)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::Http {
            method,
            origin,
            path,
            request,
            key,
            emit,
        } => Effect::Http {
            method,
            origin: ctx.resolve_value_checked(&origin)?.into_owned(),
            path: ctx.resolve_value_checked(&path)?.into_owned(),
            request: resolve_http_request(&request, ctx)?,
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::Postgres {
            origin,
            sql,
            params,
            options,
            key,
            emit,
        } => Effect::Postgres {
            origin: ctx.resolve_value_checked(&origin)?.into_owned(),
            sql: ctx.resolve_value_checked(&sql)?.into_owned(),
            params: ctx.resolve_value_checked(&params)?.into_owned(),
            options: ctx.resolve_value_checked(&options)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::Redis {
            origin,
            command,
            options,
            key,
            emit,
        } => Effect::Redis {
            origin: ctx.resolve_value_checked(&origin)?.into_owned(),
            command: ctx.resolve_value_checked(&command)?.into_owned(),
            options: ctx.resolve_value_checked(&options)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::BrowserPageFacts { fields, key, emit } => Effect::BrowserPageFacts {
            fields: ctx.resolve_value_checked(&fields)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::BrowserTab {
            op,
            request,
            key,
            emit,
        } => Effect::BrowserTab {
            op,
            request: ctx.resolve_value_checked(&request)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::BrowserPrompt {
            text,
            schema,
            key,
            emit,
        } => Effect::BrowserPrompt {
            text: ctx.resolve_value_checked(&text)?.into_owned(),
            schema: ctx.resolve_value_checked(&schema)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::BrowserEmbed { texts, key, emit } => Effect::BrowserEmbed {
            texts: ctx.resolve_value_checked(&texts)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::BrowserBeacon {
            url,
            request,
            key,
            emit,
        } => Effect::BrowserBeacon {
            url: ctx.resolve_value_checked(&url)?.into_owned(),
            request: ctx.resolve_value_checked(&request)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::CancelCall { key } => Effect::CancelCall {
            key: match ctx.resolve_value_checked(&key)?.into_owned() {
                list @ Value::Array(_) => list,
                other => Value::string(other.to_string_value()),
            },
        },
        Effect::Os {
            op,
            args,
            key,
            emit,
        } => Effect::Os {
            op,
            args: args
                .iter()
                .map(|arg| {
                    ctx.resolve_value_checked(arg)
                        .map(std::borrow::Cow::into_owned)
                })
                .collect::<Result<_, EvalError>>()?,
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::RandomBytes { bytes, key, emit } => Effect::RandomBytes {
            bytes: ctx.resolve_value_checked(&bytes)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::CredentialStatus { credential, emit } => Effect::CredentialStatus {
            credential: ctx.resolve_value_checked(&credential)?.into_owned(),
            emit,
        },
        Effect::CredentialForget { credential, emit } => Effect::CredentialForget {
            credential: ctx.resolve_value_checked(&credential)?.into_owned(),
            emit,
        },
        Effect::CredentialAdmin { op, request, emit } => Effect::CredentialAdmin {
            op,
            request: ctx.resolve_value_checked(&request)?.into_owned(),
            emit,
        },
        Effect::AccessCheck { request, emit } => Effect::AccessCheck {
            request: ctx.resolve_value_checked(&request)?.into_owned(),
            emit,
        },
        Effect::OAuthAuthorize {
            client,
            options,
            key,
            emit,
        } => Effect::OAuthAuthorize {
            client: ctx.resolve_value_checked(&client)?.into_owned(),
            options: ctx.resolve_value_checked(&options)?.into_owned(),
            key: key.map(|k| resolve_call_key(&k, ctx)).transpose()?,
            emit,
        },
        Effect::OAuthConnectionStatus { provider, emit } => Effect::OAuthConnectionStatus {
            provider: ctx.resolve_value_checked(&provider)?.into_owned(),
            emit,
        },
        Effect::OAuthConnectionForget { provider, emit } => Effect::OAuthConnectionForget {
            provider: ctx.resolve_value_checked(&provider)?.into_owned(),
            emit,
        },
        other => other,
    })
}

/// A substrate operator (`Effect::Custom`) with its arguments resolved against the step, as the JS executor resolves
/// every argument before the host sees it; a typed argument mismatch is the effect's failure. Not part of
/// `resolve_effect_bindings`, which also re-reads already-run effects for reporting and must not resolve data twice.
pub(super) fn resolved_custom_or_failed(
    effect: &Effect,
    ctx: &BindingContext,
    effect_results: &mut Vec<EffectResult>,
) -> Option<Effect> {
    let Effect::Custom {
        name,
        args,
        key,
        emit,
    } = effect
    else {
        return Some(effect.clone());
    };
    let mut resolved = Vec::with_capacity(args.len() + 1);
    for arg in args.iter().chain(key) {
        match ctx.resolve_value_checked(arg) {
            Ok(value) => resolved.push(value.into_owned()),
            Err(fault) => {
                effect_results.push(EffectResult::failed(
                    effect.clone(),
                    &OrbitalError::Eval(fault).to_string(),
                ));
                return None;
            }
        }
    }
    let key = key.as_ref().and_then(|_| resolved.pop());
    Some(Effect::Custom {
        name: name.clone(),
        args: resolved,
        key,
        emit: emit.clone(),
    })
}

/// `effect` with its bindings resolved, or `None` after recording the typed argument mismatch as the effect's failure.
pub(super) fn resolved_or_failed(
    effect: &Effect,
    ctx: &BindingContext,
    effect_results: &mut Vec<EffectResult>,
) -> Option<Effect> {
    match resolve_effect_bindings(effect.clone(), ctx) {
        Ok(resolved) => Some(resolved),
        Err(fault) => {
            tracing::error!(effect = %effect.operator(), error = %fault, "kernel: effect arguments failed to evaluate");
            effect_results.push(EffectResult::failed(
                effect.clone(),
                &OrbitalError::Eval(fault).to_string(),
            ));
            None
        }
    }
}

/// The effect with its bindings resolved; a typed argument mismatch in an embedded operator call is the
/// effect's failure, as it is when the JS executor throws.
pub(super) fn resolve_effect_result(result: EffectResult, ctx: &BindingContext) -> EffectResult {
    let mut result = result;
    if resolved_when_run(&result.effect) {
        return result;
    }
    match resolve_effect_bindings(result.effect.clone(), ctx) {
        Ok(effect) => {
            result.effect = effect;
            result
        }
        Err(fault) => EffectResult::failed(result.effect, &OrbitalError::Eval(fault).to_string()),
    }
}

/// An effect already resolved when it ran — every `emit` (its payload at emit time; a completion's is data) and
/// each kind `exec_leaf_effect` passes through `resolved_or_failed`. Resolving it again would evaluate its data
/// as code: a `(quote x)` payload's operator-headed lists, a row that holds an expression.
pub(super) fn resolved_when_run(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Emit { .. }
            | Effect::CallService { .. }
            | Effect::Http { .. }
            | Effect::Redis { .. }
            | Effect::Postgres { .. }
            | Effect::Os { .. }
            | Effect::BrowserBeacon { .. }
            | Effect::BrowserPrompt { .. }
            | Effect::BrowserTab { .. }
            | Effect::BrowserPageFacts { .. }
            | Effect::CancelCall { .. }
            | Effect::CredentialAdmin { .. }
            | Effect::AccessCheck { .. }
    )
}

/// A failed `os/` effect: its failure payload (`{ error, kind }`) rides the result's value.
pub(super) fn os_failure(effect: &Effect, error: &OsError) -> EffectResult {
    http_failure(
        effect,
        Value::object(ValueMap::from_iter([
            ("error".to_string(), Value::String(error.message.clone().into())),
            (
                "kind".to_string(),
                Value::String(error.kind.as_str().to_string().into()),
            ),
        ])),
    )
}

/// A failed `http` whose failure payload (`{ error, kind }`) rides the result's value.
pub(super) fn http_failure(effect: &Effect, payload: Value) -> EffectResult {
    let message = payload
        .get_path("error")
        .map(Value::to_string_value)
        .unwrap_or_default();
    let mut result = EffectResult::failed(effect.clone(), &message);
    result.return_value = Some(payload);
    result
}

/// The request with its bindings resolved, except `paginate.next` and `paginate.collect`: those are
/// evaluated per page against `@response`.
pub(super) fn resolve_http_request(request: &Value, ctx: &BindingContext) -> Result<Value, EvalError> {
    let held = request
        .as_object()
        .and_then(|fields| fields.get("paginate"))
        .and_then(Value::as_object)
        .map(|paginate| {
            ["next", "collect"]
                .into_iter()
                .filter_map(|k| paginate.get(k).map(|v| (k.to_string(), v.clone())))
                .collect::<Vec<_>>()
        })
        .filter(|held| !held.is_empty());
    let Some(held) = held else {
        return Ok(ctx.resolve_value_checked(request)?.into_owned());
    };
    let mut stripped = request.clone();
    if let Some(Value::Object(paginate)) = stripped
        .as_object_mut()
        .and_then(|fields| fields.get_mut("paginate"))
    {
        for (k, _) in &held {
            paginate.shift_remove(k);
        }
    }
    let mut resolved = ctx.resolve_value_checked(&stripped)?.into_owned();
    if let Some(Value::Object(paginate)) = resolved
        .as_object_mut()
        .and_then(|fields| fields.get_mut("paginate"))
    {
        for (k, v) in held {
            paginate.insert(k, v);
        }
    }
    Ok(resolved)
}

/// The fields a `browser/page-facts` asks for: a non-empty, duplicate-free list of the closed words.
pub(super) fn page_fact_fields(
    value: &Value,
) -> Result<Vec<orbital_core_base::ir::effects::PageFactField>, String> {
    use orbital_core_base::ir::effects::PageFactField;
    let Value::Array(words) = value else {
        return Err(
            "browser/page-facts: the fields must be a list of hostname, referrer, screenWidth"
                .to_string(),
        );
    };
    if words.is_empty() {
        return Err("browser/page-facts: the fields must name at least one fact".to_string());
    }
    let mut fields = Vec::with_capacity(words.len());
    for word in words {
        let text = word.to_string_value();
        let field = PageFactField::parse(&text).ok_or_else(|| {
            format!("browser/page-facts: {text} is not one of hostname, referrer, screenWidth")
        })?;
        if fields.contains(&field) {
            return Err(format!("browser/page-facts: {text} is listed twice"));
        }
        fields.push(field);
    }
    Ok(fields)
}

/// A `render-ui` slot written as a binding (`@config.detailSlot`) paints into the slot it names, as the JS
/// executor's `interpolateValue` does; one that names no slot is the effect's failure, never a slot of that text.
pub(super) fn resolve_render_slot(slot: String, ctx: &BindingContext) -> Result<String, EvalError> {
    if BindingContext::binding_token_end(&slot, 0) != Some(slot.len()) {
        return Ok(slot);
    }
    match ctx.resolve_binding(&slot) {
        Some(Value::String(name)) if !name.is_empty() => Ok(name.into_inner()),
        other => Err(EvalError::InvalidArgument(format!(
            "render-ui slot {slot} must name a slot, got {}",
            other.map_or_else(|| "nothing".to_string(), |v| v.to_string_value())
        ))),
    }
}

pub(super) fn resolve_call_key(key: &Value, ctx: &BindingContext) -> Result<Value, EvalError> {
    let value = ctx.resolve_value_checked(key)?;
    orbital_core_base::effect_types::call_key_text(&value)
        .map(|text| Value::string(text.to_string()))
}
