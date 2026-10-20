//! Expression lowering: an S-expression to an `OirExpr`. Stateless (it reads
//! no schema, only the installed operator table), so it lives here and every
//! consumer (the compiler's resolver, the shells) shares the one lowering.

use orbital_core_base::http_wire::HttpMethod;
use orbital_core_base::ir::effects::{OirEffect, OirEmitConfig};
use orbital_core_base::ir::expr::{OirBinaryOp, OirBinding, OirBindingRoot, OirExpr};
use orbital_core_base::ir::types::{OirPattern, OirValue};
use orbital_core_base::schema::sexpr::SExpression;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum LowerError {
    #[error("Failed to resolve S-expression: {0}")]
    InvalidSExpr(String),
}

/// Lower `sexpr` through the one canonical lowering. `Err` for shapes it
/// rejects (a caller may fall back to literal handling).
pub fn lower_sexpr(sexpr: &SExpression) -> Result<OirExpr, LowerError> {
    ExprLowerer.resolve_sexpr(sexpr)
}

thread_local! {
    static KEEP_TYPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// [`lower_sexpr`] keeping each `typed` form as a call, for the compiler pass that records declared argument types
/// (`orbital-compiler` `typed_args`). Every other lowering reads `["typed", value, type]` as its value.
pub fn lower_sexpr_keeping_typed(sexpr: &SExpression) -> Result<OirExpr, LowerError> {
    KEEP_TYPED.with(|keep| keep.set(true));
    let lowered = ExprLowerer.resolve_sexpr(sexpr);
    KEEP_TYPED.with(|keep| keep.set(false));
    lowered
}

/// The existing lowering decision that distinguishes a registered call from literal array data.
pub fn recognized_call_head(sexpr: &SExpression) -> Option<&str> {
    let op = sexpr.operator()?;
    (orbital_core_base::language::registry::is_known_operator(op)
        || crate::evaluator::operators::get_operator(op).is_some()
        || orbital_core_base::effect_types::is_known_effect_name(op))
    .then_some(op)
}

/// A `(list …)` literal marker, stripped; any other array unchanged.
pub fn strip_literal_list_marker(arr: &[SExpression]) -> &[SExpression] {
    match arr.first() {
        Some(SExpression::String(s)) if s == "list" => &arr[1..],
        _ => arr,
    }
}

/// A callback written as a raw body (`(array/filter xs (== @item.v 1))`, no `(fn …)`) is
/// evaluated with the operator's raw-callback names bound (orbital-core
/// `raw_callback_params`). Lower it to that lambda so every backend emits a real function.
/// A raw value binding none of those names is evaluated once and applied if it yields a
/// function, so it stays a value.
fn lower_raw_callback(op: &str, args: &[SExpression], resolved: &mut [OirExpr]) {
    let Some(params) = crate::evaluator::operators::raw_callback_params(op) else {
        return;
    };
    let Some(pos) =
        orbital_core_base::language::registry::get_operator(op).and_then(|m| m.lambda_arg_position)
    else {
        return;
    };
    let pos = pos as usize;
    let (Some(raw), Some(body)) = (args.get(pos), resolved.get_mut(pos)) else {
        return;
    };
    let is_lambda_form = matches!(raw, SExpression::Array(form)
        if matches!(form.first(), Some(SExpression::String(head)) if head == "fn" || head == "lambda"));
    let binds_element = body
        .collect_bindings()
        .iter()
        .any(|b| matches!(&b.root, OirBindingRoot::Local(name) if params.contains(&name.as_str())));
    if is_lambda_form || !binds_element {
        return;
    }
    let raw_body = std::mem::replace(body, OirExpr::Null);
    *body = OirExpr::lambda(params.iter().map(|p| p.to_string()).collect(), raw_body);
}

pub struct ExprLowerer;

impl ExprLowerer {
    pub fn resolve_sexpr(&self, sexpr: &SExpression) -> Result<OirExpr, LowerError> {
        match sexpr {
            SExpression::Null => Ok(OirExpr::null()),
            SExpression::Bool(b) => Ok(OirExpr::bool(*b)),
            SExpression::Number(n) => Ok(OirExpr::number(*n)),
            SExpression::String(s) => {
                // Check if it's a binding
                if s.starts_with('@') {
                    if let Some(binding) = OirBinding::parse(s) {
                        return Ok(OirExpr::binding(binding));
                    }
                }
                Ok(OirExpr::string(s))
            }
            SExpression::Array(arr) => {
                // Disambiguate Lisp call form `[op, arg1, arg2, …]` from a
                // string-array literal `["a", "b", "c"]`. Both look identical
                // in JSON. The OIR resolver matches a finite set of operator
                // names (its own match arms in resolve_sexpr_call) AND the
                // evaluator's runtime operator registry handles a wider
                // namespaced set (str/concat, math/map, agent/*, os/*, …).
                //
                // Use `crate::evaluator::get_operator` as the single
                // source of truth. If the head string is recognised as an
                // operator there OR matched by resolve_sexpr_call's own
                // arms, treat as a call. Otherwise it's a string-array
                // literal.
                //
                // Prevents G44 (`["positive", "negative", "neutral"]` →
                // `positive("negative", "neutral")` and TS2304) without
                // misclassifying `str/concat`, `agent/generate`, etc.
                if let Some(op) = recognized_call_head(sexpr) {
                    // Discriminate against the CANONICAL operator registry
                    // (`@almadar/std` canonical-operators.json, baked via the sync
                    // tool) — the single source of record — first, then the
                    // evaluator/effect registries for aliases not yet declared
                    // canonical. Using only the hand-maintained evaluator registry
                    // here silently DEMOTED real canonical ops it happened to miss
                    // (`array/count`, `time/relative`) to string-array literals
                    // (G-LOLO-OP-ARRAY-DEMOTION). Corpus-verified: every
                    // canonical-headed array is a call; data lists never use a
                    // canonical head.
                    return self.resolve_sexpr_call(op, &arr[1..]);
                }

                // Regular array literal
                let items = arr
                    .iter()
                    .map(|item| self.resolve_sexpr(item))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(OirExpr::array(items))
            }
            SExpression::Object(obj) => {
                let mut fields = orbital_core_base::ir::expr::OirFields::new();
                for (key, value) in obj {
                    fields.insert(key.clone(), self.resolve_sexpr(value)?);
                }
                Ok(OirExpr::object(fields))
            }
        }
    }

    fn resolve_sexpr_call(&self, op: &str, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if op == "typed" && args.len() == 2 && !KEEP_TYPED.with(std::cell::Cell::get) {
            return self.resolve_sexpr(&args[0]);
        }
        // Enforce canonical maxArity (canonical-operators.json). Variadic ops
        // (maxArity null) skip. Catches the silent-drop bug in binary_op /
        // unary_op, which keep only the first 1–2 args and discard the rest
        // (C-EMIT-MAX-ARITY-SILENT-DROP). Aliases absent from the canonical
        // registry (`add`, `&&`, …) return None here and are still covered by
        // the per-arm binary/unary minimum checks below.
        if let Some(meta) = orbital_core_base::language::registry::get_operator(op) {
            // Effect operators are async EFFECTS — like fetch/persist
            // (EFFECT_OPERATORS) they carry a trailing `{ emit }` envelope
            // that is not part of the operator's logical arity and have their
            // own validation, so they skip the static value-operator
            // max-arity check. Covers substrate ops (llm/*, workspace/*, …)
            // and every registry op flagged `has_side_effects` (os/*,
            // browser/*, set, …). Consistent with `get_arity_rule` (sexpr
            // validator), which returns None for them (C-EMIT-SUBSTRATE).
            if !orbital_core_base::ir::effects::is_substrate_operator(op) && !meta.has_side_effects
            {
                if let Some(max) = meta.max_arity {
                    if (args.len() as u32) > max {
                        return Err(LowerError::InvalidSExpr(format!(
                            "Operator '{op}' takes at most {max} argument(s), got {}",
                            args.len()
                        )));
                    }
                }
            }
        }

        match op {
            // Comparison operators
            "=" | "==" | "eq" => self.binary_op(args, OirExpr::eq),
            "!=" | "ne" => self.binary_op(args, OirExpr::ne),
            "<" | "lt" => self.binary_op(args, OirExpr::lt),
            ">" | "gt" => self.binary_op(args, OirExpr::gt),
            "<=" | "le" => self.binary_op(args, |l, r| OirExpr::BinaryOp {
                op: OirBinaryOp::Le,
                left: Box::new(l),
                right: Box::new(r),
            }),
            ">=" | "ge" => self.binary_op(args, |l, r| OirExpr::BinaryOp {
                op: OirBinaryOp::Ge,
                left: Box::new(l),
                right: Box::new(r),
            }),

            // Logical operators
            "and" | "&&" => self.variadic_binary_op(args, OirExpr::and),
            "or" | "||" => self.variadic_binary_op(args, OirExpr::or),
            "not" | "!" => self.unary_op(args, OirExpr::not),

            // Arithmetic operators
            "+" | "add" => self.binary_op(args, OirExpr::add),
            "-" | "sub" => self.binary_op(args, OirExpr::sub),
            "*" | "mul" => self.binary_op(args, |l, r| OirExpr::BinaryOp {
                op: OirBinaryOp::Mul,
                left: Box::new(l),
                right: Box::new(r),
            }),
            "/" | "div" => self.binary_op(args, |l, r| OirExpr::BinaryOp {
                op: OirBinaryOp::Div,
                left: Box::new(l),
                right: Box::new(r),
            }),
            "%" | "mod" => self.binary_op(args, |l, r| OirExpr::BinaryOp {
                op: OirBinaryOp::Mod,
                left: Box::new(l),
                right: Box::new(r),
            }),

            // Conditional
            "if" => {
                if args.len() >= 3 {
                    let cond = self.resolve_sexpr(&args[0])?;
                    let then_branch = self.resolve_sexpr(&args[1])?;
                    let else_branch = self.resolve_sexpr(&args[2])?;
                    Ok(OirExpr::if_then_else(cond, then_branch, else_branch))
                } else {
                    Err(LowerError::InvalidSExpr(
                        "if requires 3 arguments".to_string(),
                    ))
                }
            }

            // Effects
            "emit" => self.resolve_emit_effect(args),
            "persist" => self.resolve_persist_effect(args),
            "fetch" => self.resolve_fetch_effect(args),
            "fetch-stream" => self.resolve_fetch_stream_effect(args),
            "send-server" => self.resolve_send_server_effect(args),
            "set" => self.resolve_set_effect(args),
            "navigate" => self.resolve_navigate_effect(args),
            "navigate-back" => self.resolve_navigate_back_effect(args),
            "render-ui" => self.resolve_render_ui_effect(args),
            "notify" => self.resolve_notify_effect(args),
            "log" => self.resolve_log_effect(args),
            "call-service" | "call_service" => self.resolve_call_service_effect(args),
            "http" => self.resolve_http_effect(args),
            "redis" => self.resolve_redis_effect(args),
            "postgres" => self.resolve_postgres_effect(args),
            "cancel-call" => self.resolve_cancel_call_effect(args),
            "random-bytes" => self.resolve_random_bytes_effect(args),
            "credential-status" => self.resolve_credential_effect(args, true),
            "credential-forget" => self.resolve_credential_effect(args, false),
            admin if orbital_core_base::credential_admin::CredentialAdminOp::from_name(admin).is_some() => {
                self.resolve_credential_admin_effect(admin, args)
            }
            "oauth-authorize" => self.resolve_oauth_authorize_effect(args),
            "oauth-connection-status" => self.resolve_oauth_connection_effect(args, true),
            "oauth-connection-forget" => self.resolve_oauth_connection_effect(args, false),

            // Resource operators
            "ref" => self.resolve_ref_effect(args),
            "deref" => self.resolve_deref_effect(args),
            "swap" => self.resolve_swap_effect(args),
            "watch" => self.resolve_watch_effect(args),
            "atomic" => self.resolve_atomic_effect(args),

            // OS trigger effects (server-side only)
            //
            // Each may accept a trailing options object carrying `emit:`:
            //   ["os/watch-http", urlPattern, method?, { emit: {...} }]
            //   ["os/watch-cron", expr,               { emit: {...} }]
            //
            // The options object is detected by being an `Object` *and*
            // carrying an `emit` key, so existing 2-arg forms where arg[1]
            // is a non-emit options map keep working unchanged.
            "os/watch-files" => {
                let glob = self.resolve_sexpr(&args[0])?;
                let (options, emit_cfg) = self.split_watch_trailing(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::OsWatchFiles {
                    glob,
                    options,
                    emit: emit_cfg,
                }))
            }
            "os/watch-process" => {
                let name = self.resolve_sexpr(&args[0])?;
                let (subcommand, emit_cfg) = self.split_watch_trailing(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::OsWatchProcess {
                    name,
                    subcommand,
                    emit: emit_cfg,
                }))
            }
            "os/watch-port" => {
                let port = self.resolve_sexpr(&args[0])?;
                let (protocol, emit_cfg) = self.split_watch_trailing(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::OsWatchPort {
                    port,
                    protocol,
                    emit: emit_cfg,
                }))
            }
            "os/watch-http" => {
                let url_pattern = self.resolve_sexpr(&args[0])?;
                let (method, emit_cfg) = self.split_watch_trailing(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::OsWatchHttp {
                    url_pattern,
                    method,
                    emit: emit_cfg,
                }))
            }
            "os/watch-cron" => {
                let expression = self.resolve_sexpr(&args[0])?;
                let emit_cfg = self.extract_watch_only_emit(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::OsWatchCron {
                    expression,
                    emit: emit_cfg,
                }))
            }
            "os/watch-signal" => {
                let signal = self.resolve_sexpr(&args[0])?;
                let emit_cfg = self.extract_watch_only_emit(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::OsWatchSignal {
                    signal,
                    emit: emit_cfg,
                }))
            }
            "os/watch-env" => {
                let variable = self.resolve_sexpr(&args[0])?;
                let emit_cfg = self.extract_watch_only_emit(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::OsWatchEnv {
                    variable,
                    emit: emit_cfg,
                }))
            }

            // Browser device effects (client host path)
            //
            // User-initiated, async device APIs. Each accepts a trailing
            // `{ emit: { success, failure } }` envelope (mirroring os/watch-*):
            //   ["browser/open-file-picker", options?, { emit? }]
            //   ["browser/clipboard-read",            { emit? }]
            //   ["browser/clipboard-write", text,     { emit? }]
            //   ["browser/geolocation-current", options?, { emit? }]
            "browser/open-file-picker" => {
                let (options, emit_cfg) = self.split_watch_trailing(args)?;
                Ok(OirExpr::effect(OirEffect::BrowserOpenFilePicker {
                    options,
                    emit: emit_cfg,
                }))
            }
            "browser/clipboard-read" => {
                let emit_cfg = self.extract_watch_only_emit(args)?;
                Ok(OirExpr::effect(OirEffect::BrowserClipboardRead {
                    emit: emit_cfg,
                }))
            }
            "browser/clipboard-write" => {
                if args.is_empty() {
                    return Err(LowerError::InvalidSExpr(
                        "browser/clipboard-write requires text".to_string(),
                    ));
                }
                let text = self.resolve_sexpr(&args[0])?;
                let emit_cfg = self.extract_watch_only_emit(&args[1..])?;
                Ok(OirExpr::effect(OirEffect::BrowserClipboardWrite {
                    text,
                    emit: emit_cfg,
                }))
            }
            "browser/geolocation-current" => {
                let (options, emit_cfg) = self.split_watch_trailing(args)?;
                Ok(OirExpr::effect(OirEffect::BrowserGeolocationCurrent {
                    options,
                    emit: emit_cfg,
                }))
            }
            "browser/page-facts" => {
                let Some(SExpression::Array(words)) = args.first() else {
                    return Err(LowerError::InvalidSExpr(
                        "browser/page-facts takes a list of page facts (hostname, referrer, screenWidth)".to_string(),
                    ));
                };
                let mut fields = Vec::with_capacity(words.len());
                for word in words {
                    let word = self.sexpr_as_str(word, "(browser/page-facts …) a page fact")?;
                    let field = orbital_core_base::ir::effects::PageFactField::parse(&word).ok_or_else(|| {
                        LowerError::InvalidSExpr(format!(
                            "(browser/page-facts …) \"{word}\" is not one of hostname referrer screenWidth"
                        ))
                    })?;
                    fields.push(field);
                }
                let emit_cfg = self.extract_watch_only_emit(&args[1..])?;
                let key = match args.get(1) {
                    Some(SExpression::Object(options)) => match options.get("key") {
                        None | Some(SExpression::Null) => None,
                        Some(k) => Some(self.resolve_sexpr(k)?),
                    },
                    _ => None,
                };
                Ok(OirExpr::effect(OirEffect::BrowserPageFacts {
                    fields,
                    key,
                    emit: emit_cfg,
                }))
            }
            "browser/beacon" => {
                if !(1..=3).contains(&args.len()) {
                    return Err(LowerError::InvalidSExpr(
                        "browser/beacon takes a URL, then a request and options".to_string(),
                    ));
                }
                let url = self.resolve_sexpr(&args[0])?;
                let request_expr = args.get(1).ok_or_else(|| {
                    LowerError::InvalidSExpr(
                        "browser/beacon requires a request carrying `text`".to_string(),
                    )
                })?;
                let mut request = request_expr.clone();
                let mut key = None;
                if let SExpression::Object(fields) = &mut request {
                    if let Some(k) = fields.shift_remove("key") {
                        key = Some(self.resolve_sexpr(&k)?);
                    }
                }
                let request = self.resolve_sexpr(&request)?;
                let emit_cfg = match args.get(2) {
                    Some(SExpression::Object(options)) => {
                        if let Some(k) = options.get("key") {
                            key = Some(self.resolve_sexpr(k)?);
                        }
                        self.extract_emit_config(options)?
                    }
                    _ => None,
                };
                Ok(OirExpr::effect(OirEffect::BrowserBeacon {
                    url,
                    request,
                    key,
                    emit: emit_cfg,
                }))
            }
            "browser/prompt" => {
                if !(2..=3).contains(&args.len()) {
                    return Err(LowerError::InvalidSExpr(
                        "browser/prompt takes the text, the response schema text, and optional options".to_string(),
                    ));
                }
                let text = self.resolve_sexpr(&args[0])?;
                let schema = self.resolve_sexpr(&args[1])?;
                let (key, emit) = self.resolve_keyed_options(args.get(2))?;
                Ok(OirExpr::effect(OirEffect::BrowserPrompt {
                    text,
                    schema,
                    key,
                    emit,
                }))
            }
            "browser/embed" => {
                if !(1..=2).contains(&args.len()) {
                    return Err(LowerError::InvalidSExpr(
                        "browser/embed takes a list of texts and optional options".to_string(),
                    ));
                }
                let texts = self.resolve_sexpr(&args[0])?;
                let (key, emit) = self.resolve_keyed_options(args.get(1))?;
                Ok(OirExpr::effect(OirEffect::BrowserEmbed {
                    texts,
                    key,
                    emit,
                }))
            }
            op_name if orbital_core_base::tab::TabOp::from_name(op_name).is_some() => {
                let Some(op) = orbital_core_base::tab::TabOp::from_name(op_name) else {
                    return Err(LowerError::InvalidSExpr(format!("{op_name} is not a tab operation")));
                };
                if !(1..=2).contains(&args.len()) {
                    return Err(LowerError::InvalidSExpr(format!(
                        "{op_name} takes a request object and optional options"
                    )));
                }
                let request = self.resolve_sexpr(&args[0])?;
                let (key, emit) = self.resolve_keyed_options(args.get(1))?;
                Ok(OirExpr::effect(OirEffect::BrowserTab {
                    op,
                    request,
                    key,
                    emit,
                }))
            }
            "browser/push-subscribe" => {
                let emit_cfg = self.extract_watch_only_emit(args)?;
                Ok(OirExpr::effect(OirEffect::BrowserPushSubscribe {
                    emit: emit_cfg,
                }))
            }

            op_name if orbital_core_base::os::OsOp::from_name(op_name).is_some() => {
                self.resolve_os_effect(op_name, args)
            }

            // Agent operators — lowered as Custom effects so backend can codegen TypeScript
            op_name if op_name.starts_with("agent/") => {
                let resolved_args: Vec<OirExpr> = args
                    .iter()
                    .map(|a| self.resolve_sexpr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut args_map = orbital_core_base::ir::expr::OirFields::new();
                for (i, arg) in resolved_args.into_iter().enumerate() {
                    args_map.insert(format!("arg{}", i), arg);
                }
                Ok(OirExpr::effect(OirEffect::Custom {
                    name: op_name.to_string(),
                    args: args_map,
                    emit: None,
                }))
            }

            // Substrate operators — same pattern as agent/*: lowered as
            // Custom effects so the codegen backend can emit TypeScript
            // service-accessor calls. Without this routing, these would
            // fall through to `OirExpr::Call` and produce invalid TS
            // (bare `llm/generate(...)` with a `/` in the function name).
            op_name if orbital_core_base::ir::effects::is_substrate_operator(op_name) => {
                // A trailing `{ emit: { success, failure } }` options object
                // (mirroring fetch/persist) carries the completion events.
                // The op fires `getSubstrateService().invoke(name, args)` and
                // dispatches the success/failure event on completion — the
                // `.lolo` async model. Without it the op is fire-and-forget.
                // Only treated as options when the object has an `emit` key,
                // so a plain object argument is still passed positionally
                // (C-EMIT-SUBSTRATE-OPS-MARKER).
                let (positional, emit_cfg) = match args.last() {
                    Some(SExpression::Object(obj)) if obj.contains_key("emit") => {
                        (&args[..args.len() - 1], self.extract_emit_config(obj)?)
                    }
                    _ => (args, None),
                };
                let resolved_args: Vec<OirExpr> = positional
                    .iter()
                    .map(|a| self.resolve_sexpr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut args_map = orbital_core_base::ir::expr::OirFields::new();
                for (i, arg) in resolved_args.into_iter().enumerate() {
                    args_map.insert(format!("arg{}", i), arg);
                }
                Ok(OirExpr::effect(OirEffect::Custom {
                    name: op_name.to_string(),
                    args: args_map,
                    emit: emit_cfg,
                }))
            }

            // Function call (also covers namespaced operators like
            // `str/concat`, `crypto/hash`, … that downstream codegen
            // dispatches via OirExpr::Call). String-array literals are
            // diverted at the resolve_sexpr Array case BEFORE reaching
            // here, so this arm only fires for genuine call forms.
            // Lambda — the single shared representation (OirExpr::Lambda) every
            // backend consumes, replacing per-site `Call{function:"fn"}` parsing.
            // Post-normalization the shape is `[params, body]` (params = a string
            // for a single param or an array for the grouped form); the flat
            // `[p1, …, pN, body]` form is handled defensively.
            // A pair's name is a binding, not a call, even when it spells an operator.
            "let" => {
                let Some(SExpression::Array(pairs)) = args.first() else {
                    return Err(LowerError::InvalidSExpr(
                        "let requires a bindings list and a body".to_string(),
                    ));
                };
                let mut bindings = Vec::with_capacity(pairs.len());
                for pair in pairs {
                    match pair {
                        SExpression::Array(kv) if kv.len() == 2 => {
                            let SExpression::String(name) = &kv[0] else {
                                return Err(LowerError::InvalidSExpr(format!(
                                    "let binding name must be a string, got {:?}",
                                    kv[0]
                                )));
                            };
                            bindings.push(OirExpr::array(vec![
                                OirExpr::string(name),
                                self.resolve_sexpr(&kv[1])?,
                            ]));
                        }
                        other => {
                            return Err(LowerError::InvalidSExpr(format!(
                                "let binding must be [name, value], got {other:?}"
                            )))
                        }
                    }
                }
                let mut resolved = vec![OirExpr::array(bindings)];
                for body in &args[1..] {
                    resolved.push(self.resolve_sexpr(body)?);
                }
                Ok(OirExpr::call("let", resolved))
            }

            "fn" | "lambda" => {
                if args.len() < 2 {
                    return Err(LowerError::InvalidSExpr(format!(
                        "{op} lambda requires parameters and a body"
                    )));
                }
                let (param_slice, body_sexpr) = if args.len() == 2 {
                    (std::slice::from_ref(&args[0]), &args[1])
                } else {
                    (&args[..args.len() - 1], &args[args.len() - 1])
                };
                let mut params = Vec::new();
                for p in param_slice {
                    match p {
                        SExpression::String(s) => params.push(s.clone()),
                        SExpression::Array(ps) => {
                            for pp in ps {
                                if let SExpression::String(s) = pp {
                                    params.push(s.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let body = self.resolve_sexpr(body_sexpr)?;
                Ok(OirExpr::lambda(params, body))
            }

            _ => {
                let mut resolved_args = args
                    .iter()
                    .map(|a| self.resolve_sexpr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                lower_raw_callback(op, args, &mut resolved_args);
                Ok(OirExpr::call(op, resolved_args))
            }
        }
    }

    /// Left-fold a genuinely associative operator over ALL its arguments:
    /// `(or A B C)` → `Or(Or(A, B), C)`.
    ///
    /// `binary_op` reads only `args[0..2]`. That is correct for a fixed-arity
    /// operator, whose excess args the max-arity check above already rejects —
    /// but `and`/`or` are declared VARIADIC, so they sail past that check and
    /// then lost every argument from the third on, silently. A three-branch
    /// `(or supervisor-clause agent-clause customer-clause)` lowered to just
    /// the first two, so the third branch could never match and the guard
    /// denied a viewer the source plainly allowed.
    fn variadic_binary_op<F>(&self, args: &[SExpression], f: F) -> Result<OirExpr, LowerError>
    where
        F: Fn(OirExpr, OirExpr) -> OirExpr,
    {
        if args.len() < 2 {
            return Err(LowerError::InvalidSExpr(
                "Binary operation requires 2 arguments".to_string(),
            ));
        }
        let mut acc = self.resolve_sexpr(&args[0])?;
        for arg in &args[1..] {
            acc = f(acc, self.resolve_sexpr(arg)?);
        }
        Ok(acc)
    }

    fn binary_op<F>(&self, args: &[SExpression], f: F) -> Result<OirExpr, LowerError>
    where
        F: FnOnce(OirExpr, OirExpr) -> OirExpr,
    {
        if args.len() < 2 {
            return Err(LowerError::InvalidSExpr(
                "Binary operation requires 2 arguments".to_string(),
            ));
        }
        let left = self.resolve_sexpr(&args[0])?;
        let right = self.resolve_sexpr(&args[1])?;
        Ok(f(left, right))
    }

    fn unary_op<F>(&self, args: &[SExpression], f: F) -> Result<OirExpr, LowerError>
    where
        F: FnOnce(OirExpr) -> OirExpr,
    {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "Unary operation requires 1 argument".to_string(),
            ));
        }
        let operand = self.resolve_sexpr(&args[0])?;
        Ok(f(operand))
    }

    // Effect resolution helpers
    fn resolve_emit_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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
    fn resolve_persist_batch_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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
            let mut items = vec![SExpression::String("list".to_string())];
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
        let ops_oir = self.resolve_sexpr(&ops_expr)?;

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

    fn resolve_persist_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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
            OirExpr::object(orbital_core_base::ir::expr::OirFields::new())
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

    fn resolve_fetch_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_fetch_stream_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_send_server_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_set_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_navigate_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_navigate_back_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if !args.is_empty() {
            return Err(LowerError::InvalidSExpr(
                "navigate-back takes no arguments".to_string(),
            ));
        }
        Ok(OirExpr::effect(OirEffect::NavigateBack))
    }

    fn resolve_render_ui_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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
    fn resolve_notify_effect(&self, _args: &[SExpression]) -> Result<OirExpr, LowerError> {
        Err(LowerError::InvalidSExpr(
            "notify is retired sugar for render-ui toast — re-emit the .lolo".to_string(),
        ))
    }

    fn resolve_log_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if args.is_empty() {
            return Err(LowerError::InvalidSExpr("log requires message".to_string()));
        }
        let message = self.resolve_sexpr(&args[0])?;
        Ok(OirExpr::effect(OirEffect::log(
            orbital_core_base::ir::effects::OirLogLevel::Info,
            message,
        )))
    }

    fn resolve_call_service_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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
            OirExpr::object(orbital_core_base::ir::expr::OirFields::new())
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

    fn resolve_http_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_postgres_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["postgres", origin, sql, params, { auth?, user?, database?, tls?, timeoutMs?, key?, emit? }?, { key?, emit? }?]
        if !(3..=5).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "postgres takes an origin, a statement, its params, then optional options".to_string(),
            ));
        }
        let mut options = orbital_core_base::schema::sexpr::SObject::new();
        let mut routes = orbital_core_base::schema::sexpr::SObject::new();
        let mut key = None;
        for trailing in &args[3..] {
            match trailing {
                SExpression::Null => {}
                SExpression::Object(fields) => {
                    for (name, value) in fields {
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
            Some(self.resolve_sexpr(&SExpression::Object(options))?)
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

    fn resolve_redis_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        // Format: ["redis", origin, command, { auth?, user?, db?, key?, emit? }?, { key?, emit? }?]
        if !(2..=4).contains(&args.len()) {
            return Err(LowerError::InvalidSExpr(
                "redis takes an origin, a command, then optional options".to_string(),
            ));
        }
        let mut options = orbital_core_base::schema::sexpr::SObject::new();
        let mut routes = orbital_core_base::schema::sexpr::SObject::new();
        let mut key = None;
        for trailing in &args[2..] {
            match trailing {
                SExpression::Null => {}
                SExpression::Object(fields) => {
                    for (name, value) in fields {
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
            Some(self.resolve_sexpr(&SExpression::Object(options))?)
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
    fn resolve_http_request(&self, request: &SExpression) -> Result<OirExpr, LowerError> {
        let SExpression::Object(fields) = request else {
            return self.resolve_sexpr(request);
        };
        let Some(SExpression::Object(paginate)) = fields.get("paginate") else {
            return self.resolve_sexpr(request);
        };
        let mut held = paginate.clone();
        for key in ["next", "collect"] {
            if let Some(expr) = held.get(key) {
                if orbital_core_base::schema::quote::quote_body_of(expr).is_none() {
                    let quoted = orbital_core_base::schema::quote::quote_expr(expr);
                    held.insert(key.to_string(), quoted);
                }
            }
        }
        let mut fields = fields.clone();
        fields.insert("paginate".to_string(), SExpression::Object(held));
        self.resolve_sexpr(&SExpression::Object(fields))
    }

    /// The `{ emit }` options object a one-argument effect may carry.
    fn resolve_emit_only_options(
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

    fn resolve_os_effect(&self, name: &str, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_random_bytes_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_credential_effect(
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

    fn resolve_credential_admin_effect(&self, head: &str, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_oauth_authorize_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_oauth_connection_effect(
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

    fn resolve_cancel_call_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_ref_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    /// Extract the `emit:` config from an options object, if present.
    ///
    /// The stream keys are spelled `onChange` and `onMessage`, as the runtime routes them. All five keys (with `cancelled`) are optional; the
    /// validator enforces which ones are allowed per operator.
    ///
    /// Returns `None` if no `emit:` key is present, so the caller stores
    /// `emit: None` on the OIR and the field is skipped in serialization.
    fn extract_emit_config(
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
    fn split_watch_trailing(
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
    fn resolve_keyed_options(
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

    fn extract_watch_only_emit(
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

    fn resolve_deref_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_swap_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_watch_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    fn resolve_atomic_effect(&self, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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

    /// `what` names the operator + argument slot (e.g. "(emit …) argument 1 —
    /// the event name") so a resolve-stage failure is actionable instead of a
    /// bare "Expected string" with no context (T-RESOLVE-OPAQUE).
    fn sexpr_as_str(&self, sexpr: &SExpression, what: &str) -> Result<String, LowerError> {
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

    pub fn sexpr_to_value(&self, sexpr: &SExpression) -> OirValue {
        match sexpr {
            SExpression::Null => OirValue::Null,
            SExpression::Bool(b) => OirValue::Bool(*b),
            SExpression::Number(n) => OirValue::Number(*n),
            SExpression::String(s) => OirValue::String(s.clone()),
            SExpression::Array(arr) => {
                OirValue::Array(arr.iter().map(|v| self.sexpr_to_value(v)).collect())
            }
            SExpression::Object(obj) => OirValue::Object(
                obj.iter()
                    .map(|(k, v)| (k.clone(), self.sexpr_to_value(v)))
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(op: &str, args: Vec<SExpression>) -> SExpression {
        let mut items = vec![SExpression::String(op.into())];
        items.extend(args);
        SExpression::Array(items)
    }

    #[test]
    fn an_operator_call_lowers_and_keeps_its_bindings() {
        let lowered = lower_sexpr(&call(
            "+",
            vec![
                SExpression::Number(1.0),
                SExpression::String("@entity.count".into()),
            ],
        ))
        .expect("lowers");
        assert!(lowered
            .collect_bindings()
            .iter()
            .any(|b| b.root == OirBindingRoot::Entity));
    }

    fn fetch_with(options: serde_json::Value) -> OirEffect {
        let expr: SExpression =
            serde_json::from_value(serde_json::json!(["fetch", "Doc", options])).unwrap();
        match lower_sexpr(&expr).expect("lowers") {
            OirExpr::Effect { effect } => *effect,
            other => panic!("not an effect: {other:?}"),
        }
    }

    #[test]
    fn a_fetch_nearest_option_lowers_with_its_bindings_and_without_it_the_field_is_absent() {
        let OirEffect::Fetch { nearest, .. } = fetch_with(serde_json::json!({
            "nearest": { "field": "embedding", "vector": "@entity.embedding", "k": 3 }
        })) else {
            panic!("not a fetch")
        };
        let nearest = nearest.expect("nearest lowered");
        assert!(nearest
            .collect_bindings()
            .iter()
            .any(|b| b.root == OirBindingRoot::Entity));
        // control: no option, no field
        let OirEffect::Fetch { nearest, .. } = fetch_with(serde_json::json!({ "limit": 2 })) else {
            panic!("not a fetch")
        };
        assert!(nearest.is_none());
    }

    #[test]
    fn control_a_literal_lowers_to_itself() {
        assert_eq!(
            lower_sexpr(&SExpression::Bool(true)),
            Ok(OirExpr::bool(true))
        );
    }

    #[test]
    fn a_malformed_call_is_an_invalid_sexpr_error() {
        let error = lower_sexpr(&call("if", vec![SExpression::Bool(true)])).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Failed to resolve S-expression: "),
            "{error}"
        );
    }

    #[test]
    fn the_list_marker_is_stripped_and_nothing_else() {
        let marked = [SExpression::String("list".into()), SExpression::Number(1.0)];
        assert_eq!(strip_literal_list_marker(&marked), &marked[1..]);
        let bare = [SExpression::String("+".into()), SExpression::Number(1.0)];
        assert_eq!(strip_literal_list_marker(&bare), &bare[..]);
    }

    fn redis_call(trailing: Vec<serde_json::Value>) -> SExpression {
        let mut items = vec![serde_json::json!("redis"), serde_json::json!("redis://cache.internal"), serde_json::json!(["GET", "@payload.k"])];
        items.extend(trailing);
        serde_json::from_value(serde_json::Value::Array(items)).expect("s-expression")
    }

    #[test]
    fn a_redis_call_lowers_to_the_redis_effect_with_options_key_and_routes() {
        let lowered = lower_sexpr(&redis_call(vec![
            serde_json::json!({ "auth": "@config.pw", "db": 1 }),
            serde_json::json!({ "key": "@payload.k", "emit": { "success": "GOT", "failure": "FAILED" } }),
        ]))
        .expect("lowers");
        let OirExpr::Effect { effect } = lowered else { panic!("not an effect: {lowered:?}") };
        let OirEffect::Redis { options, key, emit, command, .. } = *effect else { panic!("not redis") };
        assert!(matches!(options, Some(OirExpr::Object { ref fields }) if fields.contains_key("auth") && fields.contains_key("db") && !fields.contains_key("key")));
        assert!(key.is_some());
        let emit = emit.expect("routes");
        assert_eq!((emit.success.as_deref(), emit.failure.as_deref()), (Some("GOT"), Some("FAILED")));
        assert!(command.collect_bindings().iter().any(|b| b.root == OirBindingRoot::Payload));
    }

    #[test]
    fn control_a_redis_call_with_no_trailing_object_has_no_options_key_or_routes() {
        let lowered = lower_sexpr(&redis_call(Vec::new())).expect("lowers");
        let OirExpr::Effect { effect } = lowered else { panic!("not an effect") };
        let OirEffect::Redis { options, key, emit, .. } = *effect else { panic!("not redis") };
        assert!(options.is_none() && key.is_none() && emit.is_none());
    }

    #[test]
    fn a_postgres_call_lowers_with_params_options_key_and_routes_and_a_bare_one_has_none() {
        let call = |trailing: Vec<serde_json::Value>| {
            let mut items = vec![serde_json::json!("postgres"), serde_json::json!("postgres://db.internal"), serde_json::json!("SELECT $1::int"), serde_json::json!([1])];
            items.extend(trailing);
            serde_json::from_value::<SExpression>(serde_json::Value::Array(items)).expect("s-expression")
        };
        let lowered = lower_sexpr(&call(vec![
            serde_json::json!({ "auth": "@config.pw", "database": "main" }),
            serde_json::json!({ "key": "@payload.k", "emit": { "success": "ROWS", "failure": "FAILED" } }),
        ]))
        .expect("lowers");
        let OirExpr::Effect { effect } = lowered else { panic!("not an effect") };
        let OirEffect::Postgres { options, key, emit, params, .. } = *effect else { panic!("not postgres") };
        assert!(matches!(options, Some(OirExpr::Object { ref fields }) if fields.contains_key("auth") && !fields.contains_key("key")));
        assert!(key.is_some() && matches!(params, OirExpr::Array { .. }));
        assert_eq!(emit.expect("routes").success.as_deref(), Some("ROWS"));
        let OirExpr::Effect { effect } = lower_sexpr(&call(Vec::new())).expect("lowers") else { panic!("not an effect") };
        let OirEffect::Postgres { options, key, emit, .. } = *effect else { panic!("not postgres") };
        assert!(options.is_none() && key.is_none() && emit.is_none());
        let short: SExpression = serde_json::from_value(serde_json::json!(["postgres", "postgres://h", "SELECT 1"])).expect("s-expression");
        assert!(lower_sexpr(&short).is_err());
    }

    #[test]
    fn a_redis_call_with_the_wrong_arity_or_a_non_object_trailer_is_invalid() {
        let bare: SExpression = serde_json::from_value(serde_json::json!(["redis", "redis://h"])).expect("s-expression");
        assert!(lower_sexpr(&bare).is_err());
        assert!(lower_sexpr(&redis_call(vec![serde_json::json!("x")])).is_err());
    }

    #[test]
    fn a_browser_tab_effect_lowers_with_its_request_key_and_routes() {
        use orbital_core_base::tab::TabOp;
        let open: SExpression = serde_json::from_value(serde_json::json!([
            "browser/tab-open",
            { "url": "@config.searchUrl", "active": false },
            { "key": "@entity.id", "emit": { "success": "OPENED", "failure": "OPEN_FAILED" } }
        ]))
        .expect("s-expression");
        let OirExpr::Effect { effect } = lower_sexpr(&open).expect("lowers") else { panic!("not an effect") };
        let OirEffect::BrowserTab { op, request, key, emit } = *effect else { panic!("not a tab effect") };
        assert_eq!(op, TabOp::Open);
        assert!(matches!(request, OirExpr::Object { ref fields } if fields.contains_key("url") && fields.contains_key("active")));
        assert!(key.is_some());
        assert_eq!(emit.expect("routes").failure.as_deref(), Some("OPEN_FAILED"));
        for tab_op in TabOp::ALL {
            let bare: SExpression = serde_json::from_value(serde_json::json!([tab_op.name(), {}])).expect("s-expression");
            let OirExpr::Effect { effect } = lower_sexpr(&bare).expect("lowers") else { panic!("not an effect") };
            assert_eq!(effect.kind(), tab_op.name());
        }
    }

    #[test]
    fn control_a_browser_tab_effect_without_a_request_is_invalid() {
        let none: SExpression = serde_json::from_value(serde_json::json!(["browser/tab-list"])).expect("s-expression");
        assert!(lower_sexpr(&none).is_err());
        let many: SExpression = serde_json::from_value(serde_json::json!(["browser/tab-list", {}, {}, {}])).expect("s-expression");
        assert!(lower_sexpr(&many).is_err());
    }

    #[test]
    fn the_retired_oal_family_no_longer_resolves_to_an_operator() {
        use crate::evaluator::operators::get_operator;
        assert!(get_operator("oal/battery-status").is_none());
        assert!(get_operator("oal/cpu-usage").is_none());
        assert!(get_operator("os/path-join").is_some());
    }
}
