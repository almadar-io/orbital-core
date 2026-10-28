//! Lowering effects that ask the host: services, HTTP, stores, OS, credentials and OAuth.

use super::*;

impl NodeLowerer {
    pub(super) fn resolve_call_service_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["call-service", "serviceName", "action", { ...params }]
        // With emit: ["call-service", "svc", "action", argsExpr, { emit: {...} }]
        if args.len() < 2 {
            return Err(LowerError::InvalidSExpr(
                "call-service requires service name and action".to_string(),
            ));
        }
        let service =
            self.sexpr_as_str(&args[0], "(call-service …) argument 1 — the service name")?;
        let method =
            self.sexpr_as_str(&args[1], "(call-service …) argument 2 — the action name")?;

        // Detect trailing options object carrying `emit:` and/or `key:`. Only
        // pulled out of the args list if it carries one of them — a plain
        // trailing object is still treated as the call's args payload.
        let mut args_end = args.len();
        let mut emit_cfg: Option<OirEmitConfig> = None;
        let mut key: Option<OirExpr> = None;

        if args.len() > 3 {
            if let SExpression::Object(obj) = &args[args.len() - 1] {
                if obj.contains_key("emit") || obj.contains_key("key") {
                    emit_cfg = self.extract_emit_config(obj)?;
                    key = match obj.get("key") {
                        None | Some(SExpression::Null) => None,
                        Some(k) => Some(self.resolve_sexpr(k)?),
                    };
                    args_end -= 1;
                }
            }
        }

        let call_args = if args_end > 2 {
            self.resolve_sexpr(&args[args_end - 1])?
        } else {
            OirExpr::object(orbital_core_base::ir::expr::OirFields::default())
        };

        let mut effect = OirEffect::call_service(service, method, call_args);
        if let OirEffect::CallService {
            emit: ref mut em,
            key: ref mut k,
            ..
        } = effect
        {
            *em = emit_cfg;
            *k = key;
        }
        Ok(OirExpr::effect(effect))
    }

    pub(super) fn resolve_http_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["http", METHOD, origin, path, request?, { key?, emit? }?]
        if !(3..=5).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "http takes a method, an origin, a path, then an optional request and options"
                    .to_string(),
            ));
        }
        let word = self.sexpr_as_str(&args[0], "(http …) argument 1 — the method")?;
        let method: HttpMethod = serde_json::from_value(serde_json::Value::String(word.clone()))
            .map_err(|_| {
                LowerError::InvalidSExpr(format!(
                    "(http …) argument 1 — \"{word}\" is not one of GET POST PUT PATCH DELETE"
                ))
            })?;
        let origin = self.resolve_sexpr(&args[1])?;
        let path = self.resolve_sexpr(&args[2])?;
        let request = args
            .get(3)
            .map(|request| self.resolve_http_request(request))
            .transpose()?;
        let (key, emit) = match args.get(4) {
            None | Some(SExpression::Null) => (None, None),
            Some(SExpression::Object(options)) => (
                match options.get("key") {
                    None | Some(SExpression::Null) => None,
                    Some(k) => Some(self.resolve_sexpr(k)?),
                },
                self.extract_emit_config(options)?,
            ),
            Some(other) => {
                return Err(LowerError::InvalidSExpr(format!(
                    "(http …) options must be an object, got {other:?}"
                )))
            }
        };
        let mut effect = OirEffect::http(method, origin, path);
        if let OirEffect::Http {
            request: ref mut r,
            key: ref mut k,
            emit: ref mut e,
            ..
        } = effect
        {
            *r = request;
            *k = key;
            *e = emit;
        }
        Ok(OirExpr::effect(effect))
    }

    pub(super) fn resolve_postgres_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["postgres", origin, sql, params, { auth?, user?, database?, tls?, timeoutMs?, key?, emit? }?, { key?, emit? }?]
        if !(3..=5).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "postgres takes an origin, a statement, its params, then optional options".to_string(),
            ));
        }
        let mut options = orbital_core_base::schema::sexpr::SObject::default();
        let mut routes = orbital_core_base::schema::sexpr::SObject::default();
        let mut key = None;
        for trailing in &args[3..] {
            match trailing {
                SExpression::Null => {}
                SExpression::Object(fields) => {
                    for (name, value) in fields.iter() {
                        match name.as_str() {
                            "key" if matches!(value, SExpression::Null) => {}
                            "key" => key = Some(self.resolve_sexpr(value)?),
                            "emit" => {
                                routes.insert(name.clone(), value.clone());
                            }
                            _ => {
                                options.insert(name.clone(), value.clone());
                            }
                        }
                    }
                }
                other => {
                    return Err(LowerError::InvalidSExpr(format!(
                        "(postgres …) options must be an object, got {other:?}"
                    )))
                }
            }
        }
        let options = if options.is_empty() {
            None
        } else {
            Some(self.resolve_owned(SExpression::object(options))?)
        };
        Ok(OirExpr::effect(OirEffect::Postgres {
            origin: self.resolve_sexpr(&args[0])?,
            sql: self.resolve_sexpr(&args[1])?,
            params: self.resolve_sexpr(&args[2])?,
            options,
            key,
            emit: self.extract_emit_config(&routes)?,
        }))
    }

    pub(super) fn resolve_redis_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["redis", origin, command, { auth?, user?, db?, key?, emit? }?, { key?, emit? }?]
        if !(2..=4).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "redis takes an origin, a command, then optional options".to_string(),
            ));
        }
        let mut options = orbital_core_base::schema::sexpr::SObject::default();
        let mut routes = orbital_core_base::schema::sexpr::SObject::default();
        let mut key = None;
        for trailing in &args[2..] {
            match trailing {
                SExpression::Null => {}
                SExpression::Object(fields) => {
                    for (name, value) in fields.iter() {
                        match name.as_str() {
                            "key" if matches!(value, SExpression::Null) => {}
                            "key" => key = Some(self.resolve_sexpr(value)?),
                            "emit" => {
                                routes.insert(name.clone(), value.clone());
                            }
                            _ => {
                                options.insert(name.clone(), value.clone());
                            }
                        }
                    }
                }
                other => {
                    return Err(LowerError::InvalidSExpr(format!(
                        "(redis …) options must be an object, got {other:?}"
                    )))
                }
            }
        }
        let options = if options.is_empty() {
            None
        } else {
            Some(self.resolve_owned(SExpression::object(options))?)
        };
        Ok(OirExpr::effect(OirEffect::Redis {
            origin: self.resolve_sexpr(&args[0])?,
            command: self.resolve_sexpr(&args[1])?,
            options,
            key,
            emit: self.extract_emit_config(&routes)?,
        }))
    }

    /// The request object, with `paginate.next` and `paginate.collect` held as `(quote …)` data: they are
    /// evaluated per page against `@response`, so lowering must not read them.
    pub(super) fn resolve_http_request(&self, request: &SExpression) -> Result<OirExpr, LowerError> {
        let SExpression::Object(fields) = request else {
            return self.resolve_sexpr(request);
        };
        let Some(SExpression::Object(paginate)) = fields.get("paginate") else {
            return self.resolve_sexpr(request);
        };
        let mut held = (**paginate).clone();
        for key in ["next", "collect"] {
            if let Some(expr) = held.get(key) {
                if orbital_core_base::schema::quote::quote_body_of(expr).is_none() {
                    let quoted = orbital_core_base::schema::quote::quote_expr(expr);
                    held.insert(key.to_string(), quoted);
                }
            }
        }
        let mut fields = (**fields).clone();
        fields.insert("paginate".to_string(), SExpression::object(held));
        self.resolve_owned(SExpression::object(fields))
    }

    /// The `{ emit }` options object a one-argument effect may carry.
    pub(super) fn resolve_emit_only_options(
        &self,
        head: &str,
        options: Option<&SExpression>,
    ) -> Result<Option<OirEmitConfig>, LowerError> {
        match options {
            None | Some(SExpression::Null) => Ok(None),
            Some(SExpression::Object(options)) => self.extract_emit_config(options),
            Some(other) => Err(LowerError::InvalidSExpr(format!(
                "({head} …) options must be an object, got {other:?}"
            ))),
        }
    }

    pub(super) fn resolve_os_effect(&self, name: &str, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["os/<op>", args…, { key?, emit? }?]
        use orbital_core_base::os::{accepts_arity, split_envelope, OsOp};
        let op = OsOp::from_name(name)
            .ok_or_else(|| LowerError::InvalidSExpr(format!("{name} is not an os operation")))?;
        let (positional, envelope) = split_envelope(op, args);
        if !accepts_arity(op, positional.len()) {
            return Err(LowerError::InvalidSExpr(format!(
                "{name} takes {} to {} arguments, got {}",
                op.min_args(),
                op.max_args(),
                positional.len()
            )));
        }
        let (key, emit) = match envelope {
            None => (None, None),
            Some(options) => (
                match options.get("key") {
                    None | Some(SExpression::Null) => None,
                    Some(k) => Some(self.resolve_sexpr(k)?),
                },
                self.extract_emit_config(options)?,
            ),
        };
        let resolved = positional
            .iter()
            .map(|a| self.resolve_sexpr(a))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(OirExpr::effect(OirEffect::os(op, resolved, key, emit)))
    }

    pub(super) fn resolve_random_bytes_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["random-bytes", n, { key?, emit? }?]
        if !(1..=2).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "random-bytes takes a byte count and optional options".to_string(),
            ));
        }
        let (key, emit) = match args.get(1) {
            None | Some(SExpression::Null) => (None, None),
            Some(SExpression::Object(options)) => (
                match options.get("key") {
                    None | Some(SExpression::Null) => None,
                    Some(k) => Some(self.resolve_sexpr(k)?),
                },
                self.extract_emit_config(options)?,
            ),
            Some(other) => {
                return Err(LowerError::InvalidSExpr(format!(
                    "(random-bytes …) options must be an object, got {other:?}"
                )))
            }
        };
        Ok(OirExpr::effect(OirEffect::RandomBytes {
            bytes: self.resolve_sexpr(&args[0])?,
            key,
            emit,
        }))
    }

    pub(super) fn resolve_credential_effect(
        &self,
        args: &[SExpression],
        status: bool,
    ) -> Result<OirExpr, LowerError> {
        // Format: ["credential-status" | "credential-forget", credential, { emit? }?]
        let head = if status {
            "credential-status"
        } else {
            "credential-forget"
        };
        if !(1..=2).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(format!(
                "{head} takes a credential knob and optional options"
            )));
        }
        let emit = self.resolve_emit_only_options(head, args.get(1))?;
        let credential = self.resolve_sexpr(&args[0])?;
        Ok(OirExpr::effect(if status {
            OirEffect::credential_status(credential, emit)
        } else {
            OirEffect::credential_forget(credential, emit)
        }))
    }

    pub(super) fn resolve_credential_admin_effect(&self, head: &str, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["credential-<verb>", request, { emit? }?]; `credential-rotate` takes no request.
        let Some(op) = orbital_core_base::credential_admin::CredentialAdminOp::from_name(head) else {
            return Err(LowerError::InvalidSExpr(format!("{head} is not a credential effect")));
        };
        let first = usize::from(op.takes_request());
        if !(first..=first + 1).contains(&args.len()) {
            let shape = if op.takes_request() { "a request object and optional options" } else { "only optional options" };
            return Err(LowerError::InvalidSExpr(format!("{head} takes {shape}")));
        }
        let emit = self.resolve_emit_only_options(head, args.get(first))?;
        let request = if op.takes_request() { self.resolve_sexpr(&args[0])? } else { OirExpr::null() };
        Ok(OirExpr::effect(OirEffect::credential_admin(op, request, emit)))
    }

    pub(super) fn resolve_access_check_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["access-check", { entity, action, row, viewer }, { emit? }?]
        if !(1..=2).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "access-check takes a request object and optional options".to_string(),
            ));
        }
        let emit = self.resolve_emit_only_options("access-check", args.get(1))?;
        Ok(OirExpr::effect(OirEffect::access_check(self.resolve_sexpr(&args[0])?, emit)))
    }

    pub(super) fn resolve_oauth_authorize_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["oauth-authorize", client, options, { key?, emit? }?]
        if !(2..=3).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "oauth-authorize takes a client, its options and optional key and emit options"
                    .to_string(),
            ));
        }
        let (key, emit) = match args.get(2) {
            None | Some(SExpression::Null) => (None, None),
            Some(SExpression::Object(options)) => (
                match options.get("key") {
                    None | Some(SExpression::Null) => None,
                    Some(k) => Some(self.resolve_sexpr(k)?),
                },
                self.extract_emit_config(options)?,
            ),
            Some(other) => {
                return Err(LowerError::InvalidSExpr(format!(
                    "(oauth-authorize …) options must be an object, got {other:?}"
                )))
            }
        };
        Ok(OirExpr::effect(OirEffect::OAuthAuthorize {
            client: self.resolve_sexpr(&args[0])?,
            options: self.resolve_sexpr(&args[1])?,
            key,
            emit,
        }))
    }

    pub(super) fn resolve_oauth_connection_effect(
        &self,
        args: &[SExpression],
        status: bool,
    ) -> Result<OirExpr, LowerError> {
        // Format: ["oauth-connection-status" | "oauth-connection-forget", provider, { emit? }?]
        let head = if status {
            "oauth-connection-status"
        } else {
            "oauth-connection-forget"
        };
        if !(1..=2).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(format!(
                "{head} takes a provider word and optional options"
            )));
        }
        let emit = self.resolve_emit_only_options(head, args.get(1))?;
        let provider = self.resolve_sexpr(&args[0])?;
        Ok(OirExpr::effect(if status {
            OirEffect::OAuthConnectionStatus { provider, emit }
        } else {
            OirEffect::OAuthConnectionForget { provider, emit }
        }))
    }

    pub(super) fn resolve_cancel_call_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["cancel-call", keyExpr]
        let [key] = args else {
            return Err(LowerError::InvalidSExpr(
                "cancel-call takes exactly one argument — the key expression".to_string(),
            ));
        };
        Ok(OirExpr::effect(OirEffect::cancel_call(
            self.resolve_sexpr(key)?,
        )))
    }
}
