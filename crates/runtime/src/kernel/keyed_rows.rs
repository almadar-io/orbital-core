//! Rows addressed by a listener's `per` clause (EXEC-STEP-1a/1b): resolving the key, creating a row on first use,
//! and dropping a row once it is quiet.

use super::*;
use orbital_core_base::envelope::DELIVERY_REJECTED;
use std::collections::BTreeMap;

/// A keyed ask the row's step deferred and the host has not completed yet: the routes that end it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DeferredAsk {
    pub(super) key: String,
    pub(super) routes: Vec<String>,
}

pub(super) const ROW_FRAME_SEPARATOR: char = '\u{1e}';
pub(super) const ROW_INIT_EVENT: &str = "INIT";

impl RuntimeKernel {
    fn entity_type_of(&self, trait_name: &str) -> String {
        self.linked_entity_of(trait_name)
            .unwrap_or_else(|| trait_name.to_string())
    }

    /// The row a `per` clause names: its expression over the raw source payload, a non-empty string.
    pub(super) fn resolve_listen_key(
        &self,
        key: &ListenKey,
        source_payload: &Value,
    ) -> Result<String, String> {
        let ctx = BindingContextBuilder::new()
            .payload(source_payload.clone())
            .build();
        match Evaluator::new().evaluate(&key.expr, &ctx) {
            Ok(value) => match value.as_str() {
                Some(row) if !row.is_empty() => Ok(row.to_string()),
                _ => Err(format!(
                    "the `per` key evaluated to {value:?}, not a non-empty string"
                )),
            },
            Err(error) => Err(format!("the `per` key did not evaluate: {error}")),
        }
    }

    /// Whether `trait_name` holds no instance for `row` in the current session yet.
    pub(super) fn keyed_instance_missing(&self, trait_name: &str, row: &str) -> bool {
        self.get_state(trait_name, Some(row)).is_none()
    }

    /// Create the row of `trait_name`'s entity named `row` (its `id` is the key, every other field a declared
    /// default) unless the entity already holds it.
    pub(super) fn create_keyed_row(&self, trait_name: &str, row: &str) -> Result<(), Fault> {
        let entity_type = self.entity_type_of(trait_name);
        let found = self.entity_source.find(&entity_type, row);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        if found.map_err(KernelError::Internal)?.is_some() {
            return Ok(());
        }
        let id: ValueMap = ValueMap::from_iter([("id".into(), Value::string(row.to_string()))]);
        let created = self.entity_source.create(&entity_type, &Value::object(id));
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        created.map_err(KernelError::Internal)?;
        Ok(())
    }

    /// Queue the `DELIVERY_REJECTED` of a delivery whose key was refused for the emitter's declared listeners.
    pub(super) fn refuse_key(
        &self,
        item: &CascadeItem,
        key: &ListenKey,
        reason: &str,
        out: &mut Vec<CascadeItem>,
    ) -> Result<(), KernelError> {
        let refused = self.key_refusal(&item.trait_name, &item.event);
        let before = out.len();
        let source = self.emit_source(&key.emitter);
        self.collect_listener_targets(
            &source,
            &key.emitter,
            DELIVERY_REJECTED,
            &refused.payload(),
            &[],
            out,
        );
        if out.len() == before {
            return Err(KernelError::PayloadInvalid(format!(
                "delivery of '{}' to {} refused ({}, envelope {}): {reason}; no trait listens for {DELIVERY_REJECTED}",
                refused.event,
                item.trait_name,
                refused.reason.as_str(),
                refused.id,
            )));
        }
        Ok(())
    }

    fn non_default_fields(entity: &SharedEntity) -> BTreeMap<String, Value> {
        entity
            .fields()
            .into_iter()
            .flat_map(|fields| fields.iter())
            .filter(|(name, value)| name.as_str() != "id" && !value.is_null())
            .map(|(name, value)| (name.to_string(), (**value).clone()))
            .collect()
    }

    /// Whether the row of `trait_name` named `row` is quiet (EXEC-STEP-1b): its trait is in `initial`, its fields
    /// are the declared defaults, and it has no held delivery, no deferred ask and no tick of its own.
    pub(super) fn keyed_row_is_quiet(&self, trait_name: &str, row: &str) -> bool {
        let Some(def) = self
            .registry
            .trait_definitions
            .read()
            .get(trait_name)
            .cloned()
        else {
            return false;
        };
        if def.ticks.as_ref().is_some_and(|ticks| !ticks.is_empty()) {
            return false;
        }
        let initial =
            orbital_core_eval::state_machine::core::StateMachineCore::get_trait_initial_state(&def);
        if initial != Some(self.current_state_name(trait_name, Some(row)).as_str()) {
            return false;
        }
        if self
            .mount
            .read()
            .held
            .get(trait_name)
            .is_some_and(|held| held.iter().any(|i| i.row.as_deref() == Some(row)))
        {
            return false;
        }
        if self
            .deferred_asks
            .read()
            .get(&(trait_name.to_string(), self.session_eid(row)))
            .is_some_and(|asks| !asks.is_empty())
        {
            return false;
        }
        let entity_type = self.entity_type_of(trait_name);
        let defaults = self
            .registry
            .entity_defaults
            .read()
            .get(&entity_type)
            .cloned()
            .unwrap_or_default();
        Self::non_default_fields(&self.read_trait_entity(trait_name, &entity_type, row))
            == Self::non_default_fields(&defaults)
    }

    /// The envelope `trait_name` stamps on the emits of its step on `row`: the trait's own, plus the row when it is keyed.
    pub(super) fn emit_source_on(&self, trait_name: &str, row: &str) -> BusEventSource {
        let mut source = self.emit_source(trait_name);
        if self.is_keyed_row(trait_name, row) {
            source.instance_key = Some(row.to_string());
        }
        source
    }

    /// Whether `row` of `trait_name` is addressed by key: its trait has a `per` listener and `row` is not the singleton.
    pub(super) fn is_keyed_row(&self, trait_name: &str, row: &str) -> bool {
        row != RUNTIME_SINGLETON && self.listener_manager.is_keyed(trait_name)
    }

    /// Drop the row once the dispatch has nothing more queued for it and it is quiet (EXEC-STEP-1b).
    pub(super) fn drop_quiet_row(
        &self,
        queued_in: &[&[CascadeItem]],
        queue: &VecDeque<CascadeItem>,
        default_row: &str,
        trait_name: &str,
        row: &str,
    ) -> Result<(), Fault> {
        let on_row = |i: &CascadeItem| {
            i.trait_name == trait_name && i.row.as_deref().unwrap_or(default_row) == row
        };
        let queued =
            queue.iter().any(on_row) || queued_in.iter().any(|items| items.iter().any(on_row));
        if queued || !self.keyed_row_is_quiet(trait_name, row) {
            return Ok(());
        }
        self.drop_keyed_row(trait_name, row)
    }

    /// Drop the row of `trait_name` named `row`: its state, frame and entity row. The entity row stays while another
    /// session still holds an instance of it.
    pub(super) fn drop_keyed_row(&self, trait_name: &str, row: &str) -> Result<(), Fault> {
        let entity_type = self.entity_type_of(trait_name);
        let frame = self.row_frame_key(trait_name, row);
        self.removed_rows
            .write()
            .push((self.thread_session().map(|s| s.to_string()), frame));
        let state_key = self.state_key(Some(row));
        self.state_manager
            .write()
            .remove_trait_instance(trait_name, state_key.as_deref());
        let slot = self.frame_slot(trait_name, &entity_type, row);
        self.frames.write().remove(&slot);
        self.carried_rows.write().remove(&slot);
        let held_elsewhere = self
            .state_manager
            .read()
            .get_instances_for_trait(trait_name)
            .iter()
            .any(|state| {
                state
                    .entity_id
                    .as_deref()
                    .is_some_and(|eid| eid.split(SESSION_SEPARATOR).next() == Some(row))
            });
        if held_elsewhere {
            return Ok(());
        }
        let deleted = self.entity_source.delete(&entity_type, row);
        if self.io.suspended() {
            return Err(Fault::Suspended);
        }
        deleted.map_err(KernelError::Internal)?;
        Ok(())
    }

    /// The frame a step of `trait_name` on `row` writes: the trait's own, or for a keyed row the trait's qualified by it.
    pub(super) fn row_frame_key(&self, trait_name: &str, row: &str) -> String {
        let frame = self.trait_frame_key(trait_name);
        if self.is_keyed_row(trait_name, row) {
            format!("{frame}{ROW_FRAME_SEPARATOR}{row}")
        } else {
            frame
        }
    }

    /// The keyed rows the current session dropped since its client was last told, as frames to remove (EXEC-DIST-7).
    pub(super) fn take_removed_rows(&self) -> Vec<String> {
        let session = self.thread_session().map(|s| s.to_string());
        let mut removed = self.removed_rows.write();
        let (mine, rest): (Vec<_>, Vec<_>) =
            removed.drain(..).partition(|(owner, _)| *owner == session);
        *removed = rest;
        mine.into_iter().map(|(_, frame)| frame).collect()
    }

    /// Record a keyed ask the step in flight deferred, to be ended by an event of `routes` carrying its `key`.
    pub(super) fn note_deferred_ask(&self, key: Option<&str>, routes: Vec<String>) {
        let Some(key) = key else { return };
        let Some((trait_name, _)) = self.step.step_site.read().clone() else {
            return;
        };
        let row = self
            .step
            .step_frame
            .read()
            .clone()
            .unwrap_or_else(|| RUNTIME_SINGLETON.to_string());
        let mut asks = self.deferred_asks.write();
        let list = asks
            .entry((trait_name, self.session_eid(&row)))
            .or_default();
        let ask = DeferredAsk {
            key: key.to_string(),
            routes,
        };
        if !list.contains(&ask) {
            list.push(ask);
        }
    }

    /// An event of `trait_name` on `row` ends the deferred asks it answers.
    pub(super) fn settle_deferred_asks(
        &self,
        trait_name: &str,
        row: &str,
        event: &str,
        payload: &Value,
    ) {
        let mut asks = self.deferred_asks.write();
        let slot = (trait_name.to_string(), self.session_eid(row));
        let Some(list) = asks.get_mut(&slot) else {
            return;
        };
        let key = payload
            .get_path("key")
            .and_then(|k| k.as_str().map(str::to_string));
        list.retain(|ask| {
            !(ask.routes.iter().any(|r| r == event) && key.as_deref() == Some(ask.key.as_str()))
        });
        if list.is_empty() {
            asks.remove(&slot);
        }
    }
}
