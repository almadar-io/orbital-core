//! The client view record (K5, EXEC-DIST-7): per session, the frames the kernel last sent and a version. A response
//! carries the diff of what its dispatch wrote against the record, then the record advances; `frames_since` answers
//! a client that fell behind with a snapshot. The record is the one reconciliation owner: a client applies ops only at
//! the version they start from and never merges rows by its own rules.

use super::*;
use crate::effects::frame_delta::{FrameDelta, FrameOp};
use orbital_core_base::collections::IndexMap;
use orbital_core_base::sync::Mutex;

/// The session of dispatches that name no client (ticks, boot, the client-role kernel's own responses).
pub const PROGRAM_SESSION: &str = "$program";

/// What a dispatch did to one frame.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameWrite {
    /// These fields, absolute; the frame's other fields stay.
    Fields(ValueMap),
    /// These fields and no others.
    Replaced(ValueMap),
    Removed,
}

#[derive(Default)]
struct ViewRecord {
    version: u64,
    frames: IndexMap<String, SharedEntity>,
}

impl ViewRecord {
    fn diff(&mut self, writes: Vec<(String, FrameWrite)>) -> Vec<FrameOp> {
        let mut ops = Vec::new();
        for (frame, write) in writes {
            match write {
                FrameWrite::Removed => {
                    if self.frames.shift_remove(&frame).is_some() {
                        ops.push(FrameOp::Del { frame });
                    }
                }
                FrameWrite::Replaced(fields) => {
                    let whole = SharedEntity::from_value_ref(&Value::object(fields.clone()));
                    let same = self
                        .frames
                        .get(&frame)
                        .is_some_and(|held| held.to_value() == whole.to_value());
                    if !same {
                        if self.frames.insert(frame.clone(), whole).is_some() {
                            ops.push(FrameOp::Del {
                                frame: frame.clone(),
                            });
                        }
                        ops.push(FrameOp::Put { frame, fields });
                    }
                }
                FrameWrite::Fields(fields) => match self.frames.get_mut(&frame) {
                    None => {
                        self.frames.insert(
                            frame.clone(),
                            SharedEntity::from_value_ref(&Value::object(fields.clone())),
                        );
                        ops.push(FrameOp::Put { frame, fields });
                    }
                    Some(held) => {
                        let mut changed = ValueMap::default();
                        for (field, value) in fields {
                            if held.get(&field) != Some(&value) {
                                held.set_field(field.clone(), value.clone());
                                changed.insert(field, value);
                            }
                        }
                        FrameOp::push_set(&mut ops, frame, changed);
                    }
                },
            }
        }
        ops
    }

    fn snapshot(&self, session: &str) -> FrameDelta {
        let mut ops = vec![FrameOp::Clear];
        for (frame, held) in &self.frames {
            let fields = held.to_value().as_object().cloned().unwrap_or_default();
            ops.push(FrameOp::Put {
                frame: frame.clone(),
                fields,
            });
        }
        FrameDelta {
            session: Some(session.to_string()),
            from: 0,
            to: self.version,
            ops,
        }
    }
}

/// Every session's record. A request served by a session that keeps none (a scratch session, a fork) gets an
/// unversioned delta: ops with no cursor, applied wherever they land.
#[derive(Default)]
pub(super) struct ViewRecords {
    records: Mutex<HashMap<String, ViewRecord>>,
    scratch: Mutex<HashSet<String>>,
    unversioned: std::sync::atomic::AtomicBool,
}

impl ViewRecords {
    pub(super) fn keep_none(&self) {
        self.unversioned
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn mark_scratch(&self, session: &str) {
        self.scratch.lock().insert(session.to_string());
    }

    /// `session` ended: its record goes with it.
    pub(super) fn drop_session(&self, session: &str) {
        self.records.lock().remove(session);
        self.scratch.lock().remove(session);
    }

    pub(super) fn retained(&self) -> usize {
        self.records.lock().len()
    }
}

/// Where a delta is cursored: the record of one session, or nowhere.
enum Cursor {
    Session(String),
    None,
}

impl RuntimeKernel {
    fn view_cursor(&self) -> Cursor {
        if self
            .view_records
            .unversioned
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Cursor::None;
        }
        match self.thread_session() {
            None => Cursor::Session(PROGRAM_SESSION.to_string()),
            Some(session) if self.view_records.scratch.lock().contains(&*session) => Cursor::None,
            Some(session) => Cursor::Session(session.to_string()),
        }
    }

    /// Put the diff of `writes` against this session's record on `response`, then advance the record. An empty diff
    /// changes nothing: no delta, no version bump.
    pub fn seal_frames(&self, response: &mut EventResponse, writes: Vec<(String, FrameWrite)>) {
        response.frames = self.diff_frames(writes);
    }

    /// The diff itself, for a caller that holds no response (a fold reporting what it painted).
    pub fn diff_frames(&self, writes: Vec<(String, FrameWrite)>) -> Option<FrameDelta> {
        let mut merged: Vec<(String, FrameWrite)> = Vec::new();
        for (frame, write) in writes {
            let last = merged.iter_mut().rev().find(|(held, _)| *held == frame);
            match (last, write) {
                (Some((_, FrameWrite::Fields(held))), FrameWrite::Fields(more)) => {
                    held.extend(more)
                }
                (_, write) => merged.push((frame, write)),
            }
        }
        let writes = merged;
        match self.view_cursor() {
            Cursor::None => {
                let mut ops = Vec::new();
                for (frame, write) in writes {
                    match write {
                        FrameWrite::Removed => ops.push(FrameOp::Del { frame }),
                        FrameWrite::Replaced(fields) => {
                            ops.push(FrameOp::Del {
                                frame: frame.clone(),
                            });
                            ops.push(FrameOp::Put { frame, fields });
                        }
                        FrameWrite::Fields(fields) => {
                            FrameOp::push_set(&mut ops, frame, fields);
                        }
                    }
                }
                (!ops.is_empty()).then_some(FrameDelta {
                    session: None,
                    from: 0,
                    to: 0,
                    ops,
                })
            }
            Cursor::Session(session) => {
                let mut records = self.view_records.records.lock();
                let record = records.entry(session.clone()).or_default();
                let mut ops = record.diff(writes);
                if ops.is_empty() {
                    return None;
                }
                if record.version == 0 {
                    ops.insert(0, FrameOp::Clear);
                }
                let from = record.version;
                record.version += 1;
                Some(FrameDelta {
                    session: Some(session),
                    from,
                    to: record.version,
                    ops,
                })
            }
        }
    }

    /// What a client at version `since` of this session's record needs: nothing when it is current, else a snapshot
    /// (`from` 0, `clear` and every frame). `None` when this session keeps no record.
    pub fn frames_since(&self, since: u64) -> Option<FrameDelta> {
        let Cursor::Session(session) = self.view_cursor() else {
            return None;
        };
        let mut records = self.view_records.records.lock();
        let record = records.entry(session.clone()).or_default();
        if since == record.version {
            return Some(FrameDelta {
                session: Some(session),
                from: since,
                to: since,
                ops: Vec::new(),
            });
        }
        Some(record.snapshot(&session))
    }

    /// The frames of the current session a dispatch wrote, as [`FrameWrite`]s by frame key: each executed step's
    /// `set`s and its row `id`, a `[shared]` frame once. With `whole_frames` (a `request.mount`) every non-empty
    /// frame of the session instead, which seeds the viewer.
    pub fn frame_writes(
        &self,
        outcomes: &[(String, ProcessEventResult)],
        entity_id: Option<&str>,
        whole_frames: bool,
    ) -> Vec<(String, FrameWrite)> {
        if whole_frames {
            let mut whole: IndexMap<String, ValueMap> = IndexMap::default();
            for (trait_name, outcome) in outcomes {
                if let Some(fields) = outcome
                    .entity_data
                    .fields()
                    .filter(|fields| !fields.is_empty())
                {
                    whole
                        .entry(self.trait_frame_key(trait_name))
                        .or_default()
                        .extend(fields.iter().map(|(k, v)| (k.clone(), (**v).clone())));
                }
            }
            let eid = self.session_eid(entity_id.unwrap_or(RUNTIME_SINGLETON));
            let frames = self.frames.read();
            let mut held: Vec<(&String, &SharedEntity)> = frames
                .iter()
                .filter(|((_, frame_eid), frame)| {
                    *frame_eid == eid && frame.fields().is_some_and(|f| !f.is_empty())
                })
                .map(|((key, _), frame)| (key, frame))
                .collect();
            held.sort_by(|a, b| a.0.cmp(b.0));
            for (key, frame) in held {
                if let Some(fields) = frame.fields() {
                    whole
                        .entry(key.clone())
                        .or_default()
                        .extend(fields.iter().map(|(k, v)| (k.clone(), (**v).clone())));
                }
            }
            return whole
                .into_iter()
                .map(|(key, fields)| (key, FrameWrite::Fields(fields)))
                .collect();
        }
        let mut written: IndexMap<String, ValueMap> = IndexMap::default();
        for (trait_name, outcome) in outcomes {
            if !outcome.executed {
                continue;
            }
            let fields = written.entry(self.row_frame_key(trait_name, &outcome.entity_id)).or_default();
            fields.extend(
                outcome
                    .written_values()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            if let Some(id) = outcome.entity_data.get("id").filter(|id| !id.is_null()) {
                fields.insert(orbital_core_base::static_key!("id"), id.clone());
            }
        }
        let mut writes: Vec<(String, FrameWrite)> = written
            .into_iter()
            .map(|(key, fields)| (key, FrameWrite::Fields(fields)))
            .collect();
        writes.extend(self.take_removed_rows().into_iter().map(|frame| (frame, FrameWrite::Removed)));
        writes
    }

    /// Seal what `outcomes` wrote onto `response`: [`Self::frame_writes`] then [`Self::seal_frames`].
    pub fn seal_outcomes(
        &self,
        response: &mut EventResponse,
        outcomes: &[(String, ProcessEventResult)],
        entity_id: Option<&str>,
        whole_frames: bool,
    ) {
        let writes = self.frame_writes(outcomes, entity_id, whole_frames);
        self.seal_frames(response, writes);
    }

    /// The response of a dispatch that ran `outcomes`, its frames sealed against this session's record.
    pub fn response_for(
        &self,
        outcomes: &[(String, ProcessEventResult)],
        seed_trait: &str,
        event: &str,
    ) -> EventResponse {
        let mut response = EventResponse::from_process_results(outcomes, seed_trait, event);
        self.seal_outcomes(&mut response, outcomes, None, false);
        response
    }

    /// Sessions that keep a view record now.
    pub fn view_records_held(&self) -> usize {
        self.view_records.retained()
    }
}

/// What a client holds of the server sessions' records: the version it is at and each frame's id.
#[derive(Default)]
pub(super) struct InboundViews {
    views: Mutex<HashMap<String, InboundView>>,
}

#[derive(Default, Clone)]
struct InboundView {
    version: u64,
    ids: HashMap<String, String>,
    known: std::collections::BTreeSet<String>,
}

/// The records this client must fetch the frames of again: its version of `session` fell behind a delta.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FramesResync {
    pub session: String,
    pub since: u64,
}

/// A server delta folded into this kernel: the writes it made (what a painter of this kernel's frames is owed) or
/// the resync a version gap calls for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FramesApplied {
    pub writes: Vec<(String, FrameWrite)>,
    pub resync: Option<FramesResync>,
    /// The id each written frame is held under.
    pub ids: HashMap<String, String>,
}

impl RuntimeKernel {
    /// The trait a frame is written through and its entity type: the frame's own trait, or any trait bound to the
    /// `[shared]` entity.
    fn frame_home(&self, frame: &str) -> Option<(String, String)> {
        let defs = self.registry.trait_definitions.read();
        match frame.strip_prefix("$shared::") {
            Some(entity) => defs
                .values()
                .filter(|def| def.linked_entity.as_deref() == Some(entity))
                .map(|def| def.name.clone())
                .min()
                .map(|name| (name, entity.to_string())),
            None => defs.get(frame).map(|def| {
                (
                    def.name.clone(),
                    def.linked_entity
                        .clone()
                        .unwrap_or_else(|| def.name.clone()),
                )
            }),
        }
    }

    /// Whether the client alone holds every trait bound to `frame`: a host's report of it is ignored.
    fn frame_is_client_only(&self, frame: &str, client_only: &HashSet<String>) -> bool {
        let defs = self.registry.trait_definitions.read();
        match frame.strip_prefix("$shared::") {
            Some(entity) => defs
                .values()
                .filter(|def| def.linked_entity.as_deref() == Some(entity))
                .all(|def| client_only.contains(&def.name)),
            None => client_only.contains(frame),
        }
    }

    pub(super) fn remove_trait_frame(&self, trait_name: &str, entity_type: &str, entity_id: &str) {
        let slot = self.frame_slot(trait_name, entity_type, entity_id);
        self.frames.write().remove(&slot);
    }

    /// Fold a server's frame delta into this kernel's frames, at the version it starts from: a delta already applied
    /// (`to` at or behind the version held) changes nothing, a gap applies nothing and asks for a resync, a snapshot
    /// replaces. `base` is the rows the leg carried, by frame: a field equal to the client's own is the host echoing
    /// it, not a write. A frame only client-run traits read keeps the client's value.
    pub fn apply_server_frames(
        &self,
        delta: &FrameDelta,
        client_only: &HashSet<String>,
        base: Option<&HashMap<String, ValueMap>>,
    ) -> Result<FramesApplied, KernelError> {
        let key = delta.session.clone().unwrap_or_default();
        let mut views = self.inbound_views.views.lock();
        let held = views.get(&key).cloned().unwrap_or_default();
        if let Some(session) = &delta.session {
            if !delta.is_snapshot() {
                if delta.to <= held.version && !(delta.from == held.version && delta.ops.is_empty())
                {
                    return Ok(FramesApplied::default());
                }
                if delta.from != held.version {
                    return Ok(FramesApplied {
                        resync: Some(FramesResync {
                            session: session.clone(),
                            since: held.version,
                        }),
                        ..Default::default()
                    });
                }
            }
        }
        let mut ids = held.ids.clone();
        let mut known = held.known.clone();
        let mut writes: Vec<(String, FrameWrite)> = Vec::new();
        let push_fields = |writes: &mut Vec<(String, FrameWrite)>,
                           frame: &str,
                           fields: ValueMap| {
            match writes.iter_mut().rev().find(|(held, _)| held == frame) {
                Some((_, FrameWrite::Fields(held))) => held.extend(fields),
                _ => writes.push((frame.to_string(), FrameWrite::Fields(fields))),
            }
        };
        for op in &delta.ops {
            match op {
                FrameOp::Clear => {
                    for frame in std::mem::take(&mut known) {
                        writes.push((frame, FrameWrite::Removed));
                    }
                    ids.clear();
                }
                FrameOp::Del { frame } => {
                    ids.remove(frame);
                    known.remove(frame);
                    writes.push((frame.clone(), FrameWrite::Removed));
                }
                FrameOp::Put { frame, fields } => {
                    known.insert(frame.clone());
                    push_fields(&mut writes, frame, fields.clone())
                }
                FrameOp::Set { frames } => {
                    for (frame, fields) in frames {
                        known.insert(frame.clone());
                        push_fields(&mut writes, frame, fields.clone());
                    }
                }
            }
        }
        let mut applied = Vec::new();
        for (frame, write) in writes {
            if self.frame_is_client_only(&frame, client_only) {
                continue;
            }
            match write {
                FrameWrite::Fields(mut fields) => {
                    if let Some(carried) = base.and_then(|base| base.get(&frame)) {
                        fields = fields
                            .into_iter()
                            .filter(|(field, value)| {
                                field == "id" || carried.get(field) != Some(value)
                            })
                            .collect();
                    }
                    if let Some(id) = fields.get("id") {
                        ids.insert(frame.clone(), wire_row_id_of(id));
                    }
                    applied.push((frame, FrameWrite::Fields(fields)));
                }
                other => applied.push((frame, other)),
            }
        }
        for (frame, write) in &applied {
            let Some((_, entity_type)) = self.frame_home(frame) else {
                return Err(KernelError::Internal(format!("server response refused: it writes frame `{frame}`, which this program does not declare")));
            };
            if let FrameWrite::Fields(fields) = write {
                let row = Value::object(fields.clone());
                let id = ids
                    .get(frame)
                    .cloned()
                    .unwrap_or_else(|| RUNTIME_SINGLETON.to_string());
                let keyed = match &row {
                    Value::Object(map) if !map.contains_key("id") => {
                        let mut map = map.clone();
                        map.insert(orbital_core_base::static_key!("id"), Value::string(id));
                        Value::Object(map)
                    }
                    _ => row.clone(),
                };
                if let Some(misfit) = self.rows_misfit(&entity_type, [&keyed]) {
                    return Err(KernelError::Internal(format!(
                        "server response refused: {misfit}"
                    )));
                }
            }
        }
        for (frame, write) in &applied {
            let Some((trait_name, entity_type)) = self.frame_home(frame) else {
                continue;
            };
            let id = ids
                .get(frame)
                .cloned()
                .unwrap_or_else(|| RUNTIME_SINGLETON.to_string());
            match write {
                FrameWrite::Fields(fields) => {
                    self.write_trait_frame(
                        &trait_name,
                        &entity_type,
                        &id,
                        &SharedEntity::from_value_ref(&Value::object(fields.clone())),
                    );
                }
                FrameWrite::Removed => self.remove_trait_frame(&trait_name, &entity_type, &id),
                FrameWrite::Replaced(_) => {}
            }
        }
        let version = if delta.session.is_some() {
            delta.to
        } else {
            held.version
        };
        views.insert(
            key,
            InboundView {
                version,
                ids: ids.clone(),
                known,
            },
        );
        Ok(FramesApplied {
            writes: applied,
            resync: None,
            ids,
        })
    }
}

fn wire_row_id_of(id: &Value) -> String {
    match id.as_str() {
        Some("singleton") | None => RUNTIME_SINGLETON.to_string(),
        Some(id) => id.to_string(),
    }
}
