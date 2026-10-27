//! Expression lowering: an S-expression to an `OirExpr`. Stateless (it reads
//! no schema, only the installed operator table), so it lives here and every
//! consumer (the compiler's resolver, the shells) shares the one lowering.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::http_wire::HttpMethod;
use orbital_core_base::ir::effects::{OirEffect, OirEmitConfig};
use orbital_core_base::ir::expr::{OirBinaryOp, OirBinding, OirBindingRoot, OirExpr};
use orbital_core_base::ir::types::{OirPattern, OirValue};
use orbital_core_base::schema::sexpr::SExpression;
use thiserror::Error;
use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc};

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

orbital_core_base::kernel_local! {
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

type Lowered = Result<OirExpr, LowerError>;

enum LowerChild {
    Input(usize),
    Owned(SExpression),
}

struct InputLocation {
    parent: Option<usize>,
    index: usize,
}

struct LowerInput<'a> {
    expression: Rc<Cow<'a, SExpression>>,
    locations: Vec<InputLocation>,
    positions: HashMap<usize, usize>,
}

impl<'a> LowerInput<'a> {
    fn new(expression: Cow<'a, SExpression>) -> Rc<Self> {
        let expression = Rc::new(expression);
        let mut locations = vec![InputLocation { parent: None, index: 0 }];
        let mut positions = HashMap::new();
        let mut pending = vec![(expression.as_ref().as_ref(), 0)];
        while let Some((node, location)) = pending.pop() {
            positions.insert(node as *const SExpression as usize, location);
            let children: Box<dyn Iterator<Item = &SExpression> + '_> = match node {
                SExpression::Array(items) => Box::new(items.iter()),
                SExpression::Object(fields) => Box::new(fields.values()),
                _ => continue,
            };
            for (index, child) in children.enumerate() {
                let child_location = locations.len();
                locations.push(InputLocation { parent: Some(location), index });
                pending.push((child, child_location));
            }
        }
        Rc::new(Self { expression, locations, positions })
    }

    fn expression(&self, mut location: usize) -> &SExpression {
        let mut path = Vec::new();
        while let Some(parent) = self.locations[location].parent {
            path.push(self.locations[location].index);
            location = parent;
        }
        let mut node = self.expression.as_ref().as_ref();
        for index in path.into_iter().rev() {
            node = match node {
                SExpression::Array(items) => &items[index],
                SExpression::Object(fields) => fields.get_index(index).expect("indexed field").1,
                _ => unreachable!("indexed expression container"),
            };
        }
        node
    }
}

struct NodeLowerer {
    children: RefCell<Vec<LowerChild>>,
    answers: Option<RefCell<std::vec::IntoIter<Lowered>>>,
}

impl ExprLowerer {
    pub fn resolve_sexpr(&self, sexpr: &SExpression) -> Lowered {
        enum Work<'a> {
            Visit(Rc<LowerInput<'a>>, usize),
            Assemble(Rc<LowerInput<'a>>, usize, usize),
        }
        let mut work = vec![Work::Visit(LowerInput::new(Cow::Borrowed(sexpr)), 0)];
        let mut results = Vec::new();
        while let Some(next) = work.pop() {
            match next {
                Work::Visit(input, location) => {
                    let discover = NodeLowerer { children: RefCell::new(Vec::new()), answers: None };
                    // The canonical rules request only semantic children; syntax and ignored operands stay untouched.
                    let _ = discover.resolve_node(input.expression(location));
                    let children = discover.children.into_inner();
                    work.push(Work::Assemble(Rc::clone(&input), location, children.len()));
                    for child in children.into_iter().rev() {
                        match child {
                            LowerChild::Input(address) => {
                                let location = input.positions[&address];
                                work.push(Work::Visit(Rc::clone(&input), location));
                            }
                            LowerChild::Owned(expression) => {
                                work.push(Work::Visit(LowerInput::new(Cow::Owned(expression)), 0));
                            }
                        }
                    }
                }
                Work::Assemble(input, location, count) => {
                    let children = results.split_off(results.len() - count);
                    let assemble = NodeLowerer {
                        children: RefCell::new(Vec::new()),
                        answers: Some(RefCell::new(children.into_iter())),
                    };
                    results.push(assemble.resolve_node(input.expression(location)));
                }
            }
        }
        results.pop().expect("root expression result")
    }
}

impl NodeLowerer {
    fn resolve_sexpr(&self, sexpr: &SExpression) -> Lowered {
        if let Some(answers) = &self.answers {
            return answers.borrow_mut().next().expect("requested child result");
        }
        self.children.borrow_mut().push(LowerChild::Input(sexpr as *const SExpression as usize));
        Ok(OirExpr::Null)
    }

    fn resolve_owned(&self, sexpr: SExpression) -> Lowered {
        if let Some(answers) = &self.answers {
            return answers.borrow_mut().next().expect("requested owned child result");
        }
        self.children.borrow_mut().push(LowerChild::Owned(sexpr));
        Ok(OirExpr::Null)
    }

    fn sexpr_to_value(&self, sexpr: &SExpression) -> OirValue {
        ExprLowerer.sexpr_to_value(sexpr)
    }

    fn resolve_node(&self, sexpr: &SExpression) -> Lowered {
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
                let mut fields = orbital_core_base::ir::expr::OirFields::default();
                for (key, value) in obj {
                    fields.insert(key.clone(), self.resolve_sexpr(value)?);
                }
                Ok(OirExpr::object(fields))
            }
        }
    }

    /// A substrate operator lowered as an `OirEffect::Custom`.
    fn custom_effect_call(&self, op_name: &str, args: &[SExpression]) -> Result<OirExpr, LowerError> {
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
        let mut args_map = orbital_core_base::ir::expr::OirFields::default();
        for (i, arg) in resolved_args.into_iter().enumerate() {
            args_map.insert(format!("arg{}", i), arg);
        }
        Ok(OirExpr::effect(OirEffect::Custom {
            name: op_name.to_string(),
            args: args_map,
            emit: emit_cfg,
        }))
    }

    fn resolve_sexpr_call(&self, op: &str, args: &[SExpression]) -> Result<OirExpr, LowerError> {
        if op == "typed" && args.len() == 2 && !KEEP_TYPED.with(std::cell::Cell::get) {
            return self.resolve_sexpr(&args[0]);
        }
        if op == "proven" && args.len() >= 2 {
            return self.resolve_owned(SExpression::Array(args[1..].to_vec()));
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
            "+" | "add" => self.variadic_binary_op(args, OirExpr::add),
            "-" | "sub" => self.binary_op(args, OirExpr::sub),
            "*" | "mul" => self.variadic_binary_op(args, |l, r| OirExpr::BinaryOp {
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
            "call-service" => self.resolve_call_service_effect(args),
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
            "access-check" => self.resolve_access_check_effect(args),
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
                        key = Some(self.resolve_owned(k)?);
                    }
                }
                let request = self.resolve_owned(request)?;
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
                let mut args_map = orbital_core_base::ir::expr::OirFields::default();
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
                self.custom_effect_call(op_name, args)
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
                        SExpression::String(s) => params.push(s.to_string()),
                        SExpression::Array(ps) => {
                            for pp in ps {
                                if let SExpression::String(s) = pp {
                                    params.push(s.to_string());
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
}

impl ExprLowerer {
    pub fn sexpr_to_value(&self, sexpr: &SExpression) -> OirValue {
        match sexpr {
            SExpression::Null => OirValue::Null,
            SExpression::Bool(b) => OirValue::Bool(*b),
            SExpression::Number(n) => OirValue::Number(*n),
            SExpression::String(s) => OirValue::String(s.clone().into_inner()),
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

mod data_effects;
mod ui_effects;
mod io_effects;
mod refs;
mod options;

#[cfg(test)]
mod tests;
