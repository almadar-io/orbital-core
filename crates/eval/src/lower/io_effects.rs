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

    pub(super) fn resolve_credential_connect_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["credential-connect", { credential, tokenEndpoint, ... }, { emit? }?]
        if !(1..=2).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "credential-connect takes a request object and optional options".to_string(),
            ));
        }
        let emit = self.resolve_emit_only_options("credential-connect", args.get(1))?;
        Ok(OirExpr::effect(OirEffect::credential_connect(self.resolve_sexpr(&args[0])?, emit)))
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
