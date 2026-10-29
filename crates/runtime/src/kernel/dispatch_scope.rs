//! Dispatch-scoped state: the stamp, i18n and entropy, and the viewer, session, user and locale guards.

use super::*;

/// Clears the dispatch stamp when the outermost entry that set it returns (or unwinds).
pub(super) struct DispatchStamp {
    pub(super) slot: Option<Arc<RwLock<Option<f64>>>>,
    pub(super) now: f64,
}

#[derive(Debug, Default, Clone)]
pub(super) struct KernelI18n {
    pub(super) locales: Vec<String>,
    pub(super) catalogs: HashMap<String, HashMap<String, String>>,
}

/// A `prob/*` seed source asking the kernel's host for four journaled random bytes.
pub(super) fn entropy_from(io: Arc<IoPort>) -> EntropySource {
    EntropySource::new(move || {
        let raw = io.random(4)?;
        let bytes: [u8; 4] = raw.as_slice().try_into().map_err(|_| {
            format!(
                "prob/*: the host answered {} entropy bytes, not 4",
                raw.len()
            )
        })?;
        Ok(u32::from_le_bytes(bytes))
    })
}

/// Holds a request thread's viewer; dropping it forgets the viewer.
pub struct ViewerGuard {
    pub(super) slot: Arc<RwLock<HashMap<orbital_core_base::sync::ThreadKey, Arc<Value>>>>,
    pub(super) thread: orbital_core_base::sync::ThreadKey,
    pub(super) outer: Option<Arc<Value>>,
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        let mut slot = self.slot.write();
        match self.outer.take() {
            Some(outer) => slot.insert(self.thread, outer),
            None => slot.remove(&self.thread),
        };
    }
}

/// Holds a request thread's session; dropping it restores the outer one, and a scratch session's frames go with it.
pub struct SessionGuard {
    pub(super) slot: Arc<RwLock<HashMap<orbital_core_base::sync::ThreadKey, Arc<str>>>>,
    pub(super) thread: orbital_core_base::sync::ThreadKey,
    pub(super) outer: Option<Arc<str>>,
    pub(super) scratch: Option<(
        Arc<RwLock<HashMap<(String, String), SharedEntity>>>,
        Arc<RwLock<HashMap<(String, String), String>>>,
        Arc<RwLock<StateMachineManager>>,
        Arc<view_record::ViewRecords>,
        String,
    )>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        {
            let mut slot = self.slot.write();
            match self.outer.take() {
                Some(outer) => slot.insert(self.thread, outer),
                None => slot.remove(&self.thread),
            };
        }
        if let Some((frames, carried, states, views, session)) = self.scratch.take() {
            drop_session_instances(&frames, &carried, &states, &session);
            views.drop_session(&session);
        }
    }
}

pub(super) fn drop_session_instances(
    frames: &RwLock<HashMap<(String, String), SharedEntity>>,
    carried: &RwLock<HashMap<(String, String), String>>,
    states: &RwLock<StateMachineManager>,
    session: &str,
) {
    let suffix = session_suffix(session);
    frames.write().retain(|(_, eid), _| !eid.ends_with(&suffix));
    carried.write().retain(|(_, eid), _| !eid.ends_with(&suffix));
    states
        .write()
        .retain_instances(|state| !state.entity_id.as_deref().is_some_and(|eid| eid.ends_with(&suffix)));
}

pub(super) const SESSION_SEPARATOR: char = '\u{1f}';

pub(super) fn session_suffix(session: &str) -> String {
    format!("{SESSION_SEPARATOR}{session}")
}

/// Puts a run's viewer in the kernel's `@user` slot for one unit and restores the previous one.
pub(super) struct UserHold {
    pub(super) slot: Arc<RwLock<Value>>,
    pub(super) previous: Value,
}

impl Drop for UserHold {
    fn drop(&mut self) {
        *self.slot.write() = std::mem::replace(&mut self.previous, Value::Null);
    }
}

/// Holds a dispatch's viewer locale; dropping it clears the slot. JS twin: `OrbitalEventRequest.locale`.
pub struct LocaleGuard {
    pub(super) slot: Arc<RwLock<Option<String>>>,
}

impl Drop for LocaleGuard {
    fn drop(&mut self) {
        *self.slot.write() = None;
    }
}

impl Drop for DispatchStamp {
    fn drop(&mut self) {
        if let Some(slot) = &self.slot {
            *slot.write() = None;
        }
    }
}
