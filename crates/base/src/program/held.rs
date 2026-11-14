//! Held memory, counted (EXEC-COST-2): the bytes of value data a table holds — string and byte contents, list and
//! record slots (capacity times slot size) with their keys, and each shared allocation's header — every allocation
//! counted once however many holders reach it. Allocator and hash-index overhead is not value data and is not counted.

use crate::shared_entity::SharedEntity;
use crate::value::{Shared, Value, ValueMap};
use std::collections::HashSet;
use std::sync::Arc;

const ARC_HEADER: u64 = 2 * core::mem::size_of::<usize>() as u64;

/// A running count over one or more tables; allocations already seen add nothing.
#[derive(Default)]
pub struct HeldBytes {
    seen: HashSet<usize>,
    bytes: u64,
}

impl HeldBytes {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The bytes counted since `start`, for one table of a walk that shares its seen set.
    pub fn since(&self, start: u64) -> u64 {
        self.bytes - start
    }

    fn first<T>(&mut self, ptr: *const T) -> bool {
        self.seen.insert(ptr as usize)
    }

    pub fn value(&mut self, value: &Value) {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Int(_) => {}
            Value::Bytes(bytes) => {
                if self.first(Shared::as_ptr(bytes)) {
                    self.bytes += ARC_HEADER + core::mem::size_of::<Vec<u8>>() as u64 + bytes.capacity() as u64;
                }
            }
            Value::String(text) => self.string(text),
            Value::Array(items) => {
                if self.first(Shared::as_ptr(items)) {
                    self.bytes += ARC_HEADER
                        + core::mem::size_of::<Vec<Value>>() as u64
                        + (items.capacity() * core::mem::size_of::<Value>()) as u64;
                    for item in items.iter() {
                        self.value(item);
                    }
                }
            }
            Value::Object(map) => {
                if self.first(Shared::as_ptr(map)) {
                    self.bytes += ARC_HEADER + core::mem::size_of::<ValueMap>() as u64;
                    self.record(map);
                }
            }
            Value::Lambda(lambda) => {
                if self.first(Arc::as_ptr(lambda)) {
                    self.bytes += ARC_HEADER + core::mem::size_of_val(&**lambda) as u64;
                }
            }
            Value::Sealed(sealed) => {
                if self.first(Arc::as_ptr(sealed)) {
                    self.bytes += ARC_HEADER + core::mem::size_of_val(&**sealed) as u64;
                }
            }
        }
    }

    /// A value held through its own `Arc` (a stored row).
    pub fn shared_value(&mut self, value: &Arc<Value>) {
        if self.first(Arc::as_ptr(value)) {
            self.bytes += ARC_HEADER + core::mem::size_of::<Value>() as u64;
            self.value(value);
        }
    }

    pub fn entity(&mut self, entity: &SharedEntity) {
        if let Some(fields) = entity.fields() {
            if self.first(fields as *const _) {
                self.bytes += ARC_HEADER
                    + core::mem::size_of_val(fields) as u64
                    + (fields.capacity() * core::mem::size_of::<(Shared<String>, Arc<Value>)>()) as u64;
                for (key, value) in fields {
                    self.string(key);
                    self.shared_value(value);
                }
            }
        }
    }

    pub fn string(&mut self, text: &Shared<String>) {
        if self.first(Shared::as_ptr(text)) {
            self.bytes += ARC_HEADER + core::mem::size_of::<String>() as u64 + text.capacity() as u64;
        }
    }

    /// Bytes a table holds outside any value (an arena's capacity, a row key).
    pub fn raw(&mut self, bytes: u64) {
        self.bytes += bytes;
    }

    fn record(&mut self, map: &ValueMap) {
        self.bytes += (map.capacity() * core::mem::size_of::<(Shared<String>, Value)>()) as u64;
        for (key, value) in map {
            self.string(key);
            self.value(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Value {
        Value::String(Shared::new(s.to_string()))
    }

    #[test]
    fn held_bytes_counts_a_shared_allocation_once_however_many_holders_reach_it() {
        let shared = text("abcdefgh");
        let mut once = HeldBytes::default();
        once.value(&shared);
        let mut twice = HeldBytes::default();
        twice.value(&Value::Array(Shared::new(vec![shared.clone(), shared.clone()])));
        let list_overhead = ARC_HEADER + core::mem::size_of::<Vec<Value>>() as u64 + 2 * core::mem::size_of::<Value>() as u64;
        assert_eq!(twice.bytes(), list_overhead + once.bytes(), "the string is counted once");
    }

    #[test]
    fn held_bytes_counts_a_string_by_its_capacity_and_scalars_as_nothing() {
        let mut held = HeldBytes::default();
        held.value(&Value::Number(1.0));
        held.value(&Value::Null);
        assert_eq!(held.bytes(), 0);
        let mut s = String::with_capacity(100);
        s.push('x');
        held.value(&Value::String(Shared::new(s)));
        assert_eq!(held.bytes(), ARC_HEADER + core::mem::size_of::<String>() as u64 + 100);
    }

    #[test]
    fn held_bytes_two_equal_but_separate_strings_are_two_allocations() {
        let (first, second) = (text("same"), text("same"));
        let mut held = HeldBytes::default();
        held.value(&first);
        let one = held.bytes();
        held.value(&second);
        assert_eq!(held.bytes(), 2 * one);
    }
}
