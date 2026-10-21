//! An append-only log whose clones are prefixes of one shared chain: `push` and `clone` are O(1), so a dispatch can
//! keep every step's view of `@prevEvents` / `@prevStates` without each view owning a copy of the log (G-CROSS-288).

use std::sync::Arc;

#[derive(Debug)]
struct Node<T> {
    value: T,
    prev: Option<Arc<Node<T>>>,
    len: usize,
}

#[derive(Debug)]
pub struct SharedLog<T> {
    head: Option<Arc<Node<T>>>,
}

impl<T> Default for SharedLog<T> {
    fn default() -> Self {
        Self { head: None }
    }
}

impl<T> Clone for SharedLog<T> {
    fn clone(&self) -> Self {
        Self { head: self.head.clone() }
    }
}

impl<T> SharedLog<T> {
    pub fn len(&self) -> usize {
        self.head.as_ref().map_or(0, |node| node.len)
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    /// Appends without touching the prefix any clone holds.
    pub fn push(&mut self, value: T) {
        let prev = self.head.take();
        let len = prev.as_ref().map_or(0, |node| node.len) + 1;
        self.head = Some(Arc::new(Node { value, prev, len }));
    }

    /// Oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        let mut items = Vec::with_capacity(self.len());
        let mut cur = self.head.as_deref();
        while let Some(node) = cur {
            items.push(&node.value);
            cur = node.prev.as_deref();
        }
        items.reverse();
        items.into_iter()
    }

    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.iter().cloned().collect()
    }
}

impl<T> FromIterator<T> for SharedLog<T> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        let mut log = Self::default();
        items.into_iter().for_each(|item| log.push(item));
        log
    }
}

/// Unlinks the chain one node at a time: a long log dropped recursively would overflow the stack.
impl<T> Drop for SharedLog<T> {
    fn drop(&mut self) {
        let mut cur = self.head.take();
        while let Some(node) = cur {
            match Arc::try_unwrap(node) {
                Ok(mut node) => cur = node.prev.take(),
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_keeps_its_prefix_while_the_log_grows() {
        let mut log = SharedLog::default();
        log.push(1);
        let prefix = log.clone();
        log.push(2);
        assert_eq!(prefix.to_vec(), vec![1]);
        assert_eq!(log.to_vec(), vec![1, 2]);
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn appending_to_a_shared_log_never_copies_it() {
        let held = Arc::new(());
        let mut log = SharedLog::default();
        let mut views = Vec::new();
        for _ in 0..100 {
            log.push(Arc::clone(&held));
            views.push(log.clone());
        }
        assert_eq!(Arc::strong_count(&held), 101);
        assert_eq!(views[49].len(), 50);
    }

    #[test]
    fn dropping_a_long_log_does_not_overflow_the_stack() {
        let log: SharedLog<usize> = (0..1_000_000).collect();
        assert_eq!(log.len(), 1_000_000);
        drop(log);
    }
}
