//! The insertion-ordered map every record type uses: `indexmap`'s default hasher on a hosted build, foldhash on a
//! bare-metal one (the standard library's random state is not there). Order, not hashing, is the contract.

#[cfg(feature = "std")]
pub type IndexMap<K, V> = indexmap::IndexMap<K, V>;

#[cfg(not(feature = "std"))]
pub type IndexMap<K, V> = indexmap::IndexMap<K, V, foldhash::fast::RandomState>;
