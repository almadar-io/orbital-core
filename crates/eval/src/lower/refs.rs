//! Lowering ref, deref, swap, watch and atomic effects.

use super::*;

impl NodeLowerer {
    pub(super) fn resolve_ref_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["ref", "EntityType"]
        //         ["ref", "EntityType", { "id": "@payload.id", "filter": ..., "include": [...] }]
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "ref requires entity type".to_string(),
            ));
        }
        let entity_type = self.sexpr_as_str(&args[0], "(ref …) argument 1 — the entity type")?;

        if args.len() > 1 {
            if let SExpression::Object(obj) = &args[1] {
                // Extract id (single-entity ref)
                let id = if let Some(id_expr) = obj.get("id") {
                    match id_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(id_expr)?),
                    }
                } else {
                    None
                };

                // Extract filter
                let filter = if let Some(filter_expr) = obj.get("filter") {
                    match filter_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(filter_expr)?),
                    }
                } else {
                    None
                };

                // Extract order_by
                let order_by = if let Some(order_expr) = obj.get("order_by") {
                    match order_expr {
                        SExpression::Null => None,
                        _ => Some(self.resolve_sexpr(order_expr)?),
                    }
                } else {
                    None
                };

                let limit = match obj.get("limit") {
                    None | Some(SExpression::Null) => None,
                    Some(limit_expr) => Some(self.resolve_sexpr(limit_expr)?),
                };

                // Extract include array (accepting the `["list", …]` literal
                // constructor a config-substituted value carries)
                let include = if let Some(SExpression::Array(arr)) = obj.get("include") {
                    let fields: Vec<String> = strip_literal_list_marker(arr)
                        .iter()
                        .filter_map(|v| {
                            if let SExpression::String(s) = v {
                                Some(s.clone())
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

                let emit_cfg = self.extract_emit_config(obj)?;

                let mut effect = OirEffect::ref_entity(&entity_type);
                if let OirEffect::Ref {
                    id: ref mut i,
                    filter: ref mut f,
                    order_by: ref mut ob,
                    limit: ref mut l,
                    include: ref mut inc,
                    emit: ref mut em,
                    ..
                } = effect
                {
                    *i = id;
                    *f = filter;
                    *ob = order_by;
                    *l = limit;
                    *inc = include;
                    *em = emit_cfg;
                }
                return Ok(OirExpr::effect(effect));
            }

            // Legacy shorthand: ["ref", "Entity", "@payload.id"]
            if let SExpression::String(s) = &args[1] {
                if s.starts_with('@') && s.contains('.') && s.ends_with("id") {
                    let id_expr = self.resolve_sexpr(&args[1])?;
                    let mut effect = OirEffect::ref_entity(&entity_type);
                    if let OirEffect::Ref { id: ref mut i, .. } = effect {
                        *i = Some(id_expr);
                    }
                    return Ok(OirExpr::effect(effect));
                }
            }
        }

        Ok(OirExpr::effect(OirEffect::ref_entity(entity_type)))
    }

    pub(super) fn resolve_deref_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["deref", "EntityType"] or ["deref", "EntityType", idExpr]
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "deref requires entity type".to_string(),
            ));
        }
        let entity_type = self.sexpr_as_str(&args[0], "(deref …) argument 1 — the entity type")?;

        if args.len() > 1 {
            let id = self.resolve_sexpr(&args[1])?;
            Ok(OirExpr::effect(OirEffect::deref_by_id(entity_type, id)))
        } else {
            Ok(OirExpr::effect(OirEffect::deref_entity(entity_type)))
        }
    }

    pub(super) fn resolve_swap_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["swap", "EntityType", idExpr, transformExpr]
        if args.len() < 3 {
            return Err(LowerError::InvalidSExpr(
                "swap requires entity type, id, and transform".to_string(),
            ));
        }
        let entity_type = self.sexpr_as_str(&args[0], "(swap …) argument 1 — the entity type")?;
        let id = self.resolve_sexpr(&args[1])?;
        let transform = self.resolve_sexpr(&args[2])?;
        Ok(OirExpr::effect(OirEffect::swap(entity_type, id, transform)))
    }

    pub(super) fn resolve_watch_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["watch", "EntityType", [effect1, effect2, ...]]
        if args.len() < 2 {
            return Err(LowerError::InvalidSExpr(
                "watch requires entity type and effects array".to_string(),
            ));
        }
        let entity_type = self.sexpr_as_str(&args[0], "(watch …) argument 1 — the entity type")?;

        let effects = if let SExpression::Array(arr) = &args[1] {
            arr.iter()
                .map(|e| self.resolve_sexpr(e))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            return Err(LowerError::InvalidSExpr(
                "watch effects must be an array".to_string(),
            ));
        };

        Ok(OirExpr::effect(OirEffect::watch(entity_type, effects)))
    }

    pub(super) fn resolve_atomic_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["atomic", [effect1, effect2, ...]]
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "atomic requires effects array".to_string(),
            ));
        }

        let effects = if let SExpression::Array(arr) = &args[0] {
            arr.iter()
                .map(|e| self.resolve_sexpr(e))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            return Err(LowerError::InvalidSExpr(
                "atomic effects must be an array".to_string(),
            ));
        };

        Ok(OirExpr::effect(OirEffect::atomic(effects)))
    }
}
