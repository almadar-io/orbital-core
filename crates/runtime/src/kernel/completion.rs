//! The completion emits a persist, fetch, keyed call or substrate effect fires.

use super::*;

/// The consecutive top-level plain `persist` effects of `effects` from `start`: a write run. Effects nested in
/// `let`/`do`/`if`/`when` are not top-level, and a `persist batch` is its own request, so neither joins a run.
pub(super) fn persist_run(effects: &[SExpression], start: usize) -> Vec<Effect> {
    effects[start..]
        .iter()
        .map_while(
            |expr| match orbital_core_base::effect_types::parse_effect(expr) {
                Some(effect @ Effect::Persist { .. }) => Some(effect),
                _ => None,
            },
        )
        .collect()
}

/// Whether a failed effect is one the JS executor throws, the failure a response reports as `effect-failed`.
/// A `set` that cannot evaluate, and a `call-service` or substrate operator with no declared `emit.failure`
/// route, are; a `persist` refusal (policy, required column) and a `fetch` miss are handled outcomes. A thrown
/// failure stops its transition: no later effect runs and the step reports no client effect or emit.
pub(super) fn is_thrown_failure(result: &EffectResult) -> bool {
    if !result.is_failed() {
        return false;
    }
    let routed = |emit: &Option<FetchEmit>| emit.as_ref().is_some_and(|e| e.failure.is_some());
    match &result.effect {
        Effect::Set { .. }
        | Effect::CancelCall { .. }
        | Effect::RenderUi { .. }
        | Effect::Navigate { .. }
        | Effect::Emit { .. } => true,
        Effect::CallService { emit, .. }
        | Effect::Custom { emit, .. }
        | Effect::BrowserPageFacts { emit, .. }
        | Effect::BrowserPrompt { emit, .. }
        | Effect::BrowserEmbed { emit, .. }
        | Effect::BrowserTab { emit, .. }
        | Effect::RandomBytes { emit, .. }
        | Effect::Os { emit, .. }
        | Effect::CredentialStatus { emit, .. }
        | Effect::CredentialForget { emit, .. }
        | Effect::CredentialConnect { emit, .. }
        | Effect::CredentialAdmin { emit, .. }
        | Effect::AccessCheck { emit, .. } => !routed(emit),
        _ => false,
    }
}

/// A keyed call's (`call-service`, `http`) declared completion — JS `EffectExecutor.ts`: `emit.success`
/// carries the result as the payload, `emit.failure` the failure's own payload when it has one (an `http`
/// failure carries `{ error, kind }`) else `{ error }`. A delegated or no-op call fires nothing.
pub(super) fn keyed_call_completion_emit(
    emit: &FetchEmit,
    result: &EffectResult,
    verdict: Option<CallVerdict>,
    key: Option<&str>,
) -> Option<EffectResult> {
    let (event, payload) = match (verdict, &result.outcome) {
        (Some(CallVerdict::Deferred), _) => return None,
        (Some(CallVerdict::Cancelled), _) => {
            let key = key?;
            (
                emit.cancelled.clone()?,
                Value::object(ValueMap::from_iter([(
                    "key".into(),
                    Value::String(key.to_string().into()),
                )])),
            )
        }
        (None, EffectOutcome::Executed) => (
            emit.success.clone()?,
            echo_key(
                result.return_value.clone().unwrap_or(Value::Null),
                &result.effect,
                key,
            ),
        ),
        (None, EffectOutcome::Failed) => (
            emit.failure.clone()?,
            echo_key(
                result
                    .return_value
                    .clone()
                    .unwrap_or_else(|| error_payload(result, None)),
                &result.effect,
                key,
            ),
        ),
        (None, EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) | EffectOutcome::HostAsked) => return None,
    };
    Some(EffectResult::executed(Effect::Emit { event, payload }))
}

/// A keyed `call-service` echoes its key beside the record it answers with (D-A82); an `http` payload already
/// carries it (the driver owns it), and a result that is not a record is left as it is.
pub(super) fn echo_key(payload: Value, effect: &Effect, key: Option<&str>) -> Value {
    match effect {
        Effect::CallService { .. } => with_key(payload, key),
        _ => payload,
    }
}

/// The resolved `key` of a substrate op's options.
pub(super) fn custom_key(effect: &Effect) -> Option<String> {
    match effect {
        Effect::Custom { key: Some(key), .. } => key.as_str().map(str::to_string),
        _ => None,
    }
}

/// A completion payload with the call's `key` beside it (D-A82); a payload that is not a record is left as it is.
pub(super) fn with_key(payload: Value, key: Option<&str>) -> Value {
    match (key, payload) {
        (Some(key), Value::Object(mut fields)) => {
            fields.insert("key".into(), Value::string(key.to_string()));
            Value::Object(fields)
        }
        (_, payload) => payload,
    }
}

/// A substrate op's declared completion — JS `runSubstrate`/`runProgramEffectArm`: `emit.success`
/// carries `{ result }`, `emit.failure` `{ error }` plus `errors` (a program's validator issues), each with the
/// op's `key` when it declares one.
pub(super) fn substrate_completion_emit(
    emit: &FetchEmit,
    result: &EffectResult,
    key: Option<&str>,
) -> Option<EffectResult> {
    let (event, payload) = match &result.outcome {
        EffectOutcome::Executed => (
            emit.success.clone()?,
            crate::agent_ports::completion_result(
                match &result.effect {
                    Effect::Custom { name, .. } => name.as_str(),
                    _ => "",
                },
                result.return_value.clone().unwrap_or(Value::Null),
            ),
        ),
        EffectOutcome::Failed => (
            emit.failure.clone()?,
            error_payload(result, result.return_value.clone()),
        ),
        EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) | EffectOutcome::HostAsked => return None,
    };
    let payload = with_key(payload, key);
    Some(EffectResult::executed(Effect::Emit { event, payload }))
}

pub(super) fn error_payload(result: &EffectResult, errors: Option<Value>) -> Value {
    let mut payload = ValueMap::default();
    payload.insert(
        "error".into(),
        Value::string(result.error.clone().unwrap_or_default()),
    );
    if let Some(errors) = errors {
        payload.insert("errors".into(), errors);
    }
    Value::object(payload)
}

pub(super) fn fetch_completion_emit(emit: &FetchEmit, result: &EffectResult, key: Option<&str>) -> Option<EffectResult> {
    let (event, payload) = match &result.outcome {
        EffectOutcome::Executed => {
            let event = emit.success.clone()?;
            let rows = result
                .return_value
                .clone()
                .unwrap_or_else(|| Value::array(Vec::new()));
            // The TS wire (`EffectExecutor.ts`): a by-id hit's `data` is the row itself.
            let by_id = matches!(&result.effect, Effect::Fetch { id: Some(_), .. });
            let data = match rows {
                Value::Array(mut r) if by_id && r.len() == 1 => r.remove(0),
                other => other,
            };
            let mut payload = ValueMap::default();
            payload.insert("data".into(), data);
            if let Some(total) = result.total_count {
                payload.insert("totalCount".into(), Value::Number(total as f64));
            }
            (event, Value::object(payload))
        }
        EffectOutcome::Failed => {
            let mut payload = error_payload(result, None);
            if let Value::Object(fields) = &mut payload {
                if let Some(c) = result.code {
                    fields.insert("code".into(), Value::string(c.as_str().to_string()));
                }
            }
            (emit.failure.clone()?, payload)
        }
        EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) | EffectOutcome::HostAsked => return None,
    };
    let payload = with_key(payload, key);
    Some(EffectResult::executed(Effect::Emit { event, payload }))
}

/// A `persist`'s declared completion: `emit.failure` with `{ error, code }` when it failed (access denial,
/// missing required column; `code` is the store's `duplicate-id` / `not-found` / `no-row-key`, absent for any
/// other cause), `emit.success` when it ran, carrying the
/// stored row for create/update and `{ id, deleted: true }` for delete (the JS runtime's
/// payloads, `emit_contract::success_payload`). A delegated or no-op persist fires
/// nothing here; the leg that runs it does.
pub(super) fn persist_completion_emit(
    emit: &FetchEmit,
    effect: &Effect,
    result: &EffectResult,
    ctx: &BindingContext,
) -> Option<EffectResult> {
    let (event, payload) = match result.outcome {
        EffectOutcome::Failed => {
            let mut payload = ValueMap::default();
            if let Some(err) = &result.error {
                payload.insert("error".into(), Value::string(err.clone()));
            }
            if let Some(c) = result.code {
                payload.insert("code".into(), Value::string(c.as_str().to_string()));
            }
            if matches!(effect, Effect::PersistBatch { .. }) {
                let at = result.batch_failure;
                if let Some(f) = at {
                    payload.insert("index".into(), Value::Number(f.index as f64));
                }
                payload.insert(
                    "completedCount".into(),
                    Value::Number(at.map_or(0, |f| f.completed_count) as f64),
                );
            }
            (emit.failure.clone()?, Value::object(payload))
        }
        EffectOutcome::Executed => {
            let event = emit.success.clone()?;
            let payload = match effect {
                Effect::Persist {
                    operation: PersistOperation::Delete,
                    id,
                    ..
                } => {
                    let mut payload = ValueMap::default();
                    // The declared id is a binding (`@payload.id`): the payload carries the id it named.
                    if let Some(id) = id {
                        payload.insert(
                            "id".into(),
                            ctx.resolve_value(&Value::string(id.clone())).into_owned(),
                        );
                    }
                    payload.insert("deleted".into(), Value::Bool(true));
                    Value::object(payload)
                }
                _ => result.return_value.clone().unwrap_or(Value::Null),
            };
            (event, payload)
        }
        EffectOutcome::NoOp(_) | EffectOutcome::Delegated(_) | EffectOutcome::HostAsked => return None,
    };
    let envelope = EffectResult::executed(Effect::Emit { event, payload });
    Some(match result.outcome {
        EffectOutcome::Executed => envelope.as_persist_success(),
        _ => envelope,
    })
}

#[cfg(test)]
mod absent_not_null_tests {
    use super::*;

    fn assert_no_null(payload: &Value, what: &str) {
        match payload {
            Value::Null => {
                panic!("{what}: a payload field is null, an absent optional field must be omitted")
            }
            Value::Object(fields) => fields
                .iter()
                .for_each(|(name, v)| assert_no_null(v, &format!("{what}.{name}"))),
            Value::Array(items) => items.iter().for_each(|v| assert_no_null(v, what)),
            _ => {}
        }
    }

    fn emitted(result: Option<EffectResult>) -> Value {
        match result.expect("a completion").effect {
            Effect::Emit { payload, .. } => payload,
            other => panic!("not an emit: {other:?}"),
        }
    }

    fn routes() -> FetchEmit {
        FetchEmit {
            success: Some("OK".into()),
            failure: Some("BAD".into()),
            cancelled: None,
            on_message: None,
        }
    }

    fn persist(operation: PersistOperation, id: Option<&str>) -> Effect {
        Effect::Persist {
            operation,
            entity_type: "Task".into(),
            data: Value::Null,
            id: id.map(str::to_string),
            emit: Some(routes()),
            guard: None,
        }
    }

    #[test]
    fn no_unkeyed_payload_with_an_absent_optional_leg_holds_null() {
        let fetch = orbital_core_base::effect_types::parse_effect(
            &serde_json::from_value::<SExpression>(serde_json::json!(["fetch", "Task"])).unwrap(),
        )
        .expect("a fetch effect");
        let failed = EffectResult::failed(fetch, "boom");
        let payload = emitted(fetch_completion_emit(&routes(), &failed, None));
        assert_no_null(&payload, "fetch failure");
        assert!(payload.as_object().unwrap().get("code").is_none());

        let ctx = BindingContext::new();
        for (name, effect) in [
            (
                "persist create failure",
                persist(PersistOperation::Create, None),
            ),
            (
                "persist batch failure",
                Effect::PersistBatch {
                    ops: Value::array(Vec::new()),
                    emit: Some(routes()),
                },
            ),
        ] {
            let failed = EffectResult::failed(effect.clone(), "boom");
            assert_no_null(
                &emitted(persist_completion_emit(&routes(), &effect, &failed, &ctx)),
                name,
            );
        }

        let delete = persist(PersistOperation::Delete, None);
        let done = EffectResult::executed(delete.clone());
        assert_no_null(
            &emitted(persist_completion_emit(&routes(), &delete, &done, &ctx)),
            "persist delete without id",
        );
    }
}
