//! A session's bound identity (EXEC-IDENTITY-1, -3, -4): `identity/sign-in` binds a sealed `Verified`'s subject to the
//! session, `identity/sign-out` clears it, and `@user` for a dispatch in a bound session is the program's `[identity]`
//! row of that subject.

use super::step_effects::push_with_completion;
use crate::effects::frame_delta::FrameIdentity;
use super::*;
use orbital_core_base::sealed::VERIFIED;
use orbital_core_base::viewer::{is_identity_effect, SIGN_IN};

/// What a session's `@user` is once it took part in the identity effects.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum IdentityBinding {
    /// `user` is a `[runtime]` identity's `@user`, kept in the binding so it is freed with the session; `None` reads
    /// the `[persistent]` identity's row from its store.
    Subject { subject: Arc<str>, user: Option<Value> },
    Anonymous,
}

/// Holds a step's harness identity; dropping it restores the identity the session had.
pub(crate) struct ReplayIdentity {
    identities: Arc<RwLock<HashMap<String, IdentityBinding>>>,
    session: String,
    previous: Option<IdentityBinding>,
}

impl Drop for ReplayIdentity {
    fn drop(&mut self) {
        let mut identities = self.identities.write();
        match self.previous.take() {
            Some(previous) => identities.insert(std::mem::take(&mut self.session), previous),
            None => identities.remove(&self.session),
        };
    }
}

/// The session key of the program session (ticks, boot, hooks).
const PROGRAM_SESSION: &str = "";

/// The program's `[identity]` entity, as the sign-in needs it.
struct IdentityDecl {
    name: String,
    runtime: bool,
    fields: Vec<String>,
}

enum Refusal {
    Suspended,
    Failed { kind: &'static str, error: String },
}

fn refused(kind: &'static str, error: impl Into<String>) -> Refusal {
    Refusal::Failed { kind, error: error.into() }
}

impl RuntimeKernel {
    fn identity_key(&self) -> Arc<str> {
        self.thread_session().unwrap_or_else(|| Arc::from(PROGRAM_SESSION))
    }

    /// The subject this thread's session is signed in as; `None` when it is signed out or never signed in.
    pub fn session_identity(&self) -> Option<Arc<str>> {
        match self.sessions.identities.read().get(&*self.identity_key()) {
            Some(IdentityBinding::Subject { subject, .. }) => Some(Arc::clone(subject)),
            _ => None,
        }
    }

    /// The identity a response's frame delta carries to the client (EXEC-IDENTITY-4); `None` on a client kernel (it
    /// mirrors, never owns) and when a persistent identity's row could not be read now (the next delta carries it).
    pub(super) fn mirrored_identity(&self) -> Option<FrameIdentity> {
        if self.effect_executor.environment().is_client() {
            return None;
        }
        let binding = self.sessions.identities.read().get(&*self.identity_key()).cloned();
        match binding {
            None | Some(IdentityBinding::Anonymous) => Some(FrameIdentity::default()),
            Some(IdentityBinding::Subject { user: Some(user), .. }) => Some(FrameIdentity { user: Some(user) }),
            Some(IdentityBinding::Subject { user: None, .. }) => {
                self.session_user().ok().map(|user| FrameIdentity { user: Some(user) })
            }
        }
    }

    /// A client kernel takes the server's bound identity as its own `@user` (EXEC-IDENTITY-4).
    pub(super) fn mirror_identity(&self, identity: &FrameIdentity) {
        let binding = match &identity.user {
            None => IdentityBinding::Anonymous,
            Some(user) => match user.get("id") {
                Some(Value::String(id)) if !id.is_empty() => {
                    IdentityBinding::Subject { subject: Arc::from(id.as_str()), user: Some(user.clone()) }
                }
                _ => {
                    tracing::error!(site = %self.effect_site(), "kernel: a mirrored identity names no `id`; keeping the held one");
                    return;
                }
            },
        };
        self.sessions.identities.write().insert(self.identity_key().to_string(), binding);
    }

    /// Whether this client kernel has not yet been told its session's identity by a server frame; the browser shows
    /// `pending` until then. A server kernel owns its identity and is never pending.
    pub fn identity_pending(&self) -> bool {
        self.effect_executor.environment().is_client() && !self.sessions.identities.read().contains_key(&*self.identity_key())
    }

    fn identity_decl(&self) -> Option<IdentityDecl> {
        let schemas = self.registry.schemas.read();
        schemas.values().find_map(|schema| {
            let def = orbital_core_base::policy::identity_entity_def(schema)?;
            Some(IdentityDecl {
                name: def.name.clone(),
                runtime: def.persistence == Some(Persistence::Runtime),
                fields: def.fields.iter().filter_map(|f| f.name.clone()).collect(),
            })
        })
    }

    /// The `@user` of the session this thread dispatches in (EXEC-IDENTITY-1): its bound `[identity]` row (read like any
    /// row, so a store read may suspend the run), else the anonymous viewer.
    pub(super) fn session_user(&self) -> Result<Value, Fault> {
        let binding = {
            let bound = self.sessions.identities.read();
            if bound.is_empty() {
                return Ok(self.sessions.anonymous.clone());
            }
            bound.get(&*self.identity_key()).cloned()
        };
        match binding {
            None | Some(IdentityBinding::Anonymous) => Ok(self.sessions.anonymous.clone()),
            Some(IdentityBinding::Subject { user: Some(user), .. }) => Ok(user),
            Some(IdentityBinding::Subject { subject, user: None }) => {
                let Some(decl) = self.identity_decl() else {
                    return Ok(subject_user(&subject, None));
                };
                let found = self.entity_source.find(&decl.name, &subject);
                if self.io.suspended() {
                    return Err(Fault::Suspended);
                }
                let row = found.map_err(KernelError::Internal)?;
                Ok(subject_user(&subject, row.as_deref()))
            }
        }
    }

    /// The `@user` of the session this thread is in, for a host that reads on its behalf (row visibility, row
    /// references); a read that cannot be answered in place is an error, never an empty viewer.
    pub fn session_viewer(&self) -> Result<Value, KernelError> {
        self.session_user().map_err(Fault::into_kernel)
    }

    /// The step harness's identity: `user` (its `id` the subject) is the bound identity of this thread's session until
    /// the guard drops. Only `program/step` reaches it, on the replay kernel; no program effect does.
    pub(crate) fn hold_replay_identity(&self, user: Value) -> Result<ReplayIdentity, String> {
        let subject = match user.get("id") {
            Some(Value::String(id)) if !id.is_empty() => Arc::from(id.as_str()),
            _ => return Err("a step identity needs a non-empty `id`".to_string()),
        };
        let session = self.identity_key().to_string();
        let binding = IdentityBinding::Subject { subject, user: Some(user) };
        let previous = self.sessions.identities.write().insert(session.clone(), binding);
        Ok(ReplayIdentity { identities: Arc::clone(&self.sessions.identities), session, previous })
    }

    /// Test only: bind `user` (its `id` the subject) as the identity of this thread's session, as a committed sign-in
    /// would. Production binds only through `identity/sign-in`.
    #[cfg(test)]
    pub(crate) fn bind_test_identity(&self, user: Value) {
        let Some(Value::String(subject)) = user.get("id").cloned() else {
            panic!("a test identity needs an id");
        };
        let binding = IdentityBinding::Subject { subject: Arc::from(subject.as_str()), user: Some(user) };
        self.sessions.identities.write().insert(self.identity_key().to_string(), binding);
    }

    /// `identity/sign-in` and `identity/sign-out`: a client kernel delegates them to the server (EXEC-IDENTITY-4), a
    /// server kernel runs them here. The recorded effect names its bindings, never the credential it resolved.
    pub(super) fn exec_identity(&self, effect: &Effect, ctx: &BindingContext, effect_results: &mut Vec<EffectResult>) {
        let Effect::Custom { name, emit, .. } = effect else {
            return;
        };
        debug_assert!(is_identity_effect(name));
        let Some(resolved) = resolved_custom_or_failed(effect, ctx, effect_results) else {
            return;
        };
        if self.effect_executor.environment().is_client() {
            effect_results.push(self.effect_executor.execute(&resolved, ctx));
            return;
        }
        let Effect::Custom { args, .. } = &resolved else {
            return;
        };
        let outcome = if name == SIGN_IN { self.sign_in(args) } else { self.sign_out(args) };
        let routes = emit.clone().unwrap_or_default();
        let (result, completion) = match outcome {
            Err(Refusal::Suspended) => return,
            Ok(subject) => {
                let payload = object_of([("subject", Value::string(subject))]);
                let completion = routes
                    .success
                    .clone()
                    .map(|event| EffectResult::executed(Effect::Emit { event, payload }));
                (EffectResult::executed(effect.clone()), completion)
            }
            Err(Refusal::Failed { kind, error }) => {
                tracing::error!(site = %self.effect_site(), effect = %name, kind, error = %error, "kernel: identity effect refused");
                let payload = object_of([("error", Value::string(error.as_str())), ("kind", Value::string(kind))]);
                let completion = routes
                    .failure
                    .clone()
                    .map(|event| EffectResult::executed(Effect::Emit { event, payload }));
                (EffectResult::failed(effect.clone(), &error), completion)
            }
        };
        push_with_completion(effect_results, result, completion);
    }

    /// The running step's identity change: it takes hold when the step commits ([`Self::commit_identity`]), so a step
    /// that suspends and replays starts from the identity it started from.
    fn stage_identity(&self, binding: IdentityBinding) {
        *self.step.identity_change.write() = Some((self.identity_key().to_string(), binding));
    }

    /// End the step: its identity change takes hold when `committed`, and is dropped otherwise.
    pub(super) fn settle_identity(&self, committed: bool) {
        let change = self.step.identity_change.write().take();
        if let (true, Some((session, binding))) = (committed, change) {
            self.sessions.identities.write().insert(session, binding);
        }
    }

    fn sign_in(&self, args: &[Value]) -> Result<String, Refusal> {
        let verified = match args {
            [value] if value.is_sealed_as(VERIFIED) => value,
            _ => {
                return Err(refused(
                    "unsealed",
                    "identity/sign-in accepts only a Verified, which only jws/verify makes",
                ))
            }
        };
        let subject = match verified.get("subject") {
            Some(Value::String(subject)) if !subject.is_empty() => subject.to_string(),
            _ => return Err(refused("unsealed", "the Verified names no subject")),
        };
        let decl = self
            .identity_decl()
            .ok_or_else(|| refused("no-identity", "the program declares no [identity] entity"))?;
        let user = if decl.runtime {
            let row = self.runtime_identity_row(&decl, &subject, verified);
            Some(subject_user(&subject, Some(&row)))
        } else {
            let found = self.entity_source.find(&decl.name, &subject);
            if self.io.suspended() {
                return Err(Refusal::Suspended);
            }
            match found {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(refused("no-identity", format!("{} has no row for subject {subject}", decl.name)));
                }
                Err(error) => return Err(refused("store", error)),
            }
            None
        };
        self.stage_identity(IdentityBinding::Subject { subject: Arc::from(subject.as_str()), user });
        Ok(subject)
    }

    fn sign_out(&self, args: &[Value]) -> Result<String, Refusal> {
        if !args.is_empty() {
            return Err(refused("invalid", "identity/sign-out takes no argument"));
        }
        let previous = self.session_identity();
        self.stage_identity(IdentityBinding::Anonymous);
        Ok(previous.map_or_else(|| orbital_core_base::viewer::ANONYMOUS.to_string(), |subject| subject.to_string()))
    }

    /// The row of a `[runtime]` identity: its declared defaults, `id` the subject, and each declared field a verified
    /// claim of the same name carries. It lives in the session's binding.
    fn runtime_identity_row(&self, decl: &IdentityDecl, subject: &str, verified: &Value) -> Value {
        let mut row = ValueMap::default();
        if let Some(Value::Array(claims)) = verified.get("claims") {
            for claim in claims.iter() {
                let (Some(Value::String(claim_name)), Some(value)) = (claim.get("name"), claim.get("value")) else {
                    continue;
                };
                if claim_name.as_str() != "id" && decl.fields.iter().any(|f| f == claim_name.as_str()) && !value.is_null() {
                    row.insert(claim_name.to_string().into(), value.clone());
                }
            }
        }
        row.insert("id".into(), Value::string(subject));
        let defaults = self.registry.entity_defaults.read().get(&decl.name).cloned().unwrap_or_default();
        defaults.merged(&SharedEntity::from_value_ref(&Value::object(row))).to_value()
    }
}

fn object_of<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::object(fields.into_iter().map(|(key, value)| (key.into(), value)).collect::<ValueMap>())
}

/// The anonymous viewer's `@user`.
pub(super) fn anonymous_user() -> Value {
    let anonymous = orbital_core_base::viewer::ANONYMOUS;
    object_of([("id", Value::string(anonymous)), ("role", Value::string(anonymous))])
}

/// `@user` for `subject`: its row's fields, `id` the subject; the subject alone when it has no row.
fn subject_user(subject: &str, row: Option<&Value>) -> Value {
    let mut user = match row {
        Some(Value::Object(fields)) => (**fields).clone(),
        _ => ValueMap::default(),
    };
    user.insert("id".into(), Value::string(subject));
    Value::object(user)
}
