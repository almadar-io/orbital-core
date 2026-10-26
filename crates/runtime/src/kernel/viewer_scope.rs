//! Who is viewing: the user, session, locale, theme, and which rows they may read.

use super::*;

impl RuntimeKernel {
    pub fn set_current_user(&self, user: Value) {
        *self.current_user.write() = user;
    }

    /// Dispatch as `user` from this thread until the guard drops: the dispatches this thread starts keep this `@user`
    /// however other requests set theirs meanwhile.
    pub fn enter_viewer(&self, user: Value) -> ViewerGuard {
        let thread = orbital_core_base::sync::current_thread();
        let outer = self.viewer_by_thread.write().insert(thread, Arc::new(user));
        ViewerGuard {
            slot: Arc::clone(&self.viewer_by_thread),
            thread,
            outer,
        }
    }

    /// Dispatch in `session` from this thread until the guard drops (EXEC-DIST-9): the frames this thread's dispatches
    /// read and write are that session's.
    pub fn enter_session(&self, session: &str) -> SessionGuard {
        let thread = orbital_core_base::sync::current_thread();
        let outer = self.session_by_thread.write().insert(thread, Arc::from(session));
        SessionGuard { slot: Arc::clone(&self.session_by_thread), thread, outer, scratch: None }
    }

    /// A session of its own for one request that names no client: its frames are dropped when the guard drops.
    pub fn enter_scratch_session(&self) -> SessionGuard {
        let id = format!(
            "scratch-{}",
            self.scratch_sessions.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut guard = self.enter_session(&id);
        guard.scratch = Some((Arc::clone(&self.frames), Arc::clone(&self.carried_rows), Arc::clone(&self.state_manager), id));
        guard
    }

    /// The session this thread entered; `None` is the program session (ticks, boot, hooks, in-program asks).
    pub fn thread_session(&self) -> Option<Arc<str>> {
        self.session_by_thread
            .read()
            .get(&orbital_core_base::sync::current_thread())
            .cloned()
    }

    /// Free `session`'s trait states and frames and mark it ended (EXEC-DIST-9): a request but a mount to it is refused
    /// until a mount revives it.
    pub fn free_session(&self, session: &str) {
        drop_session_instances(&self.frames, &self.carried_rows, &self.state_manager, session);
        self.ended_sessions.write().insert(session.to_string());
    }

    /// Whether `session` was freed and not revived since.
    pub fn session_ended(&self, session: &str) -> bool {
        self.ended_sessions.read().contains(session)
    }

    /// Open an ended `session` again: a mount that carries the client's own copy rebuilds it.
    pub fn revive_session(&self, session: &str) {
        self.ended_sessions.write().remove(session);
    }

    /// The sessions that hold a frame now, the program session excluded.
    pub fn frame_sessions(&self) -> Vec<String> {
        let mut sessions: Vec<String> = self
            .frames
            .read()
            .keys()
            .filter_map(|(_, eid)| eid.split_once(SESSION_SEPARATOR).map(|(_, s)| s.to_string()))
            .collect();
        sessions.sort();
        sessions.dedup();
        sessions
    }

    /// The viewer this thread entered, for a dispatch that starts now; a host that answers a call later, on another
    /// thread, re-enters it there.
    pub fn thread_viewer(&self) -> Option<Arc<Value>> {
        self.viewer_by_thread
            .read()
            .get(&orbital_core_base::sync::current_thread())
            .cloned()
    }

    /// Get the current user.
    /// The dispatch clock: the configured one, else the wall clock.
    pub(crate) fn clock_now(&self) -> f64 {
        match &self.config.clock {
            Some(clock) => clock(),
            None => orbital_core_base::time_util::now_millis_f64(),
        }
    }

    /// Stamp `now` once for the dispatch this call starts; a nested call reuses the outer stamp.
    pub(super) fn begin_dispatch(&self) -> DispatchStamp {
        let mut slot = self.dispatch_now.write();
        match *slot {
            Some(now) => DispatchStamp { slot: None, now },
            None => {
                let now = self.clock_now();
                *slot = Some(now);
                DispatchStamp {
                    slot: Some(Arc::clone(&self.dispatch_now)),
                    now,
                }
            }
        }
    }

    pub fn get_current_user(&self) -> Value {
        self.current_user.read().clone()
    }

    /// Install the program's declared locales and merged catalogs (`orb resolve`'s `<stem>.<locale>.json`).
    pub fn set_messages(
        &self,
        locales: Vec<String>,
        catalogs: HashMap<String, HashMap<String, String>>,
    ) {
        *self.i18n.write() = KernelI18n { locales, catalogs };
    }

    /// Run the next dispatches as the viewer's `locale` until the guard drops. A program
    /// that declares locales refuses one it does not declare; one without accepts any.
    pub fn enter_locale(&self, locale: Option<&str>) -> Result<LocaleGuard, KernelError> {
        if let Some(locale) = locale {
            let i18n = self.i18n.read();
            if !i18n.locales.is_empty() && !i18n.locales.iter().any(|l| l == locale) {
                return Err(KernelError::Internal(format!(
                    "locale \"{locale}\" is not one of the program's declared locales [{}]",
                    i18n.locales.join(", ")
                )));
            }
        }
        *self.dispatch_locale.write() = locale.map(str::to_string);
        Ok(LocaleGuard {
            slot: Arc::clone(&self.dispatch_locale),
        })
    }

    /// `@locale` and its catalog for `i18n/t`: the dispatch's viewer locale, else the first declared.
    pub(super) fn with_viewer_i18n(&self, builder: BindingContextBuilder) -> BindingContextBuilder {
        let i18n = self.i18n.read();
        let locale = self
            .dispatch_locale
            .read()
            .clone()
            .or_else(|| i18n.locales.first().cloned());
        match locale {
            Some(locale) => {
                let messages = i18n.catalogs.get(&locale).cloned().unwrap_or_default();
                builder.locale(Value::string(locale)).messages(messages)
            }
            None => builder,
        }
    }

    /// Set the kernel's tracked light/dark toggle. Overrides the mode
    /// encoded in a declared theme's key suffix on every subsequent
    /// `@currentTheme` resolution (`resolve_theme`'s `requested_mode`).
    pub fn set_theme_mode(&self, mode: ThemeMode) {
        *self.theme_mode.write() = Some(mode);
    }

    /// Get the kernel's tracked light/dark toggle, defaulting to `Light`
    /// when no explicit mode was ever set. External-facing convenience
    /// (status/introspection); theme RESOLUTION uses the raw `Option`
    /// directly (below) so an unset toggle still lets `resolve_theme` honor
    /// a declared theme key's own `-light`/`-dark` suffix instead of this
    /// default silently overriding it.
    pub fn get_theme_mode(&self) -> ThemeMode {
        self.theme_mode.read().unwrap_or_default()
    }

    /// Resolve the app-level `@currentTheme` (orbital precedence not
    /// applied — see [`Self::resolve_current_theme_for_trait`] for the
    /// per-trait resolution `process_event`/`process_tick` bind into
    /// render-ui). Snapshot-facing: the FFI's `orbital_kernel_snapshot`
    /// surfaces this as `"theme"`.
    pub fn resolved_theme(&self) -> ResolvedTheme {
        let mode = *self.theme_mode.read();
        match self.schemas.read().values().next() {
            Some(schema) => resolve_theme(schema, None, &self.theme_presets.read(), mode),
            None => resolve_theme_default(&self.theme_presets.read(), mode.unwrap_or_default()),
        }
    }

    /// Resolve `@currentTheme` for the orbital that owns `trait_name`,
    /// applying orbital → app → default precedence (`resolve_theme`). Walks
    /// the trait→orbital index rebuilt by [`Self::register_schema`].
    pub(super) fn resolve_current_theme_for_trait(&self, trait_name: &str) -> ResolvedTheme {
        let mode = *self.theme_mode.read();
        let homes = self.trait_homes.read();
        if let Some((schema_name, index)) = homes.get(trait_name) {
            let schemas = self.schemas.read();
            if let Some(schema) = schemas.get(schema_name) {
                if let Some(orbital) = schema.orbitals.get(*index) {
                    return resolve_theme(
                        schema,
                        Some(&orbital.name),
                        &self.theme_presets.read(),
                        mode,
                    );
                }
            }
        }
        resolve_theme_default(&self.theme_presets.read(), mode.unwrap_or_default())
    }

    /// Narrow `rows` to the ones the CURRENT USER may read, using the entity's
    /// declared `@read` policy.
    ///
    /// Exists so a caller measuring "how much does this persona see" measures the
    /// same thing a fetch would. Counting straight off the entity store answers a
    /// different question — the unfiltered store size, identical for every persona —
    /// which is how `--all-personas` came to report symmetric results for correctly
    /// ownership-scoped apps (`S-C2-ASYMMETRY-BLIND-TO-OWNERSHIP-ONLY-SCOPING`).
    ///
    /// Reuses `apply_read_access`, the one owner of read filtering, rather than
    /// re-deriving the predicate.
    pub fn retain_readable(&self, entity_type: &str, rows: &mut Vec<Value>) {
        self.retain_readable_by(entity_type, rows, &self.get_current_user());
    }

    /// [`Self::retain_readable`] for the viewer `user`, whatever the kernel's current user is.
    pub fn retain_readable_by(&self, entity_type: &str, rows: &mut Vec<Value>, user: &Value) {
        let policy = self
            .entity_policies
            .read()
            .get(entity_type)
            .and_then(|p| p.read.clone());
        if policy.is_none() {
            return;
        }
        let mut ctx = BindingContext::new();
        ctx.set_user(user.clone());
        apply_read_access(rows, policy.as_ref(), None, &ctx, entity_type);
    }

    /// Whether `viewer` may read the row a live item carries: the `@read` policy of the entity the emitting trait is
    /// bound to (the JS twin: `isRowVisibleToViewer`). An item whose entity does not resolve, or whose entity declares no
    /// policy, is visible to everyone.
    pub fn live_item_visible_to(
        &self,
        item: &crate::effects::LiveBroadcastItem,
        viewer: &Value,
    ) -> bool {
        let Some(entity) = item
            .source
            .r#trait
            .as_deref()
            .and_then(|t| self.linked_entity_of(t))
        else {
            return true;
        };
        let row = match &item.payload {
            Some(Value::Object(fields)) => Value::Object(fields.clone()),
            _ => Value::object(ValueMap::default()),
        };
        let mut rows = vec![row];
        self.retain_readable_by(&entity, &mut rows, viewer);
        !rows.is_empty()
    }
}
