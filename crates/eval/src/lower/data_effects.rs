//! Lowering emit, persist, fetch and set effects.

use super::*;

impl NodeLowerer {
    pub(super) fn resolve_emit_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "emit requires event name".to_string(),
            ));
        }
        let event = self.sexpr_as_str(&args[0], "(emit …) argument 1 — the event name")?;
        let payload = if args.len() > 1 {
            Some(self.resolve_sexpr(&args[1])?)
        } else {
            None
        };
        Ok(OirExpr::effect(if let Some(p) = payload {
            OirEffect::emit_with_payload(event, p)
        } else {
            OirEffect::emit(event)
        }))
    }

    /// `["persist", "batch", <ops>, { emit }?]`. Op tuples written out in `<ops>`
    /// (literal list elements, or the tuple an `array/map` lambda returns) are
    /// wrapped in the `["list", …]` literal constructor so they resolve as array
    /// values, never as calls to a `create`/`update`/`delete` operator.
    pub(super) fn resolve_persist_batch_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        use orbital_core_base::persist_batch::{
            batch_entities, classify_ops, is_op_tuple, BatchOps,
        };
        let ops = args.get(1).ok_or_else(|| {
            LowerError::InvalidSExpr("persist batch requires an operations argument".to_string())
        })?;
        let entities: Vec<String> = batch_entities(ops)
            .into_iter()
            .map(str::to_string)
            .collect();

        fn as_list(tuple: &SExpression) -> SExpression {
            let mut items = vec![SExpression::string("list")];
            if let SExpression::Array(t) = tuple {
                items.extend(t.iter().cloned());
            }
            SExpression::Array(items)
        }
        let mut ops_expr = ops.clone();
        if let SExpression::Array(items) = &mut ops_expr {
            match classify_ops(ops) {
                BatchOps::Literal(_) => {
                    let start = usize::from(items.first().and_then(|h| h.as_str()) == Some("list"));
                    for element in items.iter_mut().skip(start) {
                        if is_op_tuple(element) {
                            *element = as_list(element);
                        }
                    }
                }
                BatchOps::Mapped(_) => {
                    if let Some(SExpression::Array(lambda)) = items.get_mut(2) {
                        if let Some(body) = lambda.last_mut() {
                            *body = as_list(body);
                        }
                    }
                }
                BatchOps::Expression => {}
            }
        }
        let ops_oir = self.resolve_owned(ops_expr)?;

        let mut emit_cfg: Option<OirEmitConfig> = None;
        if let Some(SExpression::Object(obj)) = args.get(2) {
            if obj.contains_key("emit") {
                emit_cfg = self.extract_emit_config(obj)?;
            }
        }
        Ok(OirExpr::effect(OirEffect::PersistBatch {
            ops: ops_oir,
            entities,
            emit: emit_cfg,
        }))
    }

    pub(super) fn resolve_persist_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.first().and_then(|a| a.as_str()) == Some("batch") {
            return self.resolve_persist_batch_effect(args);
        }
        // Format: ["persist", operation, entity, ...args]
        // Examples:
        //   ["persist", "create", "Task", "@payload.data"]
        //   ["persist", "update", "Task", "@payload.id", "@payload.data"]
        //   ["persist", "delete", "Task", "@payload.row.id"]
        //   ["persist", "update", "Task", "@entity", { "emit": {...} }]
        if args.len() < 2 {
            return Err(LowerError::InvalidSExpr(
                "persist requires operation and entity".to_string(),
            ));
        }
        let operation = self.sexpr_as_str(
            &args[0],
            "(persist …) argument 1 — the operation (create|upsert|update|delete)",
        )?;
        let entity = self.sexpr_as_str(&args[1], "(persist …) argument 2 — the entity name")?;

        // The trailing arg may be an options object carrying `emit:`. When
        // present, the data arg shifts one position back. Options objects
        // are detected by presence of `emit:` (or being an empty object
        // that is not a data payload). A plain object with no `emit:` is
        // still treated as data (e.g. inline record for create/update).
        let mut data_end = args.len();
        let mut emit_cfg: Option<OirEmitConfig> = None;

        if args.len() > 3 {
            if let SExpression::Object(obj) = &args[args.len() - 1] {
                if obj.contains_key("emit") {
                    emit_cfg = self.extract_emit_config(obj)?;
                    data_end -= 1;
                }
            }
        }

        // Get data argument (may be at different positions based on operation)
        let data = if data_end > 2 {
            self.resolve_sexpr(&args[data_end - 1])?
        } else {
            OirExpr::object(orbital_core_base::ir::expr::OirFields::default())
        };

        let mut effect = match operation.as_str() {
            "create" => OirEffect::create(&entity, data),
            "upsert" => OirEffect::upsert(&entity, data),
            "update" => OirEffect::update(&entity, data),
            "delete" => OirEffect::delete(&entity, data),
            "read" | "get" => OirEffect::read(&entity, data),
            "list" => OirEffect::list(&entity, data),
            "query" => OirEffect::query(&entity, data),
            _ => OirEffect::update(&entity, data), // default to update
        };

        if let OirEffect::Persist {
            emit: ref mut em, ..
        } = effect
        {
            *em = emit_cfg;
        }

        Ok(OirExpr::effect(effect))
    }

    pub(super) fn resolve_fetch_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "fetch requires entity name".to_string(),
            ));
        }
        let entity = self.sexpr_as_str(&args[0], "(fetch …) argument 1 — the entity name")?;

        // Check if args[1] is an options object with { id, include, filter, ... }
        // This supports: ["fetch", "Entity", { "id": "@payload.id" }]
        //                 ["fetch", "Entity", { "filter": ..., "include": [...] }]
        if args.len() > 1 {
            if let SExpression::Object(obj) = &args[1] {
                // Extract id (single-entity fetch)
                let id = if let Some(id_expr) = obj.get("id") {
                    match id_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(id_expr)?),
                    }
                } else {
                    None
                };

                // Extract include array (accepting the `["list", …]` literal
                // constructor a config-substituted value carries)
                let include = if let Some(SExpression::Array(arr)) = obj.get("include") {
                    let fields: Vec<String> = strip_literal_list_marker(arr)
                        .iter()
                        .filter_map(|v| {
                            if let SExpression::String(s) = v {
                                Some(s.to_string())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if fields.is_empty() {
                        None
                    } else {
                        Some(fields)
                    }
                } else {
                    None
                };

                // Extract filter from options object
                let filter = if let Some(filter_expr) = obj.get("filter") {
                    match filter_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(filter_expr)?),
                    }
                } else {
                    None
                };

                // Extract order_by from options object. Same Null-skip
                // shape as filter; the codegen accepts both static
                // "field:dir" string literals and dynamic SExpressions.
                let order_by = if let Some(order_expr) = obj.get("orderBy") {
                    match order_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(order_expr)?),
                    }
                } else {
                    None
                };

                // Extract offset (pagination start). Captured as OirExpr
                // because schema authors compute it dynamically — e.g.
                // `(* (- @payload.page 1) @config.pageSize)`.
                let offset = if let Some(offset_expr) = obj.get("offset") {
                    match offset_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(offset_expr)?),
                    }
                } else {
                    None
                };

                // Extract limit (pagination size). Same OirExpr capture as
                // offset — `@config.pageSize` is the canonical form.
                let limit = if let Some(limit_expr) = obj.get("limit") {
                    match limit_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(limit_expr)?),
                    }
                } else {
                    None
                };

                let nearest = match obj.get("nearest") {
                    None | Some(SExpression::Null) => None,
                    Some(expr) => Some(self.resolve_sexpr(expr)?),
                };

                let emit_cfg = self.extract_emit_config(obj)?;

                if let Some(id) = id {
                    // Fetch by ID: ["fetch", "Entity", { "id": "@payload.id" }]
                    let mut effect = OirEffect::fetch_by_id(&entity, id);
                    if let OirEffect::Fetch {
                        include: ref mut inc,
                        emit: ref mut em,
                        ..
                    } = effect
                    {
                        *inc = include;
                        *em = emit_cfg;
                    }
                    return Ok(OirExpr::effect(effect));
                }

                let mut effect = OirEffect::fetch_with_options(entity, filter, order_by, limit);
                if let OirEffect::Fetch {
                    offset: ref mut off,
                    include: ref mut inc,
                    nearest: ref mut near,
                    emit: ref mut em,
                    ..
                } = effect
                {
                    *off = offset;
                    *inc = include;
                    *near = nearest;
                    *em = emit_cfg;
                }
                return Ok(OirExpr::effect(effect));
            }

            // Legacy shorthand: ["fetch", "Entity", "@payload.id"]
            // A string binding as the second arg is treated as an ID, not a filter.
            if let SExpression::String(s) = &args[1] {
                if s.starts_with('@') && s.contains('.') && s.ends_with("id") {
                    let id_expr = self.resolve_sexpr(&args[1])?;
                    return Ok(OirExpr::effect(OirEffect::fetch_by_id(entity, id_expr)));
                }
            }
        }

        // Legacy positional args: ["fetch", "Entity", filterExpr, orderByExpr, limit]
        let filter = if args.len() > 1 {
            match &args[1] {
                SExpression::Null => None,
                _ => Some(self.resolve_sexpr(&args[1])?),
            }
        } else {
            None
        };

        // args[2]: order_by expression
        let order_by = if args.len() > 2 {
            match &args[2] {
                SExpression::Null => None,
                _ => Some(self.resolve_sexpr(&args[2])?),
            }
        } else {
            None
        };

        // args[3]: limit expression. Captured as OirExpr so dynamic
        // bindings like `@config.pageSize` survive lowering. Static numeric
        // literals also flow through this path as `OirExpr::number`.
        let limit = if args.len() > 3 {
            match &args[3] {
                SExpression::Null => None,
                _ => Some(self.resolve_sexpr(&args[3])?),
            }
        } else {
            None
        };

        Ok(OirExpr::effect(OirEffect::fetch_with_options(
            entity, filter, order_by, limit,
        )))
    }

    pub(super) fn resolve_fetch_stream_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "fetch-stream requires entity name".to_string(),
            ));
        }
        let entity =
            self.sexpr_as_str(&args[0], "(fetch-stream …) argument 1 — the entity name")?;

        if args.len() > 1 {
            if let SExpression::Object(obj) = &args[1] {
                let id = if let Some(id_expr) = obj.get("id") {
                    match id_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(id_expr)?),
                    }
                } else {
                    None
                };
                let filter = if let Some(filter_expr) = obj.get("filter") {
                    match filter_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(filter_expr)?),
                    }
                } else {
                    None
                };
                let emit_cfg = self.extract_emit_config(obj)?;
                return Ok(OirExpr::effect(OirEffect::FetchStream {
                    entity_type: entity,
                    id,
                    filter,
                    emit: emit_cfg,
                }));
            }
        }

        Ok(OirExpr::effect(OirEffect::FetchStream {
            entity_type: entity,
            id: None,
            filter: None,
            emit: None,
        }))
    }

    pub(super) fn resolve_send_server_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "send-server requires event name".to_string(),
            ));
        }
        let event = self.resolve_sexpr(&args[0])?;
        let payload = if args.len() > 1 {
            Some(self.resolve_sexpr(&args[1])?)
        } else {
            None
        };
        Ok(OirExpr::effect(OirEffect::SendServer { event, payload }))
    }

    pub(super) fn resolve_set_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.len() < 2 {
            return Err(LowerError::InvalidSExpr(
                "set requires target and value".to_string(),
            ));
        }

        // Optional trailing options object: ["set", target, value, { emit: {...} }]
        let emit_cfg = if args.len() > 2 {
            if let SExpression::Object(obj) = &args[2] {
                self.extract_emit_config(obj)?
            } else {
                None
            }
        } else {
            None
        };

        // Handle dynamic path expressions like ["path", "formValues", "@payload.fieldId"]
        // In this case, we resolve the path expression and use it as a dynamic binding
        if let SExpression::Array(arr) = &args[0] {
            if arr
                .first()
                .map(|v| matches!(v, SExpression::String(s) if s == "path"))
                .unwrap_or(false)
            {
                // Dynamic path - resolve the path expression and create a set with dynamic target
                let path_expr = self.resolve_sexpr(&args[0])?;
                let value = self.resolve_sexpr(&args[1])?;
                if matches!(value, OirExpr::Effect { .. }) {
                    return Err(LowerError::InvalidSExpr(
                        "set cannot take an effect as a value — substrate/async operators (llm/generate, memory/store, …) are async and reach outside the app. Use them as a top-level effect with an emit options object, e.g. (llm/generate ... { emit: { success: E, failure: F } }), and capture the result via a transition on the success event (C-EMIT-SUBSTRATE-OPS-MARKER).".to_string()
                    ));
                }
                let mut effect = OirEffect::set_dynamic(path_expr, value);
                if let OirEffect::SetDynamic {
                    emit: ref mut em, ..
                } = effect
                {
                    *em = emit_cfg;
                }
                return Ok(OirExpr::effect(effect));
            }
        }

        let target_str = self.sexpr_as_str(
            &args[0],
            "(set …) argument 1 — the binding target (e.g. @entity.field)",
        )?;
        let target = OirBinding::parse(&target_str)
            .ok_or_else(|| LowerError::InvalidSExpr(format!("Invalid binding: {}", target_str)))?;
        let value = self.resolve_sexpr(&args[1])?;
        if matches!(value, OirExpr::Effect { .. }) {
            return Err(LowerError::InvalidSExpr(
                "set cannot take an effect as a value — substrate/async operators (llm/generate, memory/store, …) are async and reach outside the app. Use them as a top-level effect with an emit options object, e.g. (llm/generate ... { emit: { success: E, failure: F } }), and capture the result via a transition on the success event (C-EMIT-SUBSTRATE-OPS-MARKER).".to_string()
            ));
        }
        let mut effect = OirEffect::set(target, value);
        if let OirEffect::Set {
            emit: ref mut em, ..
        } = effect
        {
            *em = emit_cfg;
        }
        Ok(OirExpr::effect(effect))
    }
}
