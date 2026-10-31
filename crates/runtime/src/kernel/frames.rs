//! Trait frames: how a trait's row is keyed, read, written, snapshotted and restored, and its state.

use super::*;

impl RuntimeKernel {
    /// The key of the frame `trait_name` reads: its own name, or the shared entity's group key.
    pub fn trait_frame_key(&self, trait_name: &str) -> String {
        let entity_type = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        self.frame_slot(trait_name, &entity_type, "").0
    }

    /// Whether `trait_name` reads its entity's one `[shared]` frame.
    pub fn reads_shared_frame(&self, trait_name: &str) -> bool {
        let entity_type = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        self.registry.entity_definitions
            .read()
            .get(&entity_type)
            .is_some_and(|d| d.shared)
    }

    /// `(frame key, entity id)` of `trait_name`'s frame (Runtime Spec Clause 2.3).
    pub(super) fn frame_slot(&self, trait_name: &str, entity_type: &str, entity_id: &str) -> (String, String) {
        let shared = self
            .registry.entity_definitions
            .read()
            .get(entity_type)
            .is_some_and(|d| d.shared);
        let key = if shared {
            format!("$shared::{entity_type}")
        } else {
            trait_name.to_string()
        };
        (key, self.session_eid(entity_id))
    }

    /// The `@intrinsic` field names of `entity_type` when it is `[shared]` and declares any: those fields are each
    /// trait's own frame (G-ORB-540), never the one `$shared::<Entity>` frame.
    pub(super) fn shared_intrinsic_fields(&self, entity_type: &str) -> Option<Vec<String>> {
        let defs = self.registry.entity_definitions.read();
        let def = defs.get(entity_type).filter(|d| d.shared)?;
        let names = crate::entity_access::intrinsic_field_names(def);
        (!names.is_empty()).then_some(names)
    }

    /// The slot holding `trait_name`'s own `@intrinsic` fields of a `[shared]` entity: the trait's own frame key.
    pub(super) fn own_frame_slot(&self, trait_name: &str, entity_id: &str) -> (String, String) {
        (trait_name.to_string(), self.session_eid(entity_id))
    }

    /// `fields` split into what the `[shared]` frame holds and what `trait_name`'s own frame holds.
    pub(super) fn split_intrinsic(&self, entity_type: &str, fields: &SharedEntity) -> Option<(SharedEntity, SharedEntity)> {
        let intrinsic = self.shared_intrinsic_fields(entity_type)?;
        let all = fields.fields()?;
        let (mut shared, mut own) = (SharedEntity::empty_object(), SharedEntity::empty_object());
        for (name, value) in all.iter() {
            let side = if intrinsic.iter().any(|n| n == name.as_str()) { &mut own } else { &mut shared };
            side.set_field(name.clone(), (**value).clone());
        }
        Some((shared, own))
    }

    /// The key a trait instance's state is held under: its frame's session-folded entity id (the one fold, so state and
    /// frames cannot drift apart). `None` stays `None` in the program session.
    pub(super) fn state_key<'a>(&self, entity_id: Option<&'a str>) -> Option<std::borrow::Cow<'a, str>> {
        match self.thread_session() {
            None => entity_id.map(std::borrow::Cow::Borrowed),
            Some(_) => Some(std::borrow::Cow::Owned(self.session_eid(entity_id.unwrap_or(RUNTIME_SINGLETON)))),
        }
    }

    /// The instance's own entity id when it belongs to the current session, else `None`.
    pub(crate) fn session_instance_id(&self, held_key: Option<&str>) -> Option<Option<String>> {
        let suffix = self.thread_session().map(|s| session_suffix(&s));
        match (held_key, suffix) {
            (Some(key), Some(suffix)) => key.strip_suffix(suffix.as_str()).map(|id| Some(id.to_string())),
            (Some(key), None) if key.contains(SESSION_SEPARATOR) => None,
            (key, None) => Some(key.map(str::to_string)),
            (None, Some(_)) => None,
        }
    }

    /// `entity_id` as the current session's frames key it: unchanged in the program session.
    pub(super) fn session_eid(&self, entity_id: &str) -> String {
        match self.thread_session() {
            Some(session) => format!("{entity_id}{}", session_suffix(&session)),
            None => entity_id.to_string(),
        }
    }

    /// A trait's `@entity`: declared field defaults, then its row, then its frame.
    pub(super) fn read_trait_entity(
        &self,
        trait_name: &str,
        entity_type: &str,
        entity_id: &str,
    ) -> SharedEntity {
        let defaults = self
            .registry.entity_defaults
            .read()
            .get(entity_type)
            .cloned()
            .unwrap_or_default();
        let slot = self.frame_slot(trait_name, entity_type, entity_id);
        let row_id = self.carried_rows.read().get(&slot).cloned();
        let row = self
            .entity_source
            .read(entity_type, row_id.as_deref().unwrap_or(store_row_id(entity_id)))
            .ok()
            .map(|row| SharedEntity::from_value_ref(&row))
            .unwrap_or_default();
        let frame = self.frames.read().get(&slot).cloned().unwrap_or_default();
        let entity = defaults.merged(&row).merged(&frame);
        if self.shared_intrinsic_fields(entity_type).is_some() {
            let own = self
                .frames
                .read()
                .get(&self.own_frame_slot(trait_name, entity_id))
                .cloned()
                .unwrap_or_default();
            return entity.merged(&own);
        }
        entity
    }

    /// The `@entity` a step of `trait_name` would read now, for the cascade's visit key.
    pub(super) fn step_entity_view(&self, trait_name: &str, entity_id: &str) -> Value {
        self.step_frame(trait_name, entity_id).to_value()
    }

    /// The trait's `@entity` as the step about to run reads it, as the kernel holds it.
    pub(super) fn step_frame(&self, trait_name: &str, entity_id: &str) -> SharedEntity {
        let entity_type = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        self.read_trait_entity(trait_name, &entity_type, entity_id)
    }

    /// Merge `set` writes into a trait's frame; the row store is never touched.
    pub(super) fn write_trait_frame(
        &self,
        trait_name: &str,
        entity_type: &str,
        entity_id: &str,
        writes: &SharedEntity,
    ) {
        let split = self.split_intrinsic(entity_type, writes);
        let own_slot = split.is_some().then(|| self.own_frame_slot(trait_name, entity_id));
        let (shared_writes, own_writes) = match &split {
            Some((shared, own)) => (shared, Some(own)),
            None => (writes, None),
        };
        let slot = self.frame_slot(trait_name, entity_type, entity_id);
        let mut frames = self.frames.write();
        if shared_writes.fields().is_some_and(|f| !f.is_empty()) || own_writes.is_none() {
            let merged = frames
                .get(&slot)
                .cloned()
                .unwrap_or_default()
                .merged(shared_writes);
            frames.insert(slot, merged);
        }
        if let (Some(own_slot), Some(own_writes)) = (own_slot, own_writes) {
            if own_writes.fields().is_some_and(|f| !f.is_empty()) {
                let merged = frames
                    .get(&own_slot)
                    .cloned()
                    .unwrap_or_default()
                    .merged(own_writes);
                frames.insert(own_slot, merged);
            }
        }
    }

    /// Capture a trait instance's current state + entity frame, for
    /// `RuntimeOptimistic` rollback (`restore_trait`) when a caller's
    /// optimistic local commit disagrees with (or errors against) the
    /// server's response.
    ///
    /// Lock order (documented once, honored by every kernel method that reads
    /// more than one of these): **`state_manager` before `trait_definitions`
    /// before `entity_source`** — the order `process_event` already
    /// establishes (its `state_manager.write()` guard is held across the
    /// `trait_definitions.read()` and `entity_source.read()`/`write()` calls
    /// inside it). `snapshot_trait`/`restore_trait` never hold more than one
    /// of these at a time (each `.read()`/`.write()` call is a temporary,
    /// dropped before the next is taken), so they trivially satisfy the order
    /// and can never deadlock against a concurrent `dispatch`/`process_event`.
    pub fn snapshot_trait(&self, trait_name: &str, entity_id: Option<&str>) -> TraitSnapshot {
        let eid = entity_id.unwrap_or(RUNTIME_SINGLETON);
        let state_key = self.state_key(Some(eid));
        let state = self
            .state_manager
            .read()
            .get_state(trait_name, state_key.as_deref())
            .map(|s| s.current_state.clone())
            .unwrap_or_default();
        let entity_type = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        let slot = self.frame_slot(trait_name, &entity_type, eid);
        let frames = self.frames.read();
        let shared = frames.get(&slot);
        let own = self
            .shared_intrinsic_fields(&entity_type)
            .and_then(|_| frames.get(&self.own_frame_slot(trait_name, eid)));
        let entity = match (shared, own) {
            (None, None) => Value::Null,
            (Some(shared), None) => shared.to_value(),
            (None, Some(own)) => own.to_value(),
            (Some(shared), Some(own)) => shared.merged(own).to_value(),
        };
        TraitSnapshot { state, entity }
    }

    /// Restore a trait instance to a prior [`TraitSnapshot`] — the
    /// `RuntimeOptimistic` rollback path. Write-through in the SAME lock order
    /// `snapshot_trait` reads them (`state_manager` then `entity_source`,
    /// `trait_definitions` read only to resolve the entity type). A REPLACE of
    /// the entity, not the usual merge `EntitySource::write` performs — the
    /// optimistic commit may have ADDED fields since the snapshot that the
    /// rollback must remove, which a merge would silently keep.
    ///
    /// A bound trait's render of the rolled-back entity is stale from here: the next
    /// [`Self::rerender_stale`] repaints it (the web repaints from the restored store snapshot).
    /// Restore itself consumes nothing, so a caller with no response to carry it loses no repaint.
    pub fn restore_trait(
        &self,
        trait_name: &str,
        entity_id: Option<&str>,
        snapshot: TraitSnapshot,
    ) {
        let eid = entity_id.unwrap_or(RUNTIME_SINGLETON);
        if !snapshot.state.is_empty() {
            let mut state_manager = self.state_manager.write();
            let state_key = self.state_key(Some(eid));
            if state_manager
                .get_or_create_state(trait_name, state_key.as_deref())
                .is_ok()
            {
                if let Some(state) = state_manager.get_state_mut(trait_name, state_key.as_deref()) {
                    state.transition_to(&snapshot.state);
                }
            }
        }
        let entity_type = self
            .registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| d.linked_entity.clone())
            .unwrap_or_else(|| trait_name.to_string());
        // REPLACE the frame, never merge: the optimistic commit may have added
        // fields a merge would keep. A `Null` snapshot means there was no frame.
        let slot = self.frame_slot(trait_name, &entity_type, eid);
        let restored = SharedEntity::from_value(snapshot.entity);
        let split = self.split_intrinsic(&entity_type, &restored).or_else(|| {
            (restored.is_null() && self.shared_intrinsic_fields(&entity_type).is_some())
                .then(|| (SharedEntity::empty_object(), SharedEntity::empty_object()))
        });
        let mut frames = self.frames.write();
        if let Some((shared, own)) = split {
            let own_slot = self.own_frame_slot(trait_name, eid);
            for (slot, part) in [(slot, shared), (own_slot, own)] {
                if part.fields().is_some_and(|f| !f.is_empty()) {
                    frames.insert(slot, part);
                } else {
                    frames.remove(&slot);
                }
            }
        } else if restored.is_null() {
            frames.remove(&slot);
        } else {
            frames.insert(slot, restored);
        }
    }

    /// Every registered trait's state (its initial one while untouched) and derived dispatch mode.
    pub fn trait_snapshot(
        &self,
        entity_id: Option<&str>,
    ) -> BTreeMap<String, (String, DispatchMode)> {
        let defs: Vec<Arc<TraitDefinition>> =
            self.registry.trait_definitions.read().values().cloned().collect();
        defs.iter()
            .map(|def| {
                let state = self.current_state_name(&def.name, entity_id);
                (def.name.clone(), (state, self.dispatch_mode_of(def)))
            })
            .collect()
    }

    pub fn get_state(&self, trait_name: &str, entity_id: Option<&str>) -> Option<TraitState> {
        let state_key = self.state_key(Some(entity_id.unwrap_or(RUNTIME_SINGLETON)));
        self.state_manager
            .read()
            .get_state(trait_name, state_key.as_deref())
            .cloned()
    }

    /// Reset a trait instance to its initial state.
    pub fn reset_state(&self, trait_name: &str, entity_id: Option<&str>) {
        let state_key = self.state_key(Some(entity_id.unwrap_or(RUNTIME_SINGLETON)));
        let _ = self
            .state_manager
            .write()
            .reset_trait(trait_name, state_key.as_deref());
    }

    /// The state a trait instance is in now, or the one it will be created in; empty for an unknown trait.
    pub(super) fn current_state_name(&self, trait_name: &str, entity_id: Option<&str>) -> String {
        let state_key = self.state_key(entity_id);
        if let Some(state) = self.state_manager.read().get_state(trait_name, state_key.as_deref()) {
            return state.current_state.clone();
        }
        self.registry.trait_definitions
            .read()
            .get(trait_name)
            .and_then(|d| {
                orbital_core_eval::state_machine::core::StateMachineCore::get_trait_initial_state(d)
            })
            .map(str::to_string)
            .unwrap_or_default()
    }

    /// Get the initial state for a trait.
    pub fn get_initial_state(&self, trait_name: &str) -> Option<String> {
        let schemas = self.registry.schemas.read();

        for schema in schemas.values() {
            for orbital in &schema.orbitals {
                for trait_ref in &orbital.traits {
                    if let Some(name) = trait_ref.trait_name() {
                        if name == trait_name {
                            if let TraitReference::Inline(trait_def) = trait_ref {
                                let sm = &trait_def.state_machine;
                                for state in &sm.states {
                                    if state.is_initial.unwrap_or(false) {
                                        return Some(state.name.clone());
                                    }
                                }
                                if let Some(first_state) = sm.states.first() {
                                    return Some(first_state.clone().name);
                                }
                            }
                        }
                    }
                }
            }
        }

        None
    }
}
