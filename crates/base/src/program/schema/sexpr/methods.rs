//! What an `SExpression` answers about itself, and its rewrites.

use super::*;

impl SExpression {
    pub fn string(text: impl Into<crate::value::Shared<String>>) -> SExpression {
        SExpression::String(text.into())
    }

    pub fn object(map: SObject) -> SExpression {
        SExpression::Object(crate::value::Shared::new(map))
    }

    pub fn array(items: Vec<SExpression>) -> SExpression {
        SExpression::Array(crate::value::Shared::new(items))
    }

    /// A copy in which every list and object owns its storage, so each node has an address of its own even where the
    /// source shares identical subtrees.
    pub fn unshared(&self) -> SExpression {
        match self.rewrite_with(
            false,
            |_| Ok::<_, std::convert::Infallible>(RewriteVisit::Descend),
            Ok,
        ) {
            Ok(copy) => copy,
            Err(never) => match never {},
        }
    }

    /// A rewrite of the tree without recursion, so its depth is bounded by the heap, not the thread stack: `enter`
    /// runs on every node in pre-order, `exit` on every descended node once its children are rewritten, in the order
    /// a recursive walk visits them (the first error is the one a recursive walk would raise). A list or object whose
    /// children all come back as they went in is handed to `exit` as the shared input, not a rebuilt copy.
    pub fn try_rewrite<E>(
        &self,
        enter: impl FnMut(&SExpression) -> Result<RewriteVisit, E>,
        exit: impl FnMut(SExpression) -> Result<SExpression, E>,
    ) -> Result<SExpression, E> {
        self.rewrite_with(true, enter, exit)
    }

    fn rewrite_with<E>(
        &self,
        reuse: bool,
        mut enter: impl FnMut(&SExpression) -> Result<RewriteVisit, E>,
        mut exit: impl FnMut(SExpression) -> Result<SExpression, E>,
    ) -> Result<SExpression, E> {
        let mut frames = vec![RewriteFrame::Enter(self)];
        let mut done: Vec<SExpression> = Vec::new();
        while let Some(frame) = frames.pop() {
            match frame {
                RewriteFrame::Enter(node) => {
                    if enter(node)? == RewriteVisit::Keep {
                        done.push(node.clone());
                        continue;
                    }
                    match node {
                        SExpression::Array(items) => {
                            frames.push(RewriteFrame::Array(node, items.len()));
                            frames.extend(items.iter().rev().map(RewriteFrame::Enter));
                        }
                        SExpression::Object(fields) => {
                            frames.push(RewriteFrame::Object(node, fields.len()));
                            frames.extend(fields.values().rev().map(RewriteFrame::Enter));
                        }
                        leaf => done.push(exit(leaf.clone())?),
                    }
                }
                RewriteFrame::Array(source, len) => {
                    let items = done.split_off(done.len() - len);
                    let unchanged = reuse
                        && matches!(source, SExpression::Array(old)
                            if old.iter().zip(&items).all(|(a, b)| a.same_node(b)));
                    let node = if unchanged { source.clone() } else { SExpression::Array(items.into()) };
                    done.push(exit(node)?);
                }
                RewriteFrame::Object(source, len) => {
                    let values = done.split_off(done.len() - len);
                    let SExpression::Object(old) = source else {
                        unreachable!("an object frame is pushed for an object");
                    };
                    let unchanged = reuse && old.values().zip(&values).all(|(a, b)| a.same_node(b));
                    let node = if unchanged {
                        source.clone()
                    } else {
                        SExpression::object(old.keys().cloned().zip(values).collect())
                    };
                    done.push(exit(node)?);
                }
            }
        }
        Ok(done.pop().expect("a rewrite leaves its root"))
    }

    /// Whether `self` and `other` are the same node: the same shared allocation for a string, list or object, an equal
    /// scalar otherwise.
    pub fn same_node(&self, other: &SExpression) -> bool {
        match (self, other) {
            (SExpression::Null, SExpression::Null) => true,
            (SExpression::Bool(a), SExpression::Bool(b)) => a == b,
            (SExpression::Number(a), SExpression::Number(b)) => a.to_bits() == b.to_bits(),
            (SExpression::String(a), SExpression::String(b)) => crate::value::Shared::ptr_eq(a, b),
            (SExpression::Array(a), SExpression::Array(b)) => crate::value::Shared::ptr_eq(a, b),
            (SExpression::Object(a), SExpression::Object(b)) => crate::value::Shared::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Whether any string in the tree, not under a key, satisfies `test`; read-only, without recursion.
    pub fn any_string(&self, mut test: impl FnMut(&str) -> bool) -> bool {
        let mut pending = vec![self];
        while let Some(node) = pending.pop() {
            match node {
                SExpression::String(text) => {
                    if test(text) {
                        return true;
                    }
                }
                SExpression::Array(items) => pending.extend(items.iter()),
                SExpression::Object(fields) => pending.extend(fields.values()),
                SExpression::Null | SExpression::Bool(_) | SExpression::Number(_) => {}
            }
        }
        false
    }

    /// Check if this is a null value
    pub fn is_null(&self) -> bool {
        matches!(self, SExpression::Null)
    }

    /// Serialize to a `serde_json::Value` with every object's keys sorted.
    ///
    /// `Object` is backed by a `HashMap`, whose iteration order is randomly
    /// seeded per process — so serializing an expression containing an object
    /// literal yields a different byte sequence on each run. Any emitter that
    /// bakes an expression into generated source needs byte-stable output:
    /// otherwise identical input produces a different file every build, which
    /// churns diffs and thrashes the L1 factory signature cache.
    ///
    /// Lives here rather than in a backend because every shell (TypeScript,
    /// Python, Rust, Android, iOS) that inlines a policy or guard needs the
    /// same guarantee.
    pub fn to_canonical_json(&self) -> serde_json::Value {
        match self {
            SExpression::Null => serde_json::Value::Null,
            SExpression::Bool(b) => serde_json::Value::Bool(*b),
            SExpression::Number(n) => serde_json::Number::from_f64(*n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            SExpression::String(s) => serde_json::Value::String(s.clone().into_inner()),
            SExpression::Array(items) => {
                serde_json::Value::Array(items.iter().map(Self::to_canonical_json).collect())
            }
            SExpression::Object(map) => {
                let mut keys: Vec<&crate::value::Shared<String>> = map.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for key in keys {
                    out.insert(key.to_string(), map[key].to_canonical_json());
                }
                serde_json::Value::Object(out)
            }
        }
    }

    /// Canonicalize the flat lambda form `["fn", p1, p2, …, body]` into the
    /// grouped form `["fn", [p1, p2, …], body]` throughout this expression tree.
    /// Single-param (`["fn","p",body]`) and already-grouped forms are left as-is.
    /// Idempotent. Removing the flat shape gives every downstream consumer
    /// (the arity validator, both codegen backends, the JS-runtime parsers) one
    /// canonical multi-param `fn` shape instead of three the sites disagree on.
    pub fn normalize_fn_shapes(&mut self) {
        match self {
            SExpression::Array(items) => {
                for child in items.iter_mut() {
                    child.normalize_fn_shapes();
                }
                // Flat form has op + ≥2 string params + a trailing body (len > 3),
                // every element between op and body being a bare param name.
                let is_flat_fn = items.len() > 3
                    && matches!(items.first(), Some(SExpression::String(op)) if op == "fn")
                    && items[1..items.len() - 1]
                        .iter()
                        .all(|p| matches!(p, SExpression::String(_)));
                if is_flat_fn {
                    let body = items.pop().expect("len > 3 guarantees a body");
                    let params: Vec<SExpression> = items.drain(1..).collect();
                    items.push(SExpression::Array(params.into()));
                    items.push(body);
                }
            }
            SExpression::Object(map) => {
                for v in map.values_mut() {
                    v.normalize_fn_shapes();
                }
            }
            _ => {}
        }
    }

    /// Try to get as a boolean
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            SExpression::Bool(b) => Some(*b),
            SExpression::Null => Some(false),
            SExpression::Number(n) => Some(*n != 0.0),
            SExpression::String(s) => Some(!s.is_empty()),
            SExpression::Array(a) => Some(!a.is_empty()),
            SExpression::Object(o) => Some(!o.is_empty()),
        }
    }

    /// Try to get as a number
    pub fn as_number(&self) -> Option<f64> {
        match self {
            SExpression::Number(n) => Some(*n),
            SExpression::Bool(true) => Some(1.0),
            SExpression::Bool(false) => Some(0.0),
            SExpression::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Try to get as a string
    pub fn as_str(&self) -> Option<&str> {
        match self {
            SExpression::String(s) => Some(s),
            _ => None,
        }
    }

    /// The string handle itself, shared with the parsed expression
    pub fn as_shared_str(&self) -> Option<&crate::value::Shared<String>> {
        match self {
            SExpression::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get as an array
    pub fn as_array(&self) -> Option<&Vec<SExpression>> {
        match self {
            SExpression::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Try to get as an object
    pub fn as_object(&self) -> Option<&crate::schema::sexpr::SObject> {
        match self {
            SExpression::Object(o) => Some(&**o),
            _ => None,
        }
    }

    /// Check if this is a binding reference (starts with @)
    pub fn is_binding(&self) -> bool {
        matches!(self, SExpression::String(s) if s.starts_with('@'))
    }

    /// Recognize the FC-5 dynamic-children tuple kept verbatim as IR:
    /// `["array/map", <collectionExpr>, ["fn", "<param>", <RenderUINode>]]`
    /// (or the grouped `["fn", ["<param>", …], <node>]` lambda form). This is
    /// the ONE mechanism for `children:`-position collection rendering — the
    /// existing renderItem lambda (`["fn", …]` + `@item` + splice) reused, no
    /// new node kind. Shared by the compiler's `@item`-scope validator and the
    /// codegen backends so they agree on the shape. Returns a borrowed view of
    /// the collection expression, the lambda parameter names, and the per-item
    /// render node; `None` for any other array shape.
    pub fn as_mapped_children(&self) -> Option<MappedChildren<'_>> {
        let SExpression::Array(arr) = self else {
            return None;
        };
        if arr.len() != 3 || arr[0].as_str() != Some("array/map") {
            return None;
        }
        let SExpression::Array(lambda) = &arr[2] else {
            return None;
        };
        if lambda.len() != 3 {
            return None;
        }
        match lambda[0].as_str() {
            Some("fn") => {}
            _ => return None,
        }
        let params: Vec<&str> = match &lambda[1] {
            SExpression::String(p) => vec![p.as_str()],
            SExpression::Array(ps) => ps.iter().filter_map(|p| p.as_str()).collect(),
            _ => return None,
        };
        if params.is_empty() {
            return None;
        }
        Some(MappedChildren {
            collection: &arr[1],
            params,
            item_node: &lambda[2],
        })
    }

    /// Recognize a conditional render node kept verbatim as IR:
    /// `["if", <cond>, <then>, <else>?]`, matching the runtime kernel's own
    /// arity (`kernel.rs` `"if"` arm — the else arm is optional). Returns the
    /// branches that are themselves render nodes; the condition is a value
    /// expression checked by the binding pass, never a pattern.
    ///
    /// Exists because a conditional is a legal `children:` entry — `std-browse`
    /// has shipped `(if @config.bodySearch <toolbar> <empty>)` as `children[0]`
    /// of its dense/feed/gallery/triage bodies for months and it renders — but
    /// `validate_pattern` reached the tuple through its catch-all arm and
    /// rejected it as `ORB_RUI_INVALID_PATTERN`. The bodies above only escaped
    /// because they are referenced from inside an `(if)` arm and never walked;
    /// a body referenced directly by a guarded transition (master-detail's
    /// VIEW/CLOSE) is walked, and tripped. Same shape as `as_mapped_children`:
    /// recognize the tuple, validate the render-bearing parts, keep the IR.
    pub fn as_conditional_node(&self) -> Option<ConditionalNode<'_>> {
        let SExpression::Array(arr) = self else {
            return None;
        };
        if !(arr.len() == 3 || arr.len() == 4) || arr[0].as_str() != Some("if") {
            return None;
        }
        Some(ConditionalNode {
            then_node: &arr[2],
            else_node: arr.get(3),
        })
    }

    /// Every possible value source of this expression, resolved through the
    /// operator registry's declared `returnSemantics` (see [`ValueWalk`]).
    /// A call whose operator declares a provenance kind yields the sources of
    /// the arguments that kind names; anything else (a literal, a binding, an
    /// undeclared operator) is an OPAQUE leaf — itself, crediting nothing
    /// beyond what it literally is.
    pub fn value_leaves(&self, walk: &ValueWalk<'_>) -> Vec<&SExpression> {
        let mut out = Vec::new();
        self.collect_value_leaves(walk, &mut out);
        out
    }

    /// Direct argument slots selected by the same declared provenance plan as `value_leaves`.
    pub fn value_source_slots(&self, walk: &ValueWalk<'_>) -> Option<Vec<ValueSlot>> {
        let op = self.operator()?;
        let args = self.arguments()?;
        plan_value_sources(
            walk,
            op,
            args.len(),
            |i| args[i].is_lambda_form(walk),
            args.first().is_some_and(SExpression::is_statically_false),
        )
    }

    /// The literal of a declared-knob read, `(let ((knob_<k> <literal> <declared type>)) @knob_<k>)`: the form the
    /// inline phase substitutes for a knob whose declared type the checker needs. Every reader of a substituted
    /// knob's value sees through it with this one helper.
    pub fn declared_knob_literal(&self) -> Option<&SExpression> {
        let [SExpression::String(head), SExpression::Array(bindings), SExpression::String(body)] =
            self.as_array()?.as_slice()
        else {
            return None;
        };
        let [SExpression::Array(binding)] = bindings.as_slice() else {
            return None;
        };
        let [SExpression::String(name), literal, SExpression::String(_)] = binding.as_slice() else {
            return None;
        };
        (head == "let"
            && name.starts_with("knob_")
            && body.strip_prefix('@') == Some(name.as_str()))
        .then_some(literal)
    }

    pub fn sequential_bindings(&self) -> Option<SequentialBindings<'_>> {
        if self.operator()? != "let" {
            return None;
        }
        SequentialBindings::from_args(self.arguments()?)
    }

    pub(super) fn collect_value_leaves<'a>(&'a self, walk: &ValueWalk<'_>, out: &mut Vec<&'a SExpression>) {
        if let Some(literal) = self.declared_knob_literal() {
            return literal.collect_value_leaves(walk, out);
        }
        let SExpression::Array(arr) = self else {
            out.push(self);
            return;
        };
        let Some(slots) = self.value_source_slots(walk) else {
            out.push(self);
            return;
        };
        let args = &arr[1..];
        for slot in slots {
            match slot {
                ValueSlot::Arg(i) => args[i].collect_value_leaves(walk, out),
                ValueSlot::LambdaBody(i) => {
                    if let Some(body) = args[i].lambda_body() {
                        body.collect_value_leaves(walk, out);
                    }
                }
            }
        }
    }

    /// The single argument this call passes through unchanged: the operator
    /// declares `identity-of-arg<N>`, so the result is arg N narrowed or
    /// reordered and anything true of arg N's elements is true of the result's.
    pub fn passthrough_arg(&self, walk: &ValueWalk<'_>) -> Option<&SExpression> {
        let SExpression::Array(arr) = self else {
            return None;
        };
        let op = arr.first()?.as_str()?;
        if !(walk.lookup)(op)?
            .semantics?
            .starts_with("identity-of-arg<")
        {
            return None;
        }
        let args = &arr[1..];
        match plan_value_sources(walk, op, args.len(), |_| false, false)?.as_slice() {
            [ValueSlot::Arg(i)] => Some(&args[*i]),
            _ => None,
        }
    }

    /// The body (last element) of a `[lambda-op, params, body]` array.
    pub(super) fn lambda_body(&self) -> Option<&SExpression> {
        match self {
            SExpression::Array(arr) if arr.len() >= 3 => arr.last(),
            _ => None,
        }
    }

    /// True when this is a `[lambda-op, params, body]` call whose operator
    /// declares `returnType: function`.
    pub(super) fn is_lambda_form(&self, walk: &ValueWalk<'_>) -> bool {
        self.lambda_body().is_some()
            && self
                .operator()
                .and_then(|op| (walk.lookup)(op))
                .is_some_and(|r| r.returns_function)
    }

    /// Mutable counterpart of [`SExpression::value_leaves`], for in-place
    /// rewrites (renames, codegen scope-qualification) that must reach
    /// descriptors nested behind every value source.
    pub fn value_leaves_mut(&mut self, walk: &ValueWalk<'_>) -> Vec<&mut SExpression> {
        let mut out = Vec::new();
        self.collect_value_leaves_mut(walk, &mut out);
        out
    }

    pub(super) fn collect_value_leaves_mut<'a>(
        &'a mut self,
        walk: &ValueWalk<'_>,
        out: &mut Vec<&'a mut SExpression>,
    ) {
        let plan = match &*self {
            SExpression::Array(arr) => arr.first().and_then(SExpression::as_str).and_then(|op| {
                let args = &arr[1..];
                plan_value_sources(
                    walk,
                    op,
                    args.len(),
                    |i| args[i].is_lambda_form(walk),
                    args.first().is_some_and(SExpression::is_statically_false),
                )
            }),
            _ => None,
        };
        let Some(slots) = plan else {
            out.push(self);
            return;
        };
        let SExpression::Array(arr) = self else {
            unreachable!("a plan exists only for SExpression::Array")
        };
        for (i, arg) in arr[1..].iter_mut().enumerate() {
            match slots.iter().find(|slot| slot.arg() == i) {
                Some(ValueSlot::Arg(_)) => arg.collect_value_leaves_mut(walk, out),
                Some(ValueSlot::LambdaBody(_)) => {
                    if let SExpression::Array(lambda) = arg {
                        if let Some(body) = lambda.last_mut() {
                            body.collect_value_leaves_mut(walk, out);
                        }
                    }
                }
                None => {}
            }
        }
    }

    /// Get the binding path if this is a binding reference
    pub fn binding_path(&self) -> Option<&str> {
        match self {
            SExpression::String(s) if s.starts_with('@') => Some(&s[1..]),
            _ => None,
        }
    }

    /// Check if this is a function call (array starting with a string operator)
    pub fn is_call(&self) -> bool {
        match self {
            SExpression::Array(arr) if !arr.is_empty() => {
                matches!(&arr[0], SExpression::String(_))
            }
            _ => false,
        }
    }

    /// Get the operator if this is a function call
    pub fn operator(&self) -> Option<&str> {
        match self {
            SExpression::Array(arr) if !arr.is_empty() => arr[0].as_str(),
            _ => None,
        }
    }

    /// Get the arguments if this is a function call
    pub fn arguments(&self) -> Option<&[SExpression]> {
        match self {
            SExpression::Array(arr) if !arr.is_empty() => Some(&arr[1..]),
            _ => None,
        }
    }

    /// True only when this expression is PROVABLY false without running the
    /// program — every leaf it depends on is already a literal.
    ///
    /// The motivating case is a guard whose `@config.<knob>` has been
    /// substituted at inline: `(when @config.running ...)` on a call site that
    /// set `running: false` folds to a literal `false`, and that arm can never
    /// fire. Any check that reasons about "can these two things happen together"
    /// has to know that, or it flags the very config knob authors are told to use
    /// to keep one owner per resource.
    ///
    /// A `string`-typed knob folds to its default the same way — a config knob
    /// declared `extraAddonEvent : string = ""` (the "this optional slot is
    /// unfulfilled" contract) substitutes to a literal empty string, not
    /// `Bool(false)`. A bare `(if @config.knob ...)` on that call site is
    /// exactly as dead as the boolean case, so every literal this crate's own
    /// evaluator treats as falsy (`Value::is_truthy`, evaluator/value.rs) is
    /// covered here too: `null`, `0`, `""`, and an empty object — not just
    /// `false`. One truthiness table, read by both the runtime and this
    /// static check, so they can never disagree about which literal is dead.
    ///
    /// Deliberately conservative: an expression this cannot decide returns
    /// `false` (meaning "not provably false"), never a guess. Comparisons against
    /// a binding, arithmetic, and anything unrecognised are all undecidable here.
    pub fn is_statically_false(&self) -> bool {
        match self {
            SExpression::Bool(false) => true,
            SExpression::Null => true,
            SExpression::Number(n) => *n == 0.0,
            SExpression::String(s) => s.is_empty(),
            SExpression::Object(obj) => obj.is_empty(),
            SExpression::Array(arr) => {
                let Some(op) = arr.first().and_then(|e| e.as_str()) else {
                    // No leading operator string — a literal array VALUE
                    // (not a call), so the same truthiness table applies:
                    // empty is falsy.
                    return arr.is_empty();
                };
                let args = &arr[1..];
                match op {
                    // Conjunction: one dead operand kills the whole guard.
                    "and" => args.iter().any(|a| a.is_statically_false()),
                    // Disjunction: dead only when every operand is dead.
                    "or" => !args.is_empty() && args.iter().all(|a| a.is_statically_false()),
                    "not" => args.len() == 1 && args[0].is_statically_true(),
                    // Two literals that differ can never be equal, and two that
                    // match can never be unequal.
                    "=" | "==" | "!=" => {
                        if args.len() != 2 || !args.iter().all(|a| a.is_literal()) {
                            return false;
                        }
                        (args[0] == args[1]) == (op == "!=")
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// True only when this expression is PROVABLY truthy without running the program: the dual of
    /// [`Self::is_statically_false`], under the same truthiness table and the same conservatism. A comparison of two
    /// literals that match (a `@config.mode` substituted to its call-site value) is the motivating case: the other
    /// branch of the `if` it guards can never run.
    pub fn is_statically_true(&self) -> bool {
        match self {
            SExpression::Bool(true) => true,
            SExpression::Number(n) => *n != 0.0,
            SExpression::String(s) => !s.is_empty() && !s.starts_with('@'),
            SExpression::Object(obj) => !obj.is_empty(),
            SExpression::Array(arr) => {
                let Some(op) = arr.first().and_then(|e| e.as_str()) else {
                    return !arr.is_empty();
                };
                let args = &arr[1..];
                match op {
                    "and" => !args.is_empty() && args.iter().all(|a| a.is_statically_true()),
                    "or" => args.iter().any(|a| a.is_statically_true()),
                    "not" => args.len() == 1 && args[0].is_statically_false(),
                    "=" | "==" | "!=" => {
                        if args.len() != 2 || !args.iter().all(|a| a.is_literal()) {
                            return false;
                        }
                        (args[0] == args[1]) == (op != "!=")
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// True when this node is a fully-known value — a literal, not a binding,
    /// call, or container. `@`-prefixed strings are bindings, not literals.
    pub(super) fn is_literal(&self) -> bool {
        match self {
            SExpression::Bool(_) | SExpression::Number(_) | SExpression::Null => true,
            SExpression::String(s) => !s.starts_with('@'),
            _ => false,
        }
    }

    /// Every `@entity.<field>` this node READS, by root field. A `set` target is a write, so
    /// only its value is walked (`collect_entity_field_refs` counts the target too).
    pub fn collect_entity_reads(&self, out: &mut HashSet<String>) {
        match self {
            SExpression::String(s) => {
                if let Some(path) = s.strip_prefix("@entity.") {
                    let field = path.split(['.', '[']).next().unwrap_or("");
                    if !field.is_empty() {
                        out.insert(field.to_string());
                    }
                }
            }
            SExpression::Array(arr) => {
                let is_set = matches!(
                    (arr.first(), arr.get(1)),
                    (Some(SExpression::String(op)), Some(SExpression::String(target)))
                        if op == "set" && target.starts_with("@entity.")
                );
                let skip = if is_set { 2 } else { 0 };
                arr.iter()
                    .skip(skip)
                    .for_each(|i| i.collect_entity_reads(out));
            }
            SExpression::Object(obj) => obj.values().for_each(|v| v.collect_entity_reads(out)),
            _ => {}
        }
    }

    /// Walk this S-expression collecting every `@entity.<field>` reference into
    /// `referenced_fields` (keyed by field name → the originating event, first
    /// reference wins) and every `["set", "@entity.<field>", ...]` setter call
    /// into `set_fields`. Recurses through arrays and object values. `id`,
    /// `createdAt`, and `updatedAt` are skipped as referenced fields (runtime-
    /// managed); the setter scan keeps them.
    ///
    /// The single canonical walker behind the explicit-binding "every read has a
    /// setter" check — used by both the lolo lowerer and the post-inline
    /// compiler validator.
    pub fn collect_entity_field_refs(
        &self,
        referenced_fields: &mut HashMap<String, String>,
        set_fields: &mut HashSet<String>,
        transition_event: &str,
    ) {
        match self {
            SExpression::String(s) if s.starts_with("@entity.") => {
                let path = &s[8..];
                let field = path.split('.').next().unwrap_or("");
                let field = field.split('[').next().unwrap_or(field);
                if !field.is_empty()
                    && field != "id"
                    && field != "createdAt"
                    && field != "updatedAt"
                {
                    referenced_fields
                        .entry(field.to_string())
                        .or_insert_with(|| transition_event.to_string());
                }
            }
            SExpression::Array(arr) => {
                if arr.len() >= 2 {
                    if let (SExpression::String(op), SExpression::String(target)) =
                        (&arr[0], &arr[1])
                    {
                        if op == "set" && target.starts_with("@entity.") {
                            let setter_path = &target[8..];
                            let setter_field = setter_path.split('.').next().unwrap_or("");
                            let setter_field =
                                setter_field.split('[').next().unwrap_or(setter_field);
                            if !setter_field.is_empty() {
                                set_fields.insert(setter_field.to_string());
                            }
                        }
                    }
                }
                for item in arr {
                    item.collect_entity_field_refs(referenced_fields, set_fields, transition_event);
                }
            }
            SExpression::Object(obj) => {
                for value in obj.values() {
                    value.collect_entity_field_refs(
                        referenced_fields,
                        set_fields,
                        transition_event,
                    );
                }
            }
            _ => {}
        }
    }
}
