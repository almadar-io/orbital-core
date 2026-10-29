//! Interning of program strings and expression subtrees: while one program is read, equal text shares one
//! allocation, and so does every list or object whose children are the same shared nodes in the same order. The
//! table lives only for the outermost [`scope`] on the thread and is dropped with it, so nothing outlives the parse.

use crate::schema::sexpr::{SExpression, SObject};
use crate::value::{Shared, Value, ValueMap};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

struct Table {
    held: HashSet<Shared<String>>,
    nodes: HashMap<u64, Vec<SExpression>>,
    values: HashMap<u64, Vec<Value>>,
    node_requests: usize,
    node_distinct: usize,
    requests: usize,
    request_bytes: usize,
    held_bytes: usize,
}

thread_local! {
    static TABLE: RefCell<Option<Table>> = const { RefCell::new(None) };
}

/// What the active scope has been asked for and what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub requests: usize,
    pub distinct: usize,
    pub request_bytes: usize,
    pub distinct_bytes: usize,
    pub node_requests: usize,
    pub node_distinct: usize,
}

struct Reset(bool);

impl Drop for Reset {
    fn drop(&mut self) {
        if self.0 {
            TABLE.with(|table| table.borrow_mut().take());
        }
    }
}

/// Runs `read` with an interning table active; a nested call joins the table of the outermost one.
pub fn scope<R>(read: impl FnOnce() -> R) -> R {
    let opened = TABLE.with(|table| {
        let mut table = table.borrow_mut();
        if table.is_some() {
            false
        } else {
            *table = Some(Table {
                held: HashSet::new(),
                nodes: HashMap::new(),
                values: HashMap::new(),
                node_requests: 0,
                node_distinct: 0,
                requests: 0,
                request_bytes: 0,
                held_bytes: 0,
            });
            true
        }
    });
    let _reset = Reset(opened);
    read()
}

/// The shared copy of `text`: the one already held when a scope is active and has it, else a new allocation
/// (held by the scope, if any).
pub fn shared(text: &str) -> Shared<String> {
    TABLE.with(|table| match table.borrow_mut().as_mut() {
        Some(table) => {
            table.requests += 1;
            table.request_bytes += text.len();
            match table.held.get(text) {
                Some(found) => found.clone(),
                None => {
                    let fresh = Shared::new(text.to_owned());
                    table.held_bytes += text.len();
                    table.held.insert(fresh.clone());
                    fresh
                }
            }
        }
        None => Shared::new(text.to_owned()),
    })
}

/// [`shared`] for an owned string: reuses its buffer when it is the first of its text.
pub fn shared_owned(text: String) -> Shared<String> {
    TABLE.with(|table| match table.borrow_mut().as_mut() {
        Some(table) => {
            table.requests += 1;
            table.request_bytes += text.len();
            match table.held.get(text.as_str()) {
                Some(found) => found.clone(),
                None => {
                    table.held_bytes += text.len();
                    let fresh = Shared::new(text);
                    table.held.insert(fresh.clone());
                    fresh
                }
            }
        }
        None => Shared::new(text),
    })
}

/// The active scope's counts (`None` outside a scope).
pub fn stats() -> Option<Stats> {
    TABLE.with(|table| {
        table.borrow().as_ref().map(|table| Stats {
            requests: table.requests,
            distinct: table.held.len(),
            request_bytes: table.request_bytes,
            distinct_bytes: table.held_bytes,
            node_requests: table.node_requests,
            node_distinct: table.node_distinct,
        })
    })
}

/// How many distinct strings the active scope holds (0 outside a scope).
pub fn held() -> usize {
    TABLE.with(|table| table.borrow().as_ref().map_or(0, |table| table.held.len()))
}

fn same(a: &SExpression, b: &SExpression) -> bool {
    match (a, b) {
        (SExpression::Null, SExpression::Null) => true,
        (SExpression::Bool(a), SExpression::Bool(b)) => a == b,
        (SExpression::Number(a), SExpression::Number(b)) => a.to_bits() == b.to_bits(),
        (SExpression::String(a), SExpression::String(b)) => Shared::ptr_eq(a, b),
        (SExpression::Array(a), SExpression::Array(b)) => Shared::ptr_eq(a, b),
        (SExpression::Object(a), SExpression::Object(b)) => Shared::ptr_eq(a, b),
        _ => false,
    }
}

fn hash_node(node: &SExpression, state: &mut impl Hasher) {
    match node {
        SExpression::Null => 0u8.hash(state),
        SExpression::Bool(b) => (1u8, *b).hash(state),
        SExpression::Number(n) => (2u8, n.to_bits()).hash(state),
        SExpression::String(text) => (3u8, &**text as *const String as usize).hash(state),
        SExpression::Array(items) => {
            (4u8, &**items as *const Vec<SExpression> as usize).hash(state)
        }
        SExpression::Object(fields) => (5u8, &**fields as *const SObject as usize).hash(state),
    }
}

fn hash_items<'a>(items: impl Iterator<Item = &'a SExpression>, seed: u8) -> u64 {
    let mut state = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut state);
    for item in items {
        hash_node(item, &mut state);
    }
    state.finish()
}

fn same_fields(a: &SObject, b: &SObject) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, va), (kb, vb))| Shared::ptr_eq(ka, kb) && same(va, vb))
}

/// The list `items` as the one shared node the active scope holds for that content, else a new node (held by the
/// scope, if any). Two nodes are one only when every child is the same shared node, so a merge is never partial.
pub fn list(items: Vec<SExpression>) -> SExpression {
    TABLE.with(|table| match table.borrow_mut().as_mut() {
        Some(table) => {
            table.node_requests += 1;
            let key = hash_items(items.iter(), 4);
            let bucket = table.nodes.entry(key).or_default();
            let found = bucket.iter().find(|held| {
                matches!(held, SExpression::Array(known)
                    if known.len() == items.len() && known.iter().zip(items.iter()).all(|(a, b)| same(a, b)))
            });
            match found {
                Some(held) => held.clone(),
                None => {
                    let fresh = SExpression::array(items);
                    bucket.push(fresh.clone());
                    table.node_distinct += 1;
                    fresh
                }
            }
        }
        None => SExpression::array(items),
    })
}

/// [`list`] for an object: keys are compared in authored order, so field order stays part of the content.
pub fn record(fields: SObject) -> SExpression {
    TABLE.with(|table| match table.borrow_mut().as_mut() {
        Some(table) => {
            table.node_requests += 1;
            let mut state = std::collections::hash_map::DefaultHasher::new();
            5u8.hash(&mut state);
            for (key, value) in fields.iter() {
                (&**key as *const String as usize).hash(&mut state);
                hash_node(value, &mut state);
            }
            let key = state.finish();
            let bucket = table.nodes.entry(key).or_default();
            let found = bucket.iter().find(
                |held| matches!(held, SExpression::Object(known) if same_fields(known, &fields)),
            );
            match found {
                Some(held) => held.clone(),
                None => {
                    let fresh = SExpression::object(fields);
                    bucket.push(fresh.clone());
                    table.node_distinct += 1;
                    fresh
                }
            }
        }
        None => SExpression::object(fields),
    })
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.to_bits() == b.to_bits(),
        (Value::String(a), Value::String(b)) => Shared::ptr_eq(a, b),
        (Value::Array(a), Value::Array(b)) => Shared::ptr_eq(a, b),
        (Value::Object(a), Value::Object(b)) => Shared::ptr_eq(a, b),
        _ => false,
    }
}

fn hash_value(node: &Value, state: &mut impl Hasher) {
    match node {
        Value::Null => 0u8.hash(state),
        Value::Bool(b) => (1u8, *b).hash(state),
        Value::Number(n) => (2u8, n.to_bits()).hash(state),
        Value::String(text) => (3u8, &**text as *const String as usize).hash(state),
        Value::Array(items) => (4u8, &**items as *const Vec<Value> as usize).hash(state),
        Value::Object(fields) => (5u8, &**fields as *const ValueMap as usize).hash(state),
        Value::Lambda { body, .. } => (6u8, std::sync::Arc::as_ptr(body) as usize).hash(state),
    }
}

fn mergeable(node: &Value) -> bool {
    !matches!(node, Value::Lambda { .. })
}

/// The list `items` as the one shared value node the active scope holds for that content, else a new node (held by
/// the scope, if any). Two nodes are one only when every child is the same shared node.
pub fn value_list(items: Vec<Value>) -> Value {
    TABLE.with(|table| match table.borrow_mut().as_mut() {
        Some(table) if items.iter().all(mergeable) => {
            table.node_requests += 1;
            let mut state = std::collections::hash_map::DefaultHasher::new();
            4u8.hash(&mut state);
            items.iter().for_each(|item| hash_value(item, &mut state));
            let bucket = table.values.entry(state.finish()).or_default();
            let found = bucket.iter().find(|held| {
                matches!(held, Value::Array(known)
                    if known.len() == items.len() && known.iter().zip(items.iter()).all(|(a, b)| same_value(a, b)))
            });
            match found {
                Some(held) => held.clone(),
                None => {
                    let fresh = Value::array(items);
                    bucket.push(fresh.clone());
                    table.node_distinct += 1;
                    fresh
                }
            }
        }
        _ => Value::array(items),
    })
}

/// [`value_list`] for an object: fields are compared in order, so field order stays part of the content.
pub fn value_record(fields: ValueMap) -> Value {
    TABLE.with(|table| match table.borrow_mut().as_mut() {
        Some(table) if fields.values().all(mergeable) => {
            table.node_requests += 1;
            let mut state = std::collections::hash_map::DefaultHasher::new();
            5u8.hash(&mut state);
            for (key, value) in fields.iter() {
                key.hash(&mut state);
                hash_value(value, &mut state);
            }
            let bucket = table.values.entry(state.finish()).or_default();
            let found = bucket.iter().find(|held| {
                matches!(held, Value::Object(known)
                    if known.len() == fields.len()
                        && known.iter().zip(fields.iter()).all(|((ka, va), (kb, vb))| ka == kb && same_value(va, vb)))
            });
            match found {
                Some(held) => held.clone(),
                None => {
                    let fresh = Value::object(fields);
                    bucket.push(fresh.clone());
                    table.node_distinct += 1;
                    fresh
                }
            }
        }
        _ => Value::object(fields),
    })
}
