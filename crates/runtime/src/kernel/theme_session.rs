//! A session's bound theme (G-CROSS-395), on the identity path: `theme/set` binds a declared theme key to the session,
//! `theme/clear` returns it to its declared theme, `@currentTheme` for a dispatch in a bound session is that key ahead of
//! every declared theme. The kernel the step runs on binds it (`runsOn: any`); a server binding reaches the client in its
//! frames, and the client mirrors it into the same session map.

use super::step_effects::push_with_completion;
use super::*;
use crate::effects::frame_delta::FrameTheme;
use orbital_core_base::theme::{declared_theme_keys, resolve_declared_key};
use orbital_core_base::viewer::{is_theme_effect, THEME_SET};

/// The session key of the program session (ticks, boot, hooks).
const PROGRAM_SESSION: &str = "";

struct Refused {
    kind: &'static str,
    error: String,
}

fn refused(kind: &'static str, error: impl Into<String>) -> Refused {
    Refused { kind, error: error.into() }
}

impl RuntimeKernel {
    fn theme_session_key(&self) -> Arc<str> {
        self.thread_session().unwrap_or_else(|| Arc::from(PROGRAM_SESSION))
    }

    /// The theme key this thread's session is bound to; `None` when none is bound and the declared themes apply.
    pub fn session_theme(&self) -> Option<Arc<str>> {
        self.sessions.themes.read().get(&*self.theme_session_key()).cloned()
    }

    /// The theme a response's frame delta carries to the client; `None` on a client kernel (it mirrors, never owns).
    pub(super) fn mirrored_theme(&self) -> Option<FrameTheme> {
        if self.effect_executor.environment().is_client() {
            return None;
        }
        Some(FrameTheme { key: self.session_theme().map(|key| key.to_string()) })
    }

    /// A client kernel takes the server's bound theme as its own.
    pub(super) fn mirror_theme(&self, theme: &FrameTheme) {
        let session = self.theme_session_key().to_string();
        let mut themes = self.sessions.themes.write();
        match &theme.key {
            Some(key) => themes.insert(session, Arc::from(key.as_str())),
            None => themes.remove(&session),
        };
    }

    /// The theme keys the registered programs declare: each app's `theme` and `themes`, and each orbital's own
    /// `theme`, a registry key as written and each mode of a theme defined inline. Only these ship a stylesheet
    /// (ORB_THEME_UNDECLARED), so only these may be bound.
    fn declared_theme_keys(&self) -> Vec<String> {
        self.registry.schemas.read().values().flat_map(|schema| declared_theme_keys(schema)).collect()
    }

    /// The app-level theme a session with no bound key reads.
    fn declared_app_theme(&self) -> String {
        let mode = *self.presentation.theme_mode.read();
        let presets = self.presentation.theme_presets.read();
        match self.registry.schemas.read().values().next() {
            Some(schema) => resolve_theme(schema, None, &presets, mode).key,
            None => resolve_theme_default(&presets, mode.unwrap_or_default()).key,
        }
    }

    /// `@currentTheme` from the session's bound key, when one is bound: it overrides every declared theme.
    pub(super) fn session_resolved_theme(&self) -> Option<ResolvedTheme> {
        let key = self.session_theme()?;
        let schemas = self.registry.schemas.read();
        Some(resolve_declared_key(schemas.values().map(|schema| &**schema), &self.presentation.theme_presets.read(), &key))
    }

    /// `theme/set` and `theme/clear` (`runsOn: any`): the kernel the step runs on binds the key for its session, so a
    /// client trait repaints without a round trip and a server trait's binding reaches the client in its frames.
    pub(super) fn exec_theme(&self, effect: &Effect, ctx: &BindingContext, effect_results: &mut Vec<EffectResult>) {
        let Effect::Custom { name, emit, .. } = effect else {
            return;
        };
        debug_assert!(is_theme_effect(name));
        let Some(resolved) = resolved_custom_or_failed(effect, ctx, effect_results) else {
            return;
        };
        let Effect::Custom { args, .. } = &resolved else {
            return;
        };
        let outcome = if name == THEME_SET { self.theme_set(args) } else { self.theme_clear(args) };
        let routes = emit.clone().unwrap_or_default();
        let (result, completion) = match outcome {
            Ok(theme) => {
                let payload = theme_payload([("theme", Value::string(theme))]);
                let completion = routes.success.clone().map(|event| EffectResult::executed(Effect::Emit { event, payload }));
                (EffectResult::executed(effect.clone()), completion)
            }
            Err(Refused { kind, error }) => {
                tracing::error!(site = %self.effect_site(), effect = %name, kind, error = %error, "kernel: theme effect refused");
                let payload = theme_payload([("error", Value::string(error.as_str())), ("kind", Value::string(kind))]);
                let completion = routes.failure.clone().map(|event| EffectResult::executed(Effect::Emit { event, payload }));
                (EffectResult::failed(effect.clone(), &error), completion)
            }
        };
        push_with_completion(effect_results, result, completion);
    }

    /// End the step: its theme change takes hold when `committed`, and is dropped otherwise.
    pub(super) fn settle_theme(&self, committed: bool) {
        let change = self.step.theme_change.write().take();
        if let (true, Some((session, key))) = (committed, change) {
            let mut themes = self.sessions.themes.write();
            match key {
                Some(key) => themes.insert(session, key),
                None => themes.remove(&session),
            };
        }
    }

    fn stage_theme(&self, key: Option<Arc<str>>) {
        *self.step.theme_change.write() = Some((self.theme_session_key().to_string(), key));
    }

    fn theme_set(&self, args: &[Value]) -> Result<String, Refused> {
        let key = match args {
            [Value::String(key)] => key.to_string(),
            _ => return Err(refused("invalid", "theme/set takes one theme key string")),
        };
        if key.is_empty() || !self.declared_theme_keys().contains(&key) {
            return Err(refused(
                "undeclared-theme",
                format!("theme/set: `{key}` is not the app's `theme`, an orbital's `theme` or in the app's `themes`"),
            ));
        }
        self.stage_theme(Some(Arc::from(key.as_str())));
        Ok(key)
    }

    fn theme_clear(&self, args: &[Value]) -> Result<String, Refused> {
        if !args.is_empty() {
            return Err(refused("invalid", "theme/clear takes no argument"));
        }
        self.stage_theme(None);
        Ok(self.declared_app_theme())
    }
}

fn theme_payload<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::object(fields.into_iter().map(|(key, value)| (key.into(), value)).collect::<ValueMap>())
}
