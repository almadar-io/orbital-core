//! Binding Context for S-expression evaluation
//!
//! The BindingContext provides variable resolution for S-expressions.
//! It supports bindings like:
//! - `@entity` - current entity data
//! - `@payload` - event payload
//! - `@state` - current state machine state
//! - `@config` - configuration values
//! - `@user` - the authenticated viewer
//! - `@now` - current timestamp
//! - `@currentTheme` - render-resolved `data-theme` key (kernel-populated)
//! - `@pages` - render-resolved app nav entries (kernel-populated)
//!
//! Nested paths are supported: `@entity.address.city`

use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;
use std::collections::HashMap;
use std::sync::Arc;

/// State of one `resolve_value` walk: the binding tokens being expanded (cycle bound) and the first typed
/// argument mismatch an embedded operator call raised.
#[derive(Default)]
struct Walk {
    expanding: Vec<String>,
    fault: Option<EvalError>,
}

/// Binding context for S-expression evaluation
///
/// Provides access to runtime values during expression evaluation.
#[derive(Debug, Clone, Default)]
pub struct BindingContext {
    /// Entity data (`@entity`)
    pub entity: Arc<Value>,
    /// Event payload (`@payload`)
    pub payload: Arc<Value>,
    /// Current state (`@state`)
    pub state: Arc<Value>,
    /// Configuration (`@config`)
    pub config: Arc<Value>,
    /// Current user (`@user`)
    pub user: Arc<Value>,
    /// Render-resolved `@currentTheme` — the host orbital's active `data-theme`
    /// key, set by the kernel from `resolve_theme` (precedence: orbital theme
    /// → app theme → default). `Value::Null` until populated.
    pub current_theme: Arc<Value>,
    /// Render-resolved `@pages` — the app's root nav entries (`NavItem[]`). `Value::Null` until the
    /// kernel sets it; the token then stays unresolved.
    pub pages: Arc<Value>,
    /// The dispatch's one `now` stamp (ms epoch); `None` = unstamped, read the wall clock.
    pub now: Option<f64>,
    /// `@locale`: the active locale code (`Value::Null` until the host sets it).
    pub locale: Arc<Value>,
    /// The active locale's merged catalog (qualified key → message) for `i18n/t`.
    messages: Arc<HashMap<String, String>>,
    /// `@event`: the delivery being processed (`delivery_record_value`, Runtime Spec Clause 5.5).
    pub event: Arc<Value>,
    /// `@prevEvents`: deliveries this trait already received this dispatch.
    pub prev_events: Arc<Vec<Value>>,
    /// `@prevStates`: states this trait left this dispatch.
    pub prev_states: Arc<Vec<String>>,
    /// `@callsitePayload`: the payload of the transition that composed this trait into its embedder
    /// (`Value::Null` until composed).
    pub callsite_payload: Arc<Value>,
    /// `@fromState` / `@toState`: the transition's ends.
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    /// Local bindings (from `let` expressions)
    locals: ValueMap,
    /// Custom bindings for entity types (for `count` queries).
    ///
    /// Shared via `Arc` so cloning a child context (hot path in map/filter/reduce)
    /// is a refcount bump rather than a deep clone of every entity collection.
    entity_stores: Arc<HashMap<String, Vec<Value>>>,
    /// Resolve a whole-string binding that holds no value to an absent prop (see
    /// [`Self::resolve_dropping_unset`]) instead of leaving its token for a later resolver.
    drop_unset: bool,
}

impl BindingContext {
    /// Create a new empty binding context
    pub fn new() -> Self {
        Self::default()
    }

    /// The `now` every guard, effect and time operator in this dispatch reads.
    /// The active locale's message for a qualified catalog key.
    pub fn message(&self, key: &str) -> Option<&str> {
        self.messages.get(key).map(String::as_str)
    }

    pub fn now_millis(&self) -> f64 {
        self.now
            .unwrap_or_else(orbital_core_base::time_util::now_millis_f64)
    }

    /// Create a context with entity data
    pub fn with_entity(entity: Value) -> Self {
        Self {
            entity: Arc::new(entity),
            ..Default::default()
        }
    }

    /// Set the entity value
    pub fn set_entity(&mut self, entity: Value) -> &mut Self {
        self.entity = Arc::new(entity);
        self
    }

    /// Set the payload value
    pub fn set_payload(&mut self, payload: Value) -> &mut Self {
        self.payload = Arc::new(payload);
        self
    }

    /// Set the state value
    pub fn set_state(&mut self, state: Value) -> &mut Self {
        self.state = Arc::new(state);
        self
    }

    /// Set the config value
    pub fn set_config(&mut self, config: Value) -> &mut Self {
        self.config = Arc::new(config);
        self
    }

    /// Set the `@callsitePayload` root.
    pub fn set_callsite_payload(&mut self, payload: Value) -> &mut Self {
        self.callsite_payload = Arc::new(payload);
        self
    }

    /// Set the user value
    pub fn set_user(&mut self, user: Value) -> &mut Self {
        self.user = Arc::new(user);
        self
    }

    /// Set the render-resolved `@currentTheme` key.
    pub fn set_current_theme(&mut self, current_theme: Value) -> &mut Self {
        self.current_theme = Arc::new(current_theme);
        self
    }

    /// Set the render-resolved `@pages` nav entries.
    pub fn set_pages(&mut self, pages: Value) -> &mut Self {
        self.pages = Arc::new(pages);
        self
    }

    /// Set a local binding
    pub fn set_local(&mut self, name: &str, value: Value) -> &mut Self {
        self.locals.insert(name.to_string(), value);
        self
    }

    /// Remove a local binding
    pub fn remove_local(&mut self, name: &str) -> Option<Value> {
        self.locals.shift_remove(name)
    }

    /// Register an entity store for count queries
    pub fn register_entity_store(&mut self, entity_type: &str, entities: Vec<Value>) -> &mut Self {
        Arc::make_mut(&mut self.entity_stores).insert(entity_type.to_string(), entities);
        self
    }

    /// Share a pre-built set of entity stores without deep-cloning the map.
    pub fn set_entity_stores(&mut self, stores: Arc<HashMap<String, Vec<Value>>>) -> &mut Self {
        self.entity_stores = stores;
        self
    }

    /// Get entities of a specific type
    pub fn get_entities(&self, entity_type: &str) -> Option<&Vec<Value>> {
        self.entity_stores.get(entity_type)
    }

    /// Resolve a binding path (e.g., "payload.data.email")
    ///
    /// Returns the value at the specified path, or None if not found.
    pub fn resolve(&self, path: &str) -> Option<Value> {
        // Handle special @now binding. Ms-epoch NUMBER — the one shape all
        // three execution surfaces agree on (JS evaluator binds Date.now(),
        // TS codegen emits Date.now()). The previous RFC3339 string made
        // every `@now` arithmetic guard (reminder-scheduler / lifecycle /
        // data-erasure filters) evaluate number-minus-string on this path.
        if path == "now" {
            return Some(Value::Number(self.now_millis()));
        }

        // Split the path into root and rest
        let (root, rest) = if let Some(idx) = path.find('.') {
            (&path[..idx], Some(&path[idx + 1..]))
        } else {
            (path, None)
        };

        // Get the root value
        let root_value = match root {
            "entity" => &self.entity,
            "payload" => &self.payload,
            "state" => &self.state,
            "config" => &self.config,
            "user" => &self.user,
            "currentTheme" => &self.current_theme,
            "pages" if self.pages.is_null() => return None,
            "pages" => &self.pages,
            "locale" => &self.locale,
            "event" => &self.event,
            "callsitePayload" => &self.callsite_payload,
            "prevEvents" if rest.is_none() => return Some(Value::Array(self.prev_events.to_vec())),
            "prevStates" if rest.is_none() => {
                return Some(Value::Array(
                    self.prev_states
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ))
            }
            "fromState" if rest.is_none() => {
                return Some(self.from_state.clone().map_or(Value::Null, Value::String))
            }
            "toState" if rest.is_none() => {
                return Some(self.to_state.clone().map_or(Value::Null, Value::String))
            }
            // Check locals
            _ => self.locals.get(root)?,
        };

        // If there's a rest path, traverse it (owned walk so array `length`/
        // `count` resolve — `@payload.data.length`).
        if let Some(rest_path) = rest {
            root_value.get_path_owned(rest_path)
        } else {
            Some(root_value.clone())
        }
    }

    /// Resolve a binding reference string (starting with @)
    ///
    /// E.g., "@payload.data.email" -> resolve("payload.data.email")
    pub fn resolve_binding(&self, binding: &str) -> Option<Value> {
        if let Some(rest) = binding.strip_prefix('@') {
            self.resolve(rest)
        } else {
            None
        }
    }

    /// If a binding token starts at `at` (which must be `@`), return the index just past
    /// it; otherwise `None` (a bare `@` not followed by a word char). Grammar mirrors the
    /// JS runtime's binding token: `@[\w]+(\[\d+\])*(\.[\w]+(\[\d+\])*)*`. Hand-rolled (no
    /// `regex` dep) so orbital-core stays lean.
    pub fn binding_token_end(s: &str, at: usize) -> Option<usize> {
        let bytes = s.as_bytes();
        if at >= bytes.len() || bytes[at] != b'@' {
            return None;
        }
        // root: [\w]+
        let mut j = at + 1;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        if j == at + 1 {
            return None; // lone '@'
        }
        // optional bracket index + zero-or-more `.word[bracket]?` segments.
        loop {
            if j < bytes.len() && bytes[j] == b'[' {
                let k = j + 1;
                let mut m = k;
                while m < bytes.len() && bytes[m].is_ascii_digit() {
                    m += 1;
                }
                if m > k && m < bytes.len() && bytes[m] == b']' {
                    j = m + 1;
                }
            }
            if j + 1 < bytes.len()
                && bytes[j] == b'.'
                && (bytes[j + 1].is_ascii_alphanumeric() || bytes[j + 1] == b'_')
            {
                j += 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                continue;
            }
            break;
        }
        Some(j)
    }

    /// Interpolate every embedded `@`-binding token inside `s`, mirroring the JS
    /// runtime's `interpolateEmbeddedBindings` (`packages/almadar-runtime/src/
    /// BindingResolver.ts`). Client-only roots (`@trait.*`) pass through verbatim —
    /// the client render layer substitutes those frames. Unresolved tokens are left
    /// as-is.
    fn interpolate_embedded(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        let mut i = 0;
        let bytes = s.as_bytes();
        while i < bytes.len() {
            let Some(end) = Self::binding_token_end(s, i) else {
                i += 1;
                continue;
            };
            let token = &s[i..end];
            out.push_str(&s[last..i]);
            // Client-only binding roots round-trip verbatim (the client substitutes).
            if token.starts_with("@trait.") {
                out.push_str(token);
            } else {
                match self.resolve_binding(token) {
                    Some(resolved) => out.push_str(&resolved.to_string_value()),
                    None => out.push_str(token),
                }
            }
            i = end;
            last = end;
        }
        out.push_str(&s[last..]);
        out
    }

    /// Resolve EVERY `@`-binding inside a value, recursively, against this context.
    ///
    /// This is THE canonical value-resolver: any `@payload.X` / `@entity.X` / `@config.X`
    /// / `@state` / `@user.X` string anywhere in the tree is replaced with its resolved
    /// value (nested paths included). Effects (render-ui content, navigate/notify/emit
    /// payloads) must be run through this — never special-case one binding downstream.
    /// Binding-free subtrees are borrowed, not cloned.
    pub fn resolve_value<'v>(&self, value: &'v Value) -> std::borrow::Cow<'v, Value> {
        self.resolve_value_guarded(value, &mut Walk::default())
    }

    /// [`Self::resolve_value`] that reports a typed argument mismatch (D-12) raised by an operator call
    /// embedded in the value, as the JS runtime's `interpolateArray` does by letting it throw. Any other
    /// evaluation failure keeps the value, as `resolve_value` does.
    pub fn resolve_value_checked<'v>(
        &self,
        value: &'v Value,
    ) -> Result<std::borrow::Cow<'v, Value>, EvalError> {
        let mut walk = Walk::default();
        let resolved = self.resolve_value_guarded(value, &mut walk);
        match walk.fault {
            Some(fault) => Err(fault),
            None => Ok(resolved),
        }
    }

    /// Resolve only `@config.*` leaves inside a render-time lambda body, mirror
    /// of the JS runtime's `resolveConfigLeavesInLambdaBody`
    /// (`packages/almadar-runtime/src/BindingResolver.ts` ~232-266). A lambda
    /// body (`["fn", "item", ...]`) is otherwise raw — `@almadar/ui`'s renderer
    /// beta-reduces `@item`/`@index` per row at render time, and nothing else
    /// walks it — so `@config.*` would reach the DOM as literal text with no
    /// layer to resolve it, unlike outside a lambda where it resolves through
    /// the ordinary `resolve_value` path.
    fn resolve_config_leaves_in_lambda_body(value: &Value, ctx: &BindingContext) -> Value {
        match value {
            Value::String(s) => {
                if s.starts_with("@config.") && Self::binding_token_end(s, 0) == Some(s.len()) {
                    ctx.resolve_binding(s).unwrap_or_else(|| value.clone())
                } else {
                    value.clone()
                }
            }
            Value::Array(arr) => Value::Array(
                arr.iter()
                    .map(|v| Self::resolve_config_leaves_in_lambda_body(v, ctx))
                    .collect(),
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            Self::resolve_config_leaves_in_lambda_body(v, ctx),
                        )
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// `resolve_value` body with an occurs-check. `expanding` is the stack of
    /// pure-binding tokens whose resolved STRUCTURES are currently being
    /// re-resolved. A runtime value can contain its own binding token (e.g.
    /// `(set @entity.audit (concat @entity.audit …))` on an entity whose
    /// `audit` was still the unresolved literal stores `"@entity.audit"`
    /// INSIDE the new array) — re-expanding that token inside its own
    /// expansion recurses forever. An ancestor-token match leaves the string
    /// literal instead; the same token in SIBLING positions still resolves.
    fn resolve_value_guarded<'v>(
        &self,
        value: &'v Value,
        walk: &mut Walk,
    ) -> std::borrow::Cow<'v, Value> {
        use std::borrow::Cow;
        match value {
            Value::String(s) => {
                // Pure binding (the WHOLE string is one `@`-token, matching the JS
                // `isPureBinding`) → typed value (a number stays a number). A string
                // that merely starts with `@` but carries trailing text (e.g.
                // `"@entity.name = @entity.count"`) is NOT pure and falls through to
                // embedded interpolation — otherwise the resolver tries to resolve the
                // whole string as one path and leaves it literal.
                let is_pure = Self::binding_token_end(s, 0) == Some(s.len());
                if is_pure {
                    if walk.expanding.iter().any(|t| t == s) {
                        // Self-referential value: this token is already being
                        // expanded higher up the same chain. Leave it literal.
                        return Cow::Borrowed(value);
                    }
                    match self.resolve_binding(s) {
                        // A config-held render-ui tree (e.g. `@config.bodyContent`) is a
                        // structure whose own strings may carry `@entity.*`/`@config.*`
                        // bindings. Recurse so those resolve too. A declared config
                        // default can likewise forward to ANOTHER pure binding scalar
                        // (`theme : string = @currentTheme`, `viewerName = @user.name`)
                        // — the same one-more-hop recursion, not a special case per
                        // forwarded root. The `expanding` stack is the cycle bound for
                        // both shapes.
                        Some(resolved) => match resolved {
                            Value::Object(_) | Value::Array(_) => {
                                walk.expanding.push(s.clone());
                                let out = self.resolve_value_guarded(&resolved, walk).into_owned();
                                walk.expanding.pop();
                                Cow::Owned(out)
                            }
                            Value::String(ref rs)
                                if Self::binding_token_end(rs, 0) == Some(rs.len()) =>
                            {
                                if walk.expanding.iter().any(|t| t == s) {
                                    Cow::Borrowed(value)
                                } else {
                                    walk.expanding.push(s.clone());
                                    let out =
                                        self.resolve_value_guarded(&resolved, walk).into_owned();
                                    walk.expanding.pop();
                                    Cow::Owned(out)
                                }
                            }
                            other => Cow::Owned(other),
                        },
                        None => Cow::Borrowed(value),
                    }
                } else if s.contains('@') {
                    // Embedded bindings (e.g. "Count: @entity.count") → interpolate.
                    // Parity with the JS runtime's `interpolateString`; without this
                    // the Rust path left prefixed bindings literal (Count: @entity.count).
                    let interpolated = self.interpolate_embedded(s);
                    if interpolated == *s {
                        Cow::Borrowed(value)
                    } else {
                        Cow::Owned(Value::String(interpolated))
                    }
                } else {
                    Cow::Borrowed(value)
                }
            }
            Value::Object(map) => {
                let mut resolved_map: Option<ValueMap> = None;
                for (key, val) in map {
                    if self.is_unset_config_binding(val) {
                        resolved_map
                            .get_or_insert_with(|| map.clone())
                            .shift_remove(key);
                        continue;
                    }
                    if let Cow::Owned(new_val) = self.resolve_value_guarded(val, walk) {
                        resolved_map
                            .get_or_insert_with(|| map.clone())
                            .insert(key.clone(), new_val);
                    }
                }
                match resolved_map {
                    Some(m) => Cow::Owned(Value::Object(m)),
                    None => Cow::Borrowed(value),
                }
            }
            Value::Array(arr) => {
                // Operator forms embedded as VALUES inside render-ui content
                // (`["if", ["==", "@entity.status", "draft"], "Draft", "Published"]`)
                // are calls, not literal lists — mirror the JS runtime's
                // `interpolateArray` (`packages/almadar-runtime/src/BindingResolver.ts`
                // `isSExpression` ~533-543 + the eval dispatch ~464-476): an array whose
                // head is a REGISTERED operator name is evaluated through the one
                // evaluator against this same context. `fn`/`lambda` are themselves
                // registered operators (they build a `Value::Lambda`) but are render-time
                // lambdas the renderer beta-reduces per row later — the JS side special-
                // cases the flat `["fn", <arg>, body]` form to stay raw
                // (`BindingResolver.ts` ~445-462); mirrored here by excluding both
                // spellings from evaluation so the body passes through unevaluated (still
                // walked recursively below for its own `@`-token substitutions).
                if let Some(Value::String(op)) = arr.first() {
                    if op == "fn" || op == "lambda" {
                        // Render-time lambda: stays raw for the renderer to beta-reduce
                        // per row. Only `@config.*` leaves resolve here, the same
                        // asymmetry the JS side documents (`resolveConfigLeavesInLambdaBody`,
                        // `BindingResolver.ts` ~232-266) — nothing else walks a lambda body.
                        let resolved = Self::resolve_config_leaves_in_lambda_body(value, self);
                        return if resolved == *value {
                            Cow::Borrowed(value)
                        } else {
                            Cow::Owned(resolved)
                        };
                    }
                    if super::operators::get_operator(op).is_some() {
                        let sexpr: orbital_core_base::schema::sexpr::SExpression =
                            value.clone().into();
                        match super::operators::eval_expr(&sexpr, self) {
                            Ok(result) => return Cow::Owned(result),
                            Err(OrbitalError::Eval(fault @ EvalError::TypeMismatch { .. })) => {
                                walk.fault.get_or_insert(fault);
                            }
                            Err(_) => {}
                        }
                        // Evaluation failed (e.g. malformed args) — fall through to
                        // per-element binding resolution rather than losing the value.
                    }
                }
                let mut resolved_arr: Option<Vec<Value>> = None;
                for (i, val) in arr.iter().enumerate() {
                    if self.is_unset_config_binding(val) {
                        resolved_arr.get_or_insert_with(|| arr.clone())[i] = Value::Null;
                        continue;
                    }
                    if let Cow::Owned(new_val) = self.resolve_value_guarded(val, walk) {
                        resolved_arr.get_or_insert_with(|| arr.clone())[i] = new_val;
                    }
                }
                match resolved_arr {
                    Some(a) => Cow::Owned(Value::Array(a)),
                    None => Cow::Borrowed(value),
                }
            }
            _ => Cow::Borrowed(value),
        }
    }

    /// A whole-string binding that holds no value, which an object leaves out as the JS runtime's `undefined`:
    /// an `@config.<knob>` that is unset (declared without a default, or never declared) or forwards to an
    /// unset `@callsitePayload.<path>`, an `@callsitePayload.<path>` of a trait not yet composed, an
    /// `@event.<path>` the delivery in flight does not carry, and (when `drop_unset`) any other.
    pub fn is_unset_config_binding(&self, value: &Value) -> bool {
        let Value::String(s) = value else {
            return false;
        };
        if Self::binding_token_end(s, 0) != Some(s.len()) {
            return false;
        }
        let rooted = s.starts_with("@config.")
            || s.starts_with("@callsitePayload.")
            || (s.starts_with("@event.") && matches!(&*self.event, Value::Object(_)))
            || (self.drop_unset && s.starts_with('@') && !s.starts_with("@trait."));
        rooted && self.resolves_to_nothing(s)
    }

    /// Whether the whole-string binding `token` ends in no value, following knobs that forward to another
    /// `@config.` / `@callsitePayload.` binding.
    fn resolves_to_nothing(&self, token: &str) -> bool {
        let mut current = token.to_string();
        for _ in 0..8 {
            match self.resolve_binding(&current) {
                None => return true,
                Some(Value::String(next))
                    if Self::binding_token_end(&next, 0) == Some(next.len())
                        && (next.starts_with("@config.")
                            || next.starts_with("@callsitePayload.")) =>
                {
                    current = next;
                }
                Some(_) => return false,
            }
        }
        false
    }

    /// Create a child context with additional local bindings
    ///
    /// Useful for `let` expressions and lambda scopes.
    pub fn child(&self) -> Self {
        Self {
            entity: self.entity.clone(),
            payload: self.payload.clone(),
            state: self.state.clone(),
            config: self.config.clone(),
            user: self.user.clone(),
            current_theme: self.current_theme.clone(),
            pages: self.pages.clone(),
            now: self.now,
            locale: self.locale.clone(),
            messages: Arc::clone(&self.messages),
            event: self.event.clone(),
            callsite_payload: self.callsite_payload.clone(),
            prev_events: self.prev_events.clone(),
            prev_states: self.prev_states.clone(),
            from_state: self.from_state.clone(),
            to_state: self.to_state.clone(),
            locals: self.locals.clone(),
            entity_stores: Arc::clone(&self.entity_stores),
            drop_unset: self.drop_unset,
        }
    }

    /// [`Self::resolve_value`] for a value no later layer resolves (an `emit` payload, read only by the
    /// listeners it reaches): a whole-string binding with no value is an absent prop, as `undefined` is
    /// in the JS runtime. A client-resolved `@trait.*` token stays.
    pub fn resolve_dropping_unset(&self, value: &Value) -> Value {
        let mut ctx = self.clone();
        ctx.drop_unset = true;
        ctx.resolve_value(value).into_owned()
    }

    /// [`Self::resolve_dropping_unset`] with [`Self::resolve_value_checked`]'s typed-mismatch report.
    pub fn resolve_dropping_unset_checked(&self, value: &Value) -> Result<Value, EvalError> {
        let mut ctx = self.clone();
        ctx.drop_unset = true;
        ctx.resolve_value_checked(value)
            .map(std::borrow::Cow::into_owned)
    }

    /// Create a child context with a specific item bound (for map/filter)
    pub fn with_item(&self, item: Value) -> Self {
        let mut child = self.child();
        child.set_local("item", item);
        child
    }

    /// Create a child context with index and item bound (for map with index)
    pub fn with_item_and_index(&self, item: Value, index: usize) -> Self {
        let mut child = self.child();
        child.set_local("item", item);
        child.set_local("index", Value::Number(index as f64));
        child
    }
}

/// Builder for creating binding contexts
#[derive(Debug, Default)]
pub struct BindingContextBuilder {
    context: BindingContext,
}

impl BindingContextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn now(mut self, now: f64) -> Self {
        self.context.now = Some(now);
        self
    }

    pub fn entity(mut self, entity: Value) -> Self {
        self.context.entity = Arc::new(entity);
        self
    }

    pub fn payload(mut self, payload: Value) -> Self {
        self.context.payload = Arc::new(payload);
        self
    }

    pub fn state(mut self, state: Value) -> Self {
        self.context.state = Arc::new(state);
        self
    }

    pub fn config(mut self, config: Value) -> Self {
        self.context.config = Arc::new(config);
        self
    }

    pub fn user(mut self, user: Value) -> Self {
        self.context.user = Arc::new(user);
        self
    }

    pub fn current_theme(mut self, current_theme: Value) -> Self {
        self.context.current_theme = Arc::new(current_theme);
        self
    }

    pub fn pages(mut self, pages: Value) -> Self {
        self.context.pages = Arc::new(pages);
        self
    }

    pub fn locale(mut self, locale: Value) -> Self {
        self.context.locale = Arc::new(locale);
        self
    }

    pub fn messages(mut self, messages: HashMap<String, String>) -> Self {
        self.context.messages = Arc::new(messages);
        self
    }

    pub fn local(mut self, name: &str, value: Value) -> Self {
        self.context.locals.insert(name.to_string(), value);
        self
    }

    pub fn entity_store(mut self, entity_type: &str, entities: Vec<Value>) -> Self {
        Arc::make_mut(&mut self.context.entity_stores).insert(entity_type.to_string(), entities);
        self
    }

    pub fn build(self) -> BindingContext {
        self.context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> Value {
        let mut data = ValueMap::new();
        data.insert(
            "email".to_string(),
            Value::String("test@example.com".to_string()),
        );
        data.insert("name".to_string(), Value::String("Test User".to_string()));
        data.insert("connectionSlots".to_string(), Value::Number(10.0));

        let mut payload = ValueMap::new();
        payload.insert("data".to_string(), Value::Object(data));
        payload.insert("id".to_string(), Value::String("user-123".to_string()));

        Value::Object(payload)
    }

    fn sample_user() -> Value {
        let mut user = ValueMap::new();
        user.insert(
            "id".to_string(),
            Value::String("current-user-id".to_string()),
        );
        user.insert(
            "name".to_string(),
            Value::String("Current User".to_string()),
        );
        Value::Object(user)
    }

    #[test]
    fn test_resolve_simple_binding() {
        let ctx = BindingContextBuilder::new()
            .payload(sample_payload())
            .build();

        let result = ctx.resolve("payload.data.email");
        assert_eq!(result, Some(Value::String("test@example.com".to_string())));
    }

    #[test]
    fn test_resolve_nested_path() {
        let ctx = BindingContextBuilder::new()
            .payload(sample_payload())
            .build();

        let result = ctx.resolve("payload.data.connectionSlots");
        assert_eq!(result, Some(Value::Number(10.0)));
    }

    #[test]
    fn test_resolve_value_every_root() {
        // THE canonical value-walker resolves any `@payload.X` / `@entity.X` / `@config.X`
        // inside a tree — no per-binding special-casing. (Effects run through this.)
        let mut entity = ValueMap::new();
        entity.insert("title".to_string(), Value::String("Ticket #1".to_string()));
        let mut config = ValueMap::new();
        config.insert("look".to_string(), Value::String("striped".to_string()));
        let ctx = BindingContextBuilder::new()
            .payload(sample_payload())
            .entity(Value::Object(entity))
            .config(Value::Object(config))
            .build();

        let content = Value::Object(ValueMap::from([
            // a data table bound to the loaded collection — resolves to the payload value
            (
                "rows".to_string(),
                Value::String("@payload.data".to_string()),
            ),
            (
                "heading".to_string(),
                Value::String("@entity.title".to_string()),
            ),
            (
                "look".to_string(),
                Value::String("@config.look".to_string()),
            ),
            (
                "missing".to_string(),
                Value::String("@payload.nope".to_string()),
            ),
            ("literal".to_string(), Value::String("plain".to_string())),
        ]));
        let resolved = ctx.resolve_value(&content).into_owned();
        let obj = resolved.as_object().unwrap();
        assert!(
            obj["rows"].as_object().is_some(),
            "@payload.data → the data object"
        );
        assert_eq!(obj["heading"], Value::String("Ticket #1".to_string()));
        assert_eq!(obj["look"], Value::String("striped".to_string()));
        // Unresolvable binding is left literal (not crashed, not blanked).
        assert_eq!(obj["missing"], Value::String("@payload.nope".to_string()));
        assert_eq!(obj["literal"], Value::String("plain".to_string()));
    }

    /// A render tree resolves `i18n/t` against the context's locale and catalog
    /// (the dispatch viewer's), params included.
    #[test]
    fn resolve_value_evaluates_a_translate_call_with_the_contexts_catalog() {
        let mut entity = ValueMap::new();
        entity.insert("count".to_string(), Value::Number(3.0));
        let ctx = BindingContextBuilder::new()
            .entity(Value::Object(entity))
            .locale(Value::String("ar".to_string()))
            .messages(HashMap::from([(
                "home:posts.count".to_string(),
                "{{n}} منشورات".to_string(),
            )]))
            .build();
        let call = Value::Array(vec![
            Value::String("i18n/t".to_string()),
            Value::String("home:posts.count".to_string()),
            Value::Object(ValueMap::from([(
                "n".to_string(),
                Value::String("@entity.count".to_string()),
            )])),
        ]);
        let resolved = ctx
            .resolve_value(&Value::Object(ValueMap::from([(
                "label".to_string(),
                call,
            )])))
            .into_owned();
        assert_eq!(
            resolved.as_object().unwrap()["label"],
            Value::String("3 منشورات".to_string())
        );
    }

    #[test]
    fn control_resolve_value_still_evaluates_other_operators() {
        let ctx = BindingContextBuilder::new().build();
        let call = Value::Array(vec![
            Value::String("str/concat".to_string()),
            Value::String("/ar".to_string()),
            Value::String("/blog".to_string()),
        ]);
        assert_eq!(
            ctx.resolve_value(&call).into_owned(),
            Value::String("/ar/blog".to_string())
        );
    }

    #[test]
    fn test_resolve_value_config_forward_to_another_binding() {
        // A declared config default can itself be a bare binding token forwarding
        // to a render-resolved sigil (`theme : string = @currentTheme` on
        // std-app-layout) or another root (`viewerName = @user.name`). The first
        // hop resolves `@config.theme` to the STRING `"@currentTheme"`; without a
        // second hop that scalar round-trips literal on the wire (the exact §155.1
        // gap: `box.data-theme` arriving as `"@currentTheme"`).
        let mut config = ValueMap::new();
        config.insert(
            "theme".to_string(),
            Value::String("@currentTheme".to_string()),
        );
        let mut user = ValueMap::new();
        user.insert("name".to_string(), Value::String("Ada".to_string()));
        let ctx = BindingContextBuilder::new()
            .config(Value::Object(config))
            .current_theme(Value::String("clay-light".to_string()))
            .user(Value::Object(user))
            .build();

        let forward = Value::String("@config.theme".to_string());
        let resolved = ctx.resolve_value(&forward);
        assert_eq!(
            resolved.into_owned(),
            Value::String("clay-light".to_string()),
            "a config default forwarding to @currentTheme must resolve through BOTH hops"
        );
    }

    #[test]
    fn test_resolve_value_self_forward_stays_literal_not_infinite_loop() {
        // A config default that forwards to itself (`x = @config.x`, an authoring
        // bug) must not recurse forever — the same occurs-check that already
        // guards structural (Object/Array) re-expansion covers the scalar hop.
        let mut config = ValueMap::new();
        config.insert("x".to_string(), Value::String("@config.x".to_string()));
        let ctx = BindingContextBuilder::new()
            .config(Value::Object(config))
            .build();
        let forward = Value::String("@config.x".to_string());
        let resolved = ctx.resolve_value(&forward);
        assert_eq!(
            resolved.into_owned(),
            Value::String("@config.x".to_string())
        );
    }

    #[test]
    fn test_resolve_value_interpolates_embedded_bindings() {
        // Parity with the JS runtime's `interpolateString`: a binding embedded in a
        // longer string ("Count: @entity.count") interpolates, while a pure-binding
        // string ("@entity.count") resolves to its typed value. Locks the runtime/
        // codegen parity for prefixed bindings (the "Count: @entity.count" bug).
        let mut entity = ValueMap::new();
        entity.insert("count".to_string(), Value::Number(0.0));
        entity.insert("name".to_string(), Value::String("Counter".to_string()));
        let ctx = BindingContextBuilder::new()
            .entity(Value::Object(entity))
            .build();

        // Embedded → interpolated string.
        let content = Value::String("Count: @entity.count".to_string());
        let resolved = ctx.resolve_value(&content).into_owned();
        assert_eq!(resolved, Value::String("Count: 0".to_string()));

        // Multiple tokens in one string.
        let content = Value::String("@entity.name = @entity.count".to_string());
        let resolved = ctx.resolve_value(&content).into_owned();
        assert_eq!(resolved, Value::String("Counter = 0".to_string()));

        // Pure binding → typed value (number stays a number).
        let content = Value::String("@entity.count".to_string());
        let resolved = ctx.resolve_value(&content).into_owned();
        assert_eq!(resolved, Value::Number(0.0));

        // Unresolved embedded token is left literal (not crashed, not blanked).
        let content = Value::String("x: @entity.missing".to_string());
        let resolved = ctx.resolve_value(&content).into_owned();
        assert_eq!(resolved, Value::String("x: @entity.missing".to_string()));

        // Client-only root (`@trait.*`) passes through verbatim.
        let content = Value::String("see @trait.Other".to_string());
        let resolved = ctx.resolve_value(&content).into_owned();
        assert_eq!(resolved, Value::String("see @trait.Other".to_string()));
    }

    #[test]
    fn test_resolve_user() {
        let ctx = BindingContextBuilder::new().user(sample_user()).build();

        assert_eq!(
            ctx.resolve("user.id"),
            Some(Value::String("current-user-id".to_string()))
        );
    }

    /// `@currentUser` was retired in favour of the single `@user` sigil: it
    /// resolved here and nowhere in the JS interpreter, so an author using it
    /// got a working compiled app and a silently empty interpreted one.
    #[test]
    fn test_current_user_alias_is_retired() {
        let ctx = BindingContextBuilder::new().user(sample_user()).build();

        assert_eq!(ctx.resolve("currentUser.id"), None);
    }

    #[test]
    fn test_resolve_binding_with_at_sign() {
        let ctx = BindingContextBuilder::new()
            .payload(sample_payload())
            .build();

        let result = ctx.resolve_binding("@payload.data.name");
        assert_eq!(result, Some(Value::String("Test User".to_string())));
    }

    #[test]
    fn test_resolve_now() {
        let ctx = BindingContext::new();
        let result = ctx.resolve("now");

        assert!(result.is_some());
        if let Some(Value::Number(ms)) = result {
            // Ms-epoch, matching the JS evaluator's Date.now() binding —
            // sanity-bound to [2020, 2100).
            assert!(ms > 1_577_836_800_000.0 && ms < 4_102_444_800_000.0);
        } else {
            panic!("Expected ms-epoch number timestamp, got {result:?}");
        }
    }

    #[test]
    fn test_local_bindings() {
        let mut ctx = BindingContext::new();
        ctx.set_local("myVar", Value::Number(42.0));

        let result = ctx.resolve("myVar");
        assert_eq!(result, Some(Value::Number(42.0)));
    }

    #[test]
    fn test_child_context() {
        let ctx = BindingContextBuilder::new()
            .payload(sample_payload())
            .local("outer", Value::Number(1.0))
            .build();

        let mut child = ctx.child();
        child.set_local("inner", Value::Number(2.0));

        // Child should have both
        assert_eq!(child.resolve("outer"), Some(Value::Number(1.0)));
        assert_eq!(child.resolve("inner"), Some(Value::Number(2.0)));

        // Parent should only have outer
        assert_eq!(ctx.resolve("outer"), Some(Value::Number(1.0)));
        assert_eq!(ctx.resolve("inner"), None);
    }

    #[test]
    fn test_nonexistent_path() {
        let ctx = BindingContextBuilder::new()
            .payload(sample_payload())
            .build();

        assert_eq!(ctx.resolve("payload.data.nonexistent"), None);
        assert_eq!(ctx.resolve("nonexistent"), None);
        assert_eq!(ctx.resolve("payload.nonexistent.deep"), None);
    }

    #[test]
    fn test_entity_stores() {
        let connections = vec![
            {
                let mut c = ValueMap::new();
                c.insert(
                    "requesterId".to_string(),
                    Value::String("user-1".to_string()),
                );
                c.insert("status".to_string(), Value::String("accepted".to_string()));
                Value::Object(c)
            },
            {
                let mut c = ValueMap::new();
                c.insert(
                    "requesterId".to_string(),
                    Value::String("user-1".to_string()),
                );
                c.insert("status".to_string(), Value::String("pending".to_string()));
                Value::Object(c)
            },
        ];

        let ctx = BindingContextBuilder::new()
            .entity_store("Connection", connections.clone())
            .build();

        let stored = ctx.get_entities("Connection");
        assert!(stored.is_some());
        assert_eq!(stored.unwrap().len(), 2);
    }

    /// Regression pin (std-multi-party-transaction stack overflow): a runtime
    /// value can contain its OWN binding token — `(set @entity.audit (concat
    /// @entity.audit {…}))` on an entity whose `audit` was still the
    /// unresolved literal stores `"@entity.audit"` inside the new array.
    /// Structure re-resolution must occurs-check the expansion stack and
    /// leave the self-token literal instead of recursing forever.
    #[test]
    fn resolve_value_self_referential_structure_terminates() {
        let mut entry = ValueMap::new();
        entry.insert("action".to_string(), Value::String("restarted".to_string()));
        let mut entity = ValueMap::new();
        entity.insert(
            "audit".to_string(),
            Value::Array(vec![
                Value::String("@entity.audit".to_string()),
                Value::Object(entry),
            ]),
        );
        let ctx = BindingContextBuilder::new()
            .entity(Value::Object(entity))
            .build();

        let resolved = ctx
            .resolve_value(&Value::String("@entity.audit".to_string()))
            .into_owned();
        let Value::Array(items) = resolved else {
            panic!("expected the audit array");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            Value::String("@entity.audit".to_string()),
            "the self-token must stay literal, not re-expand"
        );
    }

    /// Mutual cycle (`@entity.a` ⇄ `@entity.b`) must terminate too, and the
    /// same token in SIBLING positions (not its own ancestor) must still
    /// resolve normally.
    #[test]
    fn resolve_value_mutual_cycle_terminates_and_siblings_still_resolve() {
        let mut entity = ValueMap::new();
        entity.insert(
            "a".to_string(),
            Value::Array(vec![Value::String("@entity.b".to_string())]),
        );
        entity.insert(
            "b".to_string(),
            Value::Array(vec![Value::String("@entity.a".to_string())]),
        );
        entity.insert("name".to_string(), Value::String("N".to_string()));
        let ctx = BindingContextBuilder::new()
            .entity(Value::Object(entity))
            .build();

        // Terminates (the inner @entity.a stays literal at the cycle edge).
        let _ = ctx
            .resolve_value(&Value::String("@entity.a".to_string()))
            .into_owned();

        // Sibling repetition of one token is NOT a cycle: both resolve.
        let siblings = Value::Array(vec![
            Value::String("@entity.name".to_string()),
            Value::String("@entity.name".to_string()),
        ]);
        let resolved = ctx.resolve_value(&siblings).into_owned();
        assert_eq!(
            resolved,
            Value::Array(vec![
                Value::String("N".to_string()),
                Value::String("N".to_string()),
            ])
        );
    }

    /// §157 `expr-in-render-props`: an operator array embedded as a VALUE inside
    /// render-ui content (`["if", ["==", "@entity.status", "draft"], "Draft",
    /// "Published"]`) is a call, not a literal list — `resolve_value` must
    /// evaluate it through the one evaluator against this same context, exactly
    /// as `packages/almadar-runtime/src/BindingResolver.ts`'s `interpolateArray`
    /// (`isSExpression` ~533-543) does on the JS side.
    #[test]
    fn render_ui_operator_forms_are_evaluated() {
        let mut entity = ValueMap::new();
        entity.insert("status".to_string(), Value::String("draft".to_string()));
        let ctx = BindingContextBuilder::new()
            .entity(Value::Object(entity))
            .build();

        let content = Value::Array(vec![
            Value::String("if".to_string()),
            Value::Array(vec![
                Value::String("==".to_string()),
                Value::String("@entity.status".to_string()),
                Value::String("draft".to_string()),
            ]),
            Value::String("Draft".to_string()),
            Value::String("Published".to_string()),
        ]);

        let resolved = ctx.resolve_value(&content).into_owned();
        assert_eq!(resolved, Value::String("Draft".to_string()));
    }

    /// §157 `expr-in-render-props`: a render-time lambda (`["fn", "item", body]`)
    /// stays unevaluated — the JS runtime special-cases the exact same flat
    /// three-arity form (`BindingResolver.ts` ~445-462) because `@almadar/ui`'s
    /// renderer beta-reduces it per row at render time; evaluating it here would
    /// collapse it into a non-serializable `Value::Lambda` before the renderer
    /// ever sees each row.
    #[test]
    fn render_ui_lambdas_stay_unevaluated() {
        let ctx = BindingContext::new();
        let content = Value::Array(vec![
            Value::String("fn".to_string()),
            Value::String("item".to_string()),
            Value::Array(vec![
                Value::String("str/concat".to_string()),
                Value::String("- ".to_string()),
                Value::String("@item".to_string()),
            ]),
        ]);

        let resolved = ctx.resolve_value(&content).into_owned();
        assert_eq!(
            resolved, content,
            "the fn form must pass through literally, not evaluate to a Value::Lambda"
        );
    }
}
