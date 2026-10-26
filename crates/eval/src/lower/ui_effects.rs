//! Lowering navigate, render-ui, notify and log effects.

use super::*;

impl NodeLowerer {
    pub(super) fn resolve_navigate_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "navigate requires path".to_string(),
            ));
        }
        let path = self.resolve_sexpr(&args[0])?;
        // Positional grammar: (navigate path payload? options?). The payload
        // (arg 2) is resolved into `params` and delivered client-side via
        // router history state, merged into INIT alongside route params
        // (`generate_auto_init_effect`). Arg 3 is the options object;
        // `crumb` labels the nav-stack entry the client pushes for the
        // target page.
        let params = match args.get(1) {
            Some(SExpression::Null) | None => None,
            Some(p) => Some(self.resolve_sexpr(p)?),
        };
        let crumb = match args.get(2) {
            Some(SExpression::Object(obj)) => obj
                .get("crumb")
                .map(|c| self.resolve_sexpr(c))
                .transpose()?,
            Some(_) => {
                return Err(LowerError::InvalidSExpr(
                    "navigate options (argument 3) must be an object like { crumb: <expr> }"
                        .to_string(),
                ));
            }
            None => None,
        };
        Ok(OirExpr::effect(OirEffect::Navigate {
            path,
            params,
            crumb,
        }))
    }

    pub(super) fn resolve_navigate_back_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if !args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "navigate-back takes no arguments".to_string(),
            ));
        }
        Ok(OirExpr::effect(OirEffect::NavigateBack))
    }

    pub(super) fn resolve_render_ui_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.len() < 2 {
            return Err(LowerError::InvalidSExpr(
                "render-ui requires slot and pattern".to_string(),
            ));
        }
        let slot = self.sexpr_as_str(&args[0], "(render-ui …) argument 1 — the slot name")?;

        // Handle null pattern - used to clear a slot
        let pattern = if matches!(&args[1], SExpression::Null) {
            OirPattern::new("clear")
        } else if let Ok(pattern_type) =
            self.sexpr_as_str(&args[1], "(render-ui …) argument 2 — the pattern")
        {
            OirPattern::new(pattern_type)
        } else if let SExpression::Object(obj) = &args[1] {
            let pattern_type = obj
                .get("type")
                .and_then(|v| {
                    if let SExpression::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("custom");
            let mut pattern = OirPattern::new(pattern_type);
            for (key, value) in obj {
                if key != "type" {
                    pattern
                        .config
                        .insert(key.clone(), self.sexpr_to_value(value));
                }
            }
            pattern
        } else {
            return Err(LowerError::InvalidSExpr(
                "render-ui pattern must be string or object".to_string(),
            ));
        };

        Ok(OirExpr::effect(OirEffect::render_ui(slot, pattern)))
    }

    /// `notify` is `.lolo`-only sugar for `render-ui toast` (owner ruling
    /// 2026-09-11), desugared away at lowering — the `.orb` IR never carries
    /// a `notify` effect kind. Validation (`ORB_EFF_NOTIFY_RETIRED`) is the
    /// gate that should have already caught this; this arm only exists so a
    /// stale tuple that somehow reaches resolve fails cleanly instead of
    /// panicking the effect parser.
    pub(super) fn resolve_notify_effect(&self, _args: &[SExpression]) -> Result<OirExpr, LowerError> {
        Err(LowerError::InvalidSExpr(
            "notify is retired sugar for render-ui toast — re-emit the .lolo".to_string(),
        ))
    }

    pub(super) fn resolve_log_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr("log requires message".to_string()));
        }
        use orbital_core_base::ir::effects::OirLogLevel;
        let message = self.resolve_sexpr(&args[0])?;
        let level = match args.get(1) {
            None => OirLogLevel::Info,
            Some(SExpression::String(level)) if level == "debug" => OirLogLevel::Debug,
            Some(SExpression::String(level)) if level == "info" => OirLogLevel::Info,
            Some(SExpression::String(level)) if level == "warn" => OirLogLevel::Warn,
            Some(SExpression::String(level)) if level == "error" => OirLogLevel::Error,
            Some(other) => {
                return Err(LowerError::InvalidSExpr(format!(
                    "(log …) argument 2, the level, must be \"debug\", \"info\", \"warn\" or \"error\"; got {other:?}"
                )))
            }
        };
        Ok(OirExpr::effect(OirEffect::log(level, message)))
    }
}
