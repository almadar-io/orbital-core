//! Option and emit-config helpers shared by the effect lowerings.

use super::*;

impl NodeLowerer {
    /// Extract the `emit:` config from an options object, if present.
    ///
    /// The stream keys are spelled `onChange` and `onMessage`, as the runtime routes them. All five keys (with `cancelled`) are optional; the
    /// validator enforces which ones are allowed per operator.
    ///
    /// Returns `None` if no `emit:` key is present, so the caller stores
    /// `emit: None` on the OIR and the field is skipped in serialization.
    pub(super) fn extract_emit_config(
        &self,
        obj: &orbital_core_base::schema::sexpr::SObject,
    ) -> Result<Option<OirEmitConfig>, LowerError> {
        let emit_entry = match obj.get("emit") {
            Some(SExpression::Object(inner)) => inner,
            Some(SExpression::Null) | None => return Ok(None),
            Some(other) => {
                return Err(LowerError::InvalidSExpr(format!(
                    "emit: must be an object, got {:?}",
                    other
                )));
            }
        };

        let read_str = |key: &str| -> Result<Option<String>, LowerError> {
            match emit_entry.get(key) {
                None | Some(SExpression::Null) => Ok(None),
                Some(SExpression::String(s)) => Ok(Some(s.clone())),
                Some(other) => Err(LowerError::InvalidSExpr(format!(
                    "emit.{} must be a string event name, got {:?}",
                    key, other
                ))),
            }
        };

        let on_change = read_str("onChange")?;
        let on_message = read_str("onMessage")?;

        let cfg = OirEmitConfig {
            success: read_str("success")?,
            failure: read_str("failure")?,
            on_change,
            on_message,
            cancelled: read_str("cancelled")?,
        };

        if cfg.is_empty() {
            Ok(None)
        } else {
            Ok(Some(cfg))
        }
    }

    /// Split the trailing args of an `os/watch-*` effect into (options, emit).
    ///
    /// `os/watch-files`, `-process`, `-port`, `-http` take a positional
    /// second arg that is the watcher's own options (glob options,
    /// subcommand, protocol, method). This helper distinguishes:
    /// - `[]`                          → (None, None)
    /// - `[optsObj_with_emit]`         → (None, Some(cfg))
    /// - `[optsObj_without_emit]`      → (Some(opts), None)
    /// - `[positional, emitObj]`       → (Some(positional), Some(cfg))
    pub(super) fn split_watch_trailing(
        &self,
        tail: &[SExpression],
    ) -> Result<(Option<OirExpr>, Option<OirEmitConfig>), LowerError> {
        match tail.len() {
            0 => Ok((None, None)),
            1 => {
                if let SExpression::Object(obj) = &tail[0] {
                    if obj.contains_key("emit") {
                        return Ok((None, self.extract_emit_config(obj)?));
                    }
                }
                Ok((Some(self.resolve_sexpr(&tail[0])?), None))
            }
            _ => {
                let positional = Some(self.resolve_sexpr(&tail[0])?);
                let emit_cfg = if let SExpression::Object(obj) = &tail[1] {
                    self.extract_emit_config(obj)?
                } else {
                    None
                };
                Ok((positional, emit_cfg))
            }
        }
    }

    /// Extract emit config from a single trailing options object for
    /// `os/watch-*` effects that take no positional secondary args
    /// (cron, signal, env).
    /// The `key` and `emit` of a keyed call's trailing options object.
    pub(super) fn resolve_keyed_options(
        &self,
        options: Option<&SExpression>,
    ) -> Result<(Option<OirExpr>, Option<OirEmitConfig>), LowerError> {
        let Some(SExpression::Object(options)) = options else {
            return Ok((None, None));
        };
        let key = match options.get("key") {
            None | Some(SExpression::Null) => None,
            Some(k) => Some(self.resolve_sexpr(k)?),
        };
        Ok((key, self.extract_emit_config(options)?))
    }

    pub(super) fn extract_watch_only_emit(
        &self,
        tail: &[SExpression],
    ) -> Result<Option<OirEmitConfig>, LowerError> {
        if tail.is_empty() {
            return Ok(None);
        }
        if let SExpression::Object(obj) = &tail[0] {
            return self.extract_emit_config(obj);
        }
        Ok(None)
    }

    /// `what` names the operator + argument slot (e.g. "(emit …) argument 1 —
    /// the event name") so a resolve-stage failure is actionable instead of a
    /// bare "Expected string" with no context (T-RESOLVE-OPAQUE).
    pub(super) fn sexpr_as_str(&self, sexpr: &SExpression, what: &str) -> Result<String, LowerError> {
        match sexpr {
            SExpression::String(s) => Ok(s.clone()),
            other => {
                let shape = match other {
                    SExpression::Object(_) => "an object",
                    SExpression::Array(_) => "a list",
                    SExpression::Number(_) => "a number",
                    SExpression::Bool(_) => "a boolean",
                    SExpression::Null => "null",
                    SExpression::String(_) => unreachable!(),
                };
                Err(LowerError::InvalidSExpr(format!(
                    "{what} must be a string, got {shape}"
                )))
            }
        }
    }
}
