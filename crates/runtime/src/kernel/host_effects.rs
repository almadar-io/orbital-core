//! Effects the host answers: access checks, random bytes, http, redis, postgres, os, beacons, on-device models, tabs and page facts.

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

    /// Run `random-bytes`, `credential-status` or `credential-forget` through one journaled ask.
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
            Effect::CredentialAdmin { op, request, .. } => {
                let caller = ServiceCaller::of_user(&ctx.user);
                let catalog = || {
                    let program = self
                        .program_credentials()
                        .map_err(orbital_core_base::credential_admin::Refusal::plain)?;
                    orbital_core_base::credential_admin::catalog(&program)
                };
                match crate::credential_admin::run(
                    &*self.io,
                    *op,
                    request,
                    caller.as_ref(),
                    &catalog,
                ) {
                    Ok(reply) => EffectResult::executed_with_value(effect.clone(), reply),
                    Err(refusal) => {
                        let mut fields = vec![("error".into(), Value::String(refusal.message.into()))];
                        if let Some(code) = refusal.code {
                            fields.push(("code".into(), Value::String(code.to_string().into())));
                        }
                        http_failure(effect, Value::object(ValueMap::from_iter(fields)))
                    }
                }
            }
            Effect::OAuthAuthorize {
                client, options, ..
            } => self.run_oauth_authorize(effect, client, options, principal.as_deref()),
            Effect::OAuthConnectionStatus { provider, .. }
            | Effect::OAuthConnectionForget { provider, .. } => {
                self.run_oauth_connection(effect, provider, principal.as_deref())
            }
            other => EffectResult::noop(other.clone(), "not a host effect"),
        }
    }

    /// Run an `http` effect on the shared driver loop; the `HttpCallDriver` owns credentials, OAuth, pagination and
    /// streaming, and every ask it makes is journaled.
    pub(super) fn run_http(
        &self,
        effect: &Effect,
        principal: Option<&str>,
        now_ms: Option<f64>,
    ) -> EffectResult {
        use crate::{
            driver::{drive, DriveContext, DriveEnd},
            http_request::HttpCallDriver,
        };
        let Effect::Http {
            method,
            origin,
            path,
            request,
            key,
            ..
        } = effect
        else {
            return EffectResult::noop(effect.clone(), "not an http effect");
        };
        let key = match key
            .as_ref()
            .map(orbital_core_base::effect_types::call_key_text)
            .transpose()
        {
            Ok(key) => key,
            Err(error) => return EffectResult::failed(effect.clone(), &error.to_string()),
        };
        let spec = match http_request::parse_http_call(
            *method,
            &origin.to_string_value(),
            &path.to_string_value(),
            request,
        ) {
            Ok(spec) => spec,
            Err(refusal) => {
                return http_failure(
                    effect,
                    http_request::with_key(
                        http_request::refusal_payload(&refusal),
                        &key.map(str::to_string),
                    ),
                )
            }
        };
        let egress = self.grants.egress.read().clone();
        let (mut driver, step) = HttpCallDriver::start_as(
            spec,
            egress,
            key.map(str::to_string),
            now_ms,
            principal.map(str::to_string),
        );
        match drive(&self.io, &mut driver, step, DriveContext { principal, key }) {
            DriveEnd::Done(Ok(payload)) => {
                EffectResult::executed_with_value(effect.clone(), payload)
            }
            DriveEnd::Done(Err(payload)) => http_failure(effect, payload),
            DriveEnd::AwaitingHost => EffectResult::failed(effect.clone(), AWAITING_HOST),
            DriveEnd::NoBeaconHost => {
                EffectResult::failed(effect.clone(), "an http call asked for a beacon")
            }
        }
    }

    #[cfg(feature = "wire-drivers")]
    /// Run a `redis` effect on the shared driver loop; the `RedisDriver` owns the RESP protocol, the handshake and the
    /// credential, and every ask it makes is a journaled `os/*` or secret ask.
    pub(super) fn run_redis(&self, effect: &Effect, principal: Option<&str>) -> EffectResult {
        use crate::{
            driver::{drive, DriveContext, DriveEnd},
            redis::{self, RedisDriver},
        };
        let Effect::Redis {
            origin,
            command,
            options,
            key,
            ..
        } = effect
        else {
            return EffectResult::noop(effect.clone(), "not a redis effect");
        };
        let key = match key
            .as_ref()
            .map(orbital_core_base::effect_types::call_key_text)
            .transpose()
        {
            Ok(key) => key.map(str::to_string),
            Err(error) => return EffectResult::failed(effect.clone(), &error.to_string()),
        };
        let spec = match redis::parse_redis_call(origin, command, options) {
            Ok(spec) => spec,
            Err(refusal) => return http_failure(effect, redis::refusal_payload(&refusal, &key)),
        };
        let egress = self.grants.egress.read().clone();
        let (mut driver, step) = RedisDriver::start(spec, egress, key.clone());
        match drive(
            &self.io,
            &mut driver,
            step,
            DriveContext {
                principal,
                key: None,
            },
        ) {
            DriveEnd::Done(Ok(payload)) => {
                EffectResult::executed_with_value(effect.clone(), payload)
            }
            DriveEnd::Done(Err(payload)) => http_failure(effect, payload),
            DriveEnd::AwaitingHost => EffectResult::failed(effect.clone(), AWAITING_HOST),
            DriveEnd::NoBeaconHost => {
                EffectResult::failed(effect.clone(), "a redis call asked for a beacon")
            }
        }
    }

    #[cfg(not(feature = "wire-drivers"))]
    pub(super) fn run_redis(&self, effect: &Effect, _principal: Option<&str>) -> EffectResult {
        EffectResult::failed(effect.clone(), WIRE_DRIVERS_UNAVAILABLE)
    }

    #[cfg(feature = "wire-drivers")]
    /// Run a `postgres` effect on the shared driver loop; the `PostgresDriver` owns the Postgres protocol, the login
    /// and the result decoding, and every ask it makes is a journaled `os/*`, secret, random or clock ask.
    pub(super) fn run_postgres(&self, effect: &Effect, principal: Option<&str>) -> EffectResult {
        use crate::{
            driver::{drive, DriveContext, DriveEnd},
            postgres::{self, PostgresDriver},
        };
        let Effect::Postgres {
            origin,
            sql,
            params,
            options,
            key,
            ..
        } = effect
        else {
            return EffectResult::noop(effect.clone(), "not a postgres effect");
        };
        let key = match key
            .as_ref()
            .map(orbital_core_base::effect_types::call_key_text)
            .transpose()
        {
            Ok(key) => key.map(str::to_string),
            Err(error) => return EffectResult::failed(effect.clone(), &error.to_string()),
        };
        let spec = match postgres::parse_postgres_call(origin, sql, params, options) {
            Ok(spec) => spec,
            Err(refusal) => {
                return http_failure(effect, crate::redis::refusal_payload(&refusal, &key))
            }
        };
        let egress = self.grants.egress.read().clone();
        let (mut driver, step) = PostgresDriver::start(spec, egress, key.clone());
        match drive(
            &self.io,
            &mut driver,
            step,
            DriveContext {
                principal,
                key: None,
            },
        ) {
            DriveEnd::Done(Ok(payload)) => {
                EffectResult::executed_with_value(effect.clone(), payload)
            }
            DriveEnd::Done(Err(payload)) => http_failure(effect, payload),
            DriveEnd::AwaitingHost => EffectResult::failed(effect.clone(), AWAITING_HOST),
            DriveEnd::NoBeaconHost => {
                EffectResult::failed(effect.clone(), "a postgres call asked for a beacon")
            }
        }
    }

    #[cfg(not(feature = "wire-drivers"))]
    pub(super) fn run_postgres(&self, effect: &Effect, _principal: Option<&str>) -> EffectResult {
        EffectResult::failed(effect.clone(), WIRE_DRIVERS_UNAVAILABLE)
    }

    /// Run an `os/<op>` effect through one journaled ask. A streaming op needs a key, so `cancel-call` can end it.
    pub(super) fn run_os(&self, effect: &Effect) -> EffectResult {
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
        match self.io.os(*op, args, key) {
            Ok(payload) => EffectResult::executed_with_value(effect.clone(), keyed(payload)),
            Err(error) => {
                let mut result = os_failure(effect, &error);
                result.return_value = result.return_value.take().map(keyed);
                result
            }
        }
    }

    /// Run a `browser/beacon`: the one http driver parses, builds and egress-checks the request, and only the
    /// built request reaches the host's beacon port. A kernel with no host to answer fails `unavailable`.
    pub(super) fn run_beacon(&self, effect: &Effect) -> EffectResult {
        use crate::{
            driver::{drive, DriveContext, DriveEnd},
            http_request::HttpCallDriver,
        };
        let Effect::BrowserBeacon {
            url, request, key, ..
        } = effect
        else {
            return EffectResult::noop(effect.clone(), "not a beacon effect");
        };
        let key = match key
            .as_ref()
            .map(orbital_core_base::effect_types::call_key_text)
            .transpose()
        {
            Ok(key) => key.map(str::to_string),
            Err(error) => return EffectResult::failed(effect.clone(), &error.to_string()),
        };
        let Value::String(url) = url else {
            return http_failure(
                effect,
                http_request::with_key(
                    http_request::refusal_payload(
                        &orbital_core_base::http_wire::HttpRefusal::Invalid(
                            "a beacon URL must be a string".to_string(),
                        ),
                    ),
                    &key,
                ),
            );
        };
        let spec = match http_request::parse_beacon_call(url, request) {
            Ok(spec) => spec,
            Err(refusal) => {
                return http_failure(
                    effect,
                    http_request::with_key(http_request::refusal_payload(&refusal), &key),
                )
            }
        };
        let egress = self.grants.egress.read().clone();
        let (mut driver, step) = HttpCallDriver::start(spec, egress, key.clone());
        match drive(
            &self.io,
            &mut driver,
            step,
            DriveContext {
                principal: None,
                key: key.as_deref(),
            },
        ) {
            DriveEnd::Done(Ok(payload)) => {
                EffectResult::executed_with_value(effect.clone(), payload)
            }
            DriveEnd::Done(Err(payload)) => http_failure(effect, payload),
            DriveEnd::AwaitingHost => EffectResult::failed(effect.clone(), AWAITING_HOST),
            DriveEnd::NoBeaconHost => http_failure(
                effect,
                http_request::with_key(
                    http_request::unavailable_payload("browser/beacon: no host answers beacons"),
                    &key,
                ),
            ),
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

    /// Run a `browser/tab-<op>` effect: the request is validated and the result shaped here, the host is asked
    /// only for raw tab and page operations, each one a journaled ask on the shared driver loop.
    pub(super) fn run_browser_tab(&self, effect: &Effect) -> EffectResult {
        use crate::{
            driver::{drive, DriveContext, DriveEnd},
            tab::{refusal_payload, TabDriver},
        };
        use orbital_core_base::tab_call::{parse_tab_call, WatchScope};
        let Effect::BrowserTab {
            op, request, key, ..
        } = effect
        else {
            return EffectResult::noop(effect.clone(), "not a browser tab effect");
        };
        let key = match key
            .as_ref()
            .map(orbital_core_base::effect_types::call_key_text)
            .transpose()
        {
            Ok(key) => key.map(str::to_string),
            Err(error) => return EffectResult::failed(effect.clone(), &error.to_string()),
        };
        let failed = |payload: Value| {
            let message = payload
                .get_path("error")
                .map(Value::to_string_value)
                .unwrap_or_default();
            let mut result = EffectResult::failed(effect.clone(), &message);
            result.return_value = Some(payload);
            result
        };
        let (caller_orbital, caller_trait) = self.caller_position();
        let inputs = self.external_inputs();
        let scope = WatchScope {
            caller_orbital: caller_orbital.as_deref(),
            caller_trait: &caller_trait,
            inputs: &inputs,
        };
        let call = match parse_tab_call(*op, request, Some(&scope), key.as_deref()) {
            Ok(call) => call,
            Err(message) => return failed(refusal_payload(&message, key.as_deref())),
        };
        let (mut driver, step) = TabDriver::start(call);
        match drive(
            &self.io,
            &mut driver,
            step,
            DriveContext {
                principal: None,
                key: key.as_deref(),
            },
        ) {
            DriveEnd::Done(Ok(payload)) => {
                EffectResult::executed_with_value(effect.clone(), payload)
            }
            DriveEnd::Done(Err(payload)) => failed(payload),
            DriveEnd::AwaitingHost => EffectResult::failed(effect.clone(), AWAITING_HOST),
            DriveEnd::NoBeaconHost => {
                EffectResult::failed(effect.clone(), "a browser tab call asked for a beacon")
            }
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
