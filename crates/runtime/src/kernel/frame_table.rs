//! The kernel's entity frames, keyed by frame slot, each with the version of its last write: a repaint compares the
//! versions it last saw for an instance's slots and reads its `@entity` again only when one moved.

use crate::shared_entity::SharedEntity;
use std::collections::HashMap;
use std::ops::Deref;

pub(crate) type FrameSlot = (String, String);

#[derive(Default)]
pub(crate) struct FrameTable {
    frames: HashMap<FrameSlot, SharedEntity>,
    versions: HashMap<FrameSlot, u64>,
    next: u64,
    /// Bumped when something every frame reads changes (the registered entity defaults).
    epoch: u64,
}

impl Deref for FrameTable {
    type Target = HashMap<FrameSlot, SharedEntity>;

    fn deref(&self) -> &Self::Target {
        &self.frames
    }
}

impl FrameTable {
    pub(crate) fn insert(&mut self, slot: FrameSlot, frame: SharedEntity) {
        self.next += 1;
        match self.versions.get_mut(&slot) {
            Some(version) => *version = self.next,
            None => {
                self.versions.insert(slot.clone(), self.next);
            }
        }
        self.frames.insert(slot, frame);
    }

    pub(crate) fn remove(&mut self, slot: &FrameSlot) -> Option<SharedEntity> {
        self.versions.remove(slot);
        self.frames.remove(slot)
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&FrameSlot, &SharedEntity) -> bool) {
        self.frames.retain(|slot, frame| keep(slot, frame));
        let frames = &self.frames;
        self.versions.retain(|slot, _| frames.contains_key(slot));
    }

    /// The version of `slot`'s last write, `None` when it holds no frame.
    pub(crate) fn version(&self, slot: &FrameSlot) -> Option<u64> {
        self.versions.get(slot).copied()
    }

    /// Record that every slot reads something new.
    pub(crate) fn mark_all(&mut self) {
        self.epoch += 1;
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(t: &str) -> FrameSlot {
        (t.to_string(), "singleton".to_string())
    }

    #[test]
    fn every_write_moves_its_slot_version_and_no_other() {
        let mut table = FrameTable::default();
        table.insert(slot("A"), SharedEntity::empty_object());
        table.insert(slot("B"), SharedEntity::empty_object());
        let (a, b) = (table.version(&slot("A")), table.version(&slot("B")));
        table.insert(slot("A"), SharedEntity::empty_object());
        assert_ne!(table.version(&slot("A")), a, "a write moves the slot's version");
        assert_eq!(table.version(&slot("B")), b, "control: a slot nothing wrote keeps its version");
        assert!(table.contains_key(&slot("A")), "reads see the frames");
    }

    #[test]
    fn a_removed_or_dropped_frame_has_no_version() {
        let mut table = FrameTable::default();
        table.insert(slot("A"), SharedEntity::empty_object());
        table.insert(slot("B"), SharedEntity::empty_object());
        table.remove(&slot("A"));
        table.retain(|s, _| *s != slot("B"));
        assert_eq!(table.version(&slot("A")), None);
        assert_eq!(table.version(&slot("B")), None);
        assert!(table.versions.is_empty(), "a version lives only as long as its frame, so a server keeps none for ended sessions");
    }

    #[test]
    fn marking_all_moves_the_epoch() {
        let mut table = FrameTable::default();
        let before = table.epoch();
        table.mark_all();
        assert_ne!(table.epoch(), before);
    }
}
