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
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::shared_entity::SharedEntity;
use orbital_core_base::value::{Shared, Value};
use orbital_core_base::value::ValueMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// State of one `resolve_value` walk: the binding tokens being expanded (cycle bound) and the first typed
/// argument mismatch an embedded operator call raised.
#[derive(Default)]
struct Walk {
    expanding: Vec<String>,
    fault: Option<EvalError>,
}

/// Where an unseeded `prob/*` stream gets its seed: the kernel's journaled `Random` ask, supplied per dispatch.
#[derive(Clone)]
pub struct EntropySource(Arc<dyn Fn() -> Result<u32, String> + Send + Sync>);

impl EntropySource {
    pub fn new(source: impl Fn() -> Result<u32, String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(source))
    }
}

impl std::fmt::Debug for EntropySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EntropySource")
    }
}

/// How a cryptographic operator that draws entropy crosses into the kernel: the operator hands over its name, its
/// operands and a closure that runs it on 32 bytes of fresh entropy. The kernel runs the closure with entropy it draws
/// itself and records only the answer; a replay answers from the record without running the closure.
#[derive(Clone)]
pub struct SealedOutputs(Arc<dyn Fn(&str, &[Value], &mut dyn FnMut([u8; 32]) -> Value) -> Result<Value, String> + Send + Sync>);

impl SealedOutputs {
    pub fn new(
        source: impl Fn(&str, &[Value], &mut dyn FnMut([u8; 32]) -> Value) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(source))
    }
}

impl std::fmt::Debug for SealedOutputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealedOutputs")
    }
}

/// The `prob/*` stream of one evaluation: Mulberry32, the generator `@almadar/evaluator` ran, and the rejection flag
/// `prob/condition` raises inside a `prob/posterior` / `prob/infer` sample. Shared by every child context, so a
/// `prob/sample` loop and a lambda body advance one stream.
#[derive(Debug, Default)]
pub struct ProbState {
    state: AtomicU32,
    started: AtomicBool,
    rejected: AtomicBool,
}

/// JavaScript `n | 0`: truncate toward zero, then wrap to 32 bits; NaN and the infinities are 0.
fn to_int32(n: f64) -> u32 {
    if !n.is_finite() {
        return 0;
    }
    n.trunc().rem_euclid(4_294_967_296.0) as u32
}

impl ProbState {
    /// Restart the stream from `n` (`prob/seed`).
    pub fn seed(&self, n: f64) {
        self.state.store(to_int32(n), Ordering::Relaxed);
        self.started.store(true, Ordering::Relaxed);
    }

    /// The next value in [0, 1). An unseeded stream asks `entropy` for its seed once; with no source it is an error
    /// naming `prob/seed`.
    pub fn next(&self, entropy: Option<&EntropySource>) -> Result<f64, String> {
        if !self.started.load(Ordering::Relaxed) {
            let Some(source) = entropy else {
                return Err("prob/*: the stream has no seed and this evaluation has no entropy source; call (prob/seed n) first".to_string());
            };
            let seed = (source.0)()?;
            self.state.store(seed, Ordering::Relaxed);
            self.started.store(true, Ordering::Relaxed);
        }
        let mut t = self
            .state
            .fetch_add(0x6D2B_79F5, Ordering::Relaxed)
            .wrapping_add(0x6D2B_79F5);
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        Ok(f64::from(t ^ (t >> 14)) / 4_294_967_296.0)
    }

    /// Mark the sample in flight rejected (`prob/condition`).
    pub fn reject(&self) {
        self.rejected.store(true, Ordering::Relaxed);
    }

    /// Whether the sample in flight was rejected; the flag is cleared.
    pub fn take_rejected(&self) -> bool {
        self.rejected.swap(false, Ordering::Relaxed)
    }

    /// Put back the flag an enclosing sample had raised.
    pub fn restore_rejected(&self, rejected: bool) {
        self.rejected.store(rejected, Ordering::Relaxed);
    }
}

/// Binding context for S-expression evaluation
///
/// Provides access to runtime values during expression evaluation.
#[derive(Debug, Clone, Default)]
pub struct BindingContext {
    /// Entity data (`@entity`), held field by field as the kernel keeps it
    pub entity: SharedEntity,
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
    pub prev_events: crate::util::SharedLog<std::sync::Arc<Value>>,
    /// `@prevStates`: states this trait left this dispatch.
    pub prev_states: crate::util::SharedLog<String>,
    /// `@callsitePayload`: the payload of the transition that composed this trait into its embedder
    /// (`Value::Null` until composed).
    pub callsite_payload: Arc<Value>,
    /// `@fromState` / `@toState`: the transition's ends.
    pub from_state: Option<Arc<str>>,
    pub to_state: Option<Arc<str>>,
    /// Local bindings (`let`, callback params, `@item`/`@index`): a chain of shared frames, so a child scope
    /// pushes one frame instead of copying every enclosing binding.
    locals: Arc<LocalFrame>,
    /// Custom bindings for entity types (for `count` queries).
    ///
    /// Shared via `Arc` so cloning a child context (hot path in map/filter/reduce)
    /// is a refcount bump rather than a deep clone of every entity collection.
    entity_stores: Arc<HashMap<String, Vec<Value>>>,
    /// Resolve a whole-string binding that holds no value to an absent prop (see
    /// [`Self::resolve_dropping_unset`]) instead of leaving its token for a later resolver.
    drop_unset: bool,
    /// Credential values the kernel resolved for this evaluation, by credential name. Only an operator that takes a
    /// credential slot (`hash/scrypt`'s salt) reads them; they are never reachable as a binding.
    secrets: Arc<SecretValues>,
    /// The `prob/*` stream and rejection flag, shared with every child of this context.
    prob: Arc<ProbState>,
    /// The dispatch's seed source for an unseeded `prob/*` stream; `None` in a pure evaluation.
    prob_entropy: Option<EntropySource>,
    /// The kernel's boundary for operators that draw entropy and must not journal it; `None` in a pure evaluation.
    sealed: Option<SealedOutputs>,
    /// A local the scope reads at most once (a reduce accumulator its body reads once): the read moves it out,
    /// so a write to it (`array/append @acc …`) is in place instead of a copy of the whole accumulator.
    linear: Option<Arc<LinearLocal>>,
}

#[derive(Debug)]
pub(crate) struct LinearLocal {
    name: String,
    slot: std::sync::Mutex<Option<Value>>,
}

/// A local's name: written in the language (`let`, `fn` params, std names like `item`), so held without copying.
#[derive(Debug, Clone)]
pub enum LocalName {
    Fixed(&'static str),
    Shared(Shared<String>),
    Owned(String),
}

impl LocalName {
    pub fn as_str(&self) -> &str {
        match self {
            LocalName::Fixed(name) => name,
            LocalName::Shared(name) => name.as_str(),
            LocalName::Owned(name) => name,
        }
    }
}

impl From<Shared<String>> for LocalName {
    fn from(name: Shared<String>) -> Self {
        LocalName::Shared(name)
    }
}

impl From<&str> for LocalName {
    fn from(name: &str) -> Self {
        LocalName::Owned(name.to_owned())
    }
}

impl From<&Shared<String>> for LocalName {
    fn from(name: &Shared<String>) -> Self {
        LocalName::Shared(name.clone())
    }
}

impl From<&String> for LocalName {
    fn from(name: &String) -> Self {
        LocalName::from(name.as_str())
    }
}

/// One scope's local bindings over its parent's. `None` marks a binding removed in this scope.
#[derive(Debug, Clone, Default)]
struct LocalFrame {
    vars: Vec<(LocalName, Option<Arc<Value>>)>,
    parent: Option<Arc<LocalFrame>>,
}

impl LocalFrame {
    fn get(&self, name: &str) -> Option<&Value> {
        let mut frame = self;
        loop {
            if let Some((_, value)) = frame.vars.iter().find(|(key, _)| key.as_str() == name) {
                return value.as_deref();
            }
            frame = frame.parent.as_deref()?;
        }
    }

    fn put(&mut self, name: LocalName, value: Option<Arc<Value>>) {
        match self.vars.iter_mut().find(|(key, _)| key.as_str() == name.as_str()) {
            Some(slot) => slot.1 = value,
            None => self.vars.push((name, value)),
        }
    }
}

/// The `let` bindings in scope at one point of a step, for a resolution that runs after the step to see them again.
#[derive(Debug, Clone)]
pub struct LocalScope(Arc<LocalFrame>);

/// The credential values of one evaluation. `Debug` shows only the names.
#[derive(Clone, Default, Debug)]
pub struct SecretValues(HashMap<String, orbital_core_base::secret::Secret<String>>);

impl BindingContext {
    /// This context with the credential `name` holding `value` for the operators that take a credential slot.
    pub fn with_secret(mut self, name: &str, value: impl Into<orbital_core_base::secret::Secret<String>>) -> Self {
        let mut held = (*self.secrets).clone();
        held.0.insert(name.to_string(), value.into());
        self.secrets = Arc::new(held);
        self
    }

    /// The value of the credential `name` the kernel resolved for this evaluation.
    pub fn secret(&self, name: &str) -> Option<&str> {
        self.secrets.0.get(name).map(|value| value.expose().as_str())
    }

    /// Run `op` over `args` through the kernel's sealed boundary: `compute` is given 32 bytes of fresh entropy that
    /// never reach the journal, and the answer it returns is what the journal records and a replay returns. Without a
    /// boundary the operation fails with a message.
    pub fn sealed(&self, op: &str, args: &[Value], mut compute: impl FnMut([u8; 32]) -> Value) -> Result<Value, OrbitalError> {
        let source = self.sealed.as_ref().ok_or_else(|| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!("{op}: this evaluation has no sealed-output boundary")))
        })?;
        (source.0)(op, args, &mut compute).map_err(|message| OrbitalError::Eval(EvalError::InvalidArgument(message)))
    }

    /// Give this evaluation the kernel's sealed boundary.
    pub fn set_sealed_outputs(&mut self, source: SealedOutputs) -> &mut Self {
        self.sealed = Some(source);
        self
    }

    /// The `prob/*` stream and rejection flag of this evaluation.
    pub fn prob(&self) -> &ProbState {
        &self.prob
    }

    /// The next `prob/*` value in [0, 1), or the error an unseeded stream without an entropy source raises.
    pub fn prob_next(&self) -> Result<f64, OrbitalError> {
        self.prob
            .next(self.prob_entropy.as_ref())
            .map_err(|message| OrbitalError::Eval(EvalError::InvalidArgument(message)))
    }

    /// Give the unseeded `prob/*` stream of this dispatch its seed source (the kernel's journaled `Random` ask).
    pub fn set_prob_entropy(&mut self, source: EntropySource) -> &mut Self {
        self.prob_entropy = Some(source);
        self
    }

    /// Restart the `prob/*` stream (`prob/seed`).
    pub fn prob_seed(&self, n: f64) {
        self.prob.seed(n);
    }

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
            entity: SharedEntity::whole(entity),
            ..Default::default()
        }
    }

    /// Set the entity value
    pub fn set_entity(&mut self, entity: Value) -> &mut Self {
        self.entity = SharedEntity::whole(entity);
        self
    }

    /// Bind the entity as the kernel holds it: no field is copied.
    pub fn set_entity_shared(&mut self, entity: SharedEntity) -> &mut Self {
        self.entity = entity;
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
    pub fn set_local(&mut self, name: impl Into<LocalName>, value: Value) -> &mut Self {
        let name = name.into();
        self.drop_linear(name.as_str());
        self.own_frame().put(name, Some(Arc::new(value)));
        self
    }

    /// Bind a local to a value another binding already holds, without copying it.
    pub fn set_local_shared(&mut self, name: impl Into<LocalName>, value: Arc<Value>) -> &mut Self {
        let name = name.into();
        self.drop_linear(name.as_str());
        self.own_frame().put(name, Some(value));
        self
    }

    /// Remove a local binding
    pub fn remove_local(&mut self, name: &str) -> Option<Value> {
        let removed = self.locals.get(name).cloned();
        self.drop_linear(name);
        if removed.is_some() {
            self.own_frame().put(name.into(), None);
        }
        removed
    }

    /// This context's own innermost frame: written in place when no other context shares it, else a fresh frame
    /// pushed over the shared chain (which stays untouched for every context holding it).
    fn own_frame(&mut self) -> &mut LocalFrame {
        if Arc::get_mut(&mut self.locals).is_none() {
            let parent = Arc::clone(&self.locals);
            self.locals = Arc::new(LocalFrame { vars: Vec::new(), parent: Some(parent) });
        }
        Arc::make_mut(&mut self.locals)
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

    /// The `let` bindings in scope here; `None` outside every `let`.
    pub fn local_scope(&self) -> Option<LocalScope> {
        let mut frame = Some(&*self.locals);
        while let Some(f) = frame {
            if !f.vars.is_empty() {
                return Some(LocalScope(Arc::clone(&self.locals)));
            }
            frame = f.parent.as_deref();
        }
        None
    }

    /// This context with `scope`'s `let` bindings in scope instead of its own.
    pub fn with_local_scope(&self, scope: &LocalScope) -> Self {
        let mut ctx = self.child();
        ctx.locals = Arc::clone(&scope.0);
        ctx
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
            locals: Arc::clone(&self.locals),
            entity_stores: Arc::clone(&self.entity_stores),
            drop_unset: self.drop_unset,
            secrets: Arc::clone(&self.secrets),
            prob: Arc::clone(&self.prob),
            prob_entropy: self.prob_entropy.clone(),
            sealed: self.sealed.clone(),
            linear: self.linear.clone(),
        }
    }

    /// Bind `name` to `value` for a single read: the read takes it. Only for a local the scope reads once
    /// and never rebinds (the caller checks); a local of the same name in an inner scope still wins.
    pub fn set_linear_local(&mut self, name: &str, value: Value) -> &mut Self {
        if self.locals.get(name).is_some() {
            self.own_frame().put(name.into(), None);
        }
        self.linear = Some(Arc::new(LinearLocal { name: name.to_string(), slot: std::sync::Mutex::new(Some(value)) }));
        self
    }

    fn drop_linear(&mut self, name: &str) {
        if self.linear.as_ref().is_some_and(|l| l.name == name) {
            self.linear = None;
        }
    }

    /// The linear local `name`, moved out, once.
    pub(super) fn take_linear(&self, name: &str) -> Option<Value> {
        let linear = self.linear.as_ref().filter(|l| l.name == name)?;
        linear.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Create a child context with a specific item bound (for map/filter)
    pub fn with_item(&self, item: Value) -> Self {
        let mut child = self.child();
        child.set_local(crate::evaluator::LocalName::Fixed("item"), item);
        child
    }

    /// Create a child context with index and item bound (for map with index)
    pub fn with_item_and_index(&self, item: Value, index: usize) -> Self {
        let mut child = self.child();
        child.set_local(crate::evaluator::LocalName::Fixed("item"), item);
        child.set_local(crate::evaluator::LocalName::Fixed("index"), Value::Number(index as f64));
        child
    }
}

use super::operators;

mod builder;
mod resolve;
pub use builder::BindingContextBuilder;

#[cfg(test)]
mod tests;
