//! Resolving an effect's ids, paging, ordering and values before it reaches the store.

use super::*;

impl EffectExecutor {
    /// Resolve bindings in a value recursively.
    ///
    /// Returns a borrowed reference when no `@`-binding is present in the subtree,
    /// so binding-free payloads (the common case) are not deep-cloned.
    /// Resolve an entity id that may be an `@`-binding (e.g. `@payload.row.id`).
    /// Resolve an effect's `id` argument to a row id.
    ///
    /// An `@`-binding that does not resolve is an **error**, not the sigil. This
    /// used to `return raw.to_string()`, so an unresolved `@payload.id` became
    /// the lookup key itself and surfaced as `Entity not found in cache:
    /// Patient:@payload.id` — a missing-row error for what is really a binding
    /// bug, in the one place that should fail loud
    /// (`C-RESOLVE-ID-FALLS-THROUGH-TO-THE-RAW-SIGIL`).
    pub(super) fn resolve_id(&self, id: Option<&str>, ctx: &BindingContext) -> Result<String, String> {
        let raw = id.unwrap_or("");
        if raw.starts_with('@') {
            let Some(v) = ctx.resolve_binding(raw) else {
                return Err(format!("unresolved binding '{raw}' used as an entity id"));
            };
            if let Some(s) = v.as_str() {
                return Ok(s.to_string());
            }
            if let Some(n) = v.as_number() {
                return Ok(if n.fract() == 0.0 {
                    (n as i64).to_string()
                } else {
                    n.to_string()
                });
            }
            return Err(format!(
                "binding '{raw}' used as an entity id resolved to {v:?}, which is not a string or number"
            ));
        }
        Ok(raw.to_string())
    }

    /// A fetch `id` expression, evaluated so it can never silently widen to a collection fetch.
    pub(super) fn resolve_fetch_id(
        &self,
        id: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<Option<String>, String> {
        match id {
            None => Ok(None),
            Some(Value::String(raw)) => self.resolve_id(Some(raw), ctx).map(Some),
            Some(expr) => {
                let sexpr: SExpression = expr.clone().into();
                match Evaluator::new().evaluate(&sexpr, ctx).map_err(|e| e.to_string())? {
                    Value::String(s) => Ok(Some(s)),
                    Value::Number(n) if n.fract() == 0.0 => Ok(Some((n as i64).to_string())),
                    other => Err(format!(
                        "fetch id expression resolved to {other:?}, which is not a string or integer"
                    )),
                }
            }
        }
    }

    /// A fetch's `nearest: { field, vector, k }`, each member evaluated at execution (EXEC-FETCH-2). It excludes
    /// `id`, `orderBy`, `offset` and `limit`; a `k` that is not a positive integer fails the fetch.
    pub(super) fn resolve_nearest(
        &self,
        nearest: Option<&Value>,
        id: Option<&Value>,
        order_by: Option<&Value>,
        limit: Option<&Value>,
        offset: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<Option<Box<RowNearest>>, String> {
        let Some(nearest) = nearest else { return Ok(None) };
        if id.is_some() || order_by.is_some() || limit.is_some() || offset.is_some() {
            return Err("nearest cannot be combined with id, orderBy, offset or limit".to_string());
        }
        let Value::Object(members) = nearest else {
            return Err("nearest must be { field, vector, k }".to_string());
        };
        let member = |name: &str| -> Result<Value, String> {
            let expr = members
                .get(name)
                .ok_or_else(|| format!("nearest needs `{name}`"))?;
            let sexpr: SExpression = expr.clone().into();
            Evaluator::new().evaluate(&sexpr, ctx).map_err(|e| e.to_string())
        };
        let Value::String(field) = member("field")? else {
            return Err("nearest `field` must be a string".to_string());
        };
        let Value::Array(items) = member("vector")? else {
            return Err("nearest `vector` must be a [number]".to_string());
        };
        let vector = items
            .iter()
            .map(|item| match item {
                Value::Number(n) => Ok(*n),
                _ => Err("nearest `vector` must be a [number]".to_string()),
            })
            .collect::<Result<Vec<f64>, String>>()?;
        let k = match member("k")? {
            Value::Number(n) if n >= 1.0 && n.fract() == 0.0 => n as usize,
            _ => return Err("nearest `k` must be a positive integer".to_string()),
        };
        Ok(Some(Box::new(RowNearest { field, vector, k })))
    }

    /// A fetch `limit`/`offset`, evaluated at execution; only a positive number applies (as `effect-stage.ts`).
    pub(super) fn resolve_page_option(
        &self,
        value: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<Option<usize>, String> {
        let Some(value) = value else { return Ok(None) };
        let sexpr: SExpression = value.clone().into();
        match Evaluator::new()
            .evaluate(&sexpr, ctx)
            .map_err(|e| e.to_string())?
        {
            Value::Number(n) if n > 0.0 => Ok(Some(n as usize)),
            _ => Ok(None),
        }
    }

    /// A fetch's `orderBy` (`"field"` or `"field:asc|desc"`, an expression) as a [`RowOrder`].
    pub(super) fn resolve_order_spec(
        &self,
        value: Option<&Value>,
        ctx: &BindingContext,
    ) -> Result<Option<RowOrder>, String> {
        let Some(value) = value else { return Ok(None) };
        let sexpr: SExpression = value.clone().into();
        match Evaluator::new()
            .evaluate(&sexpr, ctx)
            .map_err(|e| e.to_string())?
        {
            Value::String(spec) if !spec.is_empty() => {
                let (field, direction) = spec.split_once(':').unwrap_or((spec.as_str(), "asc"));
                Ok(Some(RowOrder {
                    field: field.to_string(),
                    direction: if direction == "desc" {
                        RowDirection::Desc
                    } else {
                        RowDirection::Asc
                    },
                }))
            }
            _ => Ok(None),
        }
    }

    /// Resolve every `@`-binding in a value — the canonical walker lives on
    /// `BindingContext::resolve_value`; this is a thin forwarder for in-executor callers.
    pub(super) fn resolve_value_bindings<'v>(
        &self,
        value: &'v Value,
        ctx: &BindingContext,
    ) -> std::borrow::Cow<'v, Value> {
        ctx.resolve_value(value)
    }

    /// Resolve a `persist` effect's `data` argument for real, not just
    /// sigil-substitute it. `data` came from `parse_persist`, which converts
    /// the authored `SExpression` straight to `Value` — a CALL expression
    /// like `(object/merge ?data {field: @user.id})` survives that
    /// conversion as a literal `["object/merge", ...]` array, and
    /// `resolve_value_bindings`/`ctx.resolve_value` only walks the tree
    /// substituting leaf `@`-sigils; it never dispatches a call, so the
    /// merge itself silently never ran (C-PERSIST-DATA-CALL-EXPR-NEVER-
    /// EVALUATED — invisible until an access-policy check read a field back
    /// out of the "merged" row and found the raw call array instead).
    /// Round-tripping through the full evaluator dispatches calls AND
    /// substitutes bindings identically to the old path for plain literal
    /// data, so this is a strict superset, not a behavior change for the
    /// common case. Evaluation failure falls back to the old plain
    /// substitution rather than hard-failing the persist.
    pub(super) fn resolve_persist_data<'v>(
        &self,
        data: &'v Value,
        ctx: &BindingContext,
    ) -> std::borrow::Cow<'v, Value> {
        let expr: SExpression = data.clone().into();
        match Evaluator::new().evaluate(&expr, ctx) {
            Ok(evaluated) => std::borrow::Cow::Owned(evaluated),
            Err(err) => {
                warn!(error = %err, "persist data failed to evaluate; falling back to plain binding substitution");
                self.resolve_value_bindings(data, ctx)
            }
        }
    }

    /// Resolve the target row id for `update` — the effect's own `id` arg
    /// when the author supplied one (`(persist update Entity id data)`), else
    /// the already-resolved data's own `id` field (`(persist update Entity
    /// data)` — "the data carries its own id", the dominant corpus idiom).
    /// Mirrors the interpreted path's identical fallback
    /// (`OrbitalServerRuntime.ts`'s persist handler: `data?.id || entityId`).
    pub(super) fn resolve_update_id(
        &self,
        id: Option<&str>,
        resolved_data: &Value,
        ctx: &BindingContext,
    ) -> Result<String, String> {
        let from_id_arg = self.resolve_id(id, ctx)?;
        if !from_id_arg.is_empty() {
            return Ok(from_id_arg);
        }
        let from_data = resolved_data
            .as_object()
            .and_then(|o| o.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if from_data.is_empty() {
            // FATAL. Re-measured and gated clean — a persist update/delete with
            // no resolvable row key now fails the write instead of silently
            // no-opping it.
            const NO_ROW_KEY_IS_FATAL: bool = true;
            tracing::warn!(
                "persist update/delete resolved NO ROW KEY — this write will not land. \
                 Bind it before the write, e.g. `(set @entity.id ?row.id)` on the \
                 transition that selects the row."
            );
            if !NO_ROW_KEY_IS_FATAL {
                return Ok(String::new());
            }
            // A write with NO row key is a failed write, not a write to the row
            // keyed by "". Returning `Ok(String::new())` sent an empty key
            // through to `persistence.update`, and on the JS path the same
            // condition skipped the update entirely while still reporting
            // `success: true` — two paths, two different wrong answers, both
            // silent. `ORB_BINDING_PERSIST_ROW_ID_NEVER_SET` is the compile-time
            // half of this; this is the runtime half.
            return Err(NO_ROW_KEY.to_string());
        }
        Ok(from_data.to_string())
    }
}
