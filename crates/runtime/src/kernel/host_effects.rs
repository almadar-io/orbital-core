//! Effects the host answers: access checks, random bytes, http, redis, os, beacons, on-device models, tabs and page facts.

use super::*;

impl RuntimeKernel {
    /// `access-check`: the registered access policy's verdict on `row` for `viewer`, through the one evaluator
    /// fetch and persist enforce (`entity_access`); no host ask.
    pub(super) fn run_access_check(&self, effect: &Effect) -> EffectResult {
        let Effect::AccessCheck { request, .. } = effect else {
            return EffectResult::failed(
                effect.clone(),
                "access-check: not an access-check effect",
            );
        };
        let field = |name: &str| request.as_object().and_then(|fields| fields.get(name));
        let fail = |message: String| {
            EffectResult::failed(effect.clone(), &format!("access-check: {message}"))
        };
        for name in ["entity", "action", "row", "viewer"] {
            if field(name).is_none() {
                let hint = if name == "viewer" {
                    " (null for nobody)"
                } else {
                    ""
                };
                return fail(format!("the request needs `{name}`{hint}"));
            }
        }
        let Some(entity) = field("entity").and_then(Value::as_str) else {
            return fail("entity must be an entity name".to_string());
        };
        let action = match field("action").and_then(Value::as_str) {
            Some("create") => PersistOperation::Create,
            Some("read") => PersistOperation::Read,
            Some("update") => PersistOperation::Update,
            Some("delete") => PersistOperation::Delete,
            other => {
                let got = other.map_or_else(
                    || {
                        field("action")
                            .map(Value::to_string_value)
                            .unwrap_or_default()
                    },
                    str::to_string,
                );
                return fail(format!(
                    "action must be create, read, update or delete, got `{got}`"
                ));
            }
        };
        let row = field("row").cloned().unwrap_or(Value::Null);
        if !matches!(row, Value::Object(_)) {
            return fail("row must be an object".to_string());
        }
        let Some(policies) = self.registry.entity_policies.read().get(entity).cloned() else {
            return fail(format!("`{entity}` is not an entity of this program"));
        };
        let mut viewer_ctx = BindingContext::new();
        match field("viewer") {
            Some(Value::Null) | None => {}
            Some(viewer @ Value::Object(_)) => {
                viewer_ctx.set_user(viewer.clone());
            }
            Some(_) => return fail("viewer must be a user object or null".to_string()),
        }
        let allowed = match action {
            PersistOperation::Read => {
                let mut rows = vec![row];
                apply_read_access(&mut rows, policies.read.as_ref(), None, &viewer_ctx, entity);
                !rows.is_empty()
            }
            write => {
                let policy = match write {
                    PersistOperation::Create => policies.create.as_ref(),
                    PersistOperation::Update => policies.update.as_ref(),
                    _ => policies.delete.as_ref(),
                };
                super::entity_access::check_mutation_access(
                    write,
                    &row,
                    policy,
                    &viewer_ctx,
                    entity,
                )
                .is_ok()
            }
        };
        EffectResult::executed_with_value(
            effect.clone(),
            Value::object(ValueMap::from_iter([(
                "allowed".into(),
                Value::Bool(allowed),
            )])),
        )
    }

    /// `random-bytes`: `count` bytes of host CSPRNG output as base64url, or the failure.
    pub(super) fn run_random_bytes(&self, effect: &Effect, bytes: &Value) -> EffectResult {
        let count = match bytes {
            Value::Number(n) if n.fract() == 0.0 && (1.0..=RANDOM_BYTES_MAX).contains(n) => *n as u32,
            other => {
                return EffectResult::failed(
                    effect.clone(),
                    &format!(
                        "random-bytes: the byte count must be an integer from 1 to {RANDOM_BYTES_MAX}, got {}",
                        other.to_string_value()
                    ),
                )
            }
        };
        match self.io.random(count) {
            Ok(raw) => EffectResult::executed_with_value(
                effect.clone(),
                Value::object(ValueMap::from_iter([(
                    "value".into(),
                    Value::String(codec::base64url_encode(&raw).into()),
                )])),
            ),
            Err(message) => EffectResult::failed(effect.clone(), &message),
        }
    }

    /// Run `random-bytes`, `credential-status`, `credential-forget` or `credential-connect` through one journaled ask.
    pub(super) fn run_host_effect(&self, effect: &Effect, ctx: &BindingContext) -> EffectResult {
        let principal = ServiceCaller::of_user(&ctx.user).map(|caller| caller.principal);
        match effect {
            Effect::RandomBytes { bytes, key, .. } => {
                let mut result = self.run_random_bytes(effect, bytes);
                if let Some(key) = key {
                    let payload = match result.return_value.take() {
                        Some(Value::Object(mut fields)) => {
                            fields.insert("key".into(), key.clone());
                            Value::Object(fields)
                        }
                        _ => Value::object(ValueMap::from_iter([
                            (
                                "error".into(),
                                Value::String(result.error.clone().unwrap_or_default().into()),
                            ),
                            ("key".into(), key.clone()),
                        ])),
                    };
                    result.return_value = Some(payload);
                }
                result
            }
            Effect::CredentialStatus { credential, .. }
            | Effect::CredentialForget { credential, .. } => {
                let status = matches!(effect, Effect::CredentialStatus { .. });
                let head = if status {
                    "credential-status"
                } else {
                    "credential-forget"
                };
                if !status || !matches!(credential, Value::Object(_)) {
                    if principal.is_none() {
                        return EffectResult::failed(
                            effect.clone(),
                            &format!("{head}: nobody is signed in"),
                        );
                    }
                    if !matches!(credential, Value::String(_)) {
                        return EffectResult::failed(
                            effect.clone(),
                            &format!("{head}: the credential must be a secret knob's name"),
                        );
                    }
                }
                let slot =
                    match orbital_core_base::http_wire::parse_credential_status_slot(credential) {
                        Ok(slot) => slot,
                        Err(error) => {
                            return EffectResult::failed(effect.clone(), &error.message())
                        }
                    };
                let principal = match slot.scope {
                    orbital_core_base::http_wire::SecretScope::App => None,
                    orbital_core_base::http_wire::SecretScope::User => match principal.as_deref() {
                        Some(principal) => Some(principal),
                        None => {
                            return EffectResult::failed(
                                effect.clone(),
                                &format!("{head}: nobody is signed in"),
                            )
                        }
                    },
                };
                let answer = if status {
                    self.io.credential_status(&slot.name, principal)
                } else {
                    self.io
                        .credential_forget(&slot.name, principal)
                        .map(|()| crate::credential_admin::CredentialStanding::of(false))
                };
                match answer {
                    Ok(standing) => EffectResult::executed_with_value(
                        effect.clone(),
                        Value::object(ValueMap::from_iter([
                            ("connected".into(), Value::Bool(standing.connected)),
                            (
                                "label".into(),
                                Value::String(standing.label.unwrap_or_default().into()),
                            ),
                        ])),
                    ),
                    Err(message) => EffectResult::failed(effect.clone(), &message),
                }
            }
            Effect::CredentialConnect { request, .. } => {
                let failure = |failure: crate::credential_admin::ConnectFailure| {
                    http_failure(
                        effect,
                        Value::object(ValueMap::from_iter([
                            ("error".into(), Value::String(failure.message.into())),
                            ("kind".into(), Value::String(failure.kind.as_str().to_string().into())),
                        ])),
                    )
                };
                match crate::credential_admin::CredentialGrant::from_request(request, principal.as_deref())
                    .and_then(|grant| self.io.credential_connect(grant))
                {
                    Ok(standing) => EffectResult::executed_with_value(
                        effect.clone(),
                        Value::object(ValueMap::from_iter([
                            ("connected".into(), Value::Bool(standing.connected)),
                            ("label".into(), Value::String(standing.label.unwrap_or_default().into())),
                        ])),
                    ),
                    Err(error) => failure(error),
                }
            }
            Effect::CredentialAdmin { op, request, .. } => match (if self.os_grants().credentials_allow(orbital_core_base::os_grants::CredentialKind::Admin) {
                Ok(())
            } else {
                Err(format!(
                    "{}: the program declares no `credentials: [admin]` grant",
                    op.name()
                ))
            })
            .and_then(|()| crate::credential_admin::ask(&self.io, *op, request))
            .and_then(|reply| self.with_declared(*op, reply))
            {
                Ok(reply) => EffectResult::executed_with_value(effect.clone(), reply),
                Err(message) => http_failure(
                    effect,
                    Value::object(ValueMap::from_iter([("error".into(), Value::String(message.into()))])),
                ),
            },
            other => EffectResult::noop(other.clone(), "not a host effect"),
        }
    }

    /// Run an `os/<op>` effect through one journaled ask. A streaming op needs a key, so `cancel-call` can end it.
    pub(super) fn run_os(&self, effect: &Effect, ctx: &BindingContext) -> EffectResult {
        let Effect::Os { op, args, key, .. } = effect else {
            return EffectResult::noop(effect.clone(), "not an os effect");
        };
        let key = match key
            .as_ref()
            .map(orbital_core_base::effect_types::call_key_text)
            .transpose()
        {
            Ok(key) => key,
            Err(error) => {
                return os_failure(
                    effect,
                    &OsError::new(OsErrorKind::Invalid, error.to_string()),
                )
            }
        };
        if op.is_stream() && *op != orbital_core_base::os::OsOp::StdinRead && key.is_none() {
            return os_failure(
                effect,
                &OsError::new(
                    OsErrorKind::Invalid,
                    format!("{}: a stream needs a key", op.name()),
                ),
            );
        }
        let keyed = |payload: Value| match (key, payload) {
            (Some(key), Value::Object(mut fields)) => {
                fields.insert("key".into(), Value::string(key.to_string()));
                Value::Object(fields)
            }
            (Some(key), other) => Value::object(ValueMap::from_iter([
                ("value".into(), other),
                ("key".into(), Value::String(key.to_string().into())),
            ])),
            (None, payload) => payload,
        };
        let caller = ServiceCaller::of_user(&ctx.user);
        let principal = caller.as_ref().map(|caller| caller.principal.as_str());
        #[cfg(feature = "wire-drivers")]
        let outcome = self.tls_os(*op, args, key, principal).unwrap_or_else(|| self.io.os(*op, args, key, principal));
        #[cfg(not(feature = "wire-drivers"))]
        let outcome = if op.is_kernel_owned() {
            Err(OsError::new(OsErrorKind::Unsupported, format!("{}: TLS is not built into this kernel", op.name())))
        } else {
            self.io.os(*op, args, key, principal)
        };
        match outcome {
            Ok(payload) => EffectResult::executed_with_value(effect.clone(), keyed(payload)),
            Err(error) => {
                let mut result = os_failure(effect, &error);
                result.return_value = result.return_value.take().map(keyed);
                result
            }
        }
    }

    /// Run a `browser/prompt` or `browser/embed`: one journaled ask to the host's on-device model or embedder. The
    /// payload echoes the key (absent unkeyed); a failure names its kind (`unconfigured`, `unavailable`, `failed`).
    pub(super) fn run_on_device_model(&self, effect: &Effect, key: Option<&str>) -> EffectResult {
        use super::resumable::{ModelFailure, ModelFailureKind};
        let failed = |failure: ModelFailure| {
            let mut result = EffectResult::failed(effect.clone(), &failure.message);
            result.return_value = Some(keyed(key, Value::object(ValueMap::from_iter([
                ("error".into(), Value::String(failure.message.into())),
                (
                    "kind".into(),
                    Value::String(failure.kind.as_str().to_string().into()),
                ),
                            ]))));
            result
        };
        let refuse = |message: String| failed(ModelFailure::new(ModelFailureKind::Failed, message));
        match effect {
            Effect::BrowserPrompt { text, schema, .. } => {
                let (Value::String(text), Value::String(schema)) = (text, schema) else {
                    return refuse(
                        "browser/prompt: the text and the response schema must be text".to_string(),
                    );
                };
                match self.io.prompt(text, schema, key) {
                    Ok(reply) => EffectResult::executed_with_value(
                        effect.clone(),
                        keyed(key, Value::object(ValueMap::from_iter([
                            ("reply".into(), Value::String(reply.into())),
                                                    ]))),
                    ),
                    Err(failure) => failed(failure),
                }
            }
            Effect::BrowserEmbed { texts, .. } => {
                let Value::Array(items) = texts else {
                    return refuse("browser/embed: the texts must be a list of text".to_string());
                };
                let Some(texts) = items
                    .iter()
                    .map(|t| t.as_str().map(str::to_string))
                    .collect::<Option<Vec<String>>>()
                else {
                    return refuse("browser/embed: the texts must be a list of text".to_string());
                };
                if texts.is_empty() {
                    return EffectResult::executed_with_value(
                        effect.clone(),
                        keyed(key, Value::object(ValueMap::from_iter([
                            ("embeddings".into(), Value::Array(Vec::new().into())),
                                                    ]))),
                    );
                }
                match self.io.embed(&texts, key) {
                    Ok(vectors) => EffectResult::executed_with_value(
                        effect.clone(),
                        keyed(key, Value::object(ValueMap::from_iter([
                            (
                                "embeddings".into(),
                                Value::Array(
                                    vectors
                                        .into_iter()
                                        .map(|v| {
                                            Value::Array(v.into_iter().map(Value::Number).collect())
                                        })
                                        .collect(),
                                ),
                            ),
                                                    ]))),
                    ),
                    Err(failure) => failed(failure),
                }
            }
            _ => EffectResult::noop(effect.clone(), "not an on-device model effect"),
        }
    }

    /// A `browser/tab-<op>` effect: the request is validated here; the kernel has no tab driver, so a valid one fails
    /// until a std atom shapes the results of the raw tab asks the host answers.
    pub(super) fn run_browser_tab(&self, effect: &Effect) -> EffectResult {
        use orbital_core_base::tab_call::{parse_tab_call, WatchScope};
        let Effect::BrowserTab { op, request, key, .. } = effect else {
            return EffectResult::noop(effect.clone(), "not a browser tab effect");
        };
        let key = match key.as_ref().map(orbital_core_base::effect_types::call_key_text).transpose() {
            Ok(key) => key.map(str::to_string),
            Err(error) => return EffectResult::failed(effect.clone(), &error.to_string()),
        };
        let (caller_orbital, caller_trait) = self.caller_position();
        let inputs = self.external_inputs();
        let scope = WatchScope {
            caller_orbital: caller_orbital.as_deref(),
            caller_trait: &caller_trait,
            inputs: &inputs,
        };
        match parse_tab_call(*op, request, Some(&scope), key.as_deref()) {
            Ok(_) => EffectResult::failed(effect.clone(), "browser/tab-*: the kernel has no tab driver"),
            Err(message) => EffectResult::failed(effect.clone(), &message),
        }
    }

    /// Run a `browser/page-facts`: one journaled ask, answered flat, every fact not asked for at its default.
    pub(super) fn run_page_facts(&self, effect: &Effect) -> EffectResult {
        let Effect::BrowserPageFacts { fields, key, .. } = effect else {
            return EffectResult::noop(effect.clone(), "not a page-facts effect");
        };
        let key = key.as_ref().and_then(Value::as_str).map(str::to_string);
        let failed = |message: &str| {
            let mut result = EffectResult::failed(effect.clone(), message);
            if let Some(key) = &key {
                result.return_value = Some(Value::object(ValueMap::from_iter([
                    ("error".into(), Value::String(message.to_string().into())),
                    ("key".into(), Value::String(key.clone().into())),
                ])));
            }
            result
        };
        let fields = match page_fact_fields(fields) {
            Ok(fields) => fields,
            Err(message) => return failed(&message),
        };
        match self.io.page_facts(&fields) {
            Ok(facts) => {
                use orbital_core_base::ir::effects::PageFactField;
                let live = facts.available;
                let pick = |field: PageFactField| live && fields.contains(&field);
                let mut payload = ValueMap::from_iter([
                    ("available".into(), Value::Bool(live)),
                    (
                        "hostname".into(),
                        Value::String(if pick(PageFactField::Hostname) {
                            facts.hostname.into()
                        } else {
                            String::new().into()
                        }),
                    ),
                    (
                        "referrer".into(),
                        Value::String(if pick(PageFactField::Referrer) {
                            facts.referrer.into()
                        } else {
                            String::new().into()
                        }),
                    ),
                    (
                        "screenWidth".into(),
                        match facts.screen_width {
                            Some(width) if pick(PageFactField::ScreenWidth) => Value::Number(width),
                            _ => Value::Null,
                        },
                    ),
                    (
                        "href".into(),
                        Value::String(if pick(PageFactField::Href) {
                            facts.href.into()
                        } else {
                            String::new().into()
                        }),
                    ),
                ]);
                if let Some(key) = &key {
                    payload.insert("key".into(), Value::string(key.clone()));
                }
                EffectResult::executed_with_value(effect.clone(), Value::object(payload))
            }
            Err(message) => failed(&message),
        }
    }
}

/// `value` with the call's `key` added when the call had one; an unkeyed call's payload has no `key`.
fn keyed(key: Option<&str>, mut value: Value) -> Value {
    if let (Value::Object(fields), Some(key)) = (&mut value, key) {
        fields.insert("key".into(), Value::string(key.to_string()));
    }
    value
}
