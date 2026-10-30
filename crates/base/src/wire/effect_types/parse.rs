//! Parsing an effect S-expression into an `Effect`.

use super::*;

/// Parse an effect from an S-expression
///
/// Supports formats like:
/// - `["emit", "EVENT_NAME", { payload }]`
/// - `["persist", "create", "Entity", data]`
/// - `["render-ui", "slot", { content }]`
/// - `["navigate", "/path"]`
/// - `["set", "@path", value]`
pub fn parse_effect(expr: &SExpression) -> Option<Effect> {
    let arr = expr.as_array()?;
    if arr.is_empty() {
        return None;
    }

    let op = arr[0].as_str()?;
    let args = &arr[1..];

    if let Some(os_op) = OsOp::from_name(op) {
        return parse_os(os_op, args);
    }

    if let Some(tab_op) = crate::tab::TabOp::from_name(op) {
        return parse_tab(tab_op, args);
    }

    match op {
        "emit" => parse_emit(args),
        "set" => parse_set(args),
        "persist" => parse_persist(args),
        "fetch" => parse_fetch(args),
        "call-service" => parse_call_service(args),
        "cancel-call" => parse_cancel_call(args),
        "random-bytes" => parse_random_bytes(args),
        "credential-status" => parse_credential(args, true),
        "credential-forget" => parse_credential(args, false),
        admin if crate::credential_admin::CredentialAdminOp::from_name(admin).is_some() => {
            parse_credential_admin(admin, args)
        }
        "access-check" => parse_access_check(args),
        "browser/page-facts" => parse_page_facts(args),
        "browser/prompt" => parse_prompt(args),
        "browser/embed" => parse_embed(args),
        "render-ui" => parse_render_ui(args),
        "navigate" => parse_navigate(args),
        "navigate-back" => Some(Effect::NavigateBack),
        "spawn" => parse_spawn(args),
        "despawn" => parse_despawn(args),
        "log" => parse_log(args),
        // `notify` is `.lolo`-only sugar for `render-ui toast` (owner ruling
        // 2026-09-11), desugared away at lowering; the `.orb` IR never
        // carries this kind, so a stale tuple falls through to `Custom`
        // below rather than a dedicated variant — never crashes the parser.
        _ => {
            let mut values: Vec<Value> = args.iter().map(|e| Value::from(e.clone())).collect();
            let emit = values.last().and_then(parse_emit_config);
            let options = if emit.is_some() { values.pop() } else { None };
            let key = options
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|o| o.get("key"))
                .cloned();
            // The op's own options written beside `emit`/`key` (`{ file: true emit: … }`) stay its last argument.
            if let Some(map) = options.as_ref().and_then(Value::as_object) {
                let rest: crate::value::ValueMap = map
                    .iter()
                    .filter(|(k, _)| k.as_str() != "emit" && k.as_str() != "key")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !rest.is_empty() {
                    values.push(Value::object(rest));
                }
            }
            Some(Effect::Custom {
                name: op.to_string(),
                args: values,
                key,
                emit,
            })
        }
    }
}

/// Whether a string is recognized as an effect operator name.
///
/// Used by callers (notably `orbital_compiler::phases::resolve`) that need
/// to disambiguate a Lisp-style call form from a string-array literal in
/// JSON `.orb` payloads — the only signal at the JSON level is the head
/// string. The installed operator table decides: an operator it declares with
/// side effects or a host, or a substrate operator (`is_substrate_operator`,
/// which includes the substrate read operators). A name the table does not
/// declare is not an effect.
pub fn is_known_effect_name(op: &str) -> bool {
    crate::language::registry::try_get_operator(op)
        .is_some_and(|operator| operator.has_side_effects || operator.host.is_some())
        || crate::ir::effects::is_substrate_operator(op)
}

pub(super) fn parse_emit(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let event = args[0].as_str()?.to_string();
    let payload = args
        .get(1)
        .map(|e| Value::from(e.clone()))
        .unwrap_or(Value::Null);
    Some(Effect::Emit { event, payload })
}

pub(super) fn parse_set(args: &[SExpression]) -> Option<Effect> {
    if args.len() < 2 {
        return None;
    }
    let path = args[0].as_shared_str()?.clone();
    let value = Value::from(args[1].clone());
    Some(Effect::Set { path, value })
}

/// A trailing `{ emit: {...} }` config object is options, never positional
/// id/data — mirrors the compiler validator's own `extract_emit_object_ref`
/// check (`orbital-compiler/src/phases/validation/effect/emit.rs`), which
/// treats a trailing object with an `emit` key as options-only for exactly
/// this reason. `parse_persist` must agree, or `(persist update Entity ?data
/// {emit:{...}})` — the dominant corpus idiom for "update with no separate
/// id, emit routes elsewhere" — misreads the emit config as `data` and the
/// literal data token as `id` (C-PERSIST-UPDATE-TRAILING-EMIT-MISPARSED).
pub fn strip_trailing_emit_options(args: &[SExpression]) -> &[SExpression] {
    match args.last().and_then(|a| a.as_object()) {
        Some(obj) if obj.contains_key("emit") || obj.contains_key("where") => {
            &args[..args.len() - 1]
        }
        _ => args,
    }
}

/// Parse a `{ "emit": { "success": "...", "failure": "..." } }` options
/// object into a `FetchEmit`. Shared by `parse_fetch` and `parse_persist` —
/// one shape, one reader. `None` when `obj` carries no `emit` key or neither
/// event is declared.
pub(super) fn parse_emit_config(obj: &Value) -> Option<FetchEmit> {
    obj.as_object()?
        .get("emit")
        .and_then(|v| v.as_object())
        .and_then(|e| {
            let success = e
                .get("success")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let failure = e
                .get("failure")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let route = |key: &str| e.get(key).and_then(|v| v.as_str()).map(|s| s.to_string());
            let fe = FetchEmit {
                success,
                failure,
                cancelled: route("cancelled"),
                on_message: route("onMessage"),
            };
            (!fe.is_empty() || fe.on_message.is_some()).then_some(fe)
        })
}

pub(super) fn parse_persist(args: &[SExpression]) -> Option<Effect> {
    if args.first().and_then(|a| a.as_str()) == Some("batch") {
        let ops = Value::from(args.get(1)?.clone());
        let emit = args
            .get(2)
            .and_then(|a| parse_emit_config(&Value::from(a.clone())));
        return Some(Effect::PersistBatch { ops, emit });
    }
    if args.len() < 3 {
        return None;
    }
    let op_str = args[0].as_str()?;
    let operation = PersistOperation::from_str(op_str)?;
    let entity_type = args[1].as_str()?.to_string();

    let emit = args
        .last()
        .and_then(|a| parse_emit_config(&Value::from(a.clone())));

    // `delete` carries only an id (`[delete, Type, id]`); `update` with 4
    // (non-options) args is id + data, with 3 the data carries its own id;
    // `create` takes data. A trailing options object never counts toward
    // this arity — see `strip_trailing_emit_options`.
    let positional = strip_trailing_emit_options(args);
    let (id, data) = match operation {
        PersistOperation::Delete => (
            positional
                .get(2)
                .and_then(|a| a.as_str())
                .map(|s| s.to_string()),
            Value::Null,
        ),
        PersistOperation::Update if positional.len() >= 4 => (
            positional[2].as_str().map(|s| s.to_string()),
            Value::from(positional[3].clone()),
        ),
        _ => (None, Value::from(positional.get(2)?.clone())),
    };

    let guard = args
        .last()
        .and_then(|a| a.as_object())
        .and_then(|o| o.get("where"))
        .map(|w| Value::from(w.clone()));
    Some(Effect::Persist {
        operation,
        entity_type,
        data,
        id,
        emit,
        guard,
    })
}

/// Parse fetch effect from S-expression
///
/// Supports formats from Orbital_Pattern.orb:
/// - `["fetch", "User"]` - fetch all entities
/// - `["fetch", "User", {"id": "@payload.row.id"}]` - fetch by ID
/// - `["fetch", "User", {"filter": [...], "limit": 10}]` - fetch with options
/// - `["fetch", "Inspection", {"include": ["company"]}]` - fetch with related entities
pub(super) fn parse_fetch(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }

    let entity_type = args[0].as_str()?.to_string();

    // Parse optional config object
    let (id, filter, limit, offset, order_by, include, deliver, total, nearest, emit) =
        if let Some(config) = args.get(1) {
            let config_value = Value::from(config.clone());
            if let Some(obj) = config_value.as_object() {
                let option = |key: &str| obj.get(key).filter(|v| !v.is_null()).cloned();
                let id = option("id");
                let filter = obj.get("filter").cloned();
                let limit = option("limit");
                let offset = option("offset");
                let order_by = option("orderBy");
                let include = obj.get("include").and_then(|v| {
                    v.as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|item| item.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                });
                let deliver = match obj.get("deliver").and_then(Value::as_str) {
                    Some("ids") => FetchDeliver::Ids,
                    _ => FetchDeliver::Rows,
                };
                let total = match obj.get("total").and_then(Value::as_str) {
                    Some("none") => Some(crate::row_set::RowTotalMode::None),
                    _ => None,
                };
                let nearest = option("nearest");
                let emit = parse_emit_config(&config_value);
                (
                    id, filter, limit, offset, order_by, include, deliver, total, nearest, emit,
                )
            } else {
                (
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    FetchDeliver::Rows,
                    None,
                    None,
                    None,
                )
            }
        } else {
            (
                None,
                None,
                None,
                None,
                None,
                None,
                FetchDeliver::Rows,
                None,
                None,
                None,
            )
        };

    Some(Effect::Fetch {
        entity_type,
        id,
        filter,
        limit,
        offset,
        order_by,
        include,
        deliver,
        total,
        nearest,
        emit,
    })
}

pub(super) fn parse_call_service(args: &[SExpression]) -> Option<Effect> {
    if args.len() < 2 {
        return None;
    }
    let service = args[0].as_str()?.to_string();
    let method = args[1].as_str()?.to_string();
    let call_args = args
        .get(2)
        .map(|e| Value::from(e.clone()))
        .unwrap_or(Value::Null);
    let key = match args.get(3) {
        Some(SExpression::Object(options)) => {
            options.get("key").map(|key| Value::from(key.clone()))
        }
        _ => None,
    };
    let emit = args
        .get(3)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    Some(Effect::CallService {
        service,
        method,
        args: call_args,
        key,
        emit,
    })
}

/// `["browser/page-facts", [field, …], { emit? }?]`.
pub(super) fn parse_page_facts(args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let options = args.get(1).map(|e| Value::from(e.clone()));
    let key = options
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = options.as_ref().and_then(parse_emit_config);
    Some(Effect::BrowserPageFacts {
        fields: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// The `key` and `emit` of a trailing options object.
pub(super) fn keyed_options(options: Option<&SExpression>) -> (Option<Value>, Option<FetchEmit>) {
    let options = options.map(|e| Value::from(e.clone()));
    let key = options
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = options.as_ref().and_then(parse_emit_config);
    (key, emit)
}

/// `["browser/prompt", text, schemaText, { key?, emit? }?]`.
pub(super) fn parse_prompt(args: &[SExpression]) -> Option<Effect> {
    if !(2..=3).contains(&args.len()) {
        return None;
    }
    let (key, emit) = keyed_options(args.get(2));
    Some(Effect::BrowserPrompt {
        text: Value::from(args[0].clone()),
        schema: Value::from(args[1].clone()),
        key,
        emit,
    })
}

/// `["browser/embed", [text, …], { key?, emit? }?]`.
pub(super) fn parse_embed(args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let (key, emit) = keyed_options(args.get(1));
    Some(Effect::BrowserEmbed {
        texts: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// `["browser/tab-<op>", request, { key?, emit? }?]`.
pub(super) fn parse_tab(op: crate::tab::TabOp, args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let (key, emit) = keyed_options(args.get(1));
    Some(Effect::BrowserTab {
        op,
        request: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// `["os/<op>", args…, { key?, emit? }?]`; the envelope is told from the op's own options object by
/// [`split_envelope`], and the positional count must be one the op takes.
pub(super) fn parse_os(op: OsOp, args: &[SExpression]) -> Option<Effect> {
    let (positional, envelope) = split_envelope(op, args);
    if !accepts_arity(op, positional.len()) {
        return None;
    }
    let options = envelope.map(|fields| Value::from(SExpression::object(fields.clone())));
    let key = options
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = options.as_ref().and_then(parse_emit_config);
    Some(Effect::Os {
        op,
        args: positional.iter().map(|e| Value::from(e.clone())).collect(),
        key,
        emit,
    })
}

/// `["random-bytes", n, { key?, emit? }?]`.
pub(super) fn parse_random_bytes(args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let trailing = args.get(1).map(|options| Value::from(options.clone()));
    let key = trailing
        .as_ref()
        .and_then(|o| o.as_object())
        .and_then(|o| o.get("key"))
        .cloned();
    let emit = trailing.as_ref().and_then(parse_emit_config);
    Some(Effect::RandomBytes {
        bytes: Value::from(args[0].clone()),
        key,
        emit,
    })
}

/// `["credential-status" | "credential-forget", credential, { emit? }?]`.
pub(super) fn parse_credential(args: &[SExpression], status: bool) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let credential = Value::from(args[0].clone());
    let emit = args
        .get(1)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    Some(if status {
        Effect::CredentialStatus { credential, emit }
    } else {
        Effect::CredentialForget { credential, emit }
    })
}

/// `["credential-<verb>", request, { emit? }?]`.
pub(super) fn parse_credential_admin(head: &str, args: &[SExpression]) -> Option<Effect> {
    let op = crate::credential_admin::CredentialAdminOp::from_name(head)?;
    let first = usize::from(op.takes_request());
    if !(first..=first + 1).contains(&args.len()) {
        return None;
    }
    let emit = args
        .get(first)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    let request = if op.takes_request() {
        Value::from(args[0].clone())
    } else {
        Value::Null
    };
    Some(Effect::CredentialAdmin { op, request, emit })
}

/// `["access-check", request, { emit? }?]`.
pub(super) fn parse_access_check(args: &[SExpression]) -> Option<Effect> {
    if !(1..=2).contains(&args.len()) {
        return None;
    }
    let emit = args
        .get(1)
        .and_then(|options| parse_emit_config(&Value::from(options.clone())));
    Some(Effect::AccessCheck {
        request: Value::from(args[0].clone()),
        emit,
    })
}

pub(super) fn parse_cancel_call(args: &[SExpression]) -> Option<Effect> {
    let key = Value::from(args.first()?.clone());
    matches!(key, Value::String(_) | Value::Array(_)).then_some(Effect::CancelCall { key })
}

pub(super) fn parse_render_ui(args: &[SExpression]) -> Option<Effect> {
    if args.len() < 2 {
        return None;
    }
    let slot = args[0].as_str()?.to_string();
    let content = Value::from(args[1].clone());
    Some(Effect::RenderUi { slot, content })
}

pub(super) fn parse_navigate(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let path = args[0].as_str()?.to_string();
    let params = args.get(1).and_then(|e| {
        if let Value::Object(obj) = Value::from(e.clone()) {
            Some(obj.into_inner())
        } else {
            None
        }
    });
    let options = args.get(2).and_then(|e| {
        if let Value::Object(obj) = Value::from(e.clone()) {
            let crumb = obj
                .get("crumb")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(crate::client_effect::NavigateOptions { crumb })
        } else {
            None
        }
    });
    Some(Effect::Navigate {
        path,
        params,
        options,
    })
}

pub(super) fn parse_spawn(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let entity_type = args[0].as_str()?.to_string();
    let props = args
        .get(1)
        .map(|e| Value::from(e.clone()))
        .unwrap_or(Value::Null);
    Some(Effect::Spawn { entity_type, props })
}

pub(super) fn parse_despawn(args: &[SExpression]) -> Option<Effect> {
    if args.is_empty() {
        return None;
    }
    let entity_id = args[0].as_str()?.to_string();
    Some(Effect::Despawn { entity_id })
}

pub(super) fn parse_log(args: &[SExpression]) -> Option<Effect> {
    let message = args.first()?.as_str()?.to_string();
    let level = match args.get(1) {
        None => LogLevel::Info,
        Some(level) => match level.as_str()? {
            "debug" => LogLevel::Debug,
            "info" => LogLevel::Info,
            "warn" => LogLevel::Warn,
            "error" => LogLevel::Error,
            _ => return None,
        },
    };
    Some(Effect::Log {
        level,
        message,
        data: None,
    })
}
