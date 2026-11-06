//! The kernel's `mem/*` memory (EXEC-METAL-1..4): arenas, the blocks allocated in them and the borrows lent over those
//! blocks, held in one table the kernel answers from. It is safe Rust: every access is a bounds-checked slice and every
//! size a checked sum. A step works on a staged copy of the table that takes hold when the step commits, so a step that
//! parks and replays allocates once and handle ids stay deterministic. Handles are sealed values only this module mints.

use super::*;
use orbital_core_base::os::OsOp;
use orbital_core_base::os_grants::{MemKind, OsRequirement};
use orbital_core_base::sync::Mutex;
use orbital_core_eval::evaluator::context::KernelMem;
use std::sync::atomic::{AtomicBool, Ordering};

const ARENA: &str = "Arena";
const BLOCK: &str = "Block";
const BORROW: &str = "Borrow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArenaKind {
    Bump,
    Stack,
    Shared,
}

impl ArenaKind {
    fn name(self) -> &'static str {
        match self {
            ArenaKind::Bump => "bump",
            ArenaKind::Stack => "stack",
            ArenaKind::Shared => "shared",
        }
    }
}

/// One block of a shared arena: its bytes behind a lock, so an atomic cell op is one critical section, and the freeze
/// flag every kernel holding the arena sees.
#[derive(Debug)]
struct SharedBlock {
    bytes: Mutex<Vec<u8>>,
    frozen: AtomicBool,
}

#[derive(Debug, Clone, Copy)]
enum Atomic {
    Load,
    Store(i64),
    Add(i64),
    Cas { expected: i64, new: i64 },
}

impl SharedBlock {
    fn new(size: usize) -> Self {
        Self { bytes: Mutex::new(vec![0; size]), frozen: AtomicBool::new(false) }
    }

    fn len(&self) -> usize {
        self.bytes.lock().len()
    }

    fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::SeqCst)
    }

    /// One op on the aligned 8-byte cell at `offset`, under the block's lock. A frozen block answers loads only.
    fn atomic(&self, offset: u64, op: Atomic) -> Result<Value, OsError> {
        let mut bytes = self.bytes.lock();
        if offset % 8 != 0 {
            return Err(refuse(OsErrorKind::Invalid, "an atomic cell is 8-byte aligned"));
        }
        let outside = || refuse(OsErrorKind::Invalid, "the atomic cell is outside the block");
        let start = usize::try_from(offset).map_err(|_| outside())?;
        let end = start.checked_add(8).ok_or_else(outside)?;
        let cell = bytes.get_mut(start..end).ok_or_else(outside)?;
        let current = i64::from_le_bytes(<[u8; 8]>::try_from(&*cell).map_err(|_| outside())?);
        if !matches!(op, Atomic::Load) && self.is_frozen() {
            return Err(refuse(OsErrorKind::Borrowed, "the block is frozen"));
        }
        match op {
            Atomic::Load => Ok(record([("value", Value::Int(current))])),
            Atomic::Store(value) => {
                cell.copy_from_slice(&value.to_le_bytes());
                Ok(record([]))
            }
            Atomic::Add(delta) => {
                let next = current.checked_add(delta).ok_or_else(|| refuse(OsErrorKind::Invalid, "the sum exceeds the int range"))?;
                cell.copy_from_slice(&next.to_le_bytes());
                Ok(record([("value", Value::Int(next))]))
            }
            Atomic::Cas { expected, new } => {
                let swapped = current == expected;
                if swapped {
                    cell.copy_from_slice(&new.to_le_bytes());
                }
                Ok(record([("value", Value::Int(current)), ("swapped", Value::Bool(swapped))]))
            }
        }
    }
}

/// The storage of a `shared` arena. The kernel that made it and every kernel it is handed to hold the same `Arc`, so a
/// frozen block is readable by all of them and an atomic cell op is seen by all of them at once. Blocks are keyed by
/// the id the owner minted, so a replayed step's allocation finds the block it made before.
#[derive(Debug)]
pub struct SharedArena {
    capacity: u64,
    /// Set once a guest kernel adopts the arena: from then every kernel reaches a committed block only by the frozen-or-atomics rule.
    handed: AtomicBool,
    blocks: Mutex<BTreeMap<i64, Arc<SharedBlock>>>,
}

impl SharedArena {
    fn new(capacity: u64) -> Self {
        Self { capacity, handed: AtomicBool::new(false), blocks: Mutex::new(BTreeMap::new()) }
    }

    fn publish(&self, id: i64, block: Arc<SharedBlock>) {
        self.blocks.lock().insert(id, block);
    }

    fn is_handed(&self) -> bool {
        self.handed.load(Ordering::SeqCst)
    }

    fn committed_blocks(&self) -> Vec<Arc<SharedBlock>> {
        self.blocks.lock().values().cloned().collect()
    }
}

/// The result of an atomic op as the journal records it.
fn encode_atomic(answer: Result<Value, OsError>) -> Value {
    match answer {
        Ok(value) => record([("ok", value)]),
        Err(error) => record([("error", Value::string(error.message)), ("kind", Value::string(error.kind.as_str()))]),
    }
}

fn decode_atomic(recorded: &Value) -> Result<Value, OsError> {
    if let Some(Value::String(message)) = recorded.get("error") {
        let kind = match recorded.get("kind") {
            Some(Value::String(kind)) if *kind == "borrowed" => OsErrorKind::Borrowed,
            Some(Value::String(kind)) if *kind == "closed" => OsErrorKind::Closed,
            Some(Value::String(kind)) if *kind == "denied" => OsErrorKind::Denied,
            _ => OsErrorKind::Invalid,
        };
        return Err(OsError::new(kind, message.to_string()));
    }
    recorded.get("ok").cloned().ok_or_else(|| refuse(OsErrorKind::Invalid, "the recorded atomic answer is malformed"))
}

fn span(length: usize, offset: u64, count: usize, what: &str) -> Result<std::ops::Range<usize>, OsError> {
    let outside = || refuse(OsErrorKind::Invalid, format!("the {what} is outside the block"));
    let start = usize::try_from(offset).map_err(|_| outside())?;
    let end = start.checked_add(count).ok_or_else(outside)?;
    if end > length {
        return Err(outside());
    }
    Ok(start..end)
}

#[derive(Debug, Clone)]
struct ArenaEntry {
    kind: ArenaKind,
    capacity: u64,
    used: u64,
    /// The dispatch a `stack` arena lives for.
    dispatch: Option<u64>,
    /// The storage of a `shared` arena.
    shared: Option<Arc<SharedArena>>,
    /// Held from another kernel: its committed blocks are reachable here by atomic cell ops and, once frozen, by reads;
    /// it takes no allocation, no exclusive borrow and no freeze.
    adopted: bool,
}

#[derive(Debug, Clone)]
enum BlockData {
    Local(Arc<Vec<u8>>),
    Shared(Arc<SharedBlock>),
}

#[derive(Debug, Clone)]
struct BlockEntry {
    arena: i64,
    data: BlockData,
    /// Read-only (a local block; a shared block's flag is in its storage, where every holder sees it).
    frozen: bool,
    /// Made by the running step in a shared arena: no other kernel sees it until the step commits.
    fresh: bool,
    /// The running step's writes and freeze of a committed shared block, applied to its storage when the step commits.
    pending: Pending,
}

#[derive(Debug, Clone, Default)]
struct Pending {
    writes: Vec<(usize, Vec<u8>)>,
    frozen: bool,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.writes.is_empty() && !self.frozen
    }
}

impl BlockEntry {
    fn is_frozen(&self) -> bool {
        match &self.data {
            BlockData::Local(_) => self.frozen,
            BlockData::Shared(block) => block.is_frozen() || self.pending.frozen,
        }
    }

    fn len(&self) -> usize {
        match &self.data {
            BlockData::Local(data) => data.len(),
            BlockData::Shared(block) => block.len(),
        }
    }
}

#[derive(Debug, Clone)]
struct BorrowEntry {
    block: i64,
    exclusive: bool,
}

/// Every live arena, block and borrow, with the one monotonic id sequence they are minted from.
#[derive(Debug, Clone, Default)]
pub(super) struct MemTable {
    arenas: BTreeMap<i64, ArenaEntry>,
    blocks: BTreeMap<i64, BlockEntry>,
    borrows: BTreeMap<i64, BorrowEntry>,
    next_id: i64,
}

pub(super) fn refuse(kind: OsErrorKind, message: impl Into<String>) -> OsError {
    OsError::new(kind, message)
}

pub(super) fn closed(what: &str, id: i64) -> OsError {
    refuse(OsErrorKind::Closed, format!("the {what} {id} is not live"))
}

fn record<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::object(fields.into_iter().map(|(key, value)| (key.into(), value)).collect::<ValueMap>())
}

pub(super) fn sealed<const N: usize>(kind: &'static str, fields: [(&str, Value); N]) -> Value {
    Value::sealed(kind, fields.into_iter().map(|(key, value)| (key.into(), value)).collect::<ValueMap>())
}

fn handle(value: Option<&Value>, kind: &str) -> Result<i64, OsError> {
    value
        .filter(|value| value.is_sealed_as(kind))
        .and_then(|value| value.get("id"))
        .and_then(Value::as_int)
        .ok_or_else(|| refuse(OsErrorKind::Invalid, format!("expects a live {kind} handle")))
}

pub(super) fn int(value: Option<&Value>, what: &str) -> Result<i64, OsError> {
    value.and_then(Value::as_int).ok_or_else(|| refuse(OsErrorKind::Invalid, format!("{what} must be an int")))
}

pub(super) fn non_negative(value: Option<&Value>, what: &str) -> Result<u64, OsError> {
    u64::try_from(int(value, what)?).map_err(|_| refuse(OsErrorKind::Invalid, format!("{what} must not be negative")))
}

pub(super) fn as_int64(value: u64) -> Result<i64, OsError> {
    i64::try_from(value).map_err(|_| refuse(OsErrorKind::Invalid, "the size exceeds the int range"))
}

impl MemTable {
    fn mint(&mut self) -> Result<i64, OsError> {
        let id = self.next_id.checked_add(1).ok_or_else(|| refuse(OsErrorKind::Invalid, "the handle ids are exhausted"))?;
        self.next_id = id;
        Ok(id)
    }

    fn borrows_of(&self, block: i64) -> impl Iterator<Item = &BorrowEntry> {
        self.borrows.values().filter(move |borrow| borrow.block == block)
    }

    fn block_borrowed(&self, block: i64) -> bool {
        self.borrows_of(block).next().is_some()
    }

    fn arena_borrowed(&self, arena: i64) -> bool {
        self.blocks.iter().any(|(id, block)| block.arena == arena && self.block_borrowed(*id))
    }

    fn arena_new(&mut self, kind: ArenaKind, capacity: u64, dispatch: Option<u64>) -> Result<Value, OsError> {
        let capacity_int = as_int64(capacity)?;
        let id = self.mint()?;
        let shared = (kind == ArenaKind::Shared).then(|| Arc::new(SharedArena::new(capacity)));
        self.arenas.insert(id, ArenaEntry { kind, capacity, used: 0, dispatch, shared, adopted: false });
        Ok(record([(
            "arena",
            sealed(
                ARENA,
                [("id", Value::Int(id)), ("kind", Value::string(kind.name())), ("capacity", Value::Int(capacity_int))],
            ),
        )]))
    }

    fn arena_drop(&mut self, id: i64) -> Result<Value, OsError> {
        if !self.arenas.contains_key(&id) {
            return Err(closed("arena", id));
        }
        if self.arena_borrowed(id) {
            return Err(refuse(OsErrorKind::Borrowed, format!("the arena {id} has a block with a live borrow")));
        }
        self.discard_arena(id);
        Ok(record([]))
    }

    fn discard_arena(&mut self, id: i64) {
        self.arenas.remove(&id);
        let dead: Vec<i64> = self.blocks.iter().filter(|(_, block)| block.arena == id).map(|(block, _)| *block).collect();
        for block in dead {
            self.blocks.remove(&block);
            self.borrows.retain(|_, borrow| borrow.block != block);
        }
    }

    fn alloc(&mut self, arena: i64, length: u64, align: u64) -> Result<Value, OsError> {
        if !matches!(align, 1 | 2 | 4 | 8 | 16) {
            return Err(refuse(OsErrorKind::Invalid, "align must be 1, 2, 4, 8 or 16"));
        }
        let entry = self.arenas.get(&arena).ok_or_else(|| closed("arena", arena))?;
        if entry.adopted {
            return Err(refuse(OsErrorKind::Invalid, format!("the arena {arena} is held from another kernel and takes no allocation")));
        }
        let shared = entry.shared.clone();
        let room = || refuse(OsErrorKind::Invalid, format!("the arena {arena} has no room for {length} bytes at align {align}"));
        let start = entry.used.checked_add(align - 1).ok_or_else(room)? / align * align;
        let end = start.checked_add(length).ok_or_else(room)?;
        if end > entry.capacity {
            return Err(room());
        }
        let size = usize::try_from(length).map_err(|_| room())?;
        let length_int = as_int64(length)?;
        let id = self.mint()?;
        if let Some(entry) = self.arenas.get_mut(&arena) {
            entry.used = end;
        }
        let fresh = shared.is_some();
        let data = match shared {
            Some(_) => BlockData::Shared(Arc::new(SharedBlock::new(size))),
            None => BlockData::Local(Arc::new(vec![0; size])),
        };
        self.blocks.insert(id, BlockEntry { arena, data, frozen: false, fresh, pending: Pending::default() });
        Ok(record([(
            "block",
            sealed(BLOCK, [("id", Value::Int(id)), ("arena", Value::Int(arena)), ("length", Value::Int(length_int))]),
        )]))
    }

    fn free(&mut self, id: i64) -> Result<Value, OsError> {
        if !self.blocks.contains_key(&id) {
            return Err(closed("block", id));
        }
        if self.block_borrowed(id) {
            return Err(refuse(OsErrorKind::Borrowed, format!("the block {id} has a live borrow")));
        }
        self.blocks.remove(&id);
        Ok(record([]))
    }

    /// A committed, unfrozen shared block of an arena a guest has adopted.
    fn block_handed_live(&self, block: i64) -> bool {
        self.blocks.get(&block).is_some_and(|entry| {
            matches!(entry.data, BlockData::Shared(_))
                && !entry.fresh
                && !entry.is_frozen()
                && self.arenas.get(&entry.arena).and_then(|arena| arena.shared.as_ref()).is_some_and(|shared| shared.is_handed())
        })
    }

    fn block_adopted(&self, block: i64) -> bool {
        self.blocks
            .get(&block)
            .and_then(|entry| self.arenas.get(&entry.arena))
            .is_some_and(|arena| arena.adopted)
    }

    fn borrow(&mut self, block: i64, exclusive: bool) -> Result<Value, OsError> {
        if !self.blocks.contains_key(&block) {
            return Err(closed("block", block));
        }
        if self.block_handed_live(block) {
            // Once a guest can reach it, a live shared block changes under atomics, so a plain read would not replay.
            return Err(refuse(OsErrorKind::Invalid, "a live shared block is reached only through mem/atomic-*; freeze it to read it"));
        }
        if exclusive && self.block_adopted(block) {
            return Err(refuse(OsErrorKind::Borrowed, format!("the block {block} is held from another kernel and takes no exclusive borrow")));
        }
        if exclusive && self.blocks.get(&block).is_some_and(BlockEntry::is_frozen) {
            return Err(refuse(OsErrorKind::Borrowed, format!("the block {block} is frozen")));
        }
        let held_exclusive = self.borrows_of(block).any(|borrow| borrow.exclusive);
        if held_exclusive || (exclusive && self.block_borrowed(block)) {
            return Err(refuse(OsErrorKind::Borrowed, format!("the block {block} is already borrowed")));
        }
        let id = self.mint()?;
        self.borrows.insert(id, BorrowEntry { block, exclusive });
        let mode = if exclusive { "exclusive" } else { "shared" };
        Ok(record([(
            "borrow",
            sealed(BORROW, [("id", Value::Int(id)), ("block", Value::Int(block)), ("mode", Value::string(mode))]),
        )]))
    }

    fn release(&mut self, id: i64) -> Result<Value, OsError> {
        self.borrows.remove(&id).ok_or_else(|| closed("borrow", id))?;
        Ok(record([]))
    }

    fn write(&mut self, borrow: i64, offset: u64, data: &[u8]) -> Result<Value, OsError> {
        let entry = self.borrows.get(&borrow).ok_or_else(|| closed("borrow", borrow))?;
        if !entry.exclusive {
            return Err(refuse(OsErrorKind::Borrowed, format!("the borrow {borrow} is shared and cannot write")));
        }
        let block = self.blocks.get_mut(&entry.block).ok_or_else(|| closed("block", entry.block))?;
        if block.is_frozen() {
            return Err(refuse(OsErrorKind::Borrowed, format!("the block {} is frozen", entry.block)));
        }
        match &mut block.data {
            BlockData::Local(bytes) => {
                let range = span(bytes.len(), offset, data.len(), "write")?;
                Arc::make_mut(bytes).get_mut(range).ok_or_else(|| refuse(OsErrorKind::Invalid, "the write is outside the block"))?.copy_from_slice(data);
            }
            BlockData::Shared(shared) if block.fresh => {
                let mut bytes = shared.bytes.lock();
                let range = span(bytes.len(), offset, data.len(), "write")?;
                bytes.get_mut(range).ok_or_else(|| refuse(OsErrorKind::Invalid, "the write is outside the block"))?.copy_from_slice(data);
            }
            BlockData::Shared(_) if self.arenas.get(&block.arena).and_then(|arena| arena.shared.as_ref()).is_some_and(|arena| arena.is_handed()) => {
                return Err(refuse(OsErrorKind::Invalid, "a live shared block is reached only through mem/atomic-*; freeze it to read it"));
            }
            BlockData::Shared(shared) => {
                let range = span(shared.len(), offset, data.len(), "write")?;
                block.pending.writes.push((range.start, data.to_vec()));
            }
        }
        Ok(record([]))
    }

    fn read(&self, borrow: i64, offset: u64, length: u64) -> Result<Vec<u8>, OsError> {
        let entry = self.borrows.get(&borrow).ok_or_else(|| closed("borrow", borrow))?;
        let block = self.blocks.get(&entry.block).ok_or_else(|| closed("block", entry.block))?;
        let count = usize::try_from(length).map_err(|_| refuse(OsErrorKind::Invalid, "the read is outside the block"))?;
        let outside = || refuse(OsErrorKind::Invalid, "the read is outside the block");
        match &block.data {
            BlockData::Local(bytes) => {
                let range = span(bytes.len(), offset, count, "read")?;
                bytes.get(range).map(<[u8]>::to_vec).ok_or_else(outside)
            }
            BlockData::Shared(shared) => {
                let mut whole = shared.bytes.lock().clone();
                for (start, data) in &block.pending.writes {
                    if let Some(target) = whole.get_mut(*start..*start + data.len()) {
                        target.copy_from_slice(data);
                    }
                }
                let range = span(whole.len(), offset, count, "read")?;
                whole.get(range).map(<[u8]>::to_vec).ok_or_else(outside)
            }
        }
    }

    fn length(&self, value: &Value) -> Result<i64, OsError> {
        let block = if value.is_sealed_as(BORROW) {
            let id = handle(Some(value), BORROW)?;
            self.borrows.get(&id).ok_or_else(|| closed("borrow", id))?.block
        } else {
            handle(Some(value), BLOCK)?
        };
        let entry = self.blocks.get(&block).ok_or_else(|| closed("block", block))?;
        as_int64(entry.len() as u64)
    }

    /// Make a block read-only. A shared block's flag is set in its storage at once, so every kernel holding the arena
    /// sees it.
    fn freeze(&mut self, id: i64) -> Result<Value, OsError> {
        if self.borrows_of(id).any(|borrow| borrow.exclusive) {
            return Err(refuse(OsErrorKind::Borrowed, format!("the block {id} has a live exclusive borrow")));
        }
        if self.block_adopted(id) {
            return Err(refuse(OsErrorKind::Borrowed, format!("the block {id} is held from another kernel and cannot be frozen here")));
        }
        let block = self.blocks.get_mut(&id).ok_or_else(|| closed("block", id))?;
        match &block.data {
            BlockData::Local(_) => block.frozen = true,
            BlockData::Shared(shared) if block.fresh => shared.frozen.store(true, Ordering::SeqCst),
            BlockData::Shared(_) => block.pending.frozen = true,
        }
        Ok(record([]))
    }

    /// A step committed: its new shared blocks become visible in their arenas and its writes and freezes take hold in
    /// the shared storage.
    fn publish(&mut self) {
        for (id, block) in &mut self.blocks {
            let BlockData::Shared(shared) = &block.data else { continue };
            if block.fresh {
                if let Some(arena) = self.arenas.get(&block.arena).and_then(|arena| arena.shared.as_ref()) {
                    arena.publish(*id, Arc::clone(shared));
                }
                block.fresh = false;
            }
            if block.pending.is_empty() {
                continue;
            }
            let mut bytes = shared.bytes.lock();
            for (start, data) in std::mem::take(&mut block.pending.writes) {
                if let Some(target) = bytes.get_mut(start..start + data.len()) {
                    target.copy_from_slice(&data);
                }
            }
            if std::mem::take(&mut block.pending.frozen) {
                shared.frozen.store(true, Ordering::SeqCst);
            }
        }
    }

    fn shared_block(&self, id: i64) -> Result<Arc<SharedBlock>, OsError> {
        let entry = self.blocks.get(&id).ok_or_else(|| closed("block", id))?;
        if entry.fresh || !entry.pending.is_empty() {
            return Err(refuse(
                OsErrorKind::Invalid,
                "the block is not committed yet; atomics act on committed shared blocks",
            ));
        }
        match &entry.data {
            BlockData::Shared(block) => Ok(Arc::clone(block)),
            BlockData::Local(_) => Err(refuse(OsErrorKind::Invalid, "an atomic op needs a block of a shared arena")),
        }
    }

    /// Hold `arena` from another kernel: an arena entry with no room to allocate and an entry for each of its committed
    /// blocks. The handle lists those blocks, in the order the owner made them.
    fn adopt(&mut self, arena: Arc<SharedArena>) -> Result<Value, OsError> {
        let capacity = arena.capacity;
        let capacity_int = as_int64(capacity)?;
        let id = self.mint()?;
        arena.handed.store(true, Ordering::SeqCst);
        let mut handles = Vec::new();
        for block in arena.committed_blocks() {
            let block_id = self.mint()?;
            let length = as_int64(block.len() as u64)?;
            self.blocks.insert(block_id, BlockEntry { arena: id, data: BlockData::Shared(block), frozen: false, fresh: false, pending: Pending::default() });
            handles.push(sealed(
                BLOCK,
                [("id", Value::Int(block_id)), ("arena", Value::Int(id)), ("length", Value::Int(length))],
            ));
        }
        self.arenas.insert(
            id,
            ArenaEntry { kind: ArenaKind::Shared, capacity, used: capacity, dispatch: None, shared: Some(arena), adopted: true },
        );
        Ok(sealed(
            ARENA,
            [
                ("id", Value::Int(id)),
                ("kind", Value::string(ArenaKind::Shared.name())),
                ("capacity", Value::Int(capacity_int)),
                ("blocks", Value::Array(handles.into())),
            ],
        ))
    }

    fn end_dispatch(&mut self, dispatch: u64) {
        let dead: Vec<i64> = self
            .arenas
            .iter()
            .filter(|(_, arena)| arena.kind == ArenaKind::Stack && arena.dispatch == Some(dispatch))
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.discard_arena(id);
        }
    }
}

/// What a pure `mem/*` read sees: the running step's staged table when it has one, else the committed one.
pub(super) struct StepMem {
    committed: Arc<RwLock<MemTable>>,
    staged: Arc<RwLock<Option<MemTable>>>,
}

impl StepMem {
    pub(super) fn new(committed: Arc<RwLock<MemTable>>, staged: Arc<RwLock<Option<MemTable>>>) -> Self {
        Self { committed, staged }
    }

    fn seen<R>(&self, f: impl FnOnce(&MemTable) -> R) -> R {
        match &*self.staged.read() {
            Some(table) => f(table),
            None => f(&self.committed.read()),
        }
    }
}

impl std::fmt::Debug for StepMem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepMem").finish_non_exhaustive()
    }
}

fn eval_failure(op: &str, error: OsError) -> EvalError {
    EvalError::InvalidArgument(format!("{op}: {}", error.message))
}

impl KernelMem for StepMem {
    fn read(&self, borrow: &Value, offset: i64, length: i64) -> Result<Vec<u8>, EvalError> {
        let read = || -> Result<Vec<u8>, OsError> {
            let id = handle(Some(borrow), BORROW)?;
            let offset = non_negative(Some(&Value::Int(offset)), "offset")?;
            let length = non_negative(Some(&Value::Int(length)), "length")?;
            self.seen(|table| table.read(id, offset, length))
        };
        read().map_err(|error| eval_failure("mem/read", error))
    }

    fn length(&self, handle: &Value) -> Result<i64, EvalError> {
        self.seen(|table| table.length(handle)).map_err(|error| eval_failure("mem/length", error))
    }
}

impl RuntimeKernel {
    /// The memory a step's pure `mem/*` reads see.
    pub(super) fn kernel_mem(&self) -> Arc<dyn KernelMem> {
        self.mem_view.clone()
    }

    /// The bytes of the block a live Borrow lends, as the running step sees them, for an `ffi/call` pointer argument or a `gpu/write` payload;
    /// `exclusive` demands an exclusive borrow (a `mutptr`).
    pub(super) fn borrow_bytes(&self, borrow: &Value, exclusive: bool) -> Result<Vec<u8>, OsError> {
        let id = handle(Some(borrow), BORROW)?;
        self.mem_seen(|table| {
            let entry = table.borrows.get(&id).ok_or_else(|| closed("borrow", id))?;
            if exclusive && !entry.exclusive {
                return Err(refuse(OsErrorKind::Invalid, format!("the borrow {id} is shared and a mutptr needs an exclusive one")));
            }
            let length = u64::try_from(table.length(borrow)?).map_err(|_| refuse(OsErrorKind::Invalid, "the block length is negative"))?;
            table.read(id, 0, length)
        })
    }

    /// Write the whole block a `mutptr` borrow lends, staged with the step exactly as `mem/write` stages it.
    pub(super) fn ffi_write_back(&self, borrow: &Value, bytes: &[u8]) -> Result<(), OsError> {
        let id = handle(Some(borrow), BORROW)?;
        self.with_staged_mem(|table| {
            let length = table.length(borrow)?;
            if usize::try_from(length).ok() != Some(bytes.len()) {
                return Err(refuse(OsErrorKind::Io, "the returned buffer is not the length of its block"));
            }
            table.write(id, 0, bytes).map(drop)
        })
    }

    fn with_staged_mem<R>(&self, f: impl FnOnce(&mut MemTable) -> R) -> R {
        let mut staged = self.step.mem_change.write();
        f(staged.get_or_insert_with(|| self.mem.read().clone()))
    }

    /// End the step: its `mem/*` changes take hold when `committed`, and are dropped otherwise.
    pub(super) fn settle_mem(&self, committed: bool) {
        let staged = self.step.mem_change.write().take();
        if let (true, Some(mut table)) = (committed, staged) {
            table.publish();
            *self.mem.write() = table;
        }
    }

    fn mem_seen<R>(&self, f: impl FnOnce(&MemTable) -> R) -> R {
        self.mem_view.seen(f)
    }

    /// The storage of the `shared` arena a live Arena handle names, as the running step sees it: the one `Arc` a kernel
    /// handed to a guest holds.
    pub fn shared_arena_for(&self, arena: &Value) -> Option<Arc<SharedArena>> {
        let id = handle(Some(arena), ARENA).ok()?;
        self.mem_seen(|table| table.arenas.get(&id).and_then(|arena| arena.shared.clone()))
    }

    /// Hold a shared arena another kernel made: the Arena handle of this kernel's table for it, carrying the handles of
    /// its frozen blocks in a `blocks` field.
    pub fn adopt_shared_arena(&self, arena: Arc<SharedArena>) -> Result<Value, OsError> {
        self.mem.write().adopt(arena)
    }

    /// A dispatch ended: every `stack` arena it created, with its blocks and borrows, is freed.
    pub(super) fn end_dispatch_mem(&self, dispatch: u64) {
        if self.mem.read().arenas.values().any(|arena| arena.dispatch == Some(dispatch)) {
            self.mem.write().end_dispatch(dispatch);
        }
    }

    /// Answer a `mem/*` op (EXEC-METAL-1): the kernel's own table answers it in every build, never a host. `None` for an
    /// op outside the family.
    pub(super) fn mem_os(&self, op: OsOp, args: &[Value], orbital: Option<&str>) -> Option<Result<Value, OsError>> {
        if !matches!(
            op,
            OsOp::ArenaNew
                | OsOp::ArenaDrop
                | OsOp::Alloc
                | OsOp::Free
                | OsOp::Borrow
                | OsOp::Release
                | OsOp::MemWrite
                | OsOp::Freeze
                | OsOp::AtomicLoad
                | OsOp::AtomicStore
                | OsOp::AtomicAdd
                | OsOp::AtomicCas
        ) {
            return None;
        }
        let answer = self.answer_mem(op, args, orbital);
        Some(answer.map_err(|error| OsError::new(error.kind, format!("{}: {}", op.name(), error.message))))
    }

    fn mem_granted(&self, orbital: Option<&str>, need: &OsRequirement) -> Result<(), OsError> {
        if self.grants.os_grants.read().allows(orbital, need) {
            return Ok(());
        }
        Err(refuse(OsErrorKind::Denied, "no mem grant covers it"))
    }

    /// Atomics act at once on committed shared blocks only; a block the step made or touched is refused (`invalid`).
    /// An atomic cell op acts on shared memory at once, so it is a journaled kernel answer: a replayed step returns the
    /// recorded answer and never runs the op again.
    fn answer_atomic(&self, op: OsOp, args: &[Value], orbital: Option<&str>) -> Result<Value, OsError> {
        let block = handle(args.first(), BLOCK)?;
        let offset = non_negative(args.get(1), "the offset")?;
        let action = match op {
            OsOp::AtomicLoad => Atomic::Load,
            OsOp::AtomicStore => Atomic::Store(int(args.get(2), "the value")?),
            OsOp::AtomicAdd => Atomic::Add(int(args.get(2), "the delta")?),
            OsOp::AtomicCas => Atomic::Cas { expected: int(args.get(2), "expected")?, new: int(args.get(3), "new")? },
            _ => return Err(refuse(OsErrorKind::Unsupported, "not an atomic op")),
        };
        self.mem_granted(orbital, &OsRequirement::Mem { kind: MemKind::Atomic, capacity: None })?;
        let shared = self.mem_seen(|table| table.shared_block(block))?;
        let recorded = self
            .io
            .sealed(op.name(), args, &mut |_| encode_atomic(shared.atomic(offset, action)))
            .map_err(|message| OsError::new(OsErrorKind::Io, message))?;
        decode_atomic(&recorded)
    }

    fn answer_mem(&self, op: OsOp, args: &[Value], orbital: Option<&str>) -> Result<Value, OsError> {
        match op {
            OsOp::ArenaNew => {
                let kind = match args.first() {
                    Some(Value::String(kind)) if *kind == "bump" => ArenaKind::Bump,
                    Some(Value::String(kind)) if *kind == "stack" => ArenaKind::Stack,
                    Some(Value::String(kind)) if *kind == "shared" => ArenaKind::Shared,
                    _ => return Err(refuse(OsErrorKind::Invalid, "the kind must be \"bump\", \"stack\" or \"shared\"")),
                };
                let capacity = non_negative(args.get(1), "the capacity")?;
                match kind {
                    ArenaKind::Shared => self.mem_granted(orbital, &OsRequirement::MemShared { capacity: Some(capacity) })?,
                    _ => self.mem_granted(orbital, &OsRequirement::Mem { kind: MemKind::Create, capacity: Some(capacity) })?,
                }
                let dispatch = match kind {
                    ArenaKind::Stack => Some(
                        self.held_dispatch()
                            .ok_or_else(|| refuse(OsErrorKind::Invalid, "a stack arena needs a running dispatch"))?,
                    ),
                    ArenaKind::Bump | ArenaKind::Shared => None,
                };
                self.with_staged_mem(|table| table.arena_new(kind, capacity, dispatch))
            }
            OsOp::ArenaDrop => {
                let arena = handle(args.first(), ARENA)?;
                self.mem_granted(orbital, &OsRequirement::Mem { kind: MemKind::Free, capacity: None })?;
                self.with_staged_mem(|table| table.arena_drop(arena))
            }
            OsOp::Alloc => {
                let arena = handle(args.first(), ARENA)?;
                let length = non_negative(args.get(1), "the length")?;
                let align = non_negative(args.get(2), "the align")?;
                self.with_staged_mem(|table| table.alloc(arena, length, align))
            }
            OsOp::Free => {
                let block = handle(args.first(), BLOCK)?;
                self.with_staged_mem(|table| table.free(block))
            }
            OsOp::Borrow => {
                let block = handle(args.first(), BLOCK)?;
                let exclusive = match args.get(1) {
                    Some(Value::String(mode)) if *mode == "shared" => false,
                    Some(Value::String(mode)) if *mode == "exclusive" => true,
                    _ => return Err(refuse(OsErrorKind::Invalid, "the mode must be \"shared\" or \"exclusive\"")),
                };
                self.with_staged_mem(|table| table.borrow(block, exclusive))
            }
            OsOp::Release => {
                let borrow = handle(args.first(), BORROW)?;
                self.with_staged_mem(|table| table.release(borrow))
            }
            OsOp::MemWrite => {
                let borrow = handle(args.first(), BORROW)?;
                let offset = non_negative(args.get(1), "the offset")?;
                let data = args
                    .get(2)
                    .ok_or_else(|| refuse(OsErrorKind::Invalid, "the data must be bytes"))?
                    .bytes_view()
                    .map_err(|e| refuse(OsErrorKind::Invalid, format!("the data must be bytes: {e}")))?;
                self.with_staged_mem(|table| table.write(borrow, offset, &data))
            }
            OsOp::Freeze => {
                let block = handle(args.first(), BLOCK)?;
                self.with_staged_mem(|table| table.freeze(block))
            }
            OsOp::AtomicLoad | OsOp::AtomicStore | OsOp::AtomicAdd | OsOp::AtomicCas => self.answer_atomic(op, args, orbital),
            _ => Err(refuse(OsErrorKind::Unsupported, "not a mem op")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step that makes a fresh shared block, writes it while it is step-private, and either commits or fails: the
    /// staged table replaces the committed one only on commit, as `settle_mem` does.
    fn step(committed: &mut MemTable, commits: bool) -> i64 {
        let mut staged = committed.clone();
        let arena = staged.arena_new(ArenaKind::Shared, 64, None).unwrap();
        let arena_id = handle(arena.get("arena"), ARENA).unwrap();
        let block = staged.alloc(arena_id, 16, 8).unwrap();
        let block_id = handle(block.get("block"), BLOCK).unwrap();
        let borrow = staged.borrow(block_id, true).expect("a fresh shared block takes an exclusive borrow");
        let borrow_id = handle(borrow.get("borrow"), BORROW).unwrap();
        staged.write(borrow_id, 0, &5i64.to_le_bytes()).unwrap();
        staged.release(borrow_id).unwrap();
        if commits {
            staged.publish();
            *committed = staged;
        }
        block_id
    }

    #[test]
    fn a_failed_step_leaves_no_fresh_shared_block_or_write_behind_and_a_committed_one_is_visible() {
        let mut failed = MemTable::default();
        let block = step(&mut failed, false);
        assert_eq!(failed.shared_block(block).unwrap_err().kind, OsErrorKind::Closed, "the failed step's block does not exist");
        let mut committed = MemTable::default();
        let block = step(&mut committed, true);
        let cell = committed.shared_block(block).expect("the committed block is reachable").atomic(0, Atomic::Load).unwrap();
        assert_eq!(cell.get("value"), Some(&Value::Int(5)), "the write made in the step is the committed cell");
        committed.borrow(block, false).expect("control: no guest holds the arena, so the owner still borrows it");
        let arena = committed.blocks.get(&block).and_then(|entry| committed.arenas.get(&entry.arena)).and_then(|arena| arena.shared.clone()).unwrap();
        MemTable::default().adopt(arena).unwrap();
        let borrow = committed.borrow(block, false).unwrap_err();
        assert_eq!(borrow.kind, OsErrorKind::Invalid, "once a guest holds the arena a live block is reached only through atomics");
    }
}
