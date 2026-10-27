//! The few-entry maps and sets a call's bindings keep: a signature names a handful of variables, so a sorted vector
//! beats a tree, and clearing one keeps its capacity for the next call.

use std::prelude::v1::*;

/// A map from variable name to `V`, kept sorted by name (iteration order matches a `BTreeMap`).
#[derive(Debug, Clone)]
pub(super) struct SmallMap<V>(Vec<(String, V)>);

impl<V> Default for SmallMap<V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<V> SmallMap<V> {
    fn find(&self, key: &str) -> Result<usize, usize> {
        self.0.binary_search_by(|(k, _)| k.as_str().cmp(key))
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.find(key).ok().map(|i| &self.0[i].1)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.find(key).is_ok()
    }

    pub fn insert(&mut self, key: String, value: V) -> Option<V> {
        match self.find(&key) {
            Ok(i) => Some(std::mem::replace(&mut self.0[i].1, value)),
            Err(i) => {
                self.0.insert(i, (key, value));
                None
            }
        }
    }

    pub fn entry_or_default(&mut self, key: &str) -> &mut V
    where
        V: Default,
    {
        let i = match self.find(key) {
            Ok(i) => i,
            Err(i) => {
                self.0.insert(i, (key.to_owned(), V::default()));
                i
            }
        };
        &mut self.0[i].1
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl<'a, V> IntoIterator for &'a SmallMap<V> {
    type Item = (&'a String, &'a V);
    type IntoIter = std::iter::Map<std::slice::Iter<'a, (String, V)>, fn(&'a (String, V)) -> (&'a String, &'a V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().map(|(k, v)| (k, v))
    }
}

/// A set of variable names, kept sorted (iteration order matches a `BTreeSet`).
#[derive(Debug, Clone, Default)]
pub(super) struct SmallSet(Vec<String>);

impl SmallSet {
    fn find(&self, key: &str) -> Result<usize, usize> {
        self.0.binary_search_by(|k| k.as_str().cmp(key))
    }

    pub fn contains(&self, key: &str) -> bool {
        self.find(key).is_ok()
    }

    pub fn insert(&mut self, key: String) -> bool {
        match self.find(&key) {
            Ok(_) => false,
            Err(i) => {
                self.0.insert(i, key);
                true
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> bool {
        match self.find(key) {
            Ok(i) => {
                self.0.remove(i);
                true
            }
            Err(_) => false,
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.0.iter()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl Extend<String> for SmallSet {
    fn extend<I: IntoIterator<Item = String>>(&mut self, iter: I) {
        for key in iter {
            self.insert(key);
        }
    }
}

impl<'a> IntoIterator for &'a SmallSet {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
