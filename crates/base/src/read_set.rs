//! The files a piece of work read, each with its state at the read, so a result built from them is reused only while all are unchanged.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::path::{Path, PathBuf};

/// The files read by one recording, by path, with the stamp each had when first read (`None`: the path did not exist).
/// A set a recording produced carries an identity (never stored), so a recording merges and verifies it once, and holds
/// it by reference (`includes`) rather than copying its paths: a directory listing many results replay is held once.
/// It is written as one flat map of every path it holds.
#[derive(Debug, Default, Clone)]
pub struct ReadSet {
    paths: std::sync::Arc<BTreeMap<PathBuf, Option<String>>>,
    includes: Vec<ReadSet>,
    id: u64,
}

impl serde::Serialize for ReadSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut all = BTreeMap::new();
        self.collect(&mut all);
        all.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for ReadSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let paths = BTreeMap::<PathBuf, Option<String>>::deserialize(deserializer)?;
        Ok(ReadSet { paths: std::sync::Arc::new(paths), includes: Vec::new(), id: 0 })
    }
}

/// One open recording: what it read, and the identities of the sets it has merged, each verified current when merged.
struct Open {
    reads: ReadSet,
    merged: HashSet<u64>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
}

/// The state of the file or directory at `path`: size, modification and change time, inode. A write, rename or
/// replacement changes it; `None` when nothing exists there.
pub fn stamp(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!("{} {}.{} {}.{} {}", meta.size(), meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec(), meta.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Some(format!("{} {} {}", meta.file_size(), meta.last_write_time(), meta.creation_time()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(format!("{} {}", meta.len(), modified.as_nanos()))
    }
}

/// Starts recording every read made on this thread until the matching [`end`].
pub fn begin() {
    OPEN.with(|open| open.borrow_mut().push(Open { reads: ReadSet::default(), merged: HashSet::new() }));
}

/// The reads since the matching [`begin`].
pub fn end() -> ReadSet {
    let mut reads = OPEN.with(|open| open.borrow_mut().pop().map(|o| o.reads).unwrap_or_default());
    reads.id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    reads
}

/// Counts `path` (a file, a directory listed, or a path whose absence decided something) as read, stamped now.
pub fn note(path: &Path) {
    OPEN.with(|open| {
        let mut open = open.borrow_mut();
        if open.is_empty() {
            return;
        }
        if open.iter().all(|o| o.reads.paths.contains_key(path)) {
            return;
        }
        let stamp = stamp(path);
        for o in open.iter_mut() {
            std::sync::Arc::make_mut(&mut o.reads.paths).entry(path.to_path_buf()).or_insert_with(|| stamp.clone());
        }
    });
}

/// `std::fs::read_to_string`, recorded: the path is stamped before the read, so a change in between reads as changed.
pub fn read_to_string(path: &Path) -> std::io::Result<String> {
    note(path);
    std::fs::read_to_string(path)
}

impl ReadSet {
    /// Counts every path of `reads` as read by the open recordings, with the state it was recorded in. A recording that
    /// already merged this set takes nothing again.
    pub fn replay(&self) {
        OPEN.with(|open| {
            for o in open.borrow_mut().iter_mut() {
                if self.id == 0 {
                    let mut all = BTreeMap::new();
                    self.collect(&mut all);
                    let own = std::sync::Arc::make_mut(&mut o.reads.paths);
                    for (path, seen) in all {
                        own.entry(path.clone()).or_insert_with(|| seen.clone());
                    }
                } else if o.merged.insert(self.id) {
                    o.reads.includes.push(self.clone());
                }
            }
        });
    }

    /// Every path this set holds, its own and its included sets', with the stamp it was first read with.
    fn collect<'a>(&'a self, into: &mut BTreeMap<&'a PathBuf, &'a Option<String>>) {
        for (path, seen) in self.paths.iter() {
            into.entry(path).or_insert(seen);
        }
        for included in &self.includes {
            included.collect(into);
        }
    }

    /// Every recorded path is in the state it was read in. Inside a recording that already merged this set it was
    /// verified when merged: what is built from it matches the stamps the recording keeps, which are checked the next
    /// time the recording's own result is reused.
    pub fn is_current(&self) -> bool {
        let merged = self.id != 0 && OPEN.with(|open| open.borrow().last().is_some_and(|o| o.merged.contains(&self.id)));
        merged || (self.paths.iter().all(|(path, seen)| &stamp(path) == seen) && self.includes.iter().all(ReadSet::is_current))
    }

    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        let mut all = BTreeMap::new();
        self.collect(&mut all);
        all.into_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_value::{from_json_str, from_json_value};

    #[test]
    fn a_read_is_current_until_the_file_changes_or_a_missing_one_appears() {
        let dir = tempfile::tempdir().unwrap();
        let (present, absent) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        std::fs::write(&present, "one").unwrap();
        begin();
        read_to_string(&present).unwrap();
        note(&absent);
        let reads = end();
        assert!(reads.is_current());
        std::fs::write(&absent, "now here").unwrap();
        assert!(!reads.is_current(), "a path that did not exist and now does");
        std::fs::remove_file(&absent).unwrap();
        assert!(reads.is_current());
        std::fs::write(&present, "changed").unwrap();
        assert!(!reads.is_current());
    }

    #[test]
    fn a_replayed_set_lands_in_the_open_recording() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        begin();
        read_to_string(&file).unwrap();
        let inner = end();
        begin();
        inner.replay();
        let outer = end();
        assert!(outer.paths().any(|p| p == &file));
        std::fs::write(&file, "yy").unwrap();
        assert!(!outer.is_current());
    }

    #[test]
    fn nothing_is_recorded_outside_a_recording() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        read_to_string(&file).unwrap();
        begin();
        assert_eq!(end().paths().count(), 0);
    }

    #[test]
    fn inside_a_recording_a_set_it_merged_counts_as_current_and_the_recording_keeps_the_first_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        begin();
        read_to_string(&file).unwrap();
        let inner = end();
        begin();
        assert!(inner.is_current());
        inner.replay();
        std::fs::write(&file, "changed").unwrap();
        assert!(inner.is_current(), "verified once for this recording: what is built from it matches the stamp the recording keeps");
        let outer = end();
        assert!(!outer.is_current(), "the recording kept the first stamp, so its result is seen as stale next time");
        assert!(!inner.is_current(), "control: outside the recording the change is seen");
    }

    #[test]
    fn a_set_replayed_again_into_the_same_recording_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "y").unwrap();
        begin();
        read_to_string(&a).unwrap();
        let inner = end();
        begin();
        inner.replay();
        read_to_string(&b).unwrap();
        inner.replay();
        let outer = end();
        assert_eq!(outer.paths().count(), 2);
    }

    #[test]
    fn edge_a_set_read_back_from_disk_keeps_its_paths_and_stamps_and_merges_as_any_other() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        begin();
        read_to_string(&file).unwrap();
        let written = serde_json::to_string(&end()).unwrap();
        assert!(written.starts_with('{') && written.contains("a.txt"), "the stored form is the path map alone: {written}");
        let read_back: ReadSet = from_json_str(&written).unwrap();
        assert!(read_back.is_current());
        begin();
        read_back.replay();
        assert_eq!(end().paths().count(), 1);
    }

    #[test]
    fn a_set_replayed_into_many_recordings_is_shared_by_them_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one").unwrap();
        begin();
        note(&file);
        let shared = end();
        let recorded: Vec<ReadSet> = (0..3)
            .map(|_| {
                begin();
                shared.replay();
                end()
            })
            .collect();
        for reads in &recorded {
            assert!(reads.paths.is_empty(), "the recording copied no path");
            assert!(std::sync::Arc::ptr_eq(&reads.includes[0].paths, &shared.paths));
            assert_eq!(reads.paths().collect::<Vec<_>>(), vec![&file]);
        }
        std::fs::write(&file, "two").unwrap();
        assert!(recorded.iter().all(|reads| !reads.is_current()), "a change under a shared set makes each recording stale");
    }

    #[test]
    fn control_a_recording_with_a_shared_set_writes_the_same_flat_form() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        std::fs::write(&a, "one").unwrap();
        begin();
        note(&a);
        let shared = end();
        begin();
        shared.replay();
        note(&b);
        let reads = end();
        let written = serde_json::to_value(&reads).unwrap();
        assert_eq!(written.as_object().unwrap().len(), 2, "{written}");
        let back: ReadSet = from_json_value(&written).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), written);
        assert!(back.is_current());
    }
}
