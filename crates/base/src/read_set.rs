//! The files a piece of work read, each with its state at the read, so a result built from them is reused only while all are unchanged.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The files read by one recording, by path, with the stamp each had when first read (`None`: the path did not exist).
#[derive(Debug, Default, Clone)]
pub struct ReadSet(BTreeMap<PathBuf, Option<String>>);

thread_local! {
    static OPEN: RefCell<Vec<ReadSet>> = const { RefCell::new(Vec::new()) };
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
    OPEN.with(|open| open.borrow_mut().push(ReadSet::default()));
}

/// The reads since the matching [`begin`].
pub fn end() -> ReadSet {
    OPEN.with(|open| open.borrow_mut().pop().unwrap_or_default())
}

/// Counts `path` (a file, a directory listed, or a path whose absence decided something) as read, stamped now.
pub fn note(path: &Path) {
    OPEN.with(|open| {
        let mut open = open.borrow_mut();
        if open.is_empty() {
            return;
        }
        let stamp = stamp(path);
        for set in open.iter_mut() {
            set.0.entry(path.to_path_buf()).or_insert_with(|| stamp.clone());
        }
    });
}

/// `std::fs::read_to_string`, recorded: the path is stamped before the read, so a change in between reads as changed.
pub fn read_to_string(path: &Path) -> std::io::Result<String> {
    note(path);
    std::fs::read_to_string(path)
}

impl ReadSet {
    /// Counts every path of `reads` as read by the open recordings, with the state it was recorded in.
    pub fn replay(&self) {
        OPEN.with(|open| {
            for set in open.borrow_mut().iter_mut() {
                for (path, seen) in &self.0 {
                    set.0.entry(path.clone()).or_insert_with(|| seen.clone());
                }
            }
        });
    }

    /// Every recorded path is in the state it was read in.
    pub fn is_current(&self) -> bool {
        self.0.iter().all(|(path, seen)| &stamp(path) == seen)
    }

    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.0.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
