//! Resolving bindings and values against a `BindingContext`.

use super::*;

impl BindingContext {
    /// Resolve a binding path (e.g., "payload.data.email")
    ///
    /// Returns the value at the specified path, or None if not found.
    pub fn resolve(&self, path: &str) -> Option<Value> {
        // @now is an ISO-8601 instant, the same text `time/now` answers.
        if path == "now" {
            return crate::evaluator::operators::system::instant_value(self.now_millis()).ok();
        }

        // Split the path into root and rest
        let (root, rest) = if let Some(idx) = path.find('.') {
            (&path[..idx], Some(&path[idx + 1..]))
        } else {
            (path, None)
        };

        if root == "entity" {
            return match rest {
                Some(rest_path) => self.entity.get_path_owned(rest_path),
                None => Some(self.entity.to_value()),
            };
        }

        // Get the root value
        let root_value = match root {
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
            "prevEvents" if rest.is_none() => return Some(Value::Array(self.prev_events.iter().map(|d| Value::clone(d)).collect())),
            "prevStates" if rest.is_none() => {
                return Some(Value::Array(
                    self.prev_states
                        .iter()
                        .cloned()
                        .map(Value::string)
                        .collect(),
                ))
            }
            "fromState" if rest.is_none() => {
                return Some(self.from_state.as_deref().map_or(Value::Null, |state| Value::string(state.to_owned())))
            }
            "toState" if rest.is_none() => {
                return Some(self.to_state.as_deref().map_or(Value::Null, |state| Value::string(state.to_owned())))
            }
            // Check locals, then the scope's read-once local
            _ => match self.locals.get(root) {
                Some(local) => local,
                None => {
                    let taken = self.take_linear(root)?;
                    return match rest {
                        Some(rest_path) => taken.get_path_owned(rest_path),
                        None => Some(taken),
                    };
                }
            },
        };

        // If there's a rest path, traverse it (owned walk so array `length`/
        // `count` resolve — `@payload.data.length`).
        if let Some(rest_path) = rest {
            root_value.get_path_owned(rest_path)
        } else {
            Some(root_value.clone())
        }
    }

    /// [`Self::resolve`] by reference, for a reader that needs part of the bound value: `None` for `@now`, the
    /// dispatch logs and states (computed, not held), and for a path the borrowed walk cannot finish (a virtual
    /// `length`/`count`), where the caller reads the owned value as [`Self::resolve`] gives it.
    pub fn resolve_borrowed(&self, path: &str) -> Option<&Value> {
        let (root, rest) = match path.find('.') {
            Some(idx) => (&path[..idx], Some(&path[idx + 1..])),
            None => (path, None),
        };
        let root_value: &Value = match root {
            "now" | "prevEvents" | "prevStates" | "fromState" | "toState" => return None,
            "entity" => {
                return match rest {
                    Some(rest_path) => self.entity.get_path(rest_path),
                    None => Some(self.entity.as_value()),
                }
            }
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
            _ => self.locals.get(root)?,
        };
        match rest {
            Some(rest_path) => root_value.get_path(rest_path),
            None => Some(root_value),
        }
    }

    /// Whether the root of the binding `token` (`@user` of `@user.name`) holds a non-null value.
    pub(super) fn root_is_held(&self, token: &str) -> bool {
        let path = token.strip_prefix('@').unwrap_or(token);
        let root = path.split(['.', '[']).next().unwrap_or(path);
        self.resolve(root).is_some_and(|v| !v.is_null())
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
    pub(super) fn interpolate_embedded(&self, s: &str) -> String {
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

    /// [`Self::resolve_value`] that reports the failure of an operator call embedded in the value (a typed
    /// argument mismatch, D-12, or any other evaluation error), as the JS runtime's `interpolateArray` does by
    /// letting it throw (G-ORBITAL-CORE-033).
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
    pub(super) fn resolve_config_leaves_in_lambda_body(value: &Value, ctx: &BindingContext) -> Value {
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
    pub(super) fn resolve_value_guarded<'v>(
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
                    // Runtime data (`@payload`, `@entity`, `@user`, …) is a value: it is never walked again,
                    // so an array whose head names an operator, or a string shaped like a binding, stays the
                    // caller's data. Only authored roots carry structure to resolve further.
                    if !(s.starts_with("@config.") || s.starts_with("@trait.")) {
                        return match self.resolve_binding(s) {
                            Some(resolved) => Cow::Owned(resolved),
                            // A held root missing only the path reads null, as an expression does; a root the kernel
                            // has not populated keeps its token.
                            None if self.root_is_held(s) => Cow::Owned(Value::Null),
                            None => Cow::Borrowed(value),
                        };
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
                                walk.expanding.push(s.clone().into_inner());
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
                                    walk.expanding.push(s.clone().into_inner());
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
                        Cow::Owned(Value::string(interpolated))
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
                            .get_or_insert_with(|| map.clone().into_inner())
                            .shift_remove(key);
                        continue;
                    }
                    if let Cow::Owned(new_val) = self.resolve_value_guarded(val, walk) {
                        resolved_map
                            .get_or_insert_with(|| map.clone().into_inner())
                            .insert(key.clone(), new_val);
                    }
                }
                match resolved_map {
                    Some(m) => Cow::Owned(Value::object(m)),
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
                    if op == "fn" {
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
                        // An operator-headed array is a call: its failure is a fault the checked
                        // resolution reports (G-ORBITAL-CORE-033); the unchecked one keeps the value.
                        match super::operators::eval_expr(&sexpr, self) {
                            Ok(result) => return Cow::Owned(result),
                            Err(OrbitalError::Eval(fault)) => {
                                walk.fault.get_or_insert(fault);
                            }
                            Err(other) => {
                                walk.fault.get_or_insert(EvalError::InvalidArgument(other.to_string()));
                            }
                        }
                        // Evaluation failed (e.g. malformed args) — fall through to
                        // per-element binding resolution rather than losing the value.
                    }
                }
                let mut resolved_arr: Option<Vec<Value>> = None;
                for (i, val) in arr.iter().enumerate() {
                    if self.is_unset_config_binding(val) {
                        resolved_arr.get_or_insert_with(|| arr.clone().into_inner())[i] = Value::Null;
                        continue;
                    }
                    if let Cow::Owned(new_val) = self.resolve_value_guarded(val, walk) {
                        resolved_arr.get_or_insert_with(|| arr.clone().into_inner())[i] = new_val;
                    }
                }
                match resolved_arr {
                    Some(a) => Cow::Owned(Value::array(a)),
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
    pub(super) fn resolves_to_nothing(&self, token: &str) -> bool {
        let mut current = token.to_string();
        for _ in 0..8 {
            match self.resolve_binding(&current) {
                None => return true,
                Some(Value::String(next))
                    if Self::binding_token_end(&next, 0) == Some(next.len())
                        && (next.starts_with("@config.")
                            || next.starts_with("@callsitePayload.")) =>
                {
                    current = next.into_inner();
                }
                Some(_) => return false,
            }
        }
        false
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
}
